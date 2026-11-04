# GIS skills library

Agent Skills ([open format](https://agentskills.io)) that teach an AI agent sound GIS practice. Each folder holds a `SKILL.md` with a `name` and a `description` saying when to use it; agents see only those two lines until a task matches, so the whole library costs little context. Longer reference material sits in each skill's `references/` folder and is read only when needed.

Every skill gives general practice first, so it helps an agent with or without Columbus, then a section headed **If the Columbus tools are available** naming the exact tools, arguments and check ids. The server enforces; skills advise. Nothing that keeps data safe depends on a skill being read.

| Skill | Use it for |
| --- | --- |
| [working-with-columbus](working-with-columbus) | Any task through the Columbus server: `whoami`, the context pack, reading `checks`, approvals, refusals, hidden fields, provenance |
| [crs-and-datums](crs-and-datums) | Choosing a projected CRS for measuring (Texas systems, State Plane, UTM, equal-area), NAD83 realizations, vertical datums, axis order, survey feet |
| [spatial-joins-and-keys](spatial-joins-and-keys) | Attribute and spatial joins: key types, padding, case and spacing, nulls, one-to-many, predicates, honest match rates |
| [spatial-data-qa](spatial-data-qa) | Validity, empty and duplicate geometry, mixed types, slivers and gaps, schema and domain checks, what to do with each finding |
| [spatial-sql](spatial-sql) | Read-only SQL for PostGIS, Oracle Spatial and SQL Server; index-friendly filters; what `sql_select` allows |
| [enterprise-geodatabases](enterprise-geodatabases) | Versions, reconcile and post, domains and subtypes, relationship classes, attribute rules, ST_Geometry and SDO_GEOMETRY |
| [arcpy-scripting](arcpy-scripting) | ArcPy cursors, environments, licence levels, Pro's Python environment, edit sessions |
| [raster-and-terrain](raster-and-terrain) | Resampling, nodata, Cloud-Optimized GeoTIFFs, DEM derivatives, zonal statistics, mosaics |
| [lidar-with-pdal](lidar-with-pdal) | PDAL pipelines, LAS classification codes, ground filtering, COPC and EPT, density, datums in LAS headers |
| [imagery-and-stac](imagery-and-stac) | STAC searches, Sentinel-2 and Landsat, cloud masks, scale factors, spectral indices, composites |
| [open-data-sourcing](open-data-sourcing) | Finding open data through the registry first, licences and terms, provenance |
| [linear-referencing](linear-referencing) | Routes, measures, events, calibration, gaps and overlaps, the LRS checker plugin |
| [maps-and-deliverables](maps-and-deliverables) | Layouts, symbology, metadata, meeting a delivery spec |
| [sop-conversion](sop-conversion) | Turning Word, PDF or Excel procedures into context-pack entries, with fidelity checks and sign-off |
| [writing-clearly-and-concisely](writing-clearly-and-concisely) | Prose people will read: documentation, summaries, reports (third-party, public domain; see its [SOURCE](writing-clearly-and-concisely/SOURCE.md)) |

Some skills describe Columbus tools that are planned but not built yet (`pointcloud_pipeline`, `run_python`, `propose_edits`); they say so where it matters.

## Using them

- **Claude Code:** copy the folders into `~/.claude/skills/` (personal) or a project's `.claude/skills/`.
- **Other agents** that read Agent Skills (Gemini CLI, Codex, Copilot and others): see each one's documentation for its skills folder.
- **The Columbus desktop app** (planned) will load them for you.

## Writing one

- `name` equals the folder name: lower-case words joined by `-`.
- `description` says what it does **and when to use it**, in the words a GIS user would type.
- `SKILL.md` stays under about 500 lines; move tables and long examples into `references/` and link them.
- Concrete over general: real EPSG codes, real tool calls, real failure messages.
- Name only Columbus tools and check ids that exist. `python skills/check_skills.py` checks the format, the links and every tool name against the server's source; CI runs it on every push.
