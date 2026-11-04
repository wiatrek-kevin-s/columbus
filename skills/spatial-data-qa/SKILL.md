---
name: spatial-data-qa
description: Quality-checking spatial data before it is used or delivered, covering invalid geometry (self-intersections, ring orientation), null and empty shapes, duplicates, mixed geometry types, slivers, gaps and overlaps, topology, schema and domain checks, and CRS sanity, and what to fix, flag or send to the data owner. Use it when asked to "QA this layer", "validate geometry", "check for duplicates", "fix invalid polygons", "is this data clean", or before handing any result to someone else.
---

# Spatial data QA

QA is finding what is wrong, saying so with numbers, and deciding for each finding
whether to fix it, flag it or ask the data owner. It is never quietly deleting the rows that
look bad.

## General practice (with or without Columbus)

### The rule: never silently drop rows

Every row that goes in is accounted for in what comes out: kept, fixed (with a record of
what changed), flagged, or excluded with a count and a reason the person agreed to. If you
filtered anything, say how many rows and why.

### What to check, in order

1. **CRS sanity.** A CRS is recorded; the coordinates fall inside its area of use; the
   units are what you expect; X and Y are not swapped. (See the crs-and-datums skill.)
   Everything below is meaningless if the layer is in the wrong place.
2. **Null and empty geometry.** Null (no geometry at all) and empty (a geometry with no
   coordinates) are different. Count both. They usually mean a failed geocode, a record
   with no location yet, or a broken export.
3. **Validity.** Count invalid geometries and the reasons:
   - self-intersection (a "bowtie" polygon, a ring that crosses itself);
   - ring self-intersection or a hole that touches the shell along a line;
   - hole outside its shell, or nested shells in a MultiPolygon;
   - unclosed rings or too few points (from bad exports);
   - duplicate consecutive vertices and spikes (often valid, but noisy).
4. **Ring orientation.** OGC/GEOS validity ignores it, but consumers do not agree:
   shapefiles expect clockwise outer rings, GeoJSON (RFC 7946) counter-clockwise. A
   reversed ring can make a polygon draw or compute as its complement in some tools. Fix
   orientation on export for the target format; it is not a data error by itself.
5. **Geometry types.** One layer, one geometry family. Polygon and MultiPolygon together
   are fine for most tools; points mixed with polygons are not, and shapefiles allow only
   one type.
6. **Duplicates.**
   - Exact geometry duplicates: often a double load or an append run twice.
   - Same id, different geometry: versioning or an edit conflict.
   - Near-duplicates: the same feature digitized twice, offset by a little.
   Check before counting or summing anything.
7. **Coverage topology** (for layers that should tile an area: parcels, counties,
   districts, zoning):
   - **overlaps**: two polygons claim the same ground;
   - **gaps**: ground claimed by nobody;
   - **slivers**: very thin polygons along shared edges, usually from overlaying layers
     digitized separately. A thinness ratio 4πA/P² near 0 (a circle is 1) and a tiny area
     together are a good sign of one.
8. **Network topology** (roads, pipes, streams): dangles (line ends that should connect),
   undershoots and overshoots at intersections, lines that cross without a node, wrong
   flow direction, and on a linear referencing system: measure gaps, overlaps, reversed
   measures and events off the route's measure range.
9. **Schema.** The expected fields exist with the expected types; ids are unique and not
   null; required fields are filled.
10. **Domains and ranges.** Coded values are in the domain (functional class 1 to 7, status
    `A` or `R`); numbers are in range (lanes 1 to 12, speed limits multiples of 5); dates are
    not in the future; cross-field rules hold (from-measure less than to-measure).

### Fix, flag or ask

| Finding | Usually | Why |
| --- | --- | --- |
| Missing or wrong CRS | Ask the owner | A guessed CRS is a silent error |
| Swapped X/Y | Fix (swap), say so | Unambiguous when the extent proves it |
| Invalid geometry (self-intersection) | Fix with make-valid, then re-check | Required before overlay, clip or dissolve |
| Invalid geometry that changes a lot when repaired | Flag, ask the owner | The repair may not be what was meant |
| Ring orientation | Fix on export | Format convention, not a data error |
| Null or empty geometry | Flag, keep the row | It may be a real record without a location yet |
| Exact duplicates | Flag; remove only if the owner confirms a double load | Removing the wrong copy loses attributes |
| Mixed geometry types | Split by type | Formats and tools need one type |
| Slivers from your own overlay | Fix (merge into neighbour, or tolerance) | Your process made them |
| Slivers, gaps or overlaps in source data | Flag, ask the owner | Only the owner knows the true boundary |
| Domain violations | Flag, ask the owner | A code you do not know is not a code you can fix |
| Schema missing a required field | Stop and ask | The data is not what the task assumed |

### make-valid caveats

