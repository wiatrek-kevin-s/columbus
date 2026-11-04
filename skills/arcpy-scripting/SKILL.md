---
name: arcpy-scripting
description: Writing ArcPy scripts for ArcGIS Pro that run correctly the first time, covering arcpy.da cursors in with blocks, environment settings (workspace, outputCoordinateSystem, overwriteOutput), licence levels and extensions, cloning arcgispro-py3, edit sessions with arcpy.da.Editor for versioned data, and the classic errors. Use it when asked to "write an ArcPy script", "automate this in Pro", "loop through features", "update a field with a cursor", "batch geoprocess", or when a script fails with ERROR 000725, a schema lock or "cannot update outside an edit session".
---

# ArcPy scripting

ArcPy is ArcGIS Pro's Python package. It runs geoprocessing tools, reads and writes data
with cursors, and edits geodatabases. Scripts fail mostly for boring reasons: the wrong
Python, a missing licence, a lock, a path, or editing versioned data outside an edit session.

## General practice (with or without Columbus)

### 1. Run it in the right Python

- Pro ships a conda environment, **`arcgispro-py3`**, which is read-only. Never `pip install`
  into it.
- To add packages, **clone it**: Pro > Settings > Package Manager > Environment Manager >
  Clone, or from the "Python Command Prompt":
  ```bat
  conda create --clone arcgispro-py3 --name gis-py3
  proswap gis-py3
  ```
  `proswap` makes the clone Pro's active environment. Then `conda install` (preferably from
  Esri's channel) into the clone.
- Run scripts outside Pro with the active environment through `propy.bat`:
  ```bat
  "C:\Program Files\ArcGIS\Pro\bin\Python\Scripts\propy.bat" C:\work\fix_roads.py
  ```
- After a Pro upgrade, re-clone. Old clones keep the old ArcPy and break in odd ways.
- ArcPy is Python 3, 64-bit. ArcMap's Python 2.7 `arcpy.mapping` scripts do not run;
  `arcpy.mp` replaced it.

### 2. Script skeleton

```python
import arcpy

arcpy.env.workspace = r"C:\work\roads.gdb"
arcpy.env.overwriteOutput = True                      # else ERROR 000725 on rerun
arcpy.env.outputCoordinateSystem = arcpy.SpatialReference(2277)   # NAD83 / Texas Central (ftUS)
arcpy.env.geographicTransformations = "WGS_1984_(ITRF00)_To_NAD_1983"

try:
    out = arcpy.analysis.Buffer("roads", "roads_buf_500ft", "500 Feet",
                                dissolve_option="NONE")
    n = int(arcpy.management.GetCount(out)[0])        # tools return Result objects
    arcpy.AddMessage(f"{n} buffers written to {out[0]}")
except arcpy.ExecuteError:
    arcpy.AddError(arcpy.GetMessages(2))              # severity 2 = errors
    raise
```

- Use **raw strings** for Windows paths (`r"C:\work\new.gdb"`; `"\n"` in a path is a newline).
- Call tools by toolbox alias: `arcpy.analysis.Buffer`, `arcpy.management.Project`,
  `arcpy.conversion.ExportFeatures`.
- **Distances carry units** (`"500 Feet"`, `"150 Meters"`). A bare number uses the data's
  units, which may be degrees.
- Set environments for a block only with `arcpy.EnvManager`:
  ```python
  with arcpy.EnvManager(outputCoordinateSystem=arcpy.SpatialReference(3081),
                        extent="MAXOF", parallelProcessingFactor="75%"):
      arcpy.analysis.Clip("parcels", "aoi", "parcels_aoi")
  ```
- Other environments that change results: `extent`, `XYTolerance`, `XYResolution`,
  `preserveGlobalIds`, and for rasters `cellSize`, `snapRaster`, `mask` (see the
  raster-and-terrain skill). `outputCoordinateSystem` reprojects outputs silently; know it is set.

### 3. Cursors (arcpy.da only)

Always open cursors in `with` blocks so locks are released, even on error.

