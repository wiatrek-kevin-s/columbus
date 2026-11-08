# Build brief, part 2: the owner's weekly right-of-way workflow, and web map layers

Version 1.1 · as of 2026-11-08 · prepared with Blueprint · adds to `docs/build-brief.md` (part 1); IDs continue from it.

Part 1 makes Columbus reach more data. This part makes one real job work end to end, from a weekly run through to the analyst's review: the owner's right-of-way (ROW) width workflow. Each requirement below exists because that job fails without it. The same rules as part 1 apply: one source per fact, and cite IDs.

## The job (success measure 7)

1. A read-only Oracle connection holds an LRS event table: route id, from-measure, to-measure, and ROW width in feet. It also holds the route network as polylines with M values.
2. Every Friday a workflow runs two personal tools:
   - **find-gaps** lists measure ranges with no width row.
   - **fix-widths** takes the gaps and suspect rows. It reads the owned-ROW polygon layer from ArcGIS Online, casts transects along each route at intervals, measures where each transect overlaps the polygon in feet, and groups the values into segments: 20, 23, 22, 20, 21 become one segment of 21.
3. The same workflow writes a second layer of the results dissolved by route name, for review.
4. The analyst opens the run's results on the map, beside the ROW polygons from ArcGIS Online. They click from route to route in the table; the map zooms to each one, and they measure a few widths by hand.
5. If needed they say "remove route X from the output", then export the approved layer to a local folder. They load it into Oracle themselves, outside Columbus.

**Success measure 7:** steps 1 to 5 run on fixtures in CI, from a run started with `columbus-mcp workflow run <id> --scheduled` (what Task Scheduler and launchd call) to export, with no approval except the final export to a folder that isn't granted (REQ-24 to REQ-30).

## What Columbus already does for this job (as of 2026-11-08)

- Oracle read-only connections, and weekly workflows run by Task Scheduler or launchd.
- Personal tools in the engine's Python (geopandas, shapely) that run inside workflows without asking.
- vector_op dissolve by field, export folders, provenance and checks.
- The example LRS gap checker in `examples/plugins/lrs-check`, which shows gaps and overlaps of events along routes.

The owner's tools are written later, from a chat, with `propose_tool`. Columbus must give them what they need, below.

## What fails today, and the fix

### AF-17: line geometries with M values can't be written to the workspace (Fix now)

**Reproduced 2026-11-08:**
- Oracle `SDO_GEOMETRY` with M comes back as EWKB with the M flag (`workers/python/columbus_worker.py:545-568`).
- `write_layer` (`workers/python/columbus_gis.py:419-426`) then fails with "Could not create geometry at index 0 for WKB type 1073741826" when it writes a `LINESTRING M` to GeoPackage through pyogrio.
- That was run with the engine's own code in a temporary copy, using shapely 2.2 and pyogrio 0.13.
- No test covers M geometry.

Every route network with measures fails at `query_features`.

**Fix (REQ-24):** M values (and Z) survive every step:
- `query_features` on every engine, in both the in-memory and the streamed writer;
- `vector_op` where the operation keeps vertices (reproject, clip, join, spatial_join, dissolve);
- personal-tool and plugin extracts (the plugin SDK already reads the M flag, `workers/python/columbus_plugin/__init__.py:170-186`);
- the ArcGIS pages (part 1, Part 4);
- exports to gpkg, gdb and shp;
- the map, which draws 2D and ignores M without failing.

Write geometry as ISO WKB, or with an explicit dimension, so GDAL gets the M type it expects. Operations that rebuild vertices (buffer, simplify) drop M and add an info check `geometry.m_dropped`.

### AF-18: a workflow can't read ArcGIS layers or deliver file geodatabases (Fix now, for this job)

Part 1 adds `arcgis` connections and `gdb` exports, but workflows have separate lists. `STEP_TOOLS` and `FORMATS` in `crates/shared/src/workflows.rs:71-84` would still refuse a step on an `arcgis` connection's dataset and a `gdb` output. Signed-in layers also need a sign-in that outlives two weeks of unattended runs.