Repairing can change a feature's type and shape: a bowtie polygon becomes a MultiPolygon of
two triangles, a collapsed polygon becomes a line or a GeometryCollection, and an
invalid-and-repaired layer can become mixed-type. After repair, compare the feature count,
the geometry types and the total area or length before and after, and re-run the checks.

### A QA report

```text
Layer: <name> (<rows> rows, <geometry type>, <CRS>)
CRS: ok / problem
Geometry: <n> null, <n> empty, <n> invalid (<reasons>), <n> duplicate, types: <list>
Topology: <overlaps, gaps, slivers, dangles> or "not checked"
Schema and domains: <findings> or "not checked"
Actions: fixed <what> (<n> features); flagged <what>; asked the owner about <what>
Rows in: <n>; rows out: <n>; difference explained: <yes/how>
```

Say plainly what you did not check. "Not checked" is honest; silence implies "fine".

## If the Columbus tools are available

### verify

```json
verify {"layer": "ws://<session>/results/roads-<id>.gpkg"}
verify {"layer": "austin:roads"}
verify {"layer": {"connection": "austin", "dataset": "roads", "where": "district = 14"}}
```

`layer` is a workspace handle or a dataset (`"<connection>:<dataset>"`, or the object form
with `where`). A dataset bigger than the read limit is refused; narrow it with `where`.
The result is the layer summary (`rows`, `geometry_types`, `crs`, `extent`, `fields`); the
findings are in `checks`:

| Check | Severity | Message (example) and action |
| --- | --- | --- |
| `crs.present` | fail | "The layer has no coordinate reference system". Stop; ask for the source CRS. |
| `crs.info` | info | "The layer is in EPSG:3081 (...), units metre". |
| `crs.area_of_use` | fail | Labelled geographic but coordinates cannot be lon/lat. Stop; the fix says swap X/Y or assign the right CRS. |
| `crs.area_of_use` | warn | Coordinates outside the CRS's area of use. Report; likely a wrong label. |
| `geometry.valid` | warn | "3 of 412 geometries are invalid", `detail.examples` such as `Self-intersection[1216724.3 902699.8]`. Repair with make_valid. |
| `geometry.empty` | info | "N rows have no geometry and M have an empty one". Report; keep the rows. |
| `geometry.mixed_types` | warn | "The layer mixes geometry types: LineString, Point". Split before export. Multi and single types count as one family. |
| `geometry.duplicates` | warn | "N geometries are exact duplicates of another row's". Check for a double load. |
| `input.truncated` | warn | The layer was cut off at a row limit when made; results cover part of the data. |

`geometry.duplicates` comes only from `verify`. The other geometry and CRS checks are also
attached to `query_features` and `vector_op` results, and every `vector_op` reports
`geometry.valid` for its inputs.

### Severities

- `info`: note it.
- `warn`: report it with the number and the fix. Do not call the layer "clean".
- `fail`: stop. Tell the person what failed, why, and the fix. Do not process further or
  hand the result on. The server treats fail as blocking for exports and edits.

### Repair invalid geometry

```json
vector_op {"op": "make_valid", "inputs": {"layer": "ws://<session>/results/parcels-<id>.gpkg"}}
```

No params. The result carries `geometry.repaired` ("Repaired N invalid geometries", warn
when N > 0) and the usual output checks. Then:

1. `verify` the new handle.
2. Compare `summary.rows` and `summary.geometry_types` before and after. A new
   `geometry.mixed_types` warning means some repairs changed type.
3. Report how many were repaired, and keep the original handle for the owner.

### Other checks you will meet while processing

- `dissolve.null_groups` (warn): features with an empty `by` value were dissolved together
  into one group. Fill or filter them first if they should not be.
- `extent.plausible` (warn): the output overlaps no input's extent; check CRSs and units.
- `filter.zero_rows`, `filter.all_rows`, `query.read_limit` (warn) from queries.
- `join.unmatched`, `join.many_to_many` from joins (see the spatial-joins-and-keys skill).

### Not in Columbus yet

Columbus does not yet check coverage topology (overlaps, gaps, slivers), network topology,
tolerances, or domains and field rules. Do those checks yourself and label them as yours:

- On PostGIS, `sql_select` can call PostGIS functions, for example
  `SELECT COUNT(*) FROM parcels WHERE NOT ST_IsValid(geom)`, or
  `SELECT id, ST_Area(geom) FROM parcels WHERE ST_Area(geom) < 1` (in the layer's units).
- Domain checks with `sql_select`:
  `SELECT FUNC_CLASS, COUNT(*) FROM ROADS.ROADWAY_INVENTORY GROUP BY FUNC_CLASS`, then
  compare with the domain from `context_get`.
- Engine plugins can add checks through `run_tool`; for example the sample
  `contoso.lrs-check.check_events` finds LRS measure gaps, overlaps, reversed and
  out-of-range measures and events on unknown routes. A plugin's declared checks that
  Columbus has not run yet show as `verify.pending` (info).

Run `verify` on every layer before you hand it to anyone, and include its checks in the
report.
