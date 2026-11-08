# Audit report: Columbus

Deep audit · as of 2026-11-08 · version audited: `main` at commit 8b6bf173 (2026-11-08 09:32 CST), Columbus 0.13.0 · by Blueprint

This copy is kept in the repository's `docs/` as the evidence behind the fix kit. It names no secret values and nothing private. The only credentials in the repository are test-fixture passwords for throwaway CI databases (`workers/python/tests/test_db_engines.py`, `ci.yml`), which is normal.

## Summary

*Versions here are as of 2026-11-08.*

Columbus is a well-built, heavily tested governance layer: 287 Rust tests and 115 Python tests pass, with zero failures. Its reach is much narrower than its owner's intent, though. It can't read ArcGIS Online/Portal, CSV/Excel or GeoJSON-type files. It can't use the analyst's own Python or arcpy. It can't write a file geodatabase, and it can't export rasters. One open-data reader can silently drop rows. This audit found 22 findings: 7 Fix now, 14 Fix when convenient, 1 Accept. AF-17 to AF-22 came from checking the owner's weekly right-of-way workflow against the code. The recommended next step is the fix kit beside this report: a narrow rework, built as release 0.14.0.

## As-built summary

*Versions here are as of 2026-11-08.*

- **What it is:** a governed GIS MCP server (`columbus-mcp`, Rust) that any MCP AI app can use, plus a Tauri desktop app (plain HTML/JS) with its own Anthropic chat, a MapLibre map page, approvals, workflows and an audit-log viewer.
- **Size and pace:** about 100,000 lines in 247 commits between 2026-11-03 19:37 and 2026-11-08 09:32. That's 28.7k lines of gateway code, 15.3k of gateway tests, 10k of shared rules, 13.3k of Python engine, 3.6k of Rust plugin SDK, 7k Rust + 5.9k JS for the app, and 5.9k of skills. It has 41 MCP tools.
- **Stack:** Rust 2021 (rmcp 3.8, sqlparser 0.63, tokio), with the Python 3.12 engine installed by pip (geopandas 1.2, pyogrio 0.13 with GDAL 3.12, shapely 2.2, pyproj 3.8, rasterio 1.5, DuckDB). Tauri 2. Packaged as `.mcpb`, `.dmg`, `.msi`/`.exe` and a server zip.
- **Sources served:** GeoPackage/SQLite, PostGIS, Oracle, SQL Server, folders of GeoTIFFs, shapefiles and file geodatabases (read-only) (`crates/gateway/src/server.rs:46-65`).
- **Outputs:** gpkg, geojson, fgb, shp and csv, vector only (`crates/gateway/src/export_tools.rs:795-799`).
- **Governance:** a four-layer access intersection, SQL rewriting for row filters and hidden fields, approvals bound to request hashes, per-OS confinement, a hash-chained audit log, provenance and checks on every result.
- **CI:** Linux, macOS and Windows, with PostGIS, Oracle and SQL Server containers. Releases come from a manual workflow run.
- **Planning documents** (outside the repo, supplied by the owner): the "Columbus design set" (the agreed plan), the app design overview, the sharing/approvals designs, the RustGIS exploration, and the status-and-queue page.

## Coverage

*Versions here are as of 2026-11-08.*

| Area or path | Read, sampled or skipped | Reason |
|---|---|---|
| README.md, docs/reference.md, docs/worker-protocol.md | Read | Stated intent and as-built claims |
| Owner's planning documents (design set, overview, designs, RustGIS, status) | Read | Original intent |
| crates/gateway/src (server, config, fetch_tools, export_tools, script_tools, edit_tools) | Sampled at the places each finding cites | Data reach and the intent's capabilities |
| crates/shared/src/policy.rs, sqlcheck.rs | Sampled | Connection types and SQL dialects |
| crates/broker (oauth.rs) | Sampled | Reuse for ArcGIS sign-in |
| workers/python (columbus_esri.py, columbus_worker.py, columbus_gis.py) | Sampled | Engine formats and drivers |
| app/desktop/ui (connections.js, projects.js) | Sampled | "Coming" placeholders |
| app/ui, crates/confine, crates/chat, crates/flow, workers/sdk-rust | Skipped | Not on the intent's gap list; their tests pass; UI is out of scope (separate review) |
| skills/ | Sampled (names, check script) | Advice only, checked in CI |
| geospatial-master-sheet.csv | Sampled (header, counts) | Registry seed, not Columbus code |
| Git history | Read (247 commit subjects) | Pace and drift |

