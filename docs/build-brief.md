# Build brief: Columbus 0.14.0, the data-reach rework

Version 1.0 · as of 2026-11-08 · prepared with Blueprint · size: large

This rework widens what Columbus reaches without loosening its rules: file connections, ArcGIS Online/Portal layers, the analyst's own Python environments, and file geodatabase and raster exports, plus the audit's fixes. The most important choice is D-3: a person's own Python environment is *governed* (confined, filtered copies only) by default, and *trusted* (unconfined) only where organization policy allows it.

Each fact lives in one section; elsewhere, cite its ID. Part 2, `docs/build-brief-2.md`, adds the owner's weekly ROW workflow (success measure 7, T-19 to T-23). Audit findings are AF-n, in `docs/audit-report.md`.

## Intent

> Columbus lets GIS analysts and developers use AI on all of their organization's data, safely. They connect any source (Oracle and SQL Server geodatabases, file geodatabases and shapefiles, ArcGIS Online/Portal, CSV/Excel, web APIs) and set its rules first: read-only or read-write, edit a copy, which rows and fields. The AI then works the data with any Python the analyst has, including ArcGIS Pro's arcpy and environments they build. It follows the organization's SOPs from its documentation, checks its own results, and saves outputs locally. Repeated work becomes workflows and skills. It is fast, dependable, and quick to use.

**Users:** GIS analysts at agencies and utilities, mostly on Windows with ArcGIS Pro, Oracle/SQL Server geodatabases and ArcGIS Online or Enterprise; secondarily, developers using AI agents.

**Success measures** (each tied to a requirement):
1. An analyst can add a CSV, Excel, GeoJSON, GeoParquet, KML or FlatGeobuf file as a connection and query, analyse and export it like a GeoPackage (REQ-1 to REQ-4).
2. An analyst can add an ArcGIS Online/Portal feature layer, public or signed in, and list, describe, sample and query it under row filters and hidden fields, with no row lost to paging (REQ-8 to REQ-11).
3. An analyst can run a script or a personal tool in a registered Python environment: an ArcGIS Pro clone, any other Python, or one Columbus creates (REQ-12 to REQ-17). arcpy is expected to need a **trusted** environment, because its licence check reads the person's profile and registry and, for Named User licences, the network. Governed environments serve every other library (A-6).
4. Exports can be written as a file geodatabase, and raster results as GeoTIFF (REQ-5, REQ-6).
5. Speed: the targets are a warm `query_features` on 100,000 rows under 2 s and an engine start under 10 s on the CI Linux runner. The benchmark (REQ-20) fails CI only past twice those, so runner noise doesn't flake it. A measured time between the target and the limit is logged for the final review. A bbox query uses the source's spatial index (REQ-19).
6. Every new source × capability has an automated test. Tests that need the engine fail, not pass, when the engine is missing in a CI job that installs it (REQ-22).

**Non-goals** (frozen by D-2): screen redesign (a separate UI review follows); in-app signed approvals of other AI apps' requests (status queue item 1); sharing or email changes; registry work (AF-13); save as script (AF-14); new edit targets (AF-8); the RustGIS pilot (AF-16; its branch stays untouched).

## Context

**What exists** (audit of `main` at 8b6bf173, Columbus 0.13.0, 2026-11-08):
- Rust gateway (`crates/gateway`, `crates/shared`) enforces every rule; Python engine (`workers/python`) does the GIS work; Tauri app (`app/desktop`).
- CI covers Linux, macOS and Windows, with PostGIS, Oracle and SQL Server containers.
- 287 Rust and 115 Python tests pass.
- `docs/reference.md` describes behaviour.

**The pattern to copy.** Commit `dbe0f20` ("Esri shapefiles and file geodatabases (read only)") added a file-based connection kind end to end. `git show --stat dbe0f20` lists every place a new kind touches.

**Research** (as of 2026-11-08; links under Sources):
- The engine's pyogrio wheel (GDAL 3.12.4) reads and writes CSV, XLSX, GeoJSON, KML, FlatGeobuf, GPKG and OpenFileGDB. It has no Parquet driver, so use `geopandas.read_parquet`.
- OpenFileGDB writes file geodatabases (GDAL 3.6+), with domains and relationships. 64-bit integers need `TARGET_ARCGIS_VERSION=ARCGIS_PRO_3_2_OR_LATER`.
- ArcGIS REST paging: continue while `exceededTransferLimit` is true, even after an empty page. Keep the order deterministic. Use `returnIdsOnly` batches where pagination isn't supported.
- ArcGIS sign-in for native apps: authorization code with PKCE, at `<portal>/sharing/rest/oauth2/authorize` and `/token`.
- Pro: never modify `arcgispro-py3`; use clones. Named User licences fail standalone arcpy unless Pro was signed in within about 14 days.
- uv 0.13.7 is current on PyPI.

## Design

```
                    AI app (Columbus chat, Claude, Copilot)
                                  │ MCP (unchanged)
                                  ▼
 ┌──────────────────────────── Gateway (Rust) ─────────────────────────────┐
 │ policy ∩ connection ∩ data group ∩ project   approvals   audit   checks │
 │                                                                         │
 │  NEW  files connections ─┐     NEW  arcgis connections (REST + OAuth;   │
 │                          │          tokens stay in the gateway)         │
 │  NEW  environments registry (governed | trusted) ── columbus-confine    │
 └──────────────┬───────────────────────────────┬──────────────────────────┘
                │ worker protocol v2             │ scripts / personal tools
                ▼                                ▼
        Columbus engine (Python)        Person's environment python.exe
        reads files into a cached       (Pro clone with arcpy, a uv venv
        GeoPackage, converts ArcGIS     Columbus created, any Python)
        pages, writes exports           gets extracts in, outputs back
                │
                ▼
   Files · GPKG · PostGIS · Oracle · SQL Server · FGDB · SHP · GeoTIFF · ArcGIS REST
```

### Part 1: `files` connections (AF-3, AF-7)

