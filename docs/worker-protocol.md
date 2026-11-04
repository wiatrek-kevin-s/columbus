# Worker protocol v2 and engine plugins

Engine workers are child processes of the gateway. They speak line-delimited JSON-RPC 2.0 over stdin/stdout, one JSON object per line, UTF-8, and log only to stderr. Columbus's own engine, third-party engine plugins and the RustGIS worker all speak this protocol, built on the Python SDK (`workers/python/columbus_plugin`) or the Rust SDK (`workers/sdk-rust`, crate `columbus-plugin`).

A worker never receives credentials, except Columbus's own core engine, which is the component that opens governed connections. Plugins receive datasets as **extracts**: files the gateway has already produced through the governed read path (allow lists, row filters, hidden fields, read-only session).

## Messages

| Message | Direction | Kind | Purpose |
| --- | --- | --- | --- |
| `describe` | gateway → worker | request | Return the plugin's tools; must match `plugin.yaml` |
| `run` | gateway → worker | request | Run one tool with resolved inputs and output paths |
| `cancel` | gateway → worker | request | Stop a run; the worker must finish cleanup within 10 seconds |
| `progress` | worker → gateway | notification | Percent and message for a running job |
| `log` | worker → gateway | notification | A diagnostic line (never data) |

Requests carry an `id`; notifications do not. A worker must keep reading stdin while a run executes, so that `cancel` can arrive. Workers handle one `run` at a time; the gateway serializes runs per worker.

### `describe`

Request: `{"jsonrpc":"2.0","id":1,"method":"describe","params":{}}`

Result:

```json
{
  "protocol": 2,
  "plugin": {"id": "contoso.lrs-check", "version": "1.2.0"},
  "tools": [
    {"name": "check_events", "params": ["events", "routes", "tolerance"], "outputs": ["findings", "report"]}
  ]
}
```

The gateway compares tool names, parameter names and output names with `plugin.yaml`; any difference disables the plugin.

### `run`

Request params:

```json
{
  "run": "r_8f2a",
  "tool": "check_events",
  "params": {"tolerance": 0.001},
  "inputs": {
    "events": {"kind": "dataset", "path": "C:/…/inputs/r_8f2a-events.sqlite", "rows": 1834}
  },
  "outputs": {
    "findings": {"type": "layer", "path": "C:/…/results/r_8f2a-findings.geojson"},
    "report":   {"type": "table", "path": "C:/…/results/r_8f2a-report.csv"}
  },
  "workspace": "C:/…/workspaces/<session>",
  "timeout_s": 120
}
```

* `params` holds scalar parameters, already validated against `plugin.yaml` (types, ranges, enums) with defaults filled in.
* `inputs` holds datasets (as extracts) and workspace files (`{"kind":"file","path":…}`).
* `outputs` holds the paths the tool must write. Writing anywhere else is a protocol violation.

Result:

```json
{
  "outputs": {"findings": {"features": 12}, "report": {"rows": 12}},
  "warnings": [{"id": "lrs.gap", "severity": "warn", "message": "3 gaps on IH0035"}],
  "summary": {"events_checked": 1834}
}
```

`warnings` use the verification report format (design section 4): a stable `id`, a `severity` of `fail`, `warn` or `info`, a plain `message`, optional `detail` and `fix`.

### `progress` and `log`

`{"jsonrpc":"2.0","method":"progress","params":{"run":"r_8f2a","percent":40,"message":"Checked 700 of 1834 events"}}`

`percent` may be `null` when unknown. The gateway forwards progress to the MCP client when the client asked for it.

### `cancel`

Request params: `{"run": "r_8f2a"}`. The worker acknowledges at once (`{}`), stops the run, and the run's own request then fails with code `-32004`. A worker that has not finished the run 10 seconds after `cancel` is killed.

### Errors

| Code | Meaning |
| --- | --- |
| -32700, -32601, -32602 | JSON-RPC parse error, unknown method, invalid params |
| -32001 | Unsupported (engine or feature not available) |
| -32002 | Engine error |
| -32003 | Timeout |
| -32004 | Cancelled |

## Extracts

An extract is a SQLite file, opened read-only by the plugin:

* table `data`: the rows, columns as in the source (hidden fields already removed). Geometry is stored as a BLOB.
* table `columbus_extract(key TEXT PRIMARY KEY, value TEXT)`: `connection`, `dataset`, `rows`, `geometry_column`, `geometry_format` (`gpkg` or `ewkb`), `srid`, `labels` (JSON list).

The SDKs read extracts for plugins and decode geometry to WKB.

## `plugin.yaml`

```yaml
id: contoso.lrs-check          # lower case, dots allowed; becomes the tool namespace
name: LRS event checker
version: 1.2.0
runtime: python                # python | rust
entry: worker.py               # python: script; rust: executable (".exe" added on Windows)
engine_env: columbus-py        # the managed pixi environment, or arcpy
tools:
  check_events:
    summary: Check route events for gaps, overlaps and out-of-range measures.
    params:
      events:    { type: dataset, access: read }
      routes:    { type: dataset, access: read }
      tolerance: { type: number, unit: mile, default: 0.001, min: 0 }
    outputs:
      findings: { type: layer, geometry: point }
      report:   { type: table }
    risk: read
    checks: [geometry.valid, lrs.measures]
```

Parameter types: `dataset` (`access: read`), `number` and `integer` (`unit`, `default`, `min`, `max`), `string` (`enum`, `default`), `boolean` (`default`), `file` (a workspace handle). Output types: `layer` (GeoJSON), `table` (CSV), `file` (`format`).

The agent passes a dataset as `"<connection>:<dataset>"`, for example `"roads-prod:ROADS.ROADWAY_INVENTORY"`, or as an object `{"connection": …, "dataset": …, "where": …}`. The gateway checks it against that connection's permissions, applies the row filter and the optional `where` (checked like any where clause), extracts it, and adds the connection's data label to the session and to every output of the run.