| Command or test | Where it ran | Run or not run | Result or reason |
|---|---|---|---|
| `cargo test --workspace --no-fail-fast` | Temporary copy, Linux, no GIS engine | Run | 287 passed, 0 failed; 20 tests skipped themselves but reported "ok" (no engine, no databases) |
| `cargo test -p columbus-mcp` with the GIS engine installed | Temporary copy, Linux, Python 3.13 venv | Run | 136 passed, 0 failed; 5 skipped (PostGIS, Oracle, SQL Server hosts not set) |
| `pytest workers/python/tests` | Same venv | Run | 115 passed, 69 skipped (databases and DuckDB not installed), 0 failed |
| Oracle, SQL Server, PostGIS tests | — | Not run | Need database containers; CI runs them |
| Windows and macOS confinement tests | — | Not run | Need those systems; CI runs them |
| Desktop app e2e (`app/desktop/e2e`) | — | Not run | Needs a built Tauri app and a display |

## Intent and spec

Intent (G1, chosen by Claude on 2026-11-08 at the owner's request, merged from the owner's own version):

> Columbus lets GIS analysts and developers use AI on all of their organization's data, safely. They connect any source (Oracle and SQL Server geodatabases, file geodatabases and shapefiles, ArcGIS Online/Portal, CSV/Excel, web APIs) and set its rules first: read-only or read-write, edit a copy, which rows and fields. The AI then works the data with any Python the analyst has, including ArcGIS Pro's arcpy and environments they build. It follows the organization's SOPs from its documentation, checks its own results, and saves outputs locally. Repeated work becomes workflows and skills. It is fast, dependable, and quick to use.

Outcome: **narrow rework** (owner, 2026-11-08). A separate UI-review project follows. The updated spec is the build brief in this kit (`docs/build-brief.md`).

## Standards checklist

| Item | Source | As of | Met, missed or not applicable |
|---|---|---|---|
| A1 Oracle, SQL Server, PostGIS read and read-write | Intent; design set §3 | 2026-11-08 | Met for reads; edits on PostGIS only (AF-8) |
| A2 File geodatabases and shapefiles, read and edit | Intent; design set §3 | 2026-11-08 | Partly: read only (AF-8) |
| A3 `.sde` through ArcPy | Design set §2–3 | 2026-11-08 | Missed (AF-2) |
| A4 ArcGIS Online/Portal signed in | Intent; design set §3 | 2026-11-08 | Missed (AF-1) |
| A5 CSV/Excel and web APIs as sources | Intent; design set §3 | 2026-11-08 | Missed (AF-3); public open data only through `registry_fetch` |
| A6 Rules set beforehand | Intent; design set §3 | 2026-11-08 | Met |
| A7 Any Python environment, arcpy | Intent | 2026-11-08 | Missed (AF-2) |
| A8 Local outputs in analyst formats | Intent; design set §3 | 2026-11-08 | Partly: no FGDB, no rasters (AF-5) |
| A9 Workflows and skills | Intent; design set §8 | 2026-11-08 | Met |
| A10 Documentation always referenced | Intent; design set §5 | 2026-11-08 | Met (context pack, notes, imports) |
| A11 Checks, provenance, save as script | Design set §3–4 | 2026-11-08 | Partly: no save as script (AF-14) |
| A12 Long tail of tools through the registry | Design set §3, §6 | 2026-11-08 | Missed (AF-13); superseded in part by A7 |
| A13 Usable from external AI apps | Owner 2026-11-08 | 2026-11-08 | Met |
| B1 Feature-service paging per Esri | Esri REST API Query docs | 2026-11-08 | Missed (AF-4) |
| B2 ArcGIS sign-in with PKCE | developers.arcgis.com | 2026-11-08 | Not applicable yet (AF-1) |
| B3 Never modify arcgispro-py3 | Esri "What is conda" | 2026-11-08 | Not applicable yet (AF-2) |
| B4 arcpy licensing reported plainly | Esri community threads | 2026-11-08 | Not applicable yet (AF-2) |
| B5 MCP security practice | MCP 2025-11-25 (secondary sources) | 2026-11-08 | Met (pinned definitions, untrusted output, least privilege) |
| B6 Security baseline | Blueprint | 2026-11-03 | Met for what exists |
| B7 Dependencies pinned, permissive | Blueprint | 2026-11-03 | Partly: GIS requirements unpinned (AF-11) |
| B8 Lasting formats written down | Blueprint | 2026-11-03 | Met in docs/reference.md |
| B9 Docs match code | Audit standard | 2026-11-08 | Missed (AF-10) |
| B10 Accessibility | Design set §9 | 2026-11-08 | Deferred to the UI review |
| C1 First answer under 2 s warm | Claude target | 2026-11-08 | Not measured; no benchmark exists (AF-15) |
| C2 One approval per typical task | Claude target | 2026-11-08 | Missed for scripts (AF-6) |
| C3 bbox uses the spatial index | Claude target | 2026-11-08 | Missed (AF-9) |
| C4 Plain failures with the fix | Claude target | 2026-11-08 | Met for existing sources |
| C5 Every source × capability tested | Claude target | 2026-11-08 | Partly: 20 skipped tests report "ok" (AF-12) |