A `files` connection is one file, or a folder of files, in these formats: `.csv`, `.xlsx`, `.geojson`/`.json`, `.parquet`/`.geoparquet`, `.kml`, `.fgb`. It is read-only.

- **Reading:** the engine converts each dataset on first use into a cache GeoPackage, then serves every read through the existing SQLite/GeoPackage path. That gives files every existing rule (allow lists, row filter, hidden fields, read limits, the SQLite dialect in `sql_select`, joins across files) and makes repeat queries fast.
- **The cache:** one file per connection, under `<workspaces root>/cache/files/<connection id>.gpkg`. It is rebuilt for a dataset per R-7, and deleted when the connection is removed or narrowed, as workspace files are today. Every file is resolved after following links and must lie inside the connection's folder (R-16), as for `rasters` connections.
- **Naming and parsing:** dataset names (F-1) and CSV geometry and typing (R-1 to R-6) are defined below.
- **Policy:** the new type is `files`. The old default type `file`, which no engine serves, becomes an accepted alias of `files` (D-10).

### Part 2: exports (AF-5)

`export_output` gains two formats:
- **`gdb`:** writes a new `<name>.gdb` with one feature class through OpenFileGDB. An existing `.gdb` is never replaced or changed this round, even with approval.
- **`tif`:** for raster results only. It copies the workspace's Cloud-Optimized GeoTIFF.

Existing checks and approvals apply. The delivery-spec rules for rasters are in R-9.

### Part 3: ArcGIS REST paging fix (AF-4)

One shared pager serves `registry_fetch` and the new `arcgis` connections:
- It continues while `exceededTransferLimit` is true, even after an empty page.
- It sorts by the layer's object-id field, ascending.
- Where the layer reports `advancedQueryCapabilities.supportsPagination: false`, it falls back to `returnIdsOnly` plus `objectIds` batches.
- It stops at the existing page cap and marks the result truncated (`fetch.truncated` or `query.read_limit`) whenever it stops before the server says the data is finished.

### Part 4: `arcgis` connections (AF-1)

An `arcgis` connection points at one feature or map service, or one layer in it (F-3). Each layer and table is a dataset, named per F-1b.

- **Where HTTP happens:** the gateway makes every HTTP request, in Rust with the existing HTTP client. It asks for `f=json`, so Z and M values survive for linear referencing. The engine converts each page into the workspace GeoPackage with GDAL's ESRIJSON driver.
- **Tool behaviour:**
  - `list_datasets`, `describe_dataset` and `sample_rows` read the layer's JSON: fields, coded-value domains, geometry type, `latestWkid`, extent, and the row count from `returnCountOnly` under the row filter.
  - `query_features` maps fields to `outFields` with hidden fields removed. Its checked `where` is ANDed in parentheses with the row filter. The bbox becomes an envelope with `inSR`, `order_by` becomes `orderByFields`, and the read limit is enforced with the pager.
  - `sql_select` is refused with `query.unsupported` and the fix "use query_features" (R-12).
  - **The `where` is rebuilt, never passed through.** The gateway parses the caller's `where` with the existing `query_features` parser and refuses any hidden field in it. It writes the request's `where` and `orderByFields` back out from the parsed form, and does the same with the stored row filter at save time. Raw caller text never reaches the service, so input like `1=1) OR (1=1` can't escape the parentheses. Counts (`returnCountOnly`) follow the same rule, so a hidden field can't be inferred through counts (R-10b).
- **Sign-in:** PKCE (D-4). The person signs in once from the terminal (`columbus-mcp arcgis signin <id>`) or from a **Sign in** button on the connection in the app. Tokens and refresh tokens go in the OS credential store under `arcgis:<connection id>`. An expired refresh gives the plain error "Sign in again: …". Public services need no sign-in.
- **Host rule:** the service host and the `auth.portal` host must both be on policy `hosts`. A token is sent only to the portal itself and to service hosts the portal vouches for: the same host, ArcGIS Online's `services*.arcgis.com` for `www.arcgis.com`, or a server the portal lists as federated (`/sharing/rest/portals/self/servers`). Any other host gets the request without a token, or is refused if it needs one (R-18).

### Part 5: Python environments (AF-2, AF-6, D-3, D-5)

An **environment** is a registered Python interpreter that scripts (`run_python`) and personal tools can run in. The engine's Python stays the default; environments run user code only.

- **The master switch:** policy `environments.allow` (default false). While it's off, scripts and tools run only in the engine's Python, as today, and environments can't be registered or created.
- **Registering:** the person registers an environment at the keyboard, choosing its trust (`--trust governed|trusted`, default governed; trusted only while policy allows it). Confirmation reuses the existing connection-confirmation path; it is not the in-app approvals work listed in the non-goals. In the app it's Settings › Environments › Add, confirmed with Touch ID or Windows Hello like a connection. In the terminal it's `columbus-mcp env add <id> --python <path>`, typing the id to confirm, like `servers approve`. The agent cannot register or change one.
- **What registration records (F-4):** the Python version; the package list and its SHA-256 (sorted `name==version` lines from `importlib.metadata`, plus the sorted file names in `conda-meta/` where that folder exists, so conda-based Pro clones are covered); whether `arcpy` imports; and arcpy's licence state (`arcpy.ProductInfo()`), all from a probe run with a 120 s limit. Confirmation records a SHA-256 of the registration's `python`, `trust`, `confinement` and `created_by` fields with the probe. A later change to any of them, or to the package list, marks the environment `changed`, and it stops running until the person confirms it again (R-13b), so editing the YAML can't make an environment trusted.
- **Creating one:** the new tool `propose_environment` lets the agent ask for a new environment (Python version, pinned packages, reason). The person approves it in the app at the keyboard. Columbus then runs its bundled `uv` to create the environment under `<Columbus data>/envs/<id>/`, reaching only `environments.index_hosts`. Installs are wheels only (`--no-build`), so no package's build code ever runs. The Python is one already on the machine: the engine's own version, or another found locally. Columbus never downloads a Python (`UV_PYTHON_DOWNLOADS=never`). It installs only into environments it created, never into `arcgispro-py3`, a Pro clone or a registered environment.
- **Governed environments** (`trust: governed`, the default):
  - Scripts run through `columbus-confine` at the environment's level: A (no network) or B (network), capped by policy.
  - They get extracts only, exactly as today; the environment's interpreter folder is the only extra folder they may read.
  - Level B needs policy `environments.allow_network: true`, and its approval card says the script can reach the internet.