```python
fields = ["OID@", "SHAPE@", "FUNC_CLASS", "STATUS"]
where = f"{arcpy.AddFieldDelimiters('roads', 'STATUS')} = 'A'"

with arcpy.da.SearchCursor("roads", fields, where_clause=where,
                           spatial_reference=arcpy.SpatialReference(3081)) as cur:
    for oid, shape, fc_code, status in cur:
        print(oid, fc_code, round(shape.length, 1))   # length in EPSG:3081 metres
```

```python
lookup = {row[0]: row[1] for row in arcpy.da.SearchCursor("csj_table", ["CSJ", "DISTRICT"])}

with arcpy.da.UpdateCursor("roads", ["CSJ", "DISTRICT"]) as cur:
    for row in cur:
        csj = (row[0] or "").zfill(9)                  # codes lose zero padding
        if csj in lookup:
            row[1] = lookup[csj]
            cur.updateRow(row)
```

```python
with arcpy.da.InsertCursor("signs", ["SHAPE@XY", "SIGN_TYPE"]) as cur:
    cur.insertRow(((3115000.0, 10070000.0), "R1-1"))   # coordinates in the class's CRS
```

- Useful tokens: `OID@`, `SHAPE@` (geometry object), `SHAPE@XY` (centroid for lines and
  polygons, not the first vertex), `SHAPE@WKT`, `SHAPE@LENGTH`, `SHAPE@AREA` (CRS units).
- Geometry comes back in the data's own CRS unless you pass `spatial_reference`.
- Ordering and grouping: the cursor's SQL clause argument, a 2-tuple of prefix and postfix,
  for example `(None, "ORDER BY ROUTE_ID, FROM_MEAS")`. Postfixes work on geodatabases, not
  shapefiles.
- **Build lookups in a dict first** (as above). Nested cursors on two tables are slow; nested
  cursors on the same table can lock or skip rows.
- `None` is NULL. Dates are `datetime.datetime`. Check for `None` before string methods.
- Use `arcpy.AddFieldDelimiters` for where clauses: delimiters differ between file
  geodatabases (`"FIELD"`), SQL Server (`[FIELD]` allowed) and shapefiles.

### 4. Licences and extensions