## Findings

*Versions here are as of 2026-11-08.*

| ID | Finding and why it matters | Evidence | Status | Severity | Likelihood | Label | Re-check |
|---|---|---|---|---|---|---|---|
| AF-1 | ArcGIS Online and Portal layers cannot be a connection, signed in or public. The primary users' main cloud source is unreachable except as one-off open-data downloads. | `crates/gateway/src/server.rs:46-63` (no such type); `app/desktop/ui/js/pages/connections.js:62-66` ("Coming in a later stage"); `crates/gateway/src/fetch_tools.rs:229-242` (no token) | Verified | 9 | 10 | Fix now | Confirmed by fresh reviewer |
| AF-2 | Scripts and tools run only in the single engine Python. There is no per-script environment, nothing creates one, and arcpy is unavailable. Swapping the whole engine to a Pro clone works only if that clone also carries every engine library. | `crates/gateway/src/config.rs:59-65`; `crates/gateway/src/script_tools.rs:438`, `:536`; `docs/reference.md:71` | Verified | 9 | 10 | Fix now | Partly confirmed: the engine Python can be swapped wholesale; the finding is worded to match |
| AF-3 | CSV, Excel, GeoJSON, GeoParquet, KML and FlatGeobuf files cannot be connections, although the README lists CSV as a data source. Analysts' most common hand-off formats are refused. | `crates/gateway/src/server.rs:46-63`, `:1424`; `README.md:94`; `docs/reference.md:280-287` | Verified | 8 | 10 | Fix now | Confirmed |
| AF-4 | Fetching from ArcGIS REST services can silently lose or duplicate rows. Paging stops on an empty page even when the server says more remain, and there is no fixed sort order. The result is then not marked as cut off. | `crates/gateway/src/fetch_tools.rs:290` (`!more \|\| got == 0`), `:229-242` (no `orderByFields`), `:540`, `:572-585` | Verified | 7 | 4 | Fix now | Confirmed (wrong results reported as complete) |
| AF-5 | Exports cannot write a file geodatabase, and rasters cannot be exported at all. Analysts' standard deliverable formats are missing. | `crates/gateway/src/export_tools.rs:793-801`, `:221`; `workers/python/columbus_gis.py:952-958`; `docs/reference.md:67` | Verified | 7 | 9 | Fix now | Confirmed |
| AF-6 | Every script run needs its own approval with the full code, with no option to allow it for a session. A multi-step analysis asks again and again, the main source of slowness against target C2. | `docs/reference.md:71` ("There is no \"allow for this session\" for scripts") | Verified | 5 | 9 | Fix when convenient | Not required (not Fix now) |
| AF-7 | The default policy allows a connection type `file` that no engine serves, so a person can save a connection that every query refuses. | `crates/shared/src/policy.rs:590`; `crates/gateway/src/server.rs:46-63` | Verified | 3 | 4 | Fix when convenient | Confirmed in passing by the fresh reviewer |
| AF-8 | Edits work on GeoPackage and PostGIS only. File geodatabases, shapefiles, Oracle and SQL Server cannot be edited, even into a copy. | `docs/reference.md:73` | Verified | 6 | 6 | Fix when convenient | Not required |
| AF-9 | A bbox in `query_features` is not pushed into the source's spatial index, so large tables are scanned in full. | `docs/reference.md` roadmap, unchecked item "Bounding boxes pushed into the database's spatial index" | Verified (from docs) | 5 | 6 | Fix when convenient | Not required |
| AF-10 | Documentation drift: the README says Projects and Data groups are "Coming" (both shipped in 0.11.0), and lists CSV as a source. A reader is misled about what works. | `README.md:79`, `:81`, `:94`; commits of 2026-11-07 "Data groups (app)" and "Projects page" | Verified | 3 | 8 | Fix when convenient | Not required |
| AF-11 | GIS engine dependencies are unpinned version ranges, while the project-database ones are hash-pinned. A new release of geopandas or pyogrio can change results or break installs without any code change. | `workers/python/requirements-gis.txt` (ranges such as `geopandas>=1.0,<2`); `.github/workflows/ci.yml:31` | Verified | 5 | 4 | Fix when convenient | Not required |
| AF-12 | Tests that lack the GIS engine or a database print "skipping" and report "ok", so a local run shows green while 20 tests ran nothing. | `crates/gateway/tests/gis.rs:190`, `:492`, `:641`; local run, 20 "skipping" lines, 287 "passed" | Verified | 3 | 5 | Fix when convenient | Not required |
| AF-13 | Tool search ignores the registry's operations (65 operations, 446 mapped implementations in geo-registry), so the design set's long tail of tools is unreachable. | `crates/gateway/src/registry_tools.rs` and `server.rs` contain no reference to `operations`; design set §3 | Verified | 4 | 5 | Fix when convenient | Not required |
| AF-14 | There is no "save as script" or runnable-script export, which the design set promised for rerunning work without AI. | No match for "save as script" or "runnable script" in code; design set §3 Provenance | Verified | 4 | 4 | Fix when convenient | Not required |
| AF-15 | There is no timing benchmark, so "fast" (target C1) cannot be shown or protected from regressions. | No benchmark test in `crates/gateway/tests` or `workers/python/tests` | Verified | 4 | 6 | Fix when convenient | Not required |
| AF-16 | Files returned by connected MCP servers are refused by the GIS tools, which blocks the RustGIS connected-server pilot. | `docs/reference.md` GIS tools paragraph; owner's RustGIS exploration, "Owner decision needed" | Verified | 3 | 3 | Accept: a person-owned Python environment (D-3) reaches RustGIS's and Whitebox's Python packages under Columbus's rules without weakening the untrusted-output rule | Not required |
| AF-17 | Line geometries with M values (route networks with measures) can't be written to the workspace, so `query_features` fails on an M-aware Oracle route layer. This is core to linear-referencing work. | Reproduced in a temporary copy: an M `LINESTRING` passed through `workers/python/columbus_worker.py:545-568` (EWKB with M flag) into `workers/python/columbus_gis.py:419-426` raised "Could not create geometry at index 0 for WKB type 1073741826"; no test covers M | Verified | 8 | 7 | Fix now | Reproduced by the auditor (self-check; found after the fresh-reviewer pass) |
| AF-18 | Workflows can't read the new source types or deliver the new formats: their own tool and format lists (`STEP_TOOLS`, `FORMATS`) are separate, and ArcGIS sign-ins lapse without use. | `crates/shared/src/workflows.rs:71-84` | Verified | 7 | 8 | Fix now | Self-check |
| AF-19 | A workflow run's results can't be opened in a chat or on the map for review. | No open-in-chat path in `app/desktop/src-tauri/src/workflows.rs` or `app/desktop/ui/js/pages/workflows.js` | Verified | 5 | 8 | Fix when convenient | Not required |
| AF-20 | Clicking a table row highlights but doesn't zoom to the feature, and the map has no measure tool. | `app/ui/src/MapView.tsx:198-213`; no measure code in `app/ui/src` | Verified | 4 | 8 | Fix when convenient | Not required |
| AF-21 | Removing rows from a workspace layer needs an approved script or a project-database round trip. | `query_features` takes connection datasets only (`crates/gateway/src/gis_tools.rs:31-66`); `vector_op` has no filter op | Verified | 4 | 6 | Fix when convenient | Not required |
| AF-22 | The map can't show imagery or any web map service: one fixed street basemap, and the page's content policy allows only that host. Analysts can't review against aerial photos or their own map services. | `app/ui/src/MapView.tsx:19-21`; `crates/gateway/src/approvals_page.rs:905-916` | Verified | 5 | 9 | Fix when convenient (owner asked for it: in the kit) | Not required |

