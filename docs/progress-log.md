# Progress log: Columbus 0.14.0 rework

Kit version 1.1 · as of 2026-11-08, session S1 not started

## Where things stand

- **Session:** done · **Task:** none (0.14.0 released)
- **Next step:** the owner's follow-ups (below in the newest handoff note); the separate UI-review project; CR-1 and CR-3 in a Blueprint round
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
| 2026-11-08 | Trusted runs start Python directly (no launcher) with the person's environment minus `COLUMBUS_*`; `environment:<id>` goes on outputs, audit and provenance labels, not session labels | arcpy's licence check needs the person's profile; the brief labels outputs | Decided |
| 2026-11-08 | Licence words: ArcInfo Advanced, ArcEditor Standard, ArcView Basic; ArcServer/Engine/EngineGeoDB "(not an ArcGIS Pro licence)"; others "unrecognized" | Esri's ProductInfo values | Decided |
| 2026-11-08 | Probe limit stays 120 s; only debug builds read `COLUMBUS_TEST_PROBE_SECONDS`, which can only shorten it | Test hook without changing the product | Decided |
| 2026-11-08 | On Windows, trusted runs, probes and environment creation run in a job object (kill on close); `taskkill /T` is the fallback | Ends everything a run started, also after a normal exit | Decided |
| 2026-11-08 | Shared `.columbus` tool files keep the environment id (never a path); the recipient's environment of that name is used, or the tool stops with "needs you" | The manifest hash covers the id; the recipient chooses their own interpreter | Decided |
| 2026-11-08 | Personal tools and workflow steps run in governed level-A environments only; trusted is refused before the policy check | Keeps "personal tools run at level A" and D-3 | Decided |
| 2026-11-08 | Session approval binds to code SHA-256 + environment id and its confirmed hash + the gateway process (one chat or one MCP connection) | R-15, slightly stricter | Decided |
| 2026-11-08 | The tool-code lint keeps its fixed module list inside environments (CR-1) | No rule loosens unless the brief says so | Decided |
| 2026-11-09 | `arcgis` connections stay out of the engine-only paths (plugin extracts, notes drafting, edits), which refuse them plainly; hidden-field and parser checks run before any request | No request leaves for a query that will be refused (R-10b) | Decided |
| 2026-11-09 | ArcGIS transport pins one DNS answer, follows no redirects, waits 30 s per request and caps an answer at 256 MB; the layer list is cached 5 minutes | Same safety as `fetch_tools.rs` | Decided |
| 2026-11-09 | **A-2 unconfirmed:** developers.arcgis.com needs a fetch approval nobody was present for, and web search found no Esri statement (FME and Esri's Unity SDK use a registered `http://localhost` with ports). Both the loopback redirect (`http://127.0.0.1:<port>/`, default) and the brief's `urn:ietf:wg:oauth:2.0:oob` paste-code fallback are built; manual-checks tells the admin to register both | Works whichever way Esri matches | Decided |
| 2026-11-09 | A portal that can't list its federated servers vouches for none (fails closed); in release builds `arcgis:` entries are never written to files | R-17, R-18 | Decided |
| 2026-11-09 | The refresh token's expiry (for R-19) is kept in `connections/.arcgis-state.json` (`{id: {refresh_expires_at, portal}}`, never a token), not in the F-3b entry; a missing record means exchange | F-3b is a one-way format and stays as specified; same pattern as `.confirmed.json` | Decided |
| 2026-11-09 | A failed exchange for a reason other than a lapsed sign-in (portal down) lets the step go on with an `arcgis.signin_not_renewed` warning | Failing the read is worse; the next run retries | Decided |
| 2026-11-09 | The app's Sign in asks for Touch ID / Windows Hello first, then runs `arcgis signin` and opens only an https authorize URL under the connection's portal; environments are confirmed like connections | Sign-in widens what the assistant can read; no rule loosens | Decided |
| 2026-11-09 | gdb workflow deliveries turn non-alphanumeric characters in the name into `_` | The gdb export refuses `-` (R-8 naming) | Decided |
| 2026-11-09 | Bbox pushdown goes in as a placeholder swapped in after checking and falls back to the unpushed query if it doesn't fit; not pushed where an index can't be trusted (SRID-0 PostGIS, SQL Server geography, Oracle without a valid index, shapefile/FGDB, zero-area boxes, Oracle geodetic boxes over 119°); files caches gain R-trees | Same rows either way (REQ-19); speed only where it is safe | Decided |
| 2026-11-09 | The benchmark is an ignored test run in its own Linux CI step; engine start = launch to the first GIS answer on a one-row layer; warm = the second identical read of 100,000 rows | Targets apply to the Linux runner only | Decided |
| 2026-11-09 | `sources.py` CI hang root-caused: the single-instance app (D-Bus name) still held by the closing first session; fixed in the test with `e2e/guard.py` (wait for the name, 120 s per WebDriver call, 10-minute overall limit, step names) and `timeout-minutes: 30` on the step | Not the product; a hang must fail with a message | Decided |
| 2026-11-09 | **A-4 not confirmed** for uv-compiled, hash-pinned files (GitHub docs silent; dependabot-core #10039, #12047). Fallback: `.github/workflows/engine-requirements.yml` regenerates the pins weekly and opens a pull request; `dependabot.yml` covers cargo, npm and Actions | The brief's fallback, no paid service | Decided |
| 2026-11-09 | **macOS minimum is 15** (Apple Silicon). rasterio 1.5's macOS arm64 wheels need 15; capping rasterio at 1.4 would have kept 14 | Narrows supported Macs | Asked (owner: "macOS 15+") |
| 2026-11-09 | Opening a run in a chat copies its outputs (fingerprinted in `run.json`) under the workflow's scope and labels; the run's exports are protected by folder and name; runs made before this change refuse to open ("run the workflow again") | R-20: the run never changes | Decided |
| 2026-11-09 | Zoom to selection is on by default and remembered per session; Measure is geodesic (WGS 84), shown in feet then metres | AF-20 | Decided |
| 2026-11-09 | Web layers: capability hosts approved at save live in `map-sources/.hosts.json` (F-13 unchanged); org-supplied sources fail closed; only 256 px Web Mercator layers offered; tile cache 500 MB; tile requests are not audited; `whoami` lists titles only; new dependency roxmltree 0.21.2 (MIT OR Apache-2.0) | D-14 and the security baseline | Decided |
| 2026-11-09 | Web layer settings are remembered per chat (session); a source's `rev` (12 hex of its fingerprint, never the address) lets the map redraw after an Edit | REQ-33 | Decided |
| 2026-11-09 | T-23 also runs the ROW job on a GeoPackage on all three systems; the Oracle version fails rather than skips if it can't confine; the workflow has no export folder (the only export is the approved one from the chat) | Success measure 7; the non-Oracle parts are testable everywhere | Decided |
| 2026-11-09 | App e2e "previous app still running" root-caused: the harness stopped tauri-driver before WebKitWebDriver ended the app; `guard.stop()` now waits for the app, then ends it with logged SIGTERM/SIGKILL if needed | Not a product bug: closing the window exits in 0.02-0.04 s | Decided |
| 2026-11-09 | Activity › Approvals no longer rebuilds its buttons on every poll (`core.js` `redraw()` updates words in place), with an e2e check that a button survives a poll | A product bug: a person's click could land on a replaced button (found as a shell.py flake) | Decided |
| 2026-11-09 | README pictures use US government data (Census TIGER/Line 2024, NHTSA FARS 2020-2023), credited in `docs/images/DATA-CREDITS.md`; City of Austin and TxDOT data not used | Their licences could not be confirmed | Decided |
| 2026-11-09 | README pictures and render checks run in CI (`.github/workflows/demo-data.yml`); results come back through the job log because artifact downloads are blocked here | The shell can't reach the basemap or github.com | Decided |
| 2026-11-09 | The GitHub sidebar (description, website, topics), the social preview card and private vulnerability reporting are left to the owner | The proxy refuses those settings; the owner will do them after the release | Asked (owner: after the release) |
| 2026-11-09 | Release notes live in `packaging/release-notes.md`; `THIRD-PARTY-NOTICES.md` ships in both zips; the app's four "coming" panels stay (screens out of scope) and the reference says they have no backend | D-2 | Decided |
| 2026-11-09 | Large files in a `files` folder are streamed into the cache in two passes (types from every value, as before); per-file limits: CSV 256 MB or 2.5 MB per allowed second (300 MB at 120 s), Excel 64 MB, GeoJSON/KML/FGB 256 MB, GeoParquet 256 MB and 10 M values; an over-limit or timed-out file is skipped with a plain reason and remembered until it changes | Final review F-1 (R-101): memory bounded (~310 MB) whatever the file size; the time limit, not memory, now decides | Decided |
| 2026-11-09 | Dependabot PRs #9, #13 (rmcp) and #11 (rusqlite) are left for the owner to merge | The session's safety check refuses merging pull requests the session didn't write; both advisories predate the rework | Decided (owner merges) |
| 2026-11-08 | Python baseline is 149 passed, not the audit's 115 | `main` moved past the audit commit 8b6bf173 before the kit upload; more tests and no failures, so no action | Decided |

## Change requests

| ID | Date | What and why | Options | Recommendation | Status |
|---|---|---|---|---|---|
| CR-2 | 2026-11-08 | Professional README and GitHub front page: real-data pictures, demo animation, how-it-works diagram, shorter README, SECURITY/CONTRIBUTING, sidebar. Plan: https://claude.ai/artifact/PM8rt1U5CoxQ2KMdyZoQLz | Do it in T-16 | Do it in T-16 | Done in T-16 (README 107 lines; fresh-reviewer 10-second test passed) |
| CR-3 | 2026-11-08 | Documentation site (Zensical on GitHub Pages leaning; own domain optional) | Later round | Later | Waiting: name, domain, versioning need a Blueprint round; not in 0.14 |
| CR-1 | 2026-11-08 | Let a personal tool in an environment import that environment's other packages (today the tool lint allows only its fixed module list, so whitebox or arcpy can't be used from a tool; `run_python` scripts can use them) | Keep the list; allow any package installed in the governed environment; allow a per-organization list | Keep the list this round (no rule loosens); decide in Blueprint | Sent to Blueprint (blocks no task) |

Status: raised, approved, declined, or Sent to Blueprint (touches a one-way door; affected tasks paused).

## Handoff notes

Newest first, written as you go. Mark each claim verified (checked this session) or unverified.

### Release (2026-11-09)

- **Released:** https://github.com/wiatrek-kevin-s/columbus/releases/tag/v0.14.0 (pre-release, from main e73620f; Release run 42712984935). Every task T-1–T-25 except T-18 is done; R-101 done; R-102 blocked only on the owner merging Dependabot #9, #13, #11.
- **Owner's follow-ups:** sidebar description, website and topics; upload `docs/images/social-card.png` as the social preview; turn on private vulnerability reporting; merge Dependabot #9, #13 and #11 (Rust advisories); optionally allow GitHub Actions to create pull requests (so the weekly engine-pins job can open its PR) and turn on Dependabot alerts; the optional manual checks H-1 and H-2 (docs/manual-checks.md); start the UI-review project (items for it: app title truncation, Tools page "null", relaunch hand-off, small tour pictures, the app's four "coming" panels).
- **Later:** CR-1 (tool lint list in environments) and CR-3 (docs site) for a Blueprint round. The remaining Dependabot PRs (#7, #8, #10, #12, #14, #15) are routine updates for a later round.

### Final review and release (2026-11-09)

- **Final review** (fresh subagent, docs/final-review.md; full notes kept by the lead): every acceptance check T-1–T-16, T-19–T-25 passes on re-run; 375 Rust / 225 Python tests, no engine skips; security negative tests all held. Findings: **F-1 Fix now** (large CSV OOM-killed the engine) → R-101; **F-2** (advisories, 30) → R-102 cheap parts; F-3 = CR-1 (20, Sent to Blueprint); accepted under 20: F-4 no required checks on main (Release has its own CI gate), F-5 A-2 unconfirmed / no paste-code field in the app, F-6 backslash in rebuilt where (theoretical), F-7 relaunch hand-off, F-8 Tools page "null", F-9 record slips (PR #16 named #7, fixed here; badge; two old workflow registrations), F-10 folder-wide row_filter fails files without the column (safely), raw GDAL text for a broken GeoJSON.
- **Re-check** (second fresh subagent): F-1 FIXED (400 MB and 1 GB skipped plainly at defaults, 6.6 M rows convert with limits.seconds 600 at 312 MB peak, no partial data), F-2 Pillow part FIXED. New, accepted: N-1 (12) a 250–300 MB CSV is near the 120 s limit on a slow laptop and stays skipped until it changes; N-2 (2) a >4,300-digit integer cell gives Python's own message.
- **R-101 done; R-102 blocked** only on the owner merging Dependabot #9, #13, #11.
- **Next:** release v0.14.0.

### Session S7 → final review (2026-11-09)

- **Done:** S6 merged (PR #6, 5543dd9). S7: T-23 and T-16 (with CR-2) on `rework/s7-docs`, PR #16; also two root-caused test problems (harness stop order; approvals page re-render, a product fix). CI run 42685605864 green on 15 jobs.
- **Every task T-1 to T-25 except T-18 is done.** None blocked. CR-1 (tool lint list in environments) is Sent to Blueprint and listed in the release notes; CR-3 (docs site) is for later.
- **Owner's to-do after the release:** sidebar values, social preview card (`docs/images/social-card.png`), private vulnerability reporting; optional H-1 and H-2 (docs/manual-checks.md); start the UI-review project.
- **For the final review:** relaunching the app within ~2 s of closing it hands off to the closing instance; a stray "null" on the Tools page; the README release badge updates on release.
- **Next:** merge PR #7, final review, T-17.

### Session S6 → S7 (2026-11-09)

- **Done:** S5 merged (PR #5, 27d6725). S6: T-13, T-14, T-15, T-21, T-22, T-24, T-25 on `rework/s6-speed-map`, PR #6. Each verified in a dispatched CI run green on 13 jobs (last: 42646478559).
- **Decisions and deviations:** see Decisions. A-4 used the fallback. macOS minimum is 15 (owner).
- **Loose ends:** T-16 must carry CR-2 (the README plan doc), the macOS 15 minimum, the uv licence notice (third-party notices file), the ArcGIS admin steps (redirect URIs `http://127.0.0.1` and `urn:ietf:wg:oauth:2.0:oob`, the org's own client id) in docs/manual-checks.md, and CR-1 in the release notes. For the final review: relaunching the app within ~2 s of closing it hands off to the closing instance and nothing opens; e2e scripts still occasionally wait on the previous app to close (guard reports it plainly). `cargo test --test web_layers` / `workflow_reach` alone need `cargo build --example fake_oauth_server` first.
- **Machine:** server restarts stopped helpers three times; each resumed from its saved work. Disk is small: builds without debug info.
- **Next:** S7.

### Session S5 → S6 (2026-11-09)

- **Done:** S4 merged (PR #4, 7517e06). S5: T-10, T-11, T-12, T-20 on `rework/s5-arcgis`, PR #5. CI run 42549810624 green on 13 jobs for T-10 to T-12, including the new app e2e `sources.py`.
- **Decisions and deviations:** see Decisions. A-2 unconfirmed; both redirect paths built.
- **Loose ends:** T-24 hooks: `arcgis_auth::signed_in(store, id)` before reusing a secured tile cache; `arcgis_auth::sign_out` is the one place sign-out happens (CLI and connection deletion). T-16 writes docs/manual-checks.md with the admin steps: register the org's own OAuth app with redirect URIs `http://127.0.0.1` and `urn:ietf:wg:oauth:2.0:oob`, then put its client id in `auth.client_id` (D-12). `cargo test --test workflow_reach` alone needs `cargo build --example fake_oauth_server` first.
- **Machine:** disk is small; helpers build with `CARGO_INCREMENTAL=0` and no debug info. The app's Linux libraries are installed for local e2e.
- **Next:** S6.

### Session S4 → S5 (2026-11-09)

- **Done:** S3 merged (PR #3, 01814e9). S4: T-8 and T-9 on `rework/s4-trusted`, PR #4; T-8 verified green on all systems on the helper branch `rework/s4-trusted-windows` (merged in, kept, not deleted).
- **Decisions and deviations:** see Decisions. CR-1 sent to Blueprint (tool lint module list); blocks nothing.
- **Loose ends:** T-10 must request `f=json` (ESRIJSON) so Z/M survive; the T-5 pager (`crates/gateway/src/arcgis_pager.rs`, `fetch_layer` + `LayerQuery` + `Transport`) supports it and takes a caller-supplied bearer. T-12 adds the app's approve button for environment proposals (`env_create::approve`) and Settings › Environments. Third-party notices (uv) → T-16. The approvals page may not render `detail.warning`; the sentence is in the summary.
- **Next:** S5.

### Session S3 → S4 (2026-11-08)

- **Done:** S1 merged (PR #1, 80559e2), S2 merged (PR #2, 4148908); S3 (T-6, T-7) on `rework/s3-environments`, PR #3. All verified in CI except S3's own PR run.
- **Decisions and deviations:** see Decisions; none touch a one-way door. A-1 and A-5 confirmed.
- **Loose ends:** app approve button for environment proposals → T-12; third-party notices file (uv) → T-16; T-10 must ask ArcGIS for `f=json` so M survives (the T-5 pager supports it). T-8 replaces the trusted-refusal branch in `script_environment()` (script_tools.rs); whoami cards are built in `Gateway::environment_cards()`. T-9 wires `scripts.session_approval` (parsed, no effect yet) and calls `script_environment` + `check_environment` for tools and steps.
- **How we work:** the lead keeps the log and task list; each task goes to a fresh helper with `/home/claude/briefs/common.md` (session-local). PRs via REST (`gh pr create` needs GraphQL, which is blocked here). CI runs on pull requests; dispatch CI for early checks on a branch.
- **Next:** S4.

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
- 2026-11-08 PR #3 merged (01814e9) after CI run 42530598019 green on 13 jobs.
- 2026-11-08 T-8: commit 068a440 green on Linux and macOS; Windows CI (run 42531980419) hung, cancelled to read logs. Causes were in the tests (`\\?\` prefix in a path compare; a terminal prompt waiting on the runner's console). Fixed on `rework/s4-trusted-windows` with a job-object process tree; run 42539452409 green on 13 jobs. T-8 done.
- 2026-11-09 T-9 done: commit 50e23d6; environments_tools.rs pass (verified after merge); workspace 330. A-6 recorded as expected (arcpy needs trusted; H-2 confirms).
- 2026-11-09 PR #4 merged (7517e06) after CI run 42540620881 green on 13 jobs.
- 2026-11-09 T-10 done: commit 37459a3; arcgis.rs 2/2 (fake FeatureServer with request log).
- 2026-11-09 T-11 done: commit 7c9cf15; arcgis_signin.rs pass; token found 0 times outside the credential store. A-2 unconfirmed, both redirect paths built.
- 2026-11-09 T-12 done: commit e544226; app e2e sources.py ok locally and in CI run 42549810624 (13 jobs green).
- 2026-11-09 T-20 done: commit 38a94ab; workflow_reach.rs pass (gdb delivery; 6-day exchange, 8-day none, expired stops with "needs you").
- 2026-11-09 PR #5 merged (27d6725) after CI run 42552085685 green on 13 jobs.
- 2026-11-09 T-13 done: b7ece5c, c1a8cab, b807b84; EXPLAIN names the GiST index; Oracle, SQL Server, GeoPackage, files use their indexes; CI run 42555648758.
- 2026-11-09 T-14 done: f8c1d88; CI Linux `BENCH engine_start_s=0.48 warm_query_s=1.61 rows=100000` (targets 10 s and 2 s), run 42557772645.
- 2026-11-09 sources.py hang in run 42555648758 (cancelled after ~20 min; re-run green) root-caused and fixed in a324f6d; run 42561672255 green.
- 2026-11-09 T-15 done: 1c974f1; run 42614233624 green; 0 engine skips in CI; tampered hash refused; A-4 fallback.
- 2026-11-09 T-21 done: d3c084e; run 42626537952 green (includes the vector_op filter, AF-21).
- 2026-11-09 T-22 done: e35e63b; run 42630389678 green; 100 m line shows 328 ft.
- 2026-11-09 T-24 done: ca47383; run 42637569401 green.
- 2026-11-09 T-25 done: c038f8f; run 42646478559 green.
- 2026-11-09 PR #6 merged (5543dd9) after CI run 42649289887 green.
- 2026-11-09 T-23 done: 7ea37ee; row_job_end_to_end_on_oracle ran in the Oracle job; run 42651014882 green (attempt 2; attempt 1 hit the app-exit wait, since root-caused in 45c6238).
- 2026-11-09 shell.py stale element root-caused (approvals page rebuilt buttons every poll): fixed in c198acd; run 42673657061 green; merged into S7.
- 2026-11-09 T-17 done: v0.14.0 released (pre-release) from e73620f; Release run 42712984935; PR #17 merged (e73620f) after CI run 42708308018 green.
- 2026-11-09 Final review done (F-1 Fix now). R-101 done: fb20570, run 42705526192 green on 14 jobs; R-102: e393259 Pillow 13, pip-audit clean; Dependabot merges left to the owner. Re-check confirmed both.
- 2026-11-09 T-16 done: 4d0ad47 and 641fde0..364de06; run 42685605864 green on 15 jobs; README 10-second test passed (7/10, then a polish round).

### Kit → Session S1 (2026-11-08)

- **Done:** nothing yet. The audit baseline on `main` at 8b6bf173: 287 Rust tests and 115 Python tests passed with 0 failures in a temporary copy (unverified for you; re-run in T-1).
- **Decisions and deviations:** none.
- **Loose ends:** assumptions A-1 to A-6 in the brief are open; each names the task that settles it. AF-17 (M geometry) was reproduced in the audit; T-19 fixes it.
- **Saved:** nothing yet.
- **Next:** T-1. Commit `dbe0f20` is the model for adding a connection kind (T-2).
