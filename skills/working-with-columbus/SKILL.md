---
name: working-with-columbus
description: How to work through a governed GIS server such as Columbus, from the first whoami call to describe, sample and query a dataset, pass ws:// handles between tools, read checks, wait for approvals and handle refusals or hidden fields. Use it at the start of any GIS session with Columbus tools, or when someone asks to "query a layer", "pull features", "run a query on roads", "why was this refused" or "what do these checks mean".
---

# Working with Columbus

Columbus is a governed GIS server. The agent can ask for anything; the server decides,
and enforces its rules in its own code. This skill is advice on working well inside those
rules. It never replaces them, and nothing here is a way around them.

## General practice (with or without Columbus)

These habits apply to any GIS data source: a database, a file share, an API.

1. **Know who you act for and what is allowed** before touching data. Which sources can
   you read? Can you edit anything? Is there a row limit? Are there organization rules,
   such as "deliverables in EPSG:3081"?
2. **Read what the organization already wrote down** about a dataset before you query it:
   the data dictionary, the gotchas, coded domains, the owner. Guessing what `STATUS = 'R'`
   or `FUNC_CLASS = 3` means is how wrong answers get made.
3. **Describe, then sample, then query.** Look at the fields, geometry type and CRS. Then
   look at a few real rows to see how codes are spelled, whether keys are padded, and where
   the nulls are. Only then write the filter.
4. **Prefer structured queries to free SQL.** Name fields and give a filter. The less SQL
   you write by hand, the less there is to get wrong.
5. **Move data as files, not pasted text.** Keep each intermediate result as a layer and
   pass a reference to it to the next step. Do not paste thousands of rows into a prompt.
6. **Read every warning and say what it means.** A result with a warning is not a success.
   Report it with the warning, in plain words.
7. **Ask before data leaves.** Uploading, emailing or writing governed data somewhere else
   needs the person's say-so.
8. **A refusal is an answer.** Read why, fix the request if the reason is about the data,
   and tell the person if the reason is a permission. Do not look for another way in.

## If the Columbus tools are available

Every Columbus tool returns the same envelope:

```json
{ "result": { ... }, "checks": [ {"id": "...", "severity": "info|warn|fail", "message": "...", "detail": {...}, "fix": "..."} ], "provenance": "<id or null>" }
```

Refusals come back as a tool error whose text says why, for example
`Refused (crs.measure): cannot buffer by 100 ft: the layer is in EPSG:4326 (WGS 84), whose
units are degrees Fix: Reproject first ...`.

Full argument lists are in [references/columbus-tools.md](references/columbus-tools.md);
every check id is in [references/check-ids.md](references/check-ids.md).

### 1. Start with `whoami`

Call `whoami` once, first, with no arguments. Read and keep:

- `policy.edits_allowed`, `policy.scripts_allowed`: phase 1 is read-only by default.
- `policy.rows_returned_per_call` (at most 500), `policy.rows_read_per_query` (default
  200,000), `policy.query_seconds`.
- `connections[]`: each `id`, `type`, `mode` (`read` or `read-write`), `status` (`ok` or why
  not) and `engine_ready`. A connection with `engine_ready: false` (`sde` through ArcPy
  today) passes checks but cannot be read yet.
- `always_on_rules`: the context pack's rules. Follow them for the whole session. Example
  from the sample pack: "Leave retired segments (STATUS = 'R') out of current-state work."
- `approvals`: how the person will be asked. One of
  `in your AI client, as a prompt`,
  `on the Columbus approvals page, opened in the browser`, or
  `nowhere: this client cannot show prompts and policy turns the approvals page off`.
  In the last case, anything that needs approval cannot happen in this session; say so up front.
- `labels`: data labels already in force (`connection:roads-prod`). While any label is
  active, writes and uploads to connected servers need approval.
- `notes`: setup warnings (for example, dev secrets mode or an audit-folder fallback).