**Fix (REQ-25, REQ-26):**
- Workflow steps may read `files` and `arcgis` datasets through the same tools.
- `outputs.folder` formats add `gdb` and `tif`.
- The gateway uses `grant_type=exchange_refresh_token` to swap the ArcGIS refresh token for a new one whenever fewer than 7 days of its life remain. A weekly workflow therefore keeps its sign-in alive.
- If the sign-in has lapsed anyway, the step stops with "needs you: sign in again (columbus-mcp arcgis signin row-polygons)", naming the connection, and the run report says so.

### AF-19: a workflow run's results can't be reviewed on the map (Should)

Run outputs live in the run folder and appear only in the run report. Nothing opens them in a chat, and a chat's map shows only that chat's workspace.

**Fix (REQ-27):** a run's report gets **Open in a chat**, and `columbus-mcp workflow open <id> <run>` does the same. It starts a chat whose workspace receives copies of the run's output layers, each fingerprinted and labelled exactly as the run left them, with the run's provenance. The person or the assistant can then add other layers, such as the ROW polygons, and export from there under the usual rules.

### AF-20: no zoom to a selected row, and no measuring on the map (Should)

`app/ui/src/MapView.tsx:198-213` highlights the selected row's feature but doesn't move the map, and the map page has no measure tool. Route-by-route review is slow without both.

**Fix (REQ-28):**
- Clicking a table row zooms the map to that feature. A **Zoom to selection** toggle keeps this on or off, remembered per chat.
- A **Measure** button measures a line drawn on the map. It shows feet and metres, geodesic, one result at a time, and Escape clears it.
- Both use the existing map and table components. The full redesign stays with the separate UI review.

### AF-21: rows can't be filtered out of a workspace layer without a script or a project database (Should)

"Remove route X" today needs `run_python` (an approval each time) or a `project_db` round trip. `query_features` reads only connection datasets (`crates/gateway/src/gis_tools.rs:31-66`).

**Fix (REQ-29):** `vector_op` gains `op: filter` with `params: {where}`. It takes any input `vector_op` takes and uses the same where parser as `query_features`. The result is a new layer, so the original stays and provenance records the filter. It needs no approval, because nothing leaves the workspace.

### The job, end to end (REQ-30)

A fixture-based test runs the job:
- an Oracle fixture holds an M-aware route table and an event table with two gaps;
- a fake ArcGIS service serves ROW polygons;
- two small stand-in personal tools: one finds gaps by measure, one casts transects every 50 ft and averages runs of widths within ±2 ft.

Their algorithm is a stand-in; the owner's real tools come later. The test proves the plumbing, not the owner's algorithm.

## Requirements

| ID | Requirement | Priority | Task |
|---|---|---|---|
| REQ-24 | M and Z values survive per AF-17. Given an Oracle (CI) and a GeoPackage route layer with `LINESTRING M`, when queried, reprojected, joined and exported to gpkg, gdb and shp, then each output's first vertex has the source's M value. Edge: buffer drops M with `geometry.m_dropped`. Failure: none of these steps errors on an M geometry. | Must | T-19 |
| REQ-25 | Workflow steps read `files` and `arcgis` datasets; `outputs.folder` accepts `gdb` and `tif`. Given a workflow with a step on a fake `arcgis` layer and a `gdb` output, when run, then the run succeeds and the `.gdb` is in the export folder. Failure: a step on a connection outside the workflow's scope is still refused. | Must | T-20 |
| REQ-26 | The ArcGIS refresh token is exchanged when under 7 days remain; a lapsed sign-in stops the step with "needs you". Given a fake token with 6 days left, when a workflow step runs, then a new refresh token is stored. Edge: one with 8 days left is not exchanged. Failure: an expired one stops the step and the report names the sign-in command. | Must | T-20 |
| REQ-27 | A run's outputs open in a new chat (AF-19). Given a finished run with two output layers, when opened, then the chat lists both with the run's labels, fingerprints and provenance, and they draw on the map. Failure: a run whose files changed on disk since the run refuses to open, naming the file. | Should | T-21 |
| REQ-28 | The map zooms to the selected row, and measures (AF-20). Given a layer of 3 routes, when row 2 is clicked, then the map fits route 2. When a 100 m line is measured, then it shows 328 ft (±1%). Edge: the zoom toggle off keeps the view. | Should | T-22 |
| REQ-29 | `vector_op` `filter` (AF-21). Given a layer with routes A, B and C, when filtered with `ROUTE_NAME <> 'B'`, then a new layer holds A and C and the original still holds 3. Failure: a where naming a hidden field or outside the parser's subset is refused. | Should | T-21 |
| REQ-30 | The job runs end to end on fixtures (success measure 7). Given the fixtures above, when the workflow runs, then gaps, fixed widths and the dissolved layer exist with checks passed, open in a chat, filter, and export to gdb. | Must | T-23 |