- **Trusted environments** (`trust: trusted`):
  - Scripts run unconfined as the person. They can read anything the person can, including `.sde` files and the network.
  - They need policy `environments.allow_trusted: true`, off by default.
  - Every run is approved; there is never a session allow. The card states: "Columbus cannot enforce your connection rules for this run: the script can read, write and send data anywhere you can."
  - Outputs that come back through Columbus carry every data label of the session plus `environment:<id>`, so later exports through Columbus still follow flow rules. What the script does directly is outside Columbus's control.
- **arcpy licensing:** `whoami` reports each arcpy environment's licence state in plain words ("ArcGIS Pro licence: Advanced", or "not initialized: open ArcGIS Pro and sign in; Named User licences lapse after about 14 days without Pro being opened"). Workflow approval shows this as a warning.
- **Session approval for scripts (AF-6):**
  - Policy `scripts.session_approval: true` (off by default) adds "Allow this script for the session" to the card, for governed level-A environments and the engine only.
  - The approval binds per R-15 (code SHA-256, environment id, current Columbus session). The same code with new inputs runs without asking; changed code asks again.
- **Removing an environment:** removing a registration deletes only its YAML. Columbus deletes an interpreter folder only if `created_by: columbus` is in the confirmed registration hash and the folder's real path lies under `<Columbus data>/envs/`, after the person confirms at the keyboard. It never deletes a folder the person registered, such as a Pro clone.
- **Personal tools and workflows:** both gain an optional `environment` (F-5, F-6), which must be a governed environment: trusted environments run only through interactive `run_python`, each run approved, never inside a tool or an unattended workflow (arcpy workflows wait for a later round unless arcpy works governed, A-6). A tool or workflow step whose environment is missing, `changed`, or no longer allowed stops with "needs you".

### Part 6: speed and hygiene (AF-9, AF-11, AF-12, AF-15, AF-10)

*Versions here are as of 2026-11-08.*

- **Bbox pushdown (AF-9):** the gateway builds the bbox condition itself, so it can go *inside* the filtered derived table, beside the row filter, where the source's spatial index applies: `&&` on PostGIS, the R-tree on GeoPackage, `SDO_FILTER` on Oracle, `geom.Filter()` on SQL Server.
- **Benchmark (AF-15):** a test times engine start and a warm `query_features` on a generated 100,000-row GeoPackage. Its limits and targets are in REQ-20 and success measure 5.
- **Pinned engine (AF-11):** hash-pinned requirement files for Windows x64, macOS arm64 and Linux x64 on CPython 3.12 and 3.13 (for example `uv pip compile --universal --generate-hashes`), per REQ-21. Pin `uv` too.
- **Visible skips (AF-12):** with `COLUMBUS_TEST_REQUIRE_ENGINE=1`, set in every CI job that installs the engine, a test that would skip for a missing engine fails instead.
- **Docs (AF-10):** README, `docs/reference.md`, install guides, `examples/` and the `working-with-columbus` skill describe exactly what 0.14.0 does. `skills/check_skills.py` passes.

## Requirements

*Versions here are as of 2026-11-08.*

Must and Should items carry Given / When / Then checks, each with an edge case and a failure case, in the task named.

