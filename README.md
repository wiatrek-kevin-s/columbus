# Columbus

Columbus makes a GIS analyst, paired with whatever AI their organization already licenses, faster and more trustworthy at real work on real data. This repository holds the core of it: **`columbus-mcp`, a governed GIS MCP host** that any MCP agent (Copilot, Claude, Codex, Gemini CLI, Cursor) can use. It runs its own GIS engines and connects to other MCP servers (Drive, notes, vendor APIs), and every capability, native or connected, passes the same permissions, approvals, data-flow rules, audit and provenance, enforced in its own code.

> **Status: early, phase 1.** The governance core and the first read-only tools work end to end on PostGIS, Oracle, SQL Server, GeoPackage and SQLite. Enterprise geodatabases through ArcPy, geoprocessing, verification checks, the approvals UI and the desktop app come next. See [Roadmap](#roadmap).

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
| `describe_dataset` | Fields, geometry column and type, CRS facts (code, units, projected or not, area of use), extent, and the row count under the row filter; for a raster, bands (data type, nodata), CRS facts, cell size, size, extent, tiling and overviews |
| `sample_rows` | Up to 50 rows, to see real values (codes, padding, nulls) before filtering |
| `query_features` | A structured query (fields, where, bbox in any EPSG CRS, order_by, limit) saved as a layer; returns the first rows and a handle |
| `vector_op` | reproject, buffer, simplify, make_valid, clip, dissolve, intersect, join, spatial_join on handles or governed datasets |
| `raster_op` | clip, reproject, slope, aspect, hillshade, zonal_stats and info on GeoTIFFs from a `rasters` connection or earlier results; outputs are Cloud-Optimized GeoTIFFs |
| `verify` | The spatial checks on any layer or dataset |
| `sql_select` | One checked, read-only `SELECT` (details below) |
| `context_search`, `context_get`, `describe_data_group` | The organization's context pack: what its datasets, codes, routes and procedures mean; `describe_dataset` attaches the dataset's entry, and `whoami` returns the pack's always-on rules. The person's own notes are read beside it |
| `draft_notes`, `update_notes` | The person's own notes about a connection, drafted from its database (fields, keys, relationships, comments, codes) and filled in with what the data means, each change approved by the person |
| `registry_search`, `registry_get` | The open geospatial registry: data sources (publisher, coverage, licence, access) and GIS tools; ranked cards by words, kind, theme and area |
| `registry_fetch` | An entry's open data downloaded into the workspace as a layer (OGC API Features, ArcGIS REST, GeoJSON), within a bbox; approval for hosts not on `fetch_hosts` |
| `list_providers` | Columbus's engines and connected MCP servers, their trust and state, and the data labels in force |
| `run_tool` | Run a connected server's tool as `<provider>.<tool>`, under the rules below |
| `list_outputs` | Workspace files as handles (`ws://<session>/<path>`), with row counts, labels and provenance |
| `read_output` | Rows of a workspace layer, a page at a time |
| `export_output` | A layer copied to one of the organization's export folders, in a delivery format, after the full checks (below) |
| `propose_edits` | A change set (updates, insertions, deletions by primary key) checked against the connection's rules, with before → after for the person and a preview layer; writes nothing to the source |
| `apply_edits` | A change set applied to a workspace copy, or to the source after the person approves it (details below) |
| `run_python` | A Python script on workspace layers, for what the GIS tools cannot do: the person reads the full code and approves every run; it runs confined, with no network, writing only its own run folder; files it leaves in `out/` come back as layers (details below). Needs policy `allow_scripts` |
| `approval_status` | Check an approval the agent is waiting on; it cannot approve anything |

**How `sql_select` is checked.** The SQL must parse, in the connection's dialect (Oracle, PostgreSQL, SQL Server, SQLite, DuckDB), as exactly one `SELECT`. Every table, including those in subqueries, must be on the connection's allow list and off its deny list; quoting does not get around this. System catalogs (`pg_catalog`, `information_schema`, `sys`, Oracle's `DBA_`/`ALL_`/`V$` views, `sqlite_master`) are refused even under an allow-everything pattern. `WITH`, `SELECT INTO`, `FOR UPDATE` and table functions are refused. Only ordinary scalar, aggregate and window functions and the database's own spatial functions may be called (an allow list; on PostgreSQL a spatial function counts only when the PostGIS extension installed it, whatever its name; on Oracle only Oracle's built-ins, never a name Oracle lacks (which would resolve to whatever function or synonym someone created), and Oracle Spatial's `SDO_RELATE`, `SDO_FILTER`, `SDO_WITHIN_DISTANCE`, `SDO_GEOM.*` and a few `SDO_UTIL` conversions, always rewritten to be called in `MDSYS`; on SQL Server only unqualified built-ins, so geometry methods such as `geom.STIntersects(...)` and `geometry::Point(...)` are refused, because the parser cannot tell `col.Method()` from `schema.function()`; GeoPackage and SQLite files are opened without spatial functions; there, and for SQL Server geometry, spatial questions go to `query_features` and `vector_op`). `TABLESAMPLE` is refused rather than silently dropped. Custom operators and casts to types the database defined are refused: functions that run SQL given to them as text (`query_to_xml`, `ts_stat`), read a table by name (`table_to_xml`, `ST_EstimatedExtent`), or reach files, the network or server internals would read outside every check, and the database's own functions can do anything. The server then rewrites every table reference into a filtered derived table, so the connection's row filter and data-group filters always apply, and adds a row limit with one sentinel row so truncation is detected and reported rather than returned as a complete result. The worker opens the source read-only as well: SQLite files in read-only mode, PostgreSQL sessions with `default_transaction_read_only` and a statement timeout, so even a `SELECT` that calls a writing function is stopped by the database. Oracle sessions are set read-only (`READ_ONLY` on 23ai and later), cannot commit from PL/SQL (`DISABLE COMMIT IN PROCEDURE`, which also stops an autonomous transaction in a function a query calls) and run `SET TRANSACTION READ ONLY` with a call timeout. SQL Server has no read-only session: each query runs with `ApplicationIntent=ReadOnly` and a query timeout inside a transaction that is always rolled back, so a read-only login is the real guarantee there. Anything that does not parse is refused, never passed through, and the SQL that runs must read back, parsed and written out again, as exactly itself (the parser drops the quotes of a type modifier and does not escape `]` in a `[name]`, which could otherwise turn a value into SQL no check saw).

**GIS tools.** `query_features` builds its SQL from named fields (checked against the dataset, hidden fields refused) and a `where` clause that may use only comparisons, AND/OR/NOT, IN, LIKE, BETWEEN, IS NULL and simple functions, then runs it through the same checks as `sql_select`. Every result is a layer in the session workspace (GeoPackage, or CSV without geometry), passed to the next tool as a `ws://` handle, labelled with where its data came from, and recorded with a provenance file. `vector_op` reads a dataset input through the same permissions and refuses one cut off at the read limit, because processing part of a dataset gives wrong answers silently; a layer that was cut off when it was made carries an `input.truncated` warning into everything built from it. The GIS tools open only layers Columbus's own engine, an approved engine plugin or an approved script wrote, and check each one by its content, never its name: a file saved from a connected server's result is refused, and GDAL formats that can point at other files or URLs (VRT and the like) are switched off in the engine. Every result carries checks (design section 4), for example:

| Check | Catches |
| --- | --- |
| `crs.measure` | Buffering or simplifying in degrees; refused, with the projected CRS to use for the data's area |
| `crs.area_of_use` | Coordinates outside the CRS's area, including swapped X/Y and feet read as degrees |
| `crs.agreement` | Inputs in different CRSs (aligned to the first, and said so) |
| `join.unmatched`, `join.many_to_many`, `join.key_types` | Keys that do not match (with the padding or spacing-and-case fix when that is why: join again with `key_as_text`, `pad`, `trim`, `ignore_case`), a number key joined to a text key, and joins that multiply rows |
| `filter.zero_rows`, `filter.all_rows` | A filter that keeps nothing, or everything |
| `geometry.valid`, `geometry.empty`, `geometry.duplicates`, `geometry.mixed_types` | Bad or surprising geometry |
| `query.read_limit`, `input.truncated` | A result cut off at the limit, and anything built from one |
| `raster.nodata`, `crs.geographic` | A raster with no nodata value (fill values read as data), or in degrees (terrain needs a projected CRS) |
| `raster.resampling` | An integer raster (classes, codes) resampled with bilinear, cubic or average |
| `zonal.outside`, `zonal.no_pixels`, `zonal.cell_size` | Zones off the raster, zones with no valid cell, and cells too large for the zones (median under 10 cells) |
| `output.too_large` | A raster output over 400 million pixels, or the read limit x 1,000 if smaller; refused, with the fix to clip first |

Severities are `info`, `warn` and `fail`. A `fail` means the result should not be used as it is.

**Rasters.** A `rasters` connection is a folder of GeoTIFFs; a dataset is a file's path in it with forward slashes (`dem/travis_1m.tif`), under the connection's allow and deny lists. The gateway refuses names with `..`, an absolute path, `\` or `:`, resolves the name inside the folder after following every link and requires the result to be the very file the name spells (so a symlink or junction cannot lead out of the folder, or to a file the lists do not name), and checks its first bytes are TIFF's: GDAL picks a driver from a file's content, so a ".tif" holding VRT XML could otherwise read any file. The worker checks the bytes again, opens it with the GTiff driver only, never with open options or `/vsi` paths, and keeps VRT switched off. `raster_op` takes those files (`"dem:dem/travis_1m.tif"`) or the handles of earlier raster results, and layers (masks, zones) as `vector_op` does. `reproject` needs an explicit `resampling`; `slope`, `aspect` and `hillshade` need the elevations' `z_unit` (the z-factor comes from the CRS's horizontal units) and are refused in degrees or Web Mercator; `zonal_stats` writes the zones with their statistics as a layer. Outputs are Cloud-Optimized GeoTIFFs (DEFLATE) in `results/`, labelled with the connections they came from and checked for CRS and nodata. The size limit is 400 million pixels per output (and per read for `zonal_stats`), or the read limit (`limits.rows`, the tighter of the organization's and each raster connection's) times 1,000 if that is smaller. `export_output` exports vector layers only for now.

**Exports.** `export_output` writes only to the folders the organization lists in policy (`exports: [{id, path, granted}]`), under a plain file name. Before writing it runs the layer's checks, the delivery spec's (`delivery: <context pack entry>`: CRS, field renames, formats, required metadata) and looks back through every step the layer was built from: a `fail` anywhere, or a layer cut off at a row limit, blocks the export unless the person approves an override. A folder marked `granted` needs no approval otherwise; any other folder, replacing a file, or a flow rule (`to: [folder:<id>]`) asks the person. The approval is bound to the request and the exact source content. Files are created new, never written through an existing link, and `<name>.prov.json` beside them records the sources, steps, checks, metadata and approval.

**Scripts.** `run_python` is off unless organization policy sets `allow_scripts: true`, and every run needs the person's approval: the card (in the client's own prompt or on the approvals page) shows the full code, the inputs and the data labels in play, and the approval is bound to the code's SHA-256, each input's content hash and the time limit. There is no "allow for this session" for scripts. The script runs only confined at level A through `columbus-confine` (AppContainer and a job object on Windows; Landlock and seccomp on Linux): its own run folder under `runs/` is the only folder it can write, the Python installation the only other one it can read, it has no network, and its memory (`plugins.memory_mb`) and processes are capped. If the machine cannot confine it, it does not run, whatever `servers.allow_unconfined_local` says. Its environment is cleared (no secrets, no `COLUMBUS_*` settings, PROJ and GDAL network and VRT drivers off). Inputs are workspace files a Columbus tool recorded in this session (never a connected server's), copied into `in/` with their content checked against what was recorded. The time limit (default 120 s, at most `limits.seconds`) ends the script and everything it started. Every plain file it leaves directly in `out/` is copied, without following links, to a new file in `results/` and recorded with the hash of the bytes copied; links, folders and oversized files are skipped and reported (`script.output_skipped`). Outputs carry the inputs' and the session's data labels, the inputs' fails and truncation, and the run's own fails (`script.exit_code`, `script.timeout`), so a failed run blocks export. GeoPackage and CSV outputs are checked as layers. stdout and stderr come back, 64 KB each. Provenance records the code, the inputs and their hashes, the exit code, the approval and the outputs. The engine's Python has geopandas, shapely, pyproj, rasterio and pandas; ArcPy is not available.

**Edits.** GeoPackage and PostGIS datasets, in two steps. `propose_edits` reads the touched rows with the same checked SELECT as every read (the connection's row filter applied), refuses rows it does not return, fields hidden by policy or unknown, the key, the geometry column as a plain value, values of the wrong type, and invalid or wrongly typed geometry (GeoJSON, reprojected to the dataset's CRS); it saves the change set (with a fingerprint of each row as read) and a preview layer to the workspace. `apply_edits` with `target: "copy"` writes a new workspace layer and touches nothing else. With `target: "source"` it needs organization policy's `allow_edits: true`, a connection whose effective mode is `read-write`, an unchanged change set (SHA-256) made in this session, and the person's approval of that exact change set, every time (the card lists each change). The write is one transaction: each touched row is read again, locked where the database can, and must match its fingerprint; each statement is limited to its key and the row filter and must touch exactly one row; each row written must still be inside the row filter afterwards; values travel only as query parameters. Anything else rolls everything back. A change set is used once; the audit log and provenance record the approval, the counts and the change set's hash. Oracle, SQL Server and Esri geodatabases are refused for now.

**Hidden fields and row filters.** The server rewrites every table a query reads into a filtered derived table that contains only the fields the policy does not hide: `FROM (SELECT <visible fields> FROM t WHERE <row filter> OFFSET 0) t`. Whatever the query does (`SELECT *`, UNION, column alias lists, `USING`/`NATURAL` joins, whole-row values like `to_jsonb(t)`), a hidden field is simply not there to reach. Each table name (with or without hidden fields) is resolved to exactly one listed table, and the allow and deny lists and row filters apply to that table, not to how the query spelled it by the database's own rules (quoted names exact, unquoted folded), and the rewritten query names that table in full, so the database cannot pick a different one; ambiguous or unlisted names are refused. Columns are always listed by name, so a column added after the list was taken (it is refreshed every five minutes) stays out until it has been seen. A query that names one is refused with a clear message. The `OFFSET 0` keeps the database from merging the filtered table into the outer query, so the caller's conditions only ever run on rows the row filter allows: an error raised by a condition cannot describe a row outside it. Each dialect has its own barrier: `LIMIT -1 OFFSET 0` on SQLite; `AND ROWNUM > 0` inside the filtered table's WHERE on Oracle (a view that uses ROWNUM can be neither merged nor have predicates pushed into it; the `NO_MERGE` hint stops merging only, and Oracle would still push the caller's WHERE in beside the row filter); `TOP (9223372036854775807)` on SQL Server (a filter cannot move below a TOP without changing its result), where a failed conversion would otherwise quote the value ("Conversion failed when converting the varchar value '…'"). Database errors keep only their primary message, with quoted values removed unless the error is about a name in the query. Files in the workspace are fingerprinted when Columbus writes them, and the GIS tools refuse a file whose content has changed since.

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
| 5. Local servers run confined | Local (stdio) servers and engine plugins start through the `columbus-confine` launcher at the level their manifest declares (A when none), capped by policy (below). They also get a scrubbed environment and a folder of their own in the workspace (`servers/<id>`, `plugins/<id>`), the only place they can write; they never see the workspace's results or governed extracts. A file handed to a server or plugin is copied into its folder once the call is allowed, and a plugin's outputs are copied out into `results/` by Columbus, with the hash of exactly what was copied. Remote servers must be on the policy's host list |

Review is done by the person at the keyboard:

```powershell
columbus-mcp servers list                 # each server and its state (trial, ready, needs review, blocked)
columbus-mcp servers review drive         # tools, the manifest's settings, the server's claims, and flags
columbus-mcp servers approve drive        # pins the definitions; asks you to type the id on the terminal
```

Remote servers that use OAuth (`auth: { type: oauth, scopes: [...] }`) are signed in to by the person, once, in their own browser:

```powershell
columbus-mcp servers signin drive         # opens the sign-in page; the redirect comes back to 127.0.0.1
columbus-mcp servers signout drive        # forgets the tokens
```

Sign-in follows the MCP authorization specification (OAuth 2.1 with PKCE; a client id from the manifest, or dynamic client registration as a native application). The tokens are kept in Windows Credential Manager, one entry per server, and added and refreshed by the gateway; they never reach the agent, tool results or the audit log, and no server receives another's. Scopes are the manifest's, never wider: a grant with any other scope is discarded and the sign-in fails. Until a server is signed in, `list_providers` shows it as `sign_in_required` with the command to run.

Review flags descriptions with hidden characters, wording aimed at the model ("ignore previous", "do not tell the user"), references to other providers' or Columbus's tools, credentials and destinations. Flagged tools need `--accept-flags`.

When a call needs approval, Columbus asks the person in their own MCP client (elicitation); the agent never sees or answers that prompt. When the client cannot ask, Columbus opens its **local approvals page** in the person's browser and the call waits briefly (45 s by default) for the decision. If none comes in time, the agent gets `approval_required` with an id, tells the person, and calls again with that id once they have decided. `whoami` tells the agent which way approvals will reach the person.

The approvals page is served only on 127.0.0.1 on a random port and uses no cookies (browsers share 127.0.0.1 cookies across ports, so another local program could collect them). The browser is opened on a single-use link whose secret sits after `#`, which browsers never send to any server; the page trades it once for a session token it keeps only in memory and sends in a header other websites cannot set. Requests for any other host name are refused, and decisions must come from the page's own origin. The page shows what would happen in plain words (destination, files and row counts, which connections the data came from, the flow rule that applied, the arguments) and the exact request on demand. Every decision is written to the audit log, and the agent never receives the page's address.

An approval is bound to exactly what will be sent: each file's SHA-256 and size and the data labels in play, not just the file's name. A file changed after approval, including under "allow for this session", is asked about again; a file a tool reads itself is checked again just before the call. Identical waiting requests share one approval, at most 20 may wait at once, and Columbus stops opening tabs after 10 without a decision. Organization policy can turn the page off or change the wait (`approvals: { page, wait_seconds }`).

Known limits: an agent that can run programs as the person could read the browser's command line in the instant it starts (the link is single-use, so the person would then see "already used"), and an agent that controls the person's browser can press its buttons. Give such agents only connections whose database logins are read-only.

### Confinement levels

| Level | On Windows | On macOS | Files | Network | For |
| --- | --- | --- | --- | --- | --- |
| A | AppContainer, no capabilities | Seatbelt profile, no network | its own folder in the workspace and declared folders only | none | engine plugins (the default) and compute-only servers |
| B | AppContainer with internetClient | Seatbelt profile, IPv4 internet only | same as A | internet, not loopback | local servers that call public APIs themselves |
| C | not available yet (see below) | Seatbelt profile | reads what the person can; writes only its own folder and declared folders | loopback and anything else | servers that drive a desktop app (Obsidian, QGIS, ArcGIS Pro add-ins); needs `trust: trusted` or an allow-list entry |

On Windows every level also runs in a job object (on macOS, see below): the process and anything it starts end when the gateway stops it, with memory and process limits (`servers.memory_mb`, `plugins.memory_mb`). The program's own folder and, for Python plugins, the interpreter's folders are readable; nothing else in the person's profile is. Policy lists the levels allowed: `servers.levels` (default A, B, C) and `plugins.levels` (default A; plugins never get C). `list_providers` shows each provider's level.

Confinement fails closed. If a level cannot be provided (the launcher is missing, or the operating system has no implementation), the server or plugin is blocked, unless `servers.allow_unconfined_local` allows an unconfined pilot, which `list_providers` then shows as `unconfined`. On Linux, used for development and CI, the launcher uses Landlock for files and a seccomp filter that refuses internet sockets at level A and, at A and B, Unix and netlink sockets (a sandbox could otherwise reach the session bus or a Docker socket) and leaving the process group (so stopping a program stops everything it started); run as root, it drops every capability first, and the gateway marks itself non-dumpable so no process of the same user can read its memory or environment. The gateway reaches into a confined process's folder only by walking it from the workspace root without following links, so a link planted there is refused, never followed. Known limits on Linux: the process count is not capped (memory is, per process). On Windows level C (planned as a restricted token at low integrity) is refused for now: on the systems tested, a low-integrity process could not read ordinary program folders, so it would not do what the level promises; a server that needs it is blocked until it is fixed, or can run at A or B with the folders it needs. On macOS the launcher runs the program under the system sandbox (Seatbelt, through `sandbox-exec`) with a profile it writes for each launch, every path passed as a parameter, never spliced into the rules. At every level the program is refused the system services that would act for it outside the sandbox (opening apps, Apple Events, the pasteboard, the keychain); at A it is also refused the certificate service, which fetches from the network for its caller. It cannot list other processes. macOS has no job object, so the launcher tags each launch's sandbox and finds every process in it, even one that left the process group; it sums their memory and counts them many times a second, and stops them all, round after round until none is left, past a limit, when the program ends, when the gateway goes, and (through a guard process) if the launcher itself is killed. A per-user process limit set on the program is a kernel backstop against a fork burst between two looks. Before the first launch it checks that this works and refuses confinement if not. Known limits on macOS: Apple marks `sandbox-exec` as deprecated, so a future macOS may change it (the check above then fails closed); Apple's base rules let a confined program reach a few services (preferences, logging, notifications, user names); level B opens IPv4 connections only, because the sandbox's network rules can name `localhost` but not an address and an IPv6 address can name this computer past them, so B does not work on IPv6-only networks; it cannot reach `localhost` but can reach this computer's own network address and other machines on the local network; level C refuses a list of services known to act outside the sandbox, so a service not on that list may still answer it; programs confined at the same time can pass data to each other through shared system state (notifications, named semaphores), so a script at A could hand data to a server running at B or C; any confined program may learn whether a file exists and its size and dates, though not its contents; a confined program can read the arguments and environment that other programs of the same user were started with, which the sandbox does not cover, so while a local server that gets an API key in its environment is running, an engine plugin or script could read that key (Columbus's own passwords never go in an environment); the memory limit is checked many times a second, not enforced by the kernel. Passwords are kept in the macOS Keychain.

Columbus's own engine worker is not confined: it is Columbus's code, it reads the connections (files, databases on the network or on this machine), and it is what enforces the rules on the data.

Not yet: the RustGIS worker (milestone 2 of change 1), and Windows' own MCP containment as an extra level once it leaves preview.

## Context pack

An organization describes its GIS world in a **context pack**: a folder of Markdown and YAML (datasets with their gotchas, coded domains, linear referencing, data groups, delivery specs, SOPs, roles), installed under `%ProgramData%\Columbus\context` (or `COLUMBUS_CONTEXT`). Agents search it with `context_search` and read entries with `context_get`; `describe_dataset` attaches the matching dataset entry, `describe_data_group` resolves a group's datasets and says which this session can read, and `whoami` returns the pack's always-on rules. Entries past the pack's review interval come back marked `stale`. `columbus-mcp pack lint DIR` checks a pack. See the [format](docs/context-pack.md) and the fictional [Contoso County](examples/context/contoso-county) sample.

**The person's own notes.** `draft_notes` (or **Draft notes** in the app's Settings) writes notes about a connection's datasets into the person's notes folder (`notes/` beside the session workspaces, or `COLUMBUS_NOTES`), in the context pack format: what the database says about itself only, never what it means (principle 10). Each dataset gets its fields and types, geometry and CRS, the row count within the connection's row filter, primary key, relationships, indexes and the comments written in the database (PostgreSQL `COMMENT`, Oracle `COMMENT ON`, SQL Server `MS_Description`, GeoPackage data columns); each short code field (up to 30 distinct values, ids and long text skipped, tables up to 2 million rows) gets a code list with how many rows have each code, labelled where the GeoPackage gives labels. Codes are counted by checked SQL, so the row filter applies; organization policy can turn code lists off (`notes: {values: false}`). Fields the organization hides, and tables the connection refuses, are never written, a relationship to a refused table is kept without its name, and nothing is written to the database. Every note is `status: draft`; a later draft never replaces a note anyone edited (each draft carries a hash of itself) or marked reviewed. The AI fills in meaning with `update_notes` (titles, summaries, field aliases and notes, code labels, text), on drafts only: a whole batch of changes, to up to 200 notes, is shown and approved at once, all or nothing, and the person may choose **Allow for this session**, which covers later note changes in that session while it carries the same data (reading data from somewhere new asks again; an organization rule that asks for approval always asks). Organization policy can require approval of every change (`notes: {ask_each_change: true}`). The context tools and `describe_dataset` read the notes beside the organization's pack (whose entry wins on the same id), as the rules in force now allow: a note about a connection that is gone, a dataset no longer allowed or a field now hidden is not shown, or shows without that field. They are labelled as the person's own and as information, not instructions; nothing in them becomes an always-on rule. Serving a note about a connection labels the session with that connection, exactly as reading it would, so the organization's flow rules apply to what the notes say. Each note records the rules it was drafted under (the dataset's filters, the hidden fields, whether codes were listed): when those differ, its code lists and counts are not shown until it is drafted again. Codes are listed only where the assistant could read rows anyway (a row tool allowed and `return_rows` above 0), and no more of them than a call may return. Names from the database that hold control or line-break characters are left out, and every note is read back after it is written: one that does not read back as the note it should be (its id, kind, connection and dataset) is removed. `update_notes` is bound to the note file's exact contents, refuses if it changed while the person decided, and changes the file as it is, so nothing the rules hide today is lost from it. Organization policy can deny `draft_notes`, for the app's button too. What `update_notes` writes is checked against the organization's flow rules as a write into that connection (data this session read from elsewhere, written into notes about it), and the note records the data labels it then carries (`carries`), which every later reader of the note takes on; only notes about a connection can be updated. A note's counts are never in its summary, and a note drafted before the rules changed says so (`redraft`). Ids join their parts with `--` (a slug never holds two dashes), and a draft never takes a file or id another dataset's note has.

## Registry

`registry_search` and `registry_get` read the [geo-registry](https://github.com/wiatrek-kevin-s/geo-registry) index (`registry.sqlite`, full-text search), installed with `columbus-mcp registry update` (from the registry's rolling release, or `--from` a folder for offline installs) under `%ProgramData%\Columbus\registry`. The index is checked against its `index.sha256` when installed and again before each use; a damaged or altered index is refused. Entries are community-written, so results are marked untrusted and say which entries no person has reviewed yet. A search by area leaves out entries known not to cover it and lists those with no recorded extent last, marked as unchecked. `registry_fetch` downloads an entry's data into the workspace as a layer: OGC API Features collections and ArcGIS REST feature layers (paged, optionally within a lon/lat bbox, at most the read limit of features) and GeoJSON files. Because the entry is community-written, its URL must be HTTPS to a public address (checked after DNS, and that address is the one connected to), redirects are not followed, downloads are capped at 200 MB, and the download is parsed as JSON by the engine, never handed to GDAL. Hosts listed in policy `fetch_hosts` download without asking; any other host needs the person's approval, with the URL and the entry's licence on the card. Other access types (WMS, STAC, EPT, zip files) are not fetched yet.

## Skills

[`skills/`](skills) holds the GIS skills library (design section 7): fifteen Agent Skills, from `working-with-columbus` and `crs-and-datums` to `lidar-with-pdal` and `sop-conversion`, each giving general practice first and then the exact Columbus tools and checks to use. They work in any agent that reads the open Agent Skills format. `python skills/check_skills.py`, run in CI, keeps every tool name they mention in step with the server.

## Companion window and desktop app

While any AI client works with Columbus, the person can watch the session in the companion window (design section 8): every layer and table it makes, with check results and provenance, on a MapLibre map and in an attribute table linked to the map, and the requests waiting for a decision. The window is the local Columbus page (127.0.0.1, the same server and protections as the approvals page) with its built UI installed beside the program in `ui/` ([app/ui](app/ui)). Its data calls are read-only, behind the page's session token, and audited; it opens only files Columbus's own tools wrote, unchanged since.

The Columbus desktop app ([app/desktop](app/desktop), Tauri) lists the sessions running on the computer (each server announces its port in `live/<session>.json`) and opens each one's window. Links handed to an app through a custom URL scheme can be intercepted by any program the person runs, so windows opened for the app are view-only: their approval cards have the server open the browser page that can decide. Approvals are decided only in that browser page or in the AI client's own prompt. A basemap (OpenFreeMap) is off until the person turns it on, since tile requests reveal the area in view; policy `companion: { basemap: false }` removes the choice.

**Settings** in the app edits the person's connections and the organization's switches for edits and scripts, instead of the files. Settings are never a tool, so the assistant cannot change them; only the app's settings window may call these commands. Any change that gives the assistant more access (a new connection or password, read-write, more datasets, fewer refused ones, a removed or changed row filter, edits or scripts turned on) is confirmed by the person at the keyboard first, with Touch ID or the account password on macOS and Windows Hello on Windows; tightening and removing are not. Files are written whole and checked by loading them as the server does, so the app never leaves a file the server would refuse, and comments in the policy file are kept. A policy file this account cannot write (managed by IT) shows read-only. On Linux, where neither exists, widening changes are refused in the app.

**Settings apply without a restart.** Every call re-reads the policy and connection files if they changed (hashing exactly the bytes it parses, regular files only), once the change has settled: every file untouched for a second by its times (checked again after reading), or the same contents seen again a second later, so a file being saved in place is never used half-written; files that keep changing for ten seconds stop every tool until they keep still. The new settings apply from that call on: a removed connection, a switch turned off or a tighter filter take effect at once, and anything looser only after the app's confirmation, as above. On every change, caches learned under the old settings are not reused, every approval not yet used and every "allow for this session" is withdrawn (the person is asked again), a change set from `propose_edits` is refused by `apply_edits` (propose again), and an approval given while the settings changed is not used. When a change narrows what a connection allows (removed, its type no longer allowed, or anything about it changed but its description and mode), workspace files carrying that connection's data label are removed and no tool will open them; a newly hidden field removes every file with governed data. Labels are conservative: a file made after the session read a connection carries its label. Every reload is an audit line (`settings.reload`) naming what it withdrew and removed. Files that do not load, a policy file that goes missing or is emptied once one has been in force (never a fall back to the defaults), or a change to what only a start applies (`servers`, `plugins`, `hosts`) stop every tool but `whoami`, which then starts and connects nothing, until fixed or until the AI app is restarted: a session never runs on half old, half new settings. Hand edits to the files behave the same way. A call that was running when a narrowing applied (including fewer rows or seconds allowed, by the values in effect) has its answer withheld (it may hold rows) and the files it made withdrawn (a change of limits alone removes no files); a write it already committed to a source, or an export already in its folder, is said plainly. An approval given while a change to the files is still settling is not used. An empty policy file is refused at start, too. Limits: exports already written stay where they were written, and a file that cannot be deleted at once (held open on Windows) is withdrawn from every tool and deleted at the next reload.

**Test** in the connection editor asks a running session that reads the same connections folder to open the connection as the form describes it, before saving: the organization's policy is applied, the engine lists the datasets, and the connection's allow and deny lists are applied to them. Nothing is saved and the AI sees none of it; each test is an audit line (`settings.test_connection`, client `columbus-app (settings)`) with where it pointed and whether a typed, stored or no password was used. Each session writes a random key to `live/<session>.key`, a file only this account can read (its `live/<session>.json` is private too). The key is never sent: the app sends a fresh challenge signed with the key (`POST /api/test-hello`), so only a key holder can ask for one, checks the session's HMAC-SHA256 answer, so a program that merely took over a port gets nothing, and then sends the test (`POST /api/test-connection`) with its own HMAC over the session's single-use challenge and the exact request body. Requests with any `Origin` header or another host name are refused, so no web page, this session's own included, can send one. A password typed in the form goes only to that session's engine; with none typed, the stored password is used only when the form points where the saved connection of that name already does (same type, host, port, database, login, DSN, path), and the saved connection's own dataset lists then apply too. A host name with a list, path or option characters (`,`, `;`, `/`, `@`, spaces…) is refused for every connection; SQL Server may name one instance (`db01\SQLEXPRESS`), checked against `hosts` by its host part. An Oracle `dsn` is checked like a host: an EZConnect string (`db.corp:1521/svc`) by its host, a full connect descriptor is refused, and a bare alias (`ORAPROD`) is looked up in the `tnsnames.ora` that `TNS_ADMIN` or `ORACLE_HOME` points to, whose hosts are not checked against `hosts` (so that file is the control for aliases). An EZConnect string must be exactly one address with a service: `[tcp://|tcps://][//]host[:port]/service[:server][/instance]`; a second address after `;`, options or anything else is refused. A name with no `/service` (`PROD.WORLD`, even `db.corp:1521`) is what the driver looks up in `tnsnames.ora`, so it counts as an alias.

The app also has its own chat ([crates/chat](crates/chat)): the person adds an Anthropic API key (kept in the operating system's credential store and never given to a web page), and the chat starts its own Columbus session and gives the model that session's tools and nothing else. Approvals come to the chat window as prompts written by the app, not the model, through MCP elicitation, the same route as in Claude desktop. "Show map" opens the session's companion window.

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
| Oracle / Oracle Spatial | `oracle`, `sde-oracle` | Works (python-oracledb in thin mode, no Oracle Client); `host`, `port`, `database` (the service name) or `dsn`, `user`, password from the secret store; `sslmode` `require`/`verify-full` use TCPS. `SDO_GEOMETRY` is read as WKB with the SRID from `ALL_SDO_GEOM_METADATA`; Esri `ST_Geometry` is reported as unsupported geometry |
| SQL Server / Azure SQL | `sqlserver`, `mssql`, `sde-sqlserver`, `azure-sql` | Works (pyodbc and Microsoft ODBC Driver 18 for SQL Server); `sslmode` maps to `Encrypt` (`disable` no; `require` yes without checking the certificate; default and `verify-full` yes, checked); no stored secret means integrated sign-in. `geometry` and `geography` are read with `STAsBinary()` and `STSrid` |
| Folder of GeoTIFFs | `rasters` | Works (needs `rasterio`; policy must list `rasters`); read-only, `path` and `datasets` only, see [`local-rasters.yaml`](examples/connections/local-rasters.yaml) |
| Enterprise geodatabase through ArcPy | `sde` | Configuration and query checking work; engine next (an EGDB's tables can already be read through its database's own type) |

For development and CI only, `COLUMBUS_DEV_SECRETS=1` reads passwords from `COLUMBUS_SECRET_<ID>` environment variables instead; `whoami` flags it. Platforms other than Windows have only this mode for now.

## Layout

```text
crates/shared     policy layers, query checking, approvals, sandbox, audit (no I/O to agents)
crates/broker     connected MCP servers: manifests, client sessions, tool pinning, review flags
crates/flow       data labels and organization flow rules
crates/confine    confinement levels A-C (AppContainer, restricted token, job objects; Landlock on Linux)
crates/gateway    columbus-mcp: MCP server (rmcp), tools, worker supervision, plugins, run_tool
app/ui            the companion window (React, MapLibre), served by columbus-mcp
app/desktop       the Columbus desktop app (Tauri): session list, companion windows, chat
crates/chat       the app's chat: a model driving a columbus-mcp session as its only tools
workers/python    the core engine and the Python plugin SDK (columbus_plugin)
workers/sdk-rust  the Rust plugin SDK (columbus-plugin)
docs              worker protocol v2 and plugin.yaml
examples          a sample policy, connections, server manifests and the LRS checker plugin
```

## Install

Releases ship a zip and a `.mcpb` bundle (the MCP project's one-click format for local servers) for Windows (x64) and macOS (Apple Silicon). [docs/install.md](docs/install.md) (Windows) and [docs/install-macos.md](docs/install-macos.md) walk through unpacking, setting up the GIS engine, the policy and connections, and connecting Claude desktop or any MCP client. A tag `v*`, or a manual run of the [release workflow](.github/workflows/release.yml), builds them; it publishes only a commit whose CI passed, under a tag that matches the version in `Cargo.toml`.

## Build and run

Requires Rust (stable, 1.85+) and Python 3.12+. For the GIS tools, `pip install -r workers/python/requirements-gis.txt` (geopandas, pyogrio, shapely, pyproj, rasterio; the wheels include GDAL and PROJ). For PostGIS, Oracle and SQL Server, also `pip install -r workers/python/requirements-db.txt` (psycopg, oracledb, pyodbc; SQL Server also needs Microsoft ODBC Driver 18 for SQL Server).

```powershell
cargo build --release
.\target\release\columbus-mcp.exe --version
```

The build makes two programs: `columbus-mcp.exe` and `columbus-confine.exe`, the launcher that confines local servers and plugins. Keep them in the same folder (or point `COLUMBUS_CONFINE` at the launcher).

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

On macOS the defaults are `/Library/Application Support/Columbus` (the organization's) and `~/Library/Application Support/Columbus` (yours); on Linux, `/etc/columbus` and the XDG folders. Without a policy file the server runs with safe defaults: local GeoPackage and SQLite only, read-only, no scripts, no edits. See [`examples/`](examples) for a policy and connections.

If the audit folder is not writable (a per-user install without the installer's permissions), the server falls back to a folder under the user's profile and says so in `whoami`. A call that cannot be audited does not run.

### Check the audit log

```powershell
columbus-mcp audit verify "%ProgramData%\Columbus\audit\2026-11.jsonl"
```

Each line records user, client, tool, connection, tables, row count, decision and a hash of the exact request; never row data. Editing, removing or reordering a line breaks the chain at that line.

## Tests

```powershell
cargo test --workspace                                   # unit tests + an end-to-end MCP session
python -m unittest discover -s workers/python/tests      # worker and GIS engine
```

PostGIS tests run when `COLUMBUS_TEST_PG_HOST` (and optionally `_PORT`, `_DB`) is set and [`tests/fixtures/postgis.sql`](crates/gateway/tests/fixtures/postgis.sql) is loaded; CI runs them against a PostGIS container. They connect with a login that *can* write, to show writes are refused anyway. Oracle and SQL Server tests run the same way with `COLUMBUS_TEST_ORA_HOST` (fixture [`oracle.sql`](crates/gateway/tests/fixtures/oracle.sql)) and `COLUMBUS_TEST_MSSQL_HOST` ([`mssql.sql`](crates/gateway/tests/fixtures/mssql.sql)); CI runs them against the `gvenzl/oracle-free` and `mcr.microsoft.com/mssql/server:2022-latest` containers. They include the barrier test: a caller's conversion of a column that fails on every filtered-out row (`TO_NUMBER(route_code)`, `CAST(route_code AS INT)`) runs without error and returns only allowed rows.

The broker tests run the gateway against a scriptable MCP server ([`examples/fake_mcp_server.rs`](crates/gateway/examples/fake_mcp_server.rs)) and cover the change-1 gates: a changed description is switched off, a poisoned one is flagged, governed data cannot reach a connected server without the person's approval or past a deny rule. The end-to-end test starts the real binary as an MCP client would, with a policy, a GeoPackage connection and the Python worker, and checks over the wire that row filters apply, hidden fields never leave, denied tables and writes are refused, truncation is reported and the audit chain verifies. The GIS test ([`tests/gis.rs`](crates/gateway/tests/gis.rs)) does a full session on a GeoPackage of Austin roads: describe, sample, structured queries, a bbox in lon/lat, buffering refused in degrees and done in feet, a clip, a join whose keys lost their zero padding (flagged with the fix), and verification; and the same queries on PostGIS. The raster test ([`tests/raster.rs`](crates/gateway/tests/raster.rs)) runs on a folder of GeoTIFFs: listing and describing, names that leave the folder, links and a ".tif" holding VRT XML refused, a clip by a GeoPackage layer, slope, zonal statistics and the size limit.

The confinement tests ([`tests/confine.rs`](crates/gateway/tests/confine.rs)) run the launcher at each level on the machine's own mechanism and check what a confined program can read, write and reach; the plugin test includes the change-1 milestone gate (a level A plugin cannot read outside the workspace). The OAuth tests run `servers signin` against a fake authorization server with an MCP endpoint ([`examples/fake_oauth_server.rs`](crates/gateway/examples/fake_oauth_server.rs)): sign-in, connecting with the stored token, refresh of an expired one, and a grant wider than the manifest refused.

## Roadmap

Phase 1 (this repository, in progress):

- [x] Four-layer permission model, query checking, sandbox, approvals store, hash-chained audit log
- [x] MCP server over stdio with structured output; Python worker; GeoPackage/SQLite engine
- [x] `query_features` (structured queries) and `describe_dataset`, `sample_rows`
- [x] PostGIS engine in read-only sessions; system catalogs refused on every engine
- [x] Secrets in Windows Credential Manager (`columbus-mcp secret set`)
- [x] SQL Server (ODBC 18, `ApplicationIntent=ReadOnly`) and Oracle (`python-oracledb` thin) engines
- [ ] Streamable HTTP on 127.0.0.1 with a per-session token
- [x] Broker for connected MCP servers (stdio and HTTP): manifests, pinning with auto-disable, review flags, file handles, untrusted output, data labels and flow rules (change 1, Rules 1–4)
- [x] Approvals in the person's own client through MCP elicitation
- [x] Local approvals page for clients without elicitation
- [x] Worker protocol v2 with progress and cancel; Python and Rust plugin SDKs; Columbus's engine rebuilt on it; engine plugins through `run_tool`
- [ ] RustGIS native worker
- [x] Confinement levels A–B on Windows (AppContainer and job objects) and A–C on Linux (Landlock and seccomp)
- [ ] Confinement level C on Windows (a low-integrity process cannot read program folders on the systems tested)
- [x] OAuth sign-in for remote servers (PKCE, dynamic registration, tokens in Credential Manager)
- [x] `vector_op` and `verify` through GDAL (geopandas, pyogrio, shapely, pyproj)
- [x] `raster_op` through GDAL (rasterio) on `rasters` connections: clip, reproject, slope, aspect, hillshade, zonal statistics, band statistics; COG outputs
- [ ] `pointcloud_pipeline` through PDAL; raster export, mosaicking; a pinned environment (pixi) for the engine
- [x] Verification checks (design section 4) attached to every result: CRS, units, geometry, joins, filters, limits
- [ ] Remaining checks: topology, linear referencing, tolerance and accuracy, field-level data dictionary rules
- [x] Open geospatial registry (`registry_search`, `registry_get`, `registry update`)
- [x] GIS context pack format v1 (`context_search`, `context_get`, `describe_data_group`, `pack lint`)
- [x] GIS skills library (15 skills, checked against the server in CI)
- [x] `read_output`, `export_output` (export folders, delivery specs applied and checked, provenance beside every export)
- [x] `fail` checks anywhere in a layer's history block its export without an approved override
- [x] `registry_fetch` (OGC API Features, ArcGIS REST, GeoJSON; public HTTPS only, approval outside `fetch_hosts`)
- [x] `run_python`: confined at level A, full code shown and approved every run, outputs labelled and checked
- [x] `propose_edits`/`apply_edits` on GeoPackage and PostGIS (Oracle, SQL Server, Esri versions next)
- [ ] STAC and EPT fetching; domains enforced in verification; export as a runnable script
- [ ] Bounding boxes pushed into the database's spatial index in `query_features`
- [x] Desktop app, first part: companion window (map, table, checks, provenance, waiting requests) and a Tauri app that opens it for each running session
- [x] Desktop app: in-app chat with an Anthropic API key (the session's tools only; approvals as in-app prompts)
- [ ] Desktop app: work-account AI providers (Copilot, Azure OpenAI, Vertex), streaming replies, saved chats, documents and context pack editing, connections page
- [ ] ArcPy worker for enterprise geodatabases; signed `.mcpb` bundle and MSI

Companion repository: [`geo-registry`](https://github.com/wiatrek-kevin-s/geo-registry) (open catalogue of GIS tools and data sources). The context pack format ([docs/context-pack.md](docs/context-pack.md)) and the [skills library](skills) live in this repository.

## Honest limits

The server's checks are a second line of defence. The guarantee nothing bypasses is the database's own users, roles and grants: give Columbus a read-only database login. Anyone with direct credentials and other tools can go around Columbus.

Performance: on a table with a row filter, the server keeps the database from merging the filtered table into the query, so that a caller's conditions never run on rows the filter excludes. On PostgreSQL that also stops the caller's own spatial conditions from using the spatial index: they are tested on every row the filter allows. On Oracle, the index operators (`SDO_RELATE`, `SDO_FILTER`, `SDO_WITHIN_DISTANCE`) need the indexed column itself and fail on a row-filtered table (`ORA-13226`); use `SDO_GEOM.RELATE` or `SDO_GEOM.SDO_DISTANCE` there, or `query_features` with a bbox. Tables without a row filter are not affected.

Database settings outside the server's reach:
- Leave PostGIS raster out-of-database bands (`postgis.enable_outdb_rasters`) and `postgis.gdal_enabled_drivers` at their defaults (off); with them on, raster functions can read files on the database server.
- Allowing a parent table allows its rows in every child table and partition; deny or filter at the parent.
- Functions and types in the database are trusted only when PostgreSQL's built-ins or the PostGIS extension provide them. The engine always searches `pg_catalog` first, and PostGIS functions are called by the extension's own schema.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