## Rules and formats added

| ID | Rule or format | Example | Expected result |
|---|---|---|---|
| R-19 | Refresh-token exchange threshold: 7 days | Token with 6 days left at run time | Exchanged before the request |
| R-20 | Opened run outputs are copies; the run folder never changes | The person filters an opened layer | The run's files and report stay as they were |
| F-11 | Workflow `outputs.folder.format` allowed values add `gdb`, `tif` | `format: gdb` | — |
| F-12 | `vector_op` `op` adds `filter`, with `params: {where: text}` | `{op: filter, inputs: {layer: ws://…}, params: {where: "ROUTE_NAME <> 'B'"}}` | — |

These are one-way formats (D-10 applies to them as to F-1 to F-10). Part 3 below adds web map layers for imagery and other services.

## Part 3: web map layers (AF-22)

**Today** (as of 2026-11-08) the map has one fixed street basemap. The map page's content policy allows that one internet host and nothing else (`app/ui/src/MapView.tsx:19-21`; `crates/gateway/src/approvals_page.rs:905-916`). A person can't add imagery or any other service, so in the ROW review there is no aerial photo to measure against.

**Fix (REQ-31 to REQ-34):** the person can add **web map layers**. These are display-only layers drawn on the map from a URL: XYZ tiles, WMTS, WMS, ArcGIS MapServer and ImageServer (tiled or dynamic export), and ArcGIS feature layers.

- **Who adds them:**
  - The person, in the app: **Add web layer** on the map, or on a new **Connections › Map layers** tab, by pasting a URL. Columbus reads the service's capabilities and offers its layers.
  - **Saved like database connections:** each layer is saved once (F-13) and persists across restarts. It appears in every chat's layer list and on the Map layers tab, where the person can **Test**, **Edit** or **Remove** it. Turning a layer on, and its opacity and order, are per chat.
  - The organization, by listing sources in policy (`map.sources`).
  - The agent can't add, change or turn them on (the same rule as connections), because a layer URL could report what area the person is looking at to any host.
- **Through the gateway (D-14):** every tile, image and feature request goes through the gateway on 127.0.0.1, never straight from the web view. The gateway:
  - accepts requests only from the map page, with the same per-session token the companion page already uses, sent in a header (MapLibre `transformRequest`), never in the URL; any other caller is refused;
  - serves only requests derived from a saved source, so it is never an open proxy: the page names a source id and a tile or extent, never a URL, and only that source's host is reached;
  - applies policy `map.hosts` per F-14. Hosts named inside a service's capabilities (WMTS ResourceURL, WMS OnlineResource, ArcGIS tile subdomains) count as that source's hosts only if they pass the same rule, and are recorded when the source is saved. Redirects are not followed, and each resolved address is checked and pinned, as `fetch_tools.rs` already does;
  - adds ArcGIS tokens for secured services from a named `arcgis` connection's sign-in (`auth_from`), only to hosts that connection's portal vouches for (part 1, R-18), so tokens never reach the web view or another host;
  - caches tiles under `<workspaces root>/cache/tiles/<source id>/` (up to 500 MB in all, oldest out first). A source's cache is deleted when the source is removed; secured sources' caches are also deleted when their `arcgis` connection signs out or is removed;
  - is the one origin the map page talks to for web layers, so the page's content policy stays narrow. The existing street basemap host stays allowed as today.
