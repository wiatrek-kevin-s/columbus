# Columbus

Columbus makes a GIS analyst, paired with whatever AI their organization already licenses, faster and more trustworthy at real work on real data. This repository holds the core of it: **`columbus-mcp`, a governed GIS MCP host** that any MCP agent (Copilot, Claude, Codex, Gemini CLI, Cursor) can use. It runs its own GIS engines and connects to other MCP servers (Drive, notes, vendor APIs), and every capability, native or connected, passes the same permissions, approvals, data-flow rules, audit and provenance, enforced in its own code.

> **Status: early, phase 1.** The governance core and the first read-only tools work end to end on PostGIS, GeoPackage and SQLite. Oracle, SQL Server, enterprise geodatabases through ArcPy, geoprocessing, verification checks, the approvals UI and the desktop app come next. See [Roadmap](#roadmap).

## Principles in code

- **Rules live in the server, not the prompt.** The agent can ask for anything; the server decides. It has no tool that reads or changes policy or connections.
- **Read-only by default.** Edits stay off until organization policy turns them on; until then every connection is read-only whatever it says.
- **Access is an intersection.** Organization policy ∩ connection ∩ data group ∩ project. Each layer can only narrow the one above.
- **Show your work.** Every result carries `result`, `checks` and `provenance`; every call writes one line to a hash-chained audit log, including refusals.

## What works today

| Tool | Does |
| --- | --- |
| `whoami` | Permissions in force, connections and their modes, workspace, engines. Call first. |
| `search_tools`, `describe_tool` | Find the right tool by describing the job |
| `list_connections`, `describe_connection` | Connections, effective mode, allowed datasets, row filter, limits. Never secrets. |
| `list_datasets` | Allowed tables with geometry type, SRS, extent and fields (hidden fields removed) |
| `sql_select` | One checked, read-only `SELECT` (details below) |
| `list_providers` | Columbus's engines and connected MCP servers, their trust and state, and the data labels in force |
| `run_tool` | Run a connected server's tool as `<provider>.<tool>`, under the rules below |
| `list_outputs` | Workspace files as handles (`ws://<session>/<path>`), with row counts, labels and provenance |
| `approval_status` | Check an approval the agent is waiting on; it cannot approve anything |

**How `sql_select` is checked.** The SQL must parse, in the connection's dialect (Oracle, PostgreSQL, SQL Server, SQLite, DuckDB), as exactly one `SELECT`. Every table, including those in subqueries, must be on the connection's allow list and off its deny list; quoting does not get around this. System catalogs (`pg_catalog`, `information_schema`, `sys`, Oracle's `DBA_`/`ALL_`/`V$` views, `sqlite_master`) are refused even under an allow-everything pattern. `WITH`, `SELECT INTO`, `FOR UPDATE`, table functions and functions that reach files, the network or server internals (`pg_*`, `dbms_*`, `xp_*`, `load_extension`, `read_csv`…) are refused. The server then rewrites every table reference into a filtered derived table, so the connection's row filter and data-group filters always apply, and adds a row limit with one sentinel row so truncation is detected and reported rather than returned as a complete result. The worker opens the source read-only as well: SQLite files in read-only mode, PostgreSQL sessions with `default_transaction_read_only` and a statement timeout, so even a `SELECT` that calls a writing function is stopped by the database. Anything that does not parse is refused, never passed through.

**Results.** At most 500 rows go back to the AI provider per call (organization policy can lower this). Larger results are written to the session workspace and the path returned. Fields matching the policy's `hide_fields` are stripped before anything leaves the server.

## Engine plugins

GIS tools that touch an organization's data are written as **engine plugins**: a folder with a `plugin.yaml` and a worker built on the Columbus plugin SDK, for Python ([`workers/python/columbus_plugin`](workers/python/columbus_plugin)) or Rust ([`workers/sdk-rust`](workers/sdk-rust)). They speak [worker protocol v2](docs/worker-protocol.md), the same protocol Columbus's own engine now uses. Typed parameters tell the gateway what each argument is, so it governs them argument by argument:

- A **dataset** argument (`"lrs:events"`, or `{connection, dataset, where}`) is read by the gateway through that connection's permissions, row filter and hidden fields, and handed to the plugin as a read-only extract. Plugins never receive credentials, and never a dataset cut off at the read limit.
- Outputs go only to workspace paths the gateway assigns, and carry the inputs' data labels, so Rule 3 follows them.
- A plugin runs only after a person approves its exact contents (`columbus-mcp plugins approve <id>`); any change to `plugin.yaml` or the worker switches it off, and a worker whose own description differs from its `plugin.yaml` is refused.

The Rust SDK's [`geometry_stats`](workers/sdk-rust/examples/geometry_stats.rs) example reads governed geometry (GeoPackage or PostGIS) and reports extents and validity. [`examples/plugins/lrs-check`](examples/plugins/lrs-check) is the LRS event checker from design change 1, implemented: gaps, overlaps, reversed and out-of-range measures, and events on unknown routes, with a findings layer and a report table. Agents call plugin tools through `run_tool` as `<plugin id>.<tool>`, for example `contoso.lrs-check.check_events`.

## Connected MCP servers

Columbus is an MCP client to other servers too (design change 1, "governed MCP host"). Each one needs a **Columbus manifest**, written or approved by a person or IT, under `%ProgramData%\Columbus\servers\` (personal manifests under `%APPDATA%` only if policy allows). The manifest decides everything: which tools are on, their risk, whether they send data out, which arguments are files, paths or URLs, and when to ask. The server's own hints are shown at review and never trusted. See [`examples/servers`](examples/servers).

| Rule | What Columbus does |
| --- | --- |
| 1. New and changed tools start off | Every tool definition is hashed at approval. On each connect the gateway re-hashes; a changed description or schema, or a new tool, is switched off until a person reviews it |
| 2. Data moves as files | File arguments take workspace handles only; the gateway sends the file's bytes. Results marked `to_workspace` are saved as files and a handle comes back |
| 3. Data-flow approvals | Reading a governed connection labels the session (`connection:roads-prod`) and its files. While a label is active, any write or upload to a connected server needs the person's approval, whatever its manifest says; organization `flows` rules can forbid it outright |
| 4. External output is untrusted | Connected servers' results are wrapped as data, never instructions |
| 5. Local servers run confined | **Not in this build yet.** Until it is, local (stdio) servers stay blocked unless policy sets `servers.allow_unconfined_local` for a pilot; they get a scrubbed environment and the workspace as their folder. Remote servers must be on the policy's host list |

Review is done by the person at the keyboard:

```powershell
columbus-mcp servers list                 # each server and its state (trial, ready, needs review, blocked)
columbus-mcp servers review drive         # tools, the manifest's settings, the server's claims, and flags
columbus-mcp servers approve drive        # pins the definitions; asks you to type the id on the terminal
```

Review flags descriptions with hidden characters, wording aimed at the model ("ignore previous", "do not tell the user"), references to other providers' or Columbus's tools, credentials and destinations. Flagged tools need `--accept-flags`.

When a call needs approval, Columbus asks the person in their own MCP client (elicitation): the agent never sees or answers that prompt. Clients that cannot ask get `approval_required` with an id; the approvals page that lets the person answer those is the next build.

Not yet: OAuth sign-in for remote servers, Windows confinement levels A–C, the engine plugin SDK and RustGIS worker (milestones 1–2 of change 1).

## Connections and secrets

Each connection is one YAML file the person adds (see [`examples/connections`](examples/connections)); the agent has no tool to add or change one. Database passwords are never in those files: store them once in Windows Credential Manager with

```powershell
columbus-mcp secret set gis-postgis      # prompts twice, nothing echoed
columbus-mcp secret delete gis-postgis
```

A connection with no stored secret uses integrated sign-in. The gateway reads the secret only to open a connection, hands it to the engine worker inside that one request, and never returns it to the agent; worker error messages are scrubbed of it. `whoami` reports which secret store is in use.

| Engine | Connection `type` | Status |
| --- | --- | --- |
| GeoPackage, SQLite | `gpkg`, `sqlite` | Works (standard library) |
| PostgreSQL / PostGIS | `postgis` | Works (needs `psycopg` in the engine environment) |
| SQL Server, Oracle, enterprise geodatabase | `sqlserver`, `oracle`, `sde` | Configuration and query checking work; engines next |

For development and CI only, `COLUMBUS_DEV_SECRETS=1` reads passwords from `COLUMBUS_SECRET_<ID>` environment variables instead; `whoami` flags it. Platforms other than Windows have only this mode for now.

## Layout

```text
crates/shared     policy layers, query checking, approvals, sandbox, audit (no I/O to agents)
crates/broker     connected MCP servers: manifests, client sessions, tool pinning, review flags
crates/flow       data labels and organization flow rules
crates/gateway    columbus-mcp: MCP server (rmcp), tools, worker supervision, plugins, run_tool
workers/python    the core engine and the Python plugin SDK (columbus_plugin)
workers/sdk-rust  the Rust plugin SDK (columbus-plugin)
docs              worker protocol v2 and plugin.yaml
examples          a sample policy, connections, server manifests and the LRS checker plugin
```

## Build and run

Requires Rust (stable, 1.85+) and Python 3.12+ (plus `pip install "psycopg[binary]"` for PostGIS).

```powershell
cargo build --release
.\target\release\columbus-mcp.exe --version
```

Add it to an MCP client. In VS Code with Copilot (`.vscode/mcp.json`):

```json
{
  "servers": {
    "columbus": {
      "command": "C:\\path\\to\\columbus-mcp.exe",
      "env": { "COLUMBUS_PYTHON": "C:\\path\\to\\python.exe" }
    }
  }
}
```

Claude Desktop's `claude_desktop_config.json` takes the same entry under `mcpServers` instead of `servers`.

### Where it looks

| What | Windows default | Override |
| --- | --- | --- |
| Organization policy | `%ProgramData%\Columbus\policy.yaml` | `COLUMBUS_POLICY` |
| Connections (one YAML each) | `%APPDATA%\Columbus\connections\` | `COLUMBUS_CONNECTIONS` |
| Session workspaces | `%LOCALAPPDATA%\Columbus\workspaces\<session>` | `COLUMBUS_WORKSPACES` |
| Audit log | `%ProgramData%\Columbus\audit\YYYY-MM.jsonl` | `COLUMBUS_AUDIT_DIR` |
| Engine worker | `workers\python\columbus_worker.py` beside the executable | `COLUMBUS_WORKER` |
| Python | `python.exe` on PATH | `COLUMBUS_PYTHON` |

On Linux and macOS the defaults are `/etc/columbus` and the XDG folders. Without a policy file the server runs with safe defaults: local GeoPackage and SQLite only, read-only, no scripts, no edits. See [`examples/`](examples) for a policy and connections.

If the audit folder is not writable (a per-user install without the installer's permissions), the server falls back to a folder under the user's profile and says so in `whoami`. A call that cannot be audited does not run.

### Check the audit log

```powershell
columbus-mcp audit verify "%ProgramData%\Columbus\audit\2026-11.jsonl"
```

Each line records user, client, tool, connection, tables, row count, decision and a hash of the exact request; never row data. Editing, removing or reordering a line breaks the chain at that line.

## Tests

```powershell
cargo test --workspace                                   # unit tests + an end-to-end MCP session
python -m unittest discover -s workers/python/tests      # worker
```

PostGIS tests run when `COLUMBUS_TEST_PG_HOST` (and optionally `_PORT`, `_DB`) is set and [`tests/fixtures/postgis.sql`](crates/gateway/tests/fixtures/postgis.sql) is loaded; CI runs them against a PostGIS container. They connect with a login that *can* write, to show writes are refused anyway.

The broker tests run the gateway against a scriptable MCP server ([`examples/fake_mcp_server.rs`](crates/gateway/examples/fake_mcp_server.rs)) and cover the change-1 gates: a changed description is switched off, a poisoned one is flagged, governed data cannot reach a connected server without the person's approval or past a deny rule. The end-to-end test starts the real binary as an MCP client would, with a policy, a GeoPackage connection and the Python worker, and checks over the wire that row filters apply, hidden fields never leave, denied tables and writes are refused, truncation is reported and the audit chain verifies.

## Roadmap

Phase 1 (this repository, in progress):

- [x] Four-layer permission model, query checking, sandbox, approvals store, hash-chained audit log
- [x] MCP server over stdio with structured output; Python worker; GeoPackage/SQLite engine
- [ ] `query_features` (structured queries) and `describe_dataset`, `sample_rows`
- [x] PostGIS engine in read-only sessions; system catalogs refused on every engine
- [x] Secrets in Windows Credential Manager (`columbus-mcp secret set`)
- [ ] SQL Server (ODBC 18, `ApplicationIntent=ReadOnly`) and Oracle (`python-oracledb` thin) engines
- [ ] Streamable HTTP on 127.0.0.1 with a per-session token
- [x] Broker for connected MCP servers (stdio and HTTP): manifests, pinning with auto-disable, review flags, file handles, untrusted output, data labels and flow rules (change 1, Rules 1–4)
- [x] Approvals in the person's own client through MCP elicitation
- [ ] Local approvals page for clients without elicitation
- [x] Worker protocol v2 with progress and cancel; Python and Rust plugin SDKs; Columbus's engine rebuilt on it; engine plugins through `run_tool`
- [ ] RustGIS native worker
- [ ] Confinement levels A–C on Windows; OAuth for remote servers
- [ ] `vector_op`, `raster_op`, `pointcloud_pipeline` through GDAL/PDAL in a pixi environment
- [ ] Verification checks (design section 4) attached to every result
- [ ] ArcPy worker for enterprise geodatabases; signed `.mcpb` bundle and MSI

Companion repositories: [`geo-registry`](https://github.com/wiatrek-kevin-s/geo-registry) (open catalogue of GIS tools and data sources). Planned: `gis-context-pack` (format for an organization's datasets, rules and SOPs) and `columbus-skills` (agent skills for sound GIS practice).

## Honest limits

The server's checks are a second line of defence. The guarantee nothing bypasses is the database's own users, roles and grants: give Columbus a read-only database login. Anyone with direct credentials and other tools can go around Columbus.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
