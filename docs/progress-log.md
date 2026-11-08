# Progress log: Columbus 0.14.0 rework

Kit version 1.1 · as of 2026-11-08, session S1 not started

## Where things stand

- **Session:** S1 in progress · **Task:** T-2
- **Next step:** T-2, the thin `files` connection (CSV)
- **Waiting on the owner:** nothing
- **Homework received:** setup round answered ("skip" on Dependabot alerts); H-1 and H-2 are optional, after release

## Setup round (T-1), 2026-11-08

| Access | Result |
|---|---|
| Push a branch | ok (`rework/s1-files` pushed) |
| Read Actions runs | ok (`gh run list` / `gh run view`) |
| Dispatch a workflow | ok (CI dispatched on `rework/s1-files`, run 42514607603) |
| Merge to `main` | ok: admin rights; no branch protection and no rulesets on `main` |
| Dependabot alerts | off; this session can't change repository security settings. Owner asked once and replied "skip" |

## Decisions

| Date | Decision | Why | Asked or decided |
|---|---|---|---|
| 2026-11-08 | Dependabot alerts stay off for this build. T-15 still adds `.github/dependabot.yml` (version updates don't need alerts) and settles A-4 from the docs | Owner's reply to the setup round | Asked (owner: "skip") |
| 2026-11-08 | `hide_fields` on a `files` connection means the policy's `hide_fields`, as today; no per-connection field is added | F-2 says "as today", and today only policy has `hide_fields`. Reading it as a new connection field would add a format (F-2), so the literal reading is kept. Not a one-way change | Decided |
| 2026-11-08 | `.xls` files are listed under `skipped` with "save as .xlsx" and refused if named | Meets REQ-1's failure case and tells the analyst why the file is missing | Decided |
| 2026-11-08 | Skipped files show only if their would-be dataset name passes the allow list | `skipped` must not reveal more of the folder than the datasets do | Decided |
| 2026-11-08 | XLSX sheets and KML layers are always named `file/layer`, even with one sheet | F-1 treats these as multi-layer formats; names stay stable when a sheet is added | Decided |
| 2026-11-08 | XLSX is read by a small standard-library reader, not GDAL | GDAL treated a header-only sheet's header as data and lost date-time detail, breaking R-6 and R-6b | Decided |
| 2026-11-08 | Unreadable files are listed under `skipped` with the cause; naming one gives a plain error naming the file | REQ-4's failure case without one bad file breaking the whole folder | Decided |
| 2026-11-08 | `crs.present` is a warn for `files` datasets in describe and query; `export_output` still fails a layer with no CRS unless overridden | R-1 and R-2 ask for a warn. Exports check the layer, not its source, and other types stay unchanged (no rule loosens) | Decided |
| 2026-11-08 | An existing `<name>.gdb` blocks every export of that name, in any format, even with overwrite | R-8 says it is never replaced or changed; a non-gdb export of the same name would replace its `.prov.json` | Decided |
| 2026-11-08 | gdb exports use `TARGET_ARCGIS_VERSION=ARCGIS_PRO_3_2_OR_LATER` only when a value is outside 32 bits; otherwise 32-bit integers | Keeps exports readable by every ArcGIS when nothing needs Pro 3.2 | Decided |
| 2026-11-08 | gdb export names must be a letter then letters, digits and `_`; refused otherwise | GDAL would rename silently, and R-8 says the layer name is the export name | Decided |
| 2026-11-08 | `.xlsx` workbooks are closed after every read (fix found by Windows CI on PR #1) | An open workbook locks the analyst's file in Excel | Decided |
| 2026-11-08 | Shared pager in `crates/gateway/src/arcgis_pager.rs`; layers without `advancedQueryCapabilities` (before 10.3) use object-id batches; page size is the smaller of 1,000 and `maxRecordCount` | Offset paging on old servers can't be trusted; a server cap smaller than the request would skip rows | Decided |
| 2026-11-08 | Esri query paging rules checked against Esri's own `query?f=help` pages (served by live ArcGIS servers); the developers.arcgis.com page needs a fetch approval nobody was present to give | They confirm empty pages with `exceededTransferLimit`, deterministic order and the `returnIdsOnly` + `objectIds` route | Decided |
| 2026-11-08 | Oracle measured geometries are built in the worker from SDO_GTYPE, SDO_ELEM_INFO and SDO_ORDINATES; measured arcs, compound shapes and polygons are refused with a plain error | `SDO_UTIL.TO_WKBGEOMETRY` writes 3302 lines as 2D (found by CI); wrong coordinates are worse than a refusal | Decided |
| 2026-11-08 | Engine requires shapely>=2.1; `geometry.m_dropped` fires whenever an input had M and the output has none | M support starts in shapely 2.1; one generic rule covers every vertex-rebuilding op | Decided |
| 2026-11-08 | Environment confirmations live in `environments/.confirmed.json`; every run first does a confined packages-only probe, and the full probe (arcpy) runs only at add or confirm, at the keyboard | F-4 gains no field; in-place package edits are caught; arcpy's licence check needs the person's profile | Decided |
| 2026-11-08 | Registered interpreters must be named python, python3, python3.N or python.exe; one AppContainer profile per environment; CLI adds `env confirm`, `env list` | Registration never runs another program; no ACL grant carries over | Decided |
| 2026-11-08 | Top-level policy still accepts unknown keys; the new `environments` and `scripts` sections refuse them | Refusing at top level could break existing policy files (no rule loosens, none tightens unasked) | Decided |
| 2026-11-08 | Columbus's uv is `uv==0.13.7` (current on PyPI, checked), hash-pinned in `workers/python/requirements-uv.txt` and installed into the engine; found via `uv.find_uv_bin()`, never PATH | Fits D-5 (pip, hash-pinned) and T-15 | Decided |
| 2026-11-08 | Environment creation runs uv through a per-creation proxy on 127.0.0.1 that allows only `environments.index_hosts`; uv gets a cleared environment, `--no-config`, `--no-build`, no Python downloads | Catches redirects and index links to other hosts at the network level | Decided |
| 2026-11-08 | Proposals are approved from the terminal (`env approve <id>`) now; the app's approve button comes with T-12 (Settings › Environments) | The app UI is T-12's scope | Decided |
| 2026-11-08 | Third-party notices file (uv MIT/Apache-2.0) is created in T-16 | No notices file exists yet; uv is installed from PyPI, not bundled | Decided |
| 2026-11-08 | Python baseline is 149 passed, not the audit's 115 | `main` moved past the audit commit 8b6bf173 before the kit upload; more tests and no failures, so no action | Decided |

## Change requests

| ID | Date | What and why | Options | Recommendation | Status |
|---|---|---|---|---|---|

Status: raised, approved, declined, or Sent to Blueprint (touches a one-way door; affected tasks paused).

## Handoff notes

Newest first, written as you go. Mark each claim verified (checked this session) or unverified.

### Task log

- 2026-11-08 T-1 done: access checked (verified); baseline at 9749c8f: 287 Rust and 149 Python passed, 0 failures, only database skips (verified); CI run https://github.com/wiatrek-kevin-s/columbus/actions/runs/42514607603 green on 13 jobs (verified).
- 2026-11-08 T-2 done: commit b12ef7d; files.rs 2/2 pass (verified); workspace 292 passed, pytest 159 passed. Left for T-3: R-7 SHA compare, other formats, `file` alias, `crs.present` as warn (today it is a fail).
- 2026-11-08 T-3 done: commit 4bfc56c; files.rs 4/4, test_files.py 22/22 (verified); workspace 295, pytest 171. A-5 confirmed: read_parquet keeps CRS from `geo` metadata.
- 2026-11-08 PR #1 CI run 42520073092: 12/13 jobs passed; Python worker on Windows failed (xlsx file left open). Fixed in 3ef74e8.
- 2026-11-08 T-4 done: commit 435ebdc; export_formats.rs 2/2 (verified), test_export.py 12/12; workspace 297, pytest 183.
- 2026-11-08 PR #1 merged (80559e2) after CI run 42522569468 green on 13 jobs; second failure was gis.rs matching a fragment of a hidden value inside random ids, fixed in 39aecb8.
- 2026-11-08 T-5 done: commit 8306ce2; fetch.rs pass (verified); workspace 300, pytest 183.
- 2026-11-08 T-19: first PR #2 CI run 42525997319 failed 3 jobs (gdb_export compared a raw temp path with the canonical one on macOS/Windows; Windows `\\?\` paths broke the GeoPackage M-flag check; Oracle WKB lost measures). Fixed in b3ec68e; run 42529178838 green on 13 jobs incl. Oracle; PR #2 merged (4148908). T-19 done (verified in CI).
- 2026-11-08 T-6 done: commit 3accf6e; environments.rs 7/7 on Linux, macOS and Windows (CI run 42528260440). **A-1 confirmed** on all three systems.
- 2026-11-08 T-7 done: commit 40e90d2; environment_create.rs 3/3 incl. real PyPI (verified); uv 0.13.7 pinned with hashes.

### Kit → Session S1 (2026-11-08)

- **Done:** nothing yet. The audit baseline on `main` at 8b6bf173: 287 Rust tests and 115 Python tests passed with 0 failures in a temporary copy (unverified for you; re-run in T-1).
- **Decisions and deviations:** none.
- **Loose ends:** assumptions A-1 to A-6 in the brief are open; each names the task that settles it. AF-17 (M geometry) was reproduced in the audit; T-19 fixes it.
- **Saved:** nothing yet.
- **Next:** T-1. Commit `dbe0f20` is the model for adding a connection kind (T-2).