- **On the map:**
  - Each web layer starts **off**. Turning any web layer on for the first time in a chat shows, once per chat: "Web layers' services see which area you're viewing".
  - Layers have visibility, opacity, order and zoom to extent, all remembered per chat.
  - Any raster web layer can be chosen as the basemap, beside "Streets" and "None".
  - ArcGIS feature layers draw the current view's features as vectors, at most 5,000 per view, with "Zoom in to see all" beyond that. Clicking a feature shows its attributes, with hidden fields removed when the layer comes through an `arcgis` connection.
- **The assistant never sees web layers.** They are not workspace layers and are not sent to the AI provider. `map_snapshot` keeps using only local GeoTIFFs. `whoami` lists web layer names and nothing more. To *analyse* an ArcGIS layer, the assistant uses an `arcgis` connection (part 1, Part 4).
- **Not this round:** services that need a username and password (Columbus stores no passwords), and drawing web layers in `map_snapshot`.

| ID | Requirement | Priority | Task |
|---|---|---|---|
| REQ-31 | (T-24 server side, T-25 app side) Web layers are added only by the person (app, at the keyboard) or by policy `map.sources`, and only for hosts on `map.hosts`. Given a fake WMS on an allowed host, when the person pastes its URL, then its layers are offered and saved (F-13). Edge: if the organization lists `map.hosts`, any other host is refused with the host named; if it lists none, saving the source is the person's approval of its host, confirmed at the keyboard like a connection. A page request for an arbitrary URL is refused. Failure: no MCP tool adds, changes or enables a web layer. | Must | T-24 |
| REQ-32 | Every web-layer request goes through the gateway proxy; ArcGIS tokens are added there and never reach the web view. Given a secured fake ArcGIS MapServer tied to a signed-in `arcgis` connection, when the map draws it, then tiles load. Edge: the web view's requests contain no token (checked in the e2e request log). Failure: a direct request from the page to the service host is blocked by the page's content policy. | Must | T-24 |
| REQ-33 | XYZ, WMTS, WMS, ArcGIS MapServer/ImageServer and ArcGIS feature layers draw on the map with on/off, opacity, order, zoom to extent and choose-as-basemap, all remembered per chat. Given one fake service of each kind, when each is turned on, then it draws. Edge: a feature layer with 12,000 features in view draws 5,000 and shows "Zoom in to see all". Failure: an unreachable service shows R-14 wording and the map keeps working. | Must | T-25 |
| REQ-34 | Web layers stay out of the assistant's reach. Given a web layer turned on, when `list_outputs`, `map_snapshot` and `whoami` run, then only `whoami` mentions it, by name only. | Must | T-24 |

| ID | Rule or format | Example | Expected result |
|---|---|---|---|
| R-21 | The first web layer turned on in a chat shows the privacy notice; later ones in that chat don't | Person turns on "County imagery", then "ROW service" | Notice shown once; both draw |
| F-13 | `map-sources/<id>.yaml` beside the `connections` folder (or policy `map.sources`): `id` (text, required), `title` (text, required), `kind` (`xyz`, `wmts`, `wms`, `arcgis-map`, `arcgis-image`, `arcgis-features`; required), `url` (https, required), `layer` (text; WMS/WMTS layer name or ArcGIS layer id), `auth_from` (an `arcgis` connection id, optional), `attribution` (text, optional) | `{id: county-imagery, title: County imagery 2025, kind: wmts, url: https://imagery.example.gov/wmts/1.0.0/WMTSCapabilities.xml, layer: ortho2025}` | — |
| F-14 | Policy `map`: `allow_web_layers` (boolean, default true), `hosts` (list of hosts; default empty, meaning each source's own host is allowed once the person saves that source at the keyboard; when non-empty, only these hosts), `sources` (list of F-13 entries the organization supplies) | `{allow_web_layers: true, hosts: ["*.arcgis.com", "imagery.example.gov"]}` | — |

D-14 (the gateway proxies every web-layer request) and F-13/F-14 are one-way doors, like D-10.