## What this audit could not check

- Oracle, SQL Server and PostGIS behaviour. These tests need database containers; CI runs them, and their results here come only from the code. Checking them takes Docker, or reading CI logs (GitHub API access was not enabled for this session).
- Windows and macOS confinement (AppContainer, Seatbelt). Checking them takes those systems; CI runs them.
- Whether arcpy works when launched by Columbus, confined or not. Checking it takes Windows with a licensed ArcGIS Pro; the fix kit makes this a manual check.
- The desktop app's screens. These are out of scope, left for the separate UI review.
- CI history and release state. GitHub API access was not enabled; the owner's status page says CI was green at 8b6bf17.

## Recommended fix set

*Versions here are as of 2026-11-08.*

Every Fix now finding (AF-1 to AF-5, AF-17, AF-18), the owner's-workflow findings AF-19 to AF-22, plus the Fix when convenient findings that are cheap or touch the same files: AF-6, AF-7, AF-9, AF-10, AF-11, AF-12, AF-15. Deferred to a later round: AF-8 (Esri and Oracle/SQL Server edits beyond copies), AF-13 (registry operations in tool search), AF-14 (save as script). Accepted: AF-16.

One-way doors decided at G3, delegated to Claude by the owner: D-1 (where the build happens), D-3 (trust model for the person's own Python environments), D-4 (how ArcGIS Online/Portal is reached), D-5 (engine packaging), D-12 (who owns the ArcGIS OAuth app). D-7 (publishing the 0.14.0 test release) was pre-approved by the owner. The reasons are in the build brief.