| Level | `arcpy.ProductInfo()` returns | Typically adds (confirm in Esri's licence comparison) |
| --- | --- | --- |
| Basic | `ArcView` | Most analysis and conversion |
| Standard | `ArcEditor` | Direct editing and administration of enterprise geodatabases, versioning, topology |
| Advanced | `ArcInfo` | The remaining overlay and generalization tools |

Each tool's help page has a **Licensing information** section. Check it rather than
guessing. Extensions must be checked out in a script even when Pro has them:

```python
if arcpy.CheckExtension("Spatial") != "Available":
    raise RuntimeError("Spatial Analyst licence not available")
arcpy.CheckOutExtension("Spatial")
try:
    slope = arcpy.sa.Slope("dem", "DEGREE", z_factor=1)
    slope.save(r"C:\work\terrain.gdb\slope")
finally:
    arcpy.CheckInExtension("Spatial")
```

Extension codes: `"Spatial"`, `"3D"`, `"Network"`, `"ImageAnalyst"`, `"GeoStats"`.
`arcpy.CheckProduct("ArcInfo")` tells you whether a higher level is available.

### 5. Editing versioned and complex data: arcpy.da.Editor

Versioned data, and data in topologies, networks, relationship classes with messaging or with
attribute rules, must be edited inside an edit session. Outside one, an `UpdateCursor` fails
with "Objects in this class cannot be updated outside an edit session".

```python
import os, arcpy

sde = r"C:\work\connections\roads_fix_1104.sde"   # connection file pointing at a NAMED version
with arcpy.da.Editor(sde, multiuser_mode=True) as edit:     # True for versioned data
    fc = os.path.join(sde, "ROADS.ROADWAY_INVENTORY")
    with arcpy.da.UpdateCursor(fc, ["CSJ"], "CSJ IS NOT NULL") as cur:
        for row in cur:
            padded = row[0].zfill(9)
            if padded != row[0]:
                row[0] = padded
                cur.updateRow(row)
# leaving the with block saves; an exception inside it rolls the edits back
```

- **Never point an edit script at DEFAULT.** Make a named version first, edit there, then
  reconcile and post after review (see the enterprise-geodatabases skill).
- To switch a layer's version in a script: `arcpy.management.ChangeVersion(layer,
  "TRANSACTIONAL", "ANALYST.roads_fix_1104")`.
- Non-versioned enterprise data: `multiuser_mode=False`.
- Count changes as you go and print them; dry-run first by counting without `updateRow`.

### 6. Classic pitfalls

| Symptom | Cause | Fix |
| --- | --- | --- |
| ERROR 000725: dataset already exists | `overwriteOutput` is False | Set it, or write to a new name |
| ERROR 000732: dataset does not exist or is not supported | Relative name with the wrong `env.workspace`, a typo, or a layer name used outside Pro | Use full paths; print `arcpy.Exists(path)` |
| ERROR 000464 / schema lock | Pro, a cursor or another script holds the data | Close Pro's layer, use `with` cursors, `del` leftover objects |
| Tool ran on a few features only | A **layer** with a selection was passed; tools honour selections | Pass the feature class path, or clear the selection |
| Fields renamed `ROADS.ROADWAY_INVENTORY.FUNC_CLASS` | Joined layer or enterprise qualified names | Use the qualified name, or `arcpy.management.JoinField` (permanent) instead of `AddJoin` |
| Buffer of 500 gives a huge or tiny result | Distance without units on data in degrees or feet | `"500 Feet"`, and a projected CRS |
| Output name refused | Starts with a digit, has spaces or `-`, too long for the format | `arcpy.ValidateTableName(name, workspace)` |
| Data shifted a metre or more | Datum change without a geographic transformation | Set `geographicTransformations` or pass one to `Project` |
| `in_memory` path warnings | Pro uses `memory` | `r"memory\tmp_buf"` |
| Shapefile fields cut to 10 characters | DBF limits | Use a file geodatabase or GeoPackage |

More messages and fixes: [references/errors-and-fixes.md](references/errors-and-fixes.md).

### 7. Good script habits

- Parameters at the top (paths, version name, CRS code, distance and unit) or with
  `arcpy.GetParameterAsText()` in a script tool.
- Log what it did: counts in and out, the CRS, the version, elapsed time.
- Write outputs to a new geodatabase, not over the source.
- Test on a copy or a small `where` first.

## If the Columbus tools are available

**Columbus does not run ArcPy today.** The tool that will run approved Python
(`run_python`) is planned and not yet available, and the ArcPy worker for enterprise
geodatabases is on the roadmap. Do not claim a script was run through Columbus.

What you can do now:

- **Write the script for the person to run in Pro** (or with `propy.bat`), and say so.
  Use `describe_dataset {connection, dataset}` and `sample_rows {connection, dataset, n}` on
  the governed copy, where the connection's engine is available, to get real field names,
  types, CRS and code values before you write the script, so it runs first time.
- Use `context_search` and `context_get` for the organization's rules: which version
  naming to use, who posts, which CRS deliverables use, approved scripts. `whoami` returns
  the pack's always-on rules and shows whether scripts and edits are allowed at all
  (by default they are not).
- Some of the job may already be possible without ArcPy: `query_features` for selections,
  `vector_op` for reproject, buffer, clip, dissolve, join and `spatial_join`, and `verify` for
  geometry and CRS checks. Offer those when they cover the task.
- An organization may have approved an **engine plugin** for a recurring job. Find it with
  `search_tools {"query": "..."}` and call it with `run_tool {"name": "<plugin id>.<tool>", "args": {...}}`.
  Do not assume one exists.
- Edits through Columbus (`propose_edits`, `apply_edits`) are planned and not available.