The person can change connections and the edit and script switches in the Columbus app's
Settings while you work; the change applies from your next call, with no restart. When
they say they changed something (or a call is refused for a connection or switch you
expected), call `whoami` again rather than relying on the first answer. Never ask them to
restart for a Settings change. If every tool is refused with "settings could not be read"
or "only when Columbus starts", tell them exactly that: they fix the file in Settings, or
restart their AI app after changing servers, plugins or hosts. A change set from
`propose_edits` made before a settings change must be proposed again.

### 2. Look up context before an unfamiliar dataset

```json
context_search {"query": "roadway inventory", "kind": "dataset"}
context_get    {"id": "roadway-inventory"}
describe_data_group {"id": "live-grid-linework"}
```

- `kind` is one of `dataset`, `domain`, `lrs`, `data-group`, `delivery`, `sop`, `role`,
  `glossary`, `note`. `limit` defaults to 10, at most 30.
- Cards carry owner and `last_confirmed`. Checks to act on: `context.stale` (past the
  review interval: confirm details with the owner), `context.draft` (warn: no one has
  reviewed it), `context.retired` (warn: look for the replacement), `context.no_match`.
- `describe_data_group` marks each dataset `usable_here: true|false`;
  `context.datasets_unavailable` says how many you cannot read in this session.
- If no pack is installed, these tools are refused with "no context pack is installed";
  carry on, but ask the person about codes you cannot interpret.

### 3. Find the data

```json
list_connections {}
describe_connection {"connection": "austin"}
list_datasets {"connection": "austin"}
```

`describe_connection` shows the `row_filter` the server adds to every read. Do not repeat it
in your own `where`. `list_datasets` counts datasets you may not read (`not_allowed`) but
never names them.

### 4. Describe, sample, query

```json
describe_dataset {"connection": "austin", "dataset": "roads"}
```

Returns `fields`, `geometry_column`, `geometry_type`, `crs` (`code`, `name`, `geographic`,
`units`, `metres_per_unit`, `area_of_use`), `extent`, `rows` (under the row filter) and, when
the pack has an entry, `context` with its card and body. When a row filter applies, `extent`
is null with an `extent_note`, because the stored extent covers rows you cannot see.

```json
sample_rows {"connection": "austin", "dataset": "roads", "n": 20}
```

`n` defaults to 10, at most 50. Look at the actual values: `'IH0035'` or `'IH 35'`? `'014'`
or `14`? Nulls or empty strings?

```json
query_features {
  "connection": "austin", "dataset": "roads",
  "fields": ["route_id", "district", "func_class"],
  "where": "district = 14 AND status <> 'R'",
  "bbox": [-97.805, 30.19, -97.775, 30.31], "bbox_crs": "EPSG:4326",
  "order_by": "route_id", "limit": 5000
}
```

- `where` allows comparisons, AND/OR/NOT, IN, LIKE, BETWEEN, IS NULL and simple functions
  (UPPER, TRIM, SUBSTR, LPAD, CAST, COALESCE and similar). No subqueries.
- `bbox` is `[min x, min y, max x, max y]`. `bbox_crs` must be an `EPSG:` code; without it
  the box is read in the dataset's own CRS. A lon/lat box needs `"bbox_crs": "EPSG:4326"`.
- The geometry column is always included. `limit` cannot exceed the read limit.
- The result has `rows` (the first rows only), `rows_read`, `rows_returned`, `executed_sql`
  and `layer.handle`, a `ws://` handle to every row read.

Use `sql_select {"connection", "sql"}` only for what `query_features` cannot do: aggregates,
GROUP BY, computed columns, joins inside the database. It accepts exactly one `SELECT` (no
`WITH`, no `SELECT INTO`, no system catalogs). Larger results go to a CSV in the workspace and
come back as `handle`.

### 5. Pass handles, not data

