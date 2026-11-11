# Folders of files, ArcGIS services, Python environments and web layers

What Columbus 0.14 adds, and how to work with it. The person sets all of these up; you
use them through the same tools as any connection. You cannot add, change, sign in to or
confirm any of them.

## Folders of files (`files` connections)

A `files` connection is a folder (or one file) of `.csv`, `.xlsx`, `.geojson`, `.parquet` /
`.geoparquet`, `.kml` and `.fgb` files. It is read-only.

- **Names.** `list_datasets` gives each file's path in the folder, forward slashes, no
  extension: `2026/crashes`. Workbook sheets and KML folders are `<file>/<sheet>`
  (`inventory/2026`). Any other `.`, `\` or `:` becomes `_` (`roads.v2.csv` is `roads_v2`).
- **Skipped.** `list_datasets` also returns `skipped`: files it doesn't offer, with the reason
  (`.xls` "save as .xlsx", lock files, a link out of the folder, a file that can't be read).
  Tell the person; don't try to read them another way.
- **Geometry.** CSV and Excel get points from `lon`/`lat`, `x`/`y` and similar columns, or
  from a WKT `geometry` column. Without either the dataset is a table. A dataset without a
  CRS gets a `crs.present` **warning** (not a fail): ask the person for the CRS; they can set
  `crs` on the connection.
- **Typing.** Codes with leading zeros (`000123456`) stay text; whole numbers are integers;
  ISO dates are dates. Check with `sample_rows` before you filter.
- **SQL.** `sql_select` works, in SQLite's dialect, and can join two files of the same
  connection; row filters and hidden fields apply to each.
- Checks you will see: `file.encoding`, `csv.headers`, `xlsx.formulas`, `xlsx.errors`.

## ArcGIS services (`arcgis` connections)

An `arcgis` connection is one ArcGIS Online, Portal or ArcGIS Server feature or map
service, or one layer in it. Each layer and table is a dataset under its published name
(`Roadways`; two alike get `_<layer id>`).

- `list_datasets`, `describe_dataset`, `sample_rows` and `query_features` work. `describe_dataset`
  gives the geometry type with Z/M, `crs` from `latestWkid`, coded-value domains and the
  service's own row count under the row filter.
- **`sql_select` is refused** (`query.unsupported`): use `query_features` with `fields`,
  `where`, `bbox` (with `bbox_crs`) and `order_by`. The `where` uses the usual subset and is
  rebuilt by Columbus, ANDed with the row filter.
- Paging is handled for you; a result over the read limit is cut and says `query.read_limit`.
- **Sign-in.** Some connections need the person to be signed in. If a call says "needs an
  ArcGIS sign-in" or "Sign in again: columbus-mcp arcgis signin <id>", tell the person to
  press **Sign in** on the connection in the app or to run that command. Never ask for a
  password, token or code in the chat. `whoami`'s connection card shows `signed_in`.
- In workflows, signed-in steps renew the sign-in themselves (`arcgis.signin_renewed`); a
  lapsed one stops the step with "needs you: sign in again (…)".

## Python environments

`whoami` lists `environments` when policy turns them on (`policy.environments_allowed`):
each with `id`, `trust`, `status` (`ready`, `changed`, `invalid`), `python_version`, the
`arcpy` line and `licence`.

- **Use one** by passing `environment: <id>` to `run_python` when a script needs a library
  the engine lacks. Only `ready` environments run. A `changed` one is refused with "confirm
  it again in Settings": tell the person; only they can confirm it.
- **Governed** (the default): the script runs confined, like an engine script: no network
  (level A), extracts of its inputs only, outputs from `out/`.
- **Trusted (experimental):** the script runs unconfined, as the person, and every run is
  approved; the card says Columbus cannot enforce the connection rules for that run.
  `whoami` marks these and every arcpy environment `experimental: true`. Use a trusted
  environment only when the person asks for it (typically for arcpy), and say what it means.
- **arcpy** works only in the environment that has it, and is expected to need a trusted one.
  Read the licence line: "not initialized: open ArcGIS Pro and sign in …" means the person
  must open ArcGIS Pro first. Personal tools and workflow steps can use governed level-A
  environments only, so arcpy is not available to them yet.
- **Ask for a new one** with `propose_environment` (`id`, `python_version?`, exact
  `packages` like `whitebox==2.3.6`, `reason`). The person approves it at the keyboard; then
  call `whoami` to see it `ready`. Never ask the person to `pip install` into an environment
  for you: that marks it `changed`.
- A personal tool's code may still import only the modules the tool rules allow, even in an
  environment that holds others; use `run_python` for anything else.

## Web map layers

The person can add imagery and other web services (XYZ, WMTS, WMS, ArcGIS map, image and
feature services) to their map, in the app. These are **display only**: you can't add,
turn on or read them, they are not workspace layers, `list_outputs`, `map_snapshot` and
`map_view` don't include them, and `whoami` gives only their titles (`web_layers`). To analyse an
ArcGIS layer, use an `arcgis` connection. If the person asks you to "add imagery to the
map", tell them to use **Add web layer** on the map or **Data › Map layers**.

## Exports and workflows

- `export_output` adds `gdb` (a new file geodatabase with one feature class; an existing
  `.gdb` of that name is never replaced: choose another name) and `tif` (raster results
  only; reproject first if a delivery spec wants another CRS). Workflow
  `outputs.folder.format` takes `gdb` and `tif` too.
- Workflow steps can read `files` and `arcgis` datasets like any connection.
- A finished workflow run can be opened in a chat (**Open in a chat**, or
  `columbus-mcp workflow open <id> <run>`). `whoami` then returns `opened_run` with the run's
  layers as handles: work from copies, filter them (`vector_op` `filter`) and export as usual.