| ID | Requirement | Priority | Task |
|---|---|---|---|
| REQ-1 | A `files` connection to a folder lists each supported file as a dataset (F-1). Given a folder with `crashes.csv` and `roads.geojson`, when `list_datasets` runs, then both appear. Edge: `roads.v2.csv` is offered as `roads_v2`, and `~$book.xlsx` is skipped. Failure: a `.xls` file returns "save as .xlsx". | Must | T-2, T-3 |
| REQ-2 | CSV and Excel geometry and typing follow R-1 to R-6. Given a CSV with `lon`/`lat` columns, when `describe_dataset` runs, then it is points in EPSG:4326. Edge: `CSJ` values `000123456` stay text. Failure: no coordinate columns gives a table without geometry and a `crs.present` info check. | Must | T-2, T-3 |
| REQ-3 | File datasets obey allow and deny lists, row filters, hidden fields and read limits exactly as GeoPackage datasets do. Given a row filter `STATUS <> 'R'`, when `query_features` runs, then no `R` row is returned. Edge: `sql_select` joining two CSVs applies both filters. Failure: a hidden field named in a query is refused. | Must | T-2, T-3 |
| REQ-4 | The cache rebuilds a dataset when its source file changes (R-7). Given a cached CSV, when a row is appended to the file, then the next query sees it. Edge: touching the file without changing it rebuilds nothing (same SHA-256). Failure: an unreadable file gives a plain error naming the file. | Must | T-3 |
| REQ-5 | `export_output` writes `gdb` (R-8). Given a points layer, when exported as `gdb`, then `pyogrio.read_info` reads one layer with the same row count and CRS. Edge: an Int64 value over 2,147,483,647 is kept exact, with an `export.needs_pro_3_2` info check. Failure: an existing `<name>.gdb` is always refused, never replaced or changed; the person picks another name. | Must | T-4 |
| REQ-6 | `export_output` writes raster results as `tif` (R-9). Given a `raster_op` slope result, when exported, then the file is a valid COG with the same CRS and nodata. Edge: a delivery spec requiring another CRS fails the export with "reproject first". Failure: exporting a vector layer as `tif` is refused. | Must | T-4 |
| REQ-7 | ArcGIS paging loses no rows (Part 3). Given a fake service that returns an empty page with `exceededTransferLimit: true`, when fetched, then paging continues and the total matches the service's count. Edge: a layer without pagination support is read by object-id batches. Failure: the page cap marks the result truncated. | Must | T-5 |
| REQ-8 | An `arcgis` connection lists, describes and samples layers and tables of a public service. Given a fake FeatureServer with two layers and one table, when `list_datasets` runs, then three datasets appear with geometry type and `latestWkid`. Edge: two layers named alike get `_<layer id>` suffixes (F-1b). Failure: a host not on policy `hosts` is refused before any request. | Must | T-10 |
| REQ-9 | `query_features` on `arcgis` applies fields, hidden fields, row filter, where, bbox, order and the read limit (Part 4, R-10, R-11). Given a row filter `DISTRICT = 14`, when queried with `where: "LANES > 2"`, then the request's `where` is `(DISTRICT = 14) AND (LANES > 2)`. Edge: Z and M values survive into the workspace layer. Failure: a where clause outside the parser's subset, a hidden field in `where` or `order_by`, and `1=1) OR (1=1` are each refused before any request (R-10b). | Must | T-10 |
| REQ-10 | ArcGIS sign-in by PKCE stores and refreshes tokens in the OS credential store; the agent, the engine and the audit log never see them. Given a fake OAuth server, when `columbus-mcp arcgis signin svc` completes, then queries succeed with a bearer token. Edge: an expired access token is refreshed once without asking. Failure: an expired refresh token gives "Sign in again: columbus-mcp arcgis signin svc". | Must | T-11 |
| REQ-11 | The app's Connections page can add, test and sign in to `files` and `arcgis` connections, using existing components only. Given the app, when the person adds an `arcgis` connection and presses Sign in, then the browser opens the sign-in page. Edge: Test on a public service needs no sign-in. Failure: a network error shows the plain message from R-14. | Should | T-12 |
| REQ-12 | The person can register an environment from the terminal or the app at the keyboard; the agent cannot (Part 5, F-4). Given `columbus-mcp env add pro --python <path>`, when the id is typed to confirm, then `whoami` lists it with version, arcpy and licence state. Edge: a path that isn't a Python interpreter is refused. Failure: an MCP call cannot create or change an environment. | Must | T-6 |
| REQ-13 | `run_python` and personal tools accept `environment`. Governed environments run confined with extracts only. Given a governed level-A plain venv, when a script reads its input and writes `out/x.gpkg`, then the output returns as a checked layer. Edge: a script that opens a socket fails. Failure: an environment marked `changed` refuses with "confirm it again in Settings". | Must | T-6, T-9 |
| REQ-14 | `propose_environment` creates a pinned environment with uv only after the person approves at the keyboard. Given a proposal for `whitebox==2.3.6`, when approved, then the environment exists and is registered governed. Edge: a package without an exact version is refused at proposal. Failure: a download host outside `environments.index_hosts` fails the creation with the host named, and a package with no wheel for this system is refused (no source builds). | Must | T-7 |
| REQ-15 | Trusted environments run unconfined only under `environments.allow_trusted: true`, with every run approved and outputs labelled (Part 5). Given policy without the switch, when a trusted environment is used, then it is refused. Edge: with the switch, the card shows the full "cannot enforce … anywhere you can" sentence and offers no session allow. Failure: exporting a trusted output through Columbus to a folder under a flow rule still asks for approval. | Must | T-8 |
| REQ-16 | arcpy detection and licence state are reported in plain words (Part 5). Given a test environment with a stub `arcpy` whose `ProductInfo()` returns `NotInitialized`, when registered, then `whoami` shows the "not initialized: open ArcGIS Pro and sign in …" sentence. Edge: `ArcInfo` shows "Advanced". Failure: a probe that hangs past 120 s is stopped and reported. | Must | T-8 |
| REQ-17 | Session approval for scripts works only under policy `scripts.session_approval: true`, bound to code hash and environment (Part 5). Given the switch and a governed level-A environment, when the same code runs twice with new inputs, then only the first asks. Edge: one changed character asks again. Failure: a trusted or level-B environment never offers it. | Should | T-9 |
| REQ-18 | Personal tools and workflow steps carry their environment and stop with "needs you" when it is missing, changed or disallowed. Given a workflow step in a removed environment, when the run starts, then the step stops with "needs you". Edge: a trusted environment is refused for a tool or workflow step; the arcpy warning shows when a workflow using a governed arcpy environment is approved. Failure: a shared `.columbus` file never carries an environment's path; the recipient chooses their own. | Should | T-9 |
| REQ-19 | A bbox query uses the source's spatial index, beside the row filter (Part 6). Given the PostGIS CI fixture with a row filter, when `query_features` runs with a bbox, then `EXPLAIN` shows the spatial index. Edge: a bbox in another CRS is transformed first. Failure: a source without a spatial index still returns correct rows. | Should | T-13 |
| REQ-20 | The benchmark test passes on the CI Linux runner (Part 6). Given a generated 100,000-row GeoPackage, when timed, then the warm query is under 4 s and the engine start under 20 s (twice success measure 5's targets), and both measured values are printed. | Should | T-14 |
| REQ-21 | Engine dependencies are installed by exact version and hash on every platform (Part 6). Given CI, when the engine installs, then pip runs with `--require-hashes` for `requirements-gis.txt`, `requirements-db.txt` and `requirements-projectdb.txt`, and the CI-only extras (`playwright`, `psycopg`, `selenium`, `pyyaml`) are exact-pinned. Failure: a tampered hash fails the install. | Must | T-15 |
| REQ-22 | Engine-dependent tests fail instead of skipping in CI jobs that install the engine (Part 6). Given `COLUMBUS_TEST_REQUIRE_ENGINE=1` and no engine, when `gis.rs` runs, then it fails with a message naming the variable. | Should | T-15 |
| REQ-23 | Docs, examples and the `working-with-columbus` skill match 0.14.0; `skills/check_skills.py` passes. | Must | T-16 |

## Decisions

*Versions here are as of 2026-11-08.*

| ID | Decision | Choice | Why | Door | Status |
|---|---|---|---|---|---|
| D-1 | Where the build happens | Claude Code on `wiatrek-kevin-s/columbus`, a branch per session, pull requests to `main` | The project is a Rust/Python repository with CI; branches keep each change reversible | One-way | Delegated to Claude 2026-11-08 |
| D-2 | Outcome and scope | Narrow rework; the non-goals list is frozen | Owner's decision 2026-11-08; reach before more governance features | One-way | Decided by owner |
| D-3 | Trust model for the person's own Python | Governed (confined, extracts only) by default; trusted (unconfined) only under `environments.allow_trusted` | Keeps "safely" true by default while meeting "any Python, arcpy included" where an organization accepts it | One-way | Delegated to Claude 2026-11-08; accepted risk, see Security baseline item 1 |
| D-4 | How ArcGIS Online/Portal is reached | Gateway-held PKCE sign-in and REST in Rust; pages converted by the engine | Tokens never leave the gateway; one HTTP stack already exists (`fetch_tools.rs`); the `arcgis` Python package is large and unnecessary | One-way | Delegated to Claude 2026-11-08 |
| D-5 | Engine packaging | Keep pip, now hash-pinned; no move to pixi/conda this round | Environments (D-3) give arcpy and GDAL-binding users their own interpreter, so the engine needn't grow; pixi stays a later option for Esri edits (AF-8) | One-way | Delegated to Claude 2026-11-08 |
| D-6 | External AI apps | Keep MCP for Claude, Copilot and others | Owner: "retain if easy"; the app itself is an MCP client, so it costs nothing | Two-way | Decided by owner |
| D-7 | Publishing | The builder may publish **0.14.0 as a test release** through the Release workflow dispatched on `main`, once the final review's Fix-now findings are closed. No other publishing | Owner's stated deliverable: download the new version from GitHub | One-way | Pre-approved by owner 2026-11-08 |
| D-8 | Merging | The builder merges its own pull requests to `main` only when every job in `.github/workflows/ci.yml` passes on every system | Owner asked for minimal involvement; CI covers three systems and three databases | Two-way | Delegated to Claude 2026-11-08 |
| D-9 | Check-ins | One setup round at the start (T-1) collects every access and answer the build needs; after it, the builder runs to the end without waiting, and asks only for what it cannot do or decide | Owner: set it and forget it; anything Claude can do itself, it does | Two-way | Decided by owner 2026-11-08 |
| D-10 | Old `file` policy type | Accepted as an alias of `files`; defaults list `files` | Existing policy files keep working | One-way (format) | Delegated to Claude 2026-11-08 |
| D-12 | Who owns the ArcGIS OAuth app (client id) | Each organization registers its own and puts its `client_id` in the connection; `docs/manual-checks.md` gives the admin steps; Columbus ships no client id | A shipped id would tie every user's sign-in to an account the owner must keep alive and answer for; public layers need none | One-way | Delegated to Claude 2026-11-08; risk: analysts without admin rights need IT for signed-in layers |
| D-13 | Release labelling | arcpy and trusted environments are marked **experimental** in the release notes and in `whoami` until H-2 passes | The release (D-7) ships before any real Pro test | Two-way | Delegated to Claude 2026-11-08 |
| D-11 | Version | 0.14.0 in `Cargo.toml`, the app and release notes | Minor version for new capabilities | Two-way | Delegated to Claude 2026-11-08 |

**Options weighed:**
- **D-3:** governed-only fails arcpy (A-6); trusted-only drops every guarantee. The chosen option's main risk is an injected trusted script reading an `.sde` file; mitigations are in Part 5.
- **D-4:** the `arcgis` Python package would put tokens in a worker and add a large dependency; the chosen route reuses `crates/broker/src/oauth.rs`.

**Decide yourself, and log the reason:** versions within the stack, internal names, cache internals, fixtures, the fake servers (extend `crates/gateway/examples/fake_oauth_server.rs`), log wording and step order. **Ask before** touching a one-way door, the scope, cost, security rules, the formats in F-1 to F-14, or the intent.

## Rules and examples

| Rule | Example | Expected result |
|---|---|---|
| R-1 CSV geometry from a WKT column named `geometry`, `wkt`, `the_geom` or `shape` (case-insensitive) | `geometry` = `POINT (-97.74 30.27)` | Point layer; CRS from the connection's `crs`, else none plus a `crs.present` warn |
| R-2 CSV geometry from coordinate pairs: `lon`/`lat`, `longitude`/`latitude`, `x`/`y`, `point_x`/`point_y`, or the connection's `x`/`y` settings | `lon` = -97.74, `lat` = 30.27 | Points; EPSG:4326 when every value is within ±180/±90 and no `crs` is set; otherwise no CRS plus `crs.present` warn |
| R-3 Both WKT and pairs present | `geometry` and `lon`/`lat` | WKT wins; an info check names the ignored columns |
| R-4 Typing: a column is text if any value has a leading zero before another digit (`0123`) or a thousands separator; otherwise it is integer if every value is a whole number, real if every value is a number; empty cells are null; dates only in ISO 8601 | `000123456`; `12`; `12.500`, `1.50` | Text; integer; real (mileposts and money compare as numbers) |
| R-6b Excel cells: date-formatted cells become ISO 8601 dates; General-format serial numbers stay numbers | A `Date` column formatted as dates | `2026-11-08` |
| R-6c CSV oddities: duplicate header names get `_2`, `_3` suffixes with an info check; a header-only CSV is a 0-row dataset; a blank header becomes `field_<n>` | Headers `ID,Name,Name,` | `ID`, `Name`, `Name_2`, `field_4` |
| R-5 Encoding: UTF-8 (BOM stripped); otherwise Windows-1252 with a `file.encoding` warn | A file saved by Excel in cp1252 | Reads; warn names the encoding |
| R-6 Excel: first row is the header; each sheet is a dataset; empty sheets are skipped; formulas give their cached values | `inventory.xlsx` with sheets `2025`, `2026`, `Notes` (empty) | Datasets `inventory/2025`, `inventory/2026` |
| R-7 Cache freshness: key = size + modification time; a change in either triggers a SHA-256 compare; rebuild only if the hash differs | File touched, same bytes | No rebuild |
| R-7b Two queries at once on a stale dataset | Two sessions query `crashes` while it is rebuilt | One rebuild; both wait and read the new cache |
| R-7c File deleted after caching | `crashes.csv` removed | Dataset disappears from `list_datasets`; its cache table is dropped; queries return "no dataset crashes" |
| R-8 `gdb` export: one new `.gdb` per export, layer name = export name; field names over 64 characters are refused with the list; Int64 values beyond the 32-bit range are kept exact with `TARGET_ARCGIS_VERSION=ARCGIS_PRO_3_2_OR_LATER` and an `export.needs_pro_3_2` info check | Export `crash_hotspots` | `crash_hotspots.gdb` containing `crash_hotspots` |
| R-9 `tif` export: raster results only; delivery spec CRS must already match (no automatic reprojection); nodata kept | Spec wants EPSG:3081, raster in 4326 | Export fails with "reproject first (raster_op reproject)" |
| R-10 ArcGIS row filter: parsed by the `query_features` where parser at save time and refused if it doesn't parse; every request carries the filter rebuilt from that parsed form, in parentheses, ANDed with the rebuilt caller `where` (R-10b) | `row_filter: "DISTRICT = 14"` | Every request's `where` starts `(DISTRICT = 14)` |
| R-10b ArcGIS where: rebuilt from the parsed form; hidden fields refused in `where`, `order_by` and counts | `where: "SSN LIKE '1%'"` with `SSN` hidden; `where: "1=1) OR (1=1"` | First refused naming the hidden field; second refused by the parser; no request sent for either |
| R-18 ArcGIS token hosts (Part 4) | Connection to `https://gis.example.org/server/rest/services/Roads/FeatureServer` with portal `https://portal.example.org/portal` that doesn't list `gis.example.org` | Signed-in queries refused with "the portal does not vouch for gis.example.org"; no token sent |
| R-11 ArcGIS read limit: counts features across pages; the page that crosses the limit is cut and the result marked `query.read_limit` | Limit 200,000; service has 250,000 | 200,000 rows, truncated, warning |
| R-12 `sql_select` on `arcgis` | Any SQL | Refused: `query.unsupported`, "use query_features" |
| R-13 Environment probe: runs once at registration and on confirm; 120 s limit; never imports arcpy inside the gateway or engine | Pro clone | Version, packages hash, `arcpy: yes`, licence state |
| R-13b Environment's packages change after registration | Person `pip install`s into a clone | Status `changed`; runs refused with "confirm it again in Settings" |
| R-13c Environment removed while a workflow uses it | Workflow step uses `pro` | Step stops with "needs you"; the rest of the run continues where possible |
| R-14 Plain failures: every new failure names the cause and the fix | Portal offline | "ArcGIS service at services.example.com did not answer in 30 s. Check the address or your network, then Test again." |
| R-15 Session approval binding: SHA-256 of the exact code text + environment id + the Columbus session (one chat in the app, or one MCP connection from another AI app; it ends when that ends) | Same code, other input, same chat | Runs without asking; a new chat asks again |
| R-16 Links in `files` folders: a file or folder whose real path, after following every link, lies outside the connection folder is skipped and listed under `skipped` | `crashes.csv` is a link to `C:\other\x.csv` | Not listed; reason "link leads outside the connection folder" |
| R-17 No credential store (for example headless Linux): ArcGIS sign-in is refused with "no credential store on this system"; tokens are never written to files | Linux without a secret service | Sign-in refused; public layers still work |

## Formats that last

YAML is UTF-8; dates are ISO 8601; unknown keys are refused; all formats add to those in `docs/reference.md`.

| ID | Format | Field | Type | Required in storage | Allowed values | Example |
|---|---|---|---|---|---|---|
| F-1 | `files` dataset name | name | text | n/a | Path relative to the connection folder, forward slashes, extension removed; multi-layer files (XLSX sheets, KML layers) add `/<layer>`; every `.`, `\` or `:` left in folder, file or layer names becomes `_`; names that then collide (compared case-insensitively) are refused with both files named; `~$*` lock files, hidden files and `.json` files that aren't GeoJSON are skipped and listed under `skipped` in `list_datasets` with the reason | `2026/crashes`, `crashes_2026_01` (from `crashes_2026.01.csv`), `inventory/2026` |
| F-1b | `arcgis` dataset name | name | text | n/a | The layer or table name as published; if two share a name, `<name>_<layer id>` for each | `Roadways`, `Signs_3` |
| F-2 | Connection `type: files` | id | text | yes | Existing id rules | `crash-csvs` |
| F-2 | | type | text | yes | `files` (alias `file`) | `files` |
| F-2 | | path | text | yes | Absolute path to a folder or one supported file | `C:\GIS\drops\crashes` |
| F-2 | | recursive | boolean | no (default false) | true, false | `true` |
| F-2 | | crs | text | no | `EPSG:<code>` used for CSV/XLSX geometry | `EPSG:4326` |
| F-2 | | x, y | text | no (both or neither) | Column names for coordinate pairs | `EASTING`, `NORTHING` |
| F-2 | | datasets, row_filter, hide_fields, limits, description, expires | as today | as today | as today | as today |
| F-3 | Connection `type: arcgis` | url | text | yes | `https://…/(FeatureServer|MapServer)` or a layer URL ending `/<n>` | `https://services.arcgis.com/abc/arcgis/rest/services/Roads/FeatureServer` |
| F-3 | | auth | map | no (default `none`) | `none`, or `{type: oauth, portal: <https url>, client_id: <text>}` | `{type: oauth, portal: https://www.arcgis.com, client_id: AbC123}` |
| F-3 | | page_size | integer | no (default: the layer's `maxRecordCount`) | 1 to the layer's `maxRecordCount` | `2000` |
| F-3 | | datasets, row_filter, hide_fields, limits, description, expires | as today | as today | as today | as today |
| F-3b | Credential store entry | name | text | yes | `arcgis:<connection id>` | `arcgis:roads-agol` |
| F-3b | | value | JSON text | yes | `{access_token, refresh_token, expires_at (ISO 8601), portal}` | — never logged |
| F-4 | `environments/<id>.yaml`, beside the `connections` folder; environments Columbus creates live in `<Columbus data>/envs/<id>/` | id | text | yes | `[a-z0-9-]{1,40}` | `pro` |
| F-4 | | python | text | yes | Absolute path to an interpreter | `C:\Users\a\AppData\Local\ESRI\conda\envs\arcgispro-py3-clone\python.exe` |
| F-4 | | trust | text | yes | `governed`, `trusted` | `governed` |
| F-4 | | confinement | text | no (default `A`) | `A`, `B` (governed only) | `A` |
| F-4 | | created_by | text | yes | `person`, `columbus` | `person` |
| F-4 | | description | text | no | free text | `Pro 3.5 clone with whitebox` |
| F-4 | | probe | map | yes (written by Columbus) | `{python_version, packages_sha256, arcpy: bool, licence: text or null, probed_at}` | `{python_version: "3.11.10", arcpy: true, licence: "ArcInfo", …}` |
| F-4 | | confirmed_at | text (ISO 8601) | yes | Set at the keyboard | `2026-11-12T10:04:00-06:00` |
| F-5 | Personal tool manifest addition | environment | text | no (default: engine) | A registered governed environment id | `pro` |
| F-6 | Workflow step addition | environment | text | no (default: the tool's) | A registered governed environment id | `pro` |
| F-7 | Policy additions | environments.allow | boolean | no (default false) | Master switch (Part 5) | `true` |
| F-7 | | environments.allow_trusted | boolean | no (default false) | — | `false` |
| F-7 | | environments.allow_network | boolean | no (default false) | — | `false` |
| F-7 | | environments.index_hosts | list of hosts | no (default `[pypi.org, files.pythonhosted.org]`) | host names | — |
| F-7 | | scripts.session_approval | boolean | no (default false) | — | `true` |
| F-7 | | connection_types | list | as today | adds `files`, `arcgis`; `file` is read as `files` and Columbus never rewrites the policy file | `[gpkg, files, arcgis]` |
| F-8 | Audit line additions | environment | text or null | no | environment id for script and tool calls | `"environment":"pro"` |
| F-9 | Provenance additions | environment | map | for script and tool steps | `{id, trust, python_version, packages_sha256}` | — |
| F-10 | Export formats | format | text | — | adds `gdb`, `tif` | `gdb` |

Deleting an environment leaves past provenance and audit lines as they are. Deleting a connection deletes its file cache and credential entry (F-3b).

## Security baseline

1. **Access rules in the server: applied.** New sources use the same intersection of policy, connection, data group and project. Trusted environments weaken this by design: an accepted risk under D-3, off by default, shown to the owner.
2. **Secrets: applied.** ArcGIS tokens stay in the OS credential store (F-3b, R-17, R-18). They never reach the agent, the engine, the audit log, provenance or fixtures.
3. **Spending caps: not applicable.** No paid service is added.
4. **Test and live apart: applied.** Only fixtures, fake ArcGIS and OAuth servers, and CI containers are used; never the owner's connections or accounts.
5. **Pinned dependencies: applied.** `uv` is pinned and hashed. Any new dependency (Rust crate, Python wheel, npm package, or the bundled `uv` binary) is checked to exist and be maintained, and must carry one of MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0 or unmodified MPL-2.0. For anything else, pick another dependency or write the code; don't stop to ask. Licences are logged; bundled binaries (`uv`: MIT/Apache-2.0) ship their licence text in the release's third-party notices.
6. **Managed sign-in: applied.** ArcGIS OAuth with PKCE; Columbus stores no passwords.
7. **Legal: noted.** Running arcpy under the person's Pro licence is their organization's call; the docs say so. Not legal advice.
8. **Rate limits: applied.** HTTP 429 and `Retry-After` are honoured, with at most 3 retries, then R-14.
9. **AI least privilege: applied.** No tool registers, confirms or changes environments, sign-ins or policy. Service and file text is data.
10. **Verification: applied.** CI on three systems, the benchmark, the final review, and `docs/manual-checks.md`.

## Where the work happens

Claude Code on GitHub `wiatrek-kevin-s/columbus` (D-1), with a branch per session named `rework/s<n>-<area>`. The setup round (T-1) checks access and asks the owner once. Pull requests target `main`, and the builder merges per D-8, when **every job in `.github/workflows/ci.yml` passes**, on all systems. Work is saved after every task as a commit on the session branch. Any task can be reverted by reverting its commits; history is never rewritten.

The owner's only step is the setup round (T-1); if a session ends early, they say "continue". Releases come from the existing Release workflow, dispatched on `main` with tag `v0.14.0` once every rework pull request is merged and `main`'s CI is green, per D-7. The owner downloads the release from GitHub. Homework H-1 and H-2 are optional.

## Keeping it running

1. **Owner and time:** the repository owner, about 1 hour a month. If the owner stops, the repository is archived with a README note. Releases stay downloadable.
2. **Change streams:**

| Stream | Who | How often | Trigger | Check | Needs the owner's approval |
|---|---|---|---|---|---|
| Dependency updates (crates, wheels incl. pyogrio/GDAL, uv) | Dependabot (added in T-15) opens pull requests; an agent may prepare the hash-pinned requirement files | Weekly | Dependabot schedule | CI on three systems | Merge |
| Security advisories | Owner reads GitHub alerts | Monthly | Alert email | CI | Merge |
| Esri changes (REST, OAuth, Pro licensing) | Nobody on purpose until a user reports a failure; R-14 messages make failures visible | — | User report | Fake-server tests plus one public-service check | Any fix |
| Expiring items: ArcGIS refresh tokens (about 2 weeks, set by the org) and Named User Pro licences (about 14 days) | The analyst; Columbus reports both in plain words | When reported | R-14 messages, `whoami` | — | — |
| MCP spec revisions | Owner, yearly | Yearly | New spec date | CI | Any change |
| Feature requests | Owner, through a new Blueprint Amend or Audit | As raised | GitHub issue | — | Scope |

3. **Automation policy:** agents propose pull requests and the owner merges (the builder merges under D-8 during this rework). Nothing merges with a failing check. Bots get only their job's access; secrets, if ever needed, go in GitHub's secret store. Issue and web text is untrusted input.
4. **Safety net:** required CI checks on `main`. Rollback is the previous GitHub release, which stays downloadable. The workspace, cache and environments sit under the person's Columbus folders and can be deleted safely. Alerts go to the owner's GitHub notifications.
5. **Runbook (under 15 minutes a month):**
   1. Open GitHub, then Pull requests, and merge green Dependabot pull requests.
   2. Open Security and read any alert. If one needs code, start a Blueprint Operate route with `docs/build-brief.md` and the alert.
   3. Skim open issues.
6. **Handing a problem to an agent:** bugs go to Blueprint's Audit route, new capabilities to Amend, with `docs/build-brief.md`, `docs/progress-log.md` and the issue.
7. **Tool choice (as of 2026-11-08):** GitHub Actions and Dependabot, because the owner already uses GitHub and CI runs there. Renovate is the alternative if Dependabot can't update hash-pinned pip files; check that at T-15.

## Homework

| ID | What only the person can provide | How to get it | Needed by |
|---|---|---|---|
| H-1 | Optional: an ArcGIS OAuth client id for the owner's organization, to try signed-in layers after release | In ArcGIS Online: Content → New item → Developer credentials → OAuth credentials; add the redirect URL from `docs/manual-checks.md` (settled in T-11) | After release; not blocking |
| H-2 | Optional: one manual arcpy check on a Windows PC with ArcGIS Pro | Follow `docs/manual-checks.md` (written in T-16) | After release; not blocking |

## Tasks

*Versions here are as of 2026-11-08.*

The tasks are in `docs/tasks.json`. The thin version that works end to end is T-2: a CSV connection queried, buffered and exported. Fixes chosen after the final review are added to `docs/tasks.json` as `R-101`, `R-102`, … (numbered from 101 so they never share a number with rules R-1 to R-20); this is the only time tasks are added.

The session table (areas S1 to S8, run back to back) is in `CLAUDE.md`; part 2 adds T-19 to T-23.

## Checkpoints

Per D-9 they are self-checks recorded in the log at the end of each area. The builder asks the owner once, in the setup round (T-1), then only for what it cannot do or decide.

## Open questions and assumptions

| ID | Question or assumption | Settled at | If false or unanswered |
|---|---|---|---|
| A-1 | AppContainer (Windows) and Seatbelt (macOS) can confine an interpreter outside the engine's folder when that folder is granted read access | T-6 (CI on all three systems) | Governed environments run only where confinement works; elsewhere they are refused, as scripts are today. Log it and continue |
| A-2 | ArcGIS accepts a loopback redirect `http://127.0.0.1:<port>` for PKCE with any port when `http://127.0.0.1` is registered | T-11 (check developers.arcgis.com) | Use the registered `urn:ietf:wg:oauth:2.0:oob` redirect: the code shows in the browser and is pasted into the terminal prompt or app field, never the chat |
| A-3 | arcpy imports when Columbus starts a clone's `python.exe` directly | H-2, manual, after release | Docs tell the person to run Pro once and sign in; trusted mode is the fallback |
| A-4 | Dependabot can update hash-pinned pip requirement files | T-15 | Use Renovate, or an agent task that regenerates the files weekly as a pull request |
| A-6 | arcpy fails to initialize its licence inside a governed environment, so arcpy needs a trusted environment | T-8 records it as expected; H-2 confirms on a real Pro machine | If arcpy works governed, docs say so and recommend governed |
| A-5 | GeoParquet via `geopandas.read_parquet` keeps CRS from the file's `geo` metadata | T-3 | Use the connection's `crs` setting and add a `crs.present` warn |

## Never without approval

The single list, with the one-way doors, is in `CLAUDE.md`, "When to stop and ask".

## Definition of done

*Versions here are as of 2026-11-08.*

**Every task:**
- Its acceptance checks pass, with evidence in `docs/tasks.json` and a line in the progress log.
- `cargo fmt --check` and `cargo clippy --workspace -- -D warnings` are clean.
- The Python tests pass.
- CI passes on all three systems before merge.
- No secret is in the diff (GitHub secret scanning plus a grep for `token`, `password` and `secret` in new files).
- `docs/reference.md` is updated for any behaviour change.

**The project:**
- Success measures 1 to 7 are met (measure 5 per REQ-20; 7 in part 2).
- Every REQ marked Must passes; every Should passes or has a logged reason.
- The audit's fix set is closed (AF-1 to AF-7, AF-9 to AF-12, AF-15, AF-17 to AF-21), except tasks blocked as "Sent to Blueprint", which the release notes list.
- The final review's Fix-now findings are fixed and re-checked.
- 0.14.0 is published as a test release (T-17).

## Sources

- Audit report, 2026-11-08 (`docs/audit-report.md`)
- Columbus design set, owner's planning document, revised 2026-11-05
- ESRI File Geodatabase vector (OpenFileGDB), gdal.org, as of 2026-11-08, https://gdal.org/en/stable/drivers/vector/openfilegdb.html
- Query (Feature Service/Layer), Esri ArcGIS REST API help, as of 2026-11-08, https://developers.arcgis.com/rest/services-reference/enterprise/query-feature-service-layer/
- User authentication flows; Authorization code with PKCE, Esri developers, as of 2026-11-08, https://developers.arcgis.com/documentation/security-and-authentication/user-authentication/flows/
- What is conda (ArcGIS Pro 3.2/3.3), Esri, https://pro.arcgis.com/en/pro-app/3.2/arcpy/get-started/what-is-conda.htm
- Esri Community arcpy licensing threads, 2021–2025
- uv guide and benchmarks, pydevtools.com, as of 2026-11-08, https://pydevtools.com/handbook/explanation/uv-complete-guide/