Handles look like `ws://<session>/results/roads-<id>.gpkg`. Pass them as inputs to
`vector_op`, `verify` or `run_tool`:

```json
vector_op {"op": "buffer", "inputs": {"layer": "ws://<session>/results/roads-<id>.gpkg"},
           "params": {"distance": 100, "unit": "ft"}}
```

`vector_op` and `verify` also take a dataset directly, as `"austin:roads"` or
`{"connection": "austin", "dataset": "roads", "where": "district = 14"}`. A dataset with more
rows than the read limit is refused rather than processed partly; narrow it with `where`.

The GIS tools only open layers Columbus or an approved engine plugin wrote in this session,
and only if the file is unchanged. A file saved from a connected server's result is refused
("came from a connected server"). Load that data through a connection instead.

### 6. Read `checks` on every result

- `info`: context. Mention it when it matters (`query.row_filter`, `crs.agreement`).
- `warn`: something is probably off. Report it with the number from the message, and the
  `fix` if one is given. Never call the result "done" or "successful" without it.
- `fail`: very likely wrong. Stop, tell the person what failed and why, and do not hand the
  result on. Per the server, fail blocks exports and edits.

The ones you will see most: `filter.zero_rows`, `filter.all_rows`, `query.read_limit`,
`input.truncated`, `crs.area_of_use`, `geometry.valid`, `join.unmatched`,
`join.many_to_many`. The full list with severities is in
[references/check-ids.md](references/check-ids.md).

Example of reporting honestly:

> Buffered 412 road segments by 100 ft (EPSG:3081). One warning: `geometry.valid` reports
> 3 of 412 input geometries invalid (self-intersection). The buffer ran, but I suggest
> `vector_op make_valid` first and re-running before you use the result.

### 7. Approvals

Most Columbus tools never ask: `vector_op` writes only to the workspace. Approval is needed
when `run_tool` calls a connected server tool that writes or sends data out while governed
data is in the session, when the reviewer marked a tool as needing approval, or when an
engine plugin tool changes things.

What happens:

1. Columbus asks the person in their own client (elicitation). The agent never sees or
   answers that prompt.
2. If the client cannot ask, Columbus opens its approvals page and waits briefly
   (45 s by default).
3. If no decision arrives, `run_tool` returns `result.status: "approval_required"` with
   `result.approval` = `{id, summary, expires, next}`.

Then:

- Tell the person what is waiting (use `summary`) and where (from `whoami.approvals`).
- Poll with `approval_status {"id": "<id>"}`. States: `pending`, `approved`, `declined`,
  `used`, `expired`, `unknown`. This tool cannot approve anything.
- Once `approved`, call `run_tool` again with the same `name` and `args`, plus
  `"approval_id": "<id>"`.
- The approval is bound to each file's SHA-256 and size and to the data labels. If you
  change a file, the old approval no longer applies and the person is asked again.
- If the person declines, the call fails with "the person declined: ...". Do not re-ask
  with different wording.

### 8. Refusals

Read the reason and act on what it says:

| Refusal text (excerpt) | What to do |
| --- | --- |
| `dataset "x" is not in this connection's allowed datasets` | Tell the person. Access is set by policy, not by you. |
| `Refused (crs.measure): cannot buffer ... degrees` | Reproject to a projected CRS, then retry (see the crs-and-datums skill). |
| `... has more rows than <conn>'s read limit ...` | Narrow with `where` or `bbox`. |
| `Refused (output.too_large): join would produce N rows ...` | Deduplicate or aggregate the right side first. |
| `... has no field "x" (or it is hidden by organization policy)` | Check `describe_dataset`; if it is not listed, it is hidden. Stop asking for it. |
| `organization flow rule N forbids sending data from ... to ...` | Tell the person. It cannot be approved. |
| `reading sqlserver connections is not supported yet` | Not available yet; say so. |

Never retry a refusal by rewording it: no quoting tricks, aliases, subqueries, other
spellings of a table name, a different tool that might read the same thing, or asking the
person to paste the data in. Columbus logs refusals too.

### 9. Hidden fields

Organization policy can hide fields (`hide_fields`). They are removed from
`describe_dataset` and `list_datasets`, stripped from every result, and every table a query
reads is rewritten so a hidden field is simply not there. `SELECT *`, `to_jsonb(t)` or
`USING` joins do not reach it. When a result had hidden fields, a `policy.hidden_fields`
info check names them.

Do not try to query them, infer them, or rebuild them from other fields. If the task needs a
hidden field, tell the person it is hidden by organization policy and that the data owner or
IT decides.

### 10. Provenance and outputs

Calls that produce data (`query_features`, `sql_select`, `vector_op`, `verify`, `run_tool`)
return a `provenance` id. The record holds the inputs, the executed SQL, the parameters,
the checks and the output handles. Quote provenance ids when you report results so the
person can trace them.

`list_outputs {}` lists the workspace's `results` and `external` files with `handle`,
`bytes`, `made_by`, `rows`, `labels` and `provenance`. Use it to find a layer from earlier in
the session instead of re-running a query. `read_output {handle, offset, limit}` pages
through one. `export_output` copies one out to an export folder `whoami` lists, after the
full checks; a `fail` anywhere in the layer's history blocks it unless the person approves
an override (see the maps-and-deliverables skill).

### 11. Edits

Columbus edits in two steps, and only GeoPackage and PostGIS datasets so far.

1. `propose_edits` with `connection`, `dataset` and `changes`, each one of
   `{"op": "update", "key": <primary key>, "values": {...}, "geometry": <GeoJSON>}`
   (values, geometry or both), `{"op": "insert", "values": {...}, "geometry": <GeoJSON>}` or
   `{"op": "delete", "key": <primary key>}`. Geometries are GeoJSON in EPSG:4326 unless you
   pass `crs`; the server reprojects them, checks validity and type, and refuses rows outside
   the connection's row filter, hidden or unknown fields, the key, and values of the wrong
   type. It writes nothing to the source: it returns a `change` handle, a `changes` list
   (before → after) and a `preview` layer.
2. Show the person the `changes` list in plain words, then `apply_edits` with the `change`
   handle and `target`:
   - `"copy"`: a new workspace layer with the edits made. No approval, the source untouched.
     Use it to check the result, or when edits to the source are not allowed.
   - `"source"`: writes the dataset. Needs `policy.edits_allowed`, a `read-write` connection
     and the person's approval of that exact change set (every time). It is one transaction:
     if a row changed since you proposed, or a change would move a row outside the row filter,
     nothing is written and you must propose again.

Never describe edits as made until `apply_edits` returns `"status": "written"` (source) or
`"applied_to_copy"` (copy), and say which.

### Worked session (illustrative values)

```text
whoami                                   -> rules: deliverables in EPSG:3081; approvals: in your AI client
context_search {"query": "roadway inventory"}  -> card roadway-inventory (CSJ is 9 chars, zero-padded)
describe_dataset {"connection": "roads-prod", "dataset": "ROADS.ROADWAY_INVENTORY"}
                                         -> crs EPSG:3081, 18,204 rows under row filter
sample_rows {..., "n": 10}               -> STATUS values 'A' and 'R'; CSJ like '001501077'
query_features {..., "where": "STATUS = 'A' AND FUNC_CLASS IN (1, 2)"}
                                         -> 2,310 rows, handle ws://.../roadway_inventory-<id>.gpkg
verify {"layer": "ws://.../roadway_inventory-<id>.gpkg"}
                                         -> crs.info; geometry.valid warn 2 of 2310 invalid
report: counts, the warning and its fix, provenance ids
```

### What to tell the person at the end

- What you did, in steps, with row counts.
- Every warn and fail check, in plain words, with its fix.
- Anything refused and why.
- The output handles and provenance ids.
