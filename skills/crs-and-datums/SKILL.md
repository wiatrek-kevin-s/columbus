---
name: crs-and-datums
description: Choosing and checking coordinate reference systems so distances, buffers and areas come out right, covering projected vs geographic CRS, Texas state plane and EPSG:3081/3083, UTM, equal-area, NAD83 vs WGS84, NAVD88 and geoids, lat/lon axis order and US survey feet. Use it when asked to "reproject", "buffer by 500 ft", "measure distance", "calculate acreage", "what EPSG should I use", or when coordinates look swapped, land in the wrong place or have the wrong units.
---

# CRS and datums

Most wrong GIS answers that still look plausible come from the coordinate reference system:
measuring in degrees, measuring in Web Mercator, mixing feet, or giving a layer the wrong
label. This skill is about picking the right CRS for the job and noticing when the one you
have is wrong.

## General practice (with or without Columbus)

### 1. Geographic vs projected

- A **geographic** CRS (EPSG:4326 WGS 84, EPSG:4269 NAD83, EPSG:6318 NAD83(2011)) stores
  angles in degrees. A degree is not a fixed length. At Austin (30.27° N), one degree of
  latitude is about 110.9 km and one degree of longitude about 96.2 km. A "100 unit" buffer
  in degrees is meaningless.
- A **projected** CRS stores metres or feet on a flat grid. Measure, buffer and simplify
  only in a projected CRS chosen for the area.

### 2. Never measure in EPSG:4326 or EPSG:3857

- **EPSG:4326**: units are degrees (see above). Distances and areas are wrong, and the
  error changes with latitude and direction.
- **EPSG:3857 (Web Mercator)**: units look like metres, but its scale grows as
  1/cos(latitude). At Austin every length is about 16% too long and every area about 34%
  too large. In the Panhandle (35° N) it is about 22% on length. Use 3857 for web map tiles
  only.

### 3. Choose a projected CRS by task and area

| Task | Use | Texas examples |
| --- | --- | --- |
| Distance, buffer, length, local work (a city or county) | State plane zone for the area (conformal, very low distortion) | EPSG:2277 NAD83 / Texas Central (ftUS) for Austin and Travis County; EPSG:2278 South Central (ftUS) for San Antonio and Houston |
| Statewide distance or a statewide map | Statewide Lambert conformal conic | EPSG:3081 NAD83 / Texas State Mapping System (m); EPSG:3082 NAD83 / Texas Centric Lambert Conformal (m) |
| Area, acreage, density, anything summed by area | Equal-area | EPSG:3083 NAD83 / Texas Centric Albers Equal Area (m); EPSG:5070 NAD83 / Conus Albers for multi-state work |
| A narrow north-south strip, or data already delivered that way | UTM | EPSG:26914 NAD83 / UTM 14N (central Texas, 102° W to 96° W); 26913 (west of 102° W); 26915 (east of 96° W) |

Notes:

- EPSG:3081 is often called "Texas-Centric Lambert Conformal", but that name belongs to
  EPSG:3082. They are different projections. Check the code, not the nickname.
- State plane zones and UTM zones are only accurate inside their zone. A statewide layer
  in EPSG:2277 is distorted at the edges; use 3081, 3082 or 3083 instead.
- For areas, conformal projections (state plane, UTM, 3081/3082) are close inside their
  zone, but equal-area is correct by design. Use 3083 for statewide acreage.
- If the organization has a standard (for example "deliverables in EPSG:3081"), deliver
  in it. You can measure in a better CRS and reproject the result.

The full list of Texas codes, in every NAD83 realization and in metres and US feet, is in
[references/texas-crs.md](references/texas-crs.md).

### 4. Datums

- **NAD83 has realizations**: NAD83(1986), NAD83(HARN), NAD83(NSRS2007) and NAD83(2011).
  They differ by centimetres to about a metre. For survey-grade work, match the
  realization (for example EPSG:6578 NAD83(2011) / Texas Central (ftUS), not EPSG:2277).
- **WGS 84 vs NAD83**: in the continental US they differ by roughly 1 to 2 m and the
  difference grows slowly over time. GPS devices report WGS 84 (or ITRF). Many tools
  (PROJ without grid files among them) treat the two as identical and apply a zero shift
  by default. That is fine for mapping, but not for survey or engineering work: say which
  transformation you used.
- **NAD27 vs NAD83**: tens of metres. Never mix them without a grid-based
  transformation (NADCON).
- **NATRF2022** is planned to replace NAD83. Do not convert data to it unless the person
  asks.

### 5. Vertical datums

- Heights are either **ellipsoidal** (what GPS measures, relative to the ellipsoid) or
  **orthometric** (relative to the geoid, roughly mean sea level). In the US, orthometric
  heights are **NAVD88** (EPSG:5703 in metres, EPSG:8228 in feet).
- A **geoid model** converts between them: orthometric = ellipsoidal minus geoid height.
  GEOID18 is the current model for NAVD88 in the conterminous US; GEOID12B is the previous
  one. In Texas the geoid height is about -20 to -30 m, so mixing the two kinds of height
  is a mistake of 20 to 30 m.
- Record the vertical datum, geoid model and units with any elevation data. Lidar and
  survey deliverables usually state "NAD83(2011) / Texas Central (ftUS) + NAVD88 (ftUS),
  GEOID18".

### 6. Units: US survey foot vs international foot

- International foot = 0.3048 m exactly. US survey foot = 1200/3937 m. The difference is
  2 parts per million.
- That sounds small, but state plane coordinates are large numbers. Austin in EPSG:2277
  is about (3,114,226 ft E, 10,070,436 ft N). Reading those US survey feet as
  international feet moves the point about 6 m (20 ft) north.
- NAD83 Texas state plane data is almost always US survey feet (`ftUS` in the EPSG name).
  NGS deprecated the US survey foot from 2023, so newer data may use international feet.
  Check the CRS's unit, not the word "feet".
- Distances you pass to a tool need an explicit unit. Prefer metres or name the foot.

### 7. Axis order and swapped coordinates

- EPSG:4326 is officially latitude, longitude. Most GIS software, GeoJSON (RFC 7946), WKT
  and WKB from PostGIS, and shapefiles use longitude, latitude (X, Y). Always know which
  one a source uses.
- Texas in lon/lat: X from about -106.6 to -93.5, Y from about 25.8 to 36.5.

| What you see | What happened |
| --- | --- |
| X about 30, Y about -97 | Lat and lon swapped |
| X about 97 (positive), Y about 30 | Missing minus sign; lands in western China |
| X about 3,000,000, Y about 10,000,000, labelled EPSG:4326 | State plane feet labelled as degrees |
| Values about 3.28 times what you expect | Feet read as metres |
| Points near (0, 0) off West Africa | Degrees read as a projected CRS, or nulls written as 0 |
| A whole layer offset by about 1 to 2 m | WGS 84 vs NAD83 |
| A whole layer offset by tens of metres | NAD27 vs NAD83 |

### 8. Assign vs reproject

- **Reproject** (transform) when the label is right and you want another CRS.
- **Assign** (define) when the label is wrong or missing and the numbers are already in
  some CRS. Reprojecting a wrongly labelled layer makes it worse.
- Never guess a missing CRS. Look at the metadata, the `.prj` file and the coordinate
  ranges, and ask the data owner.

### 9. Before any measurement

1. What CRS is the layer in, and is it geographic?
2. Do the coordinates fit that CRS's area of use?
3. Are all inputs in the same CRS?
4. Is the target CRS projected and suited to this area and task (conformal for distance,
   equal-area for area)?
5. What are the units, and which foot?
6. Report the CRS and units with the result.

## If the Columbus tools are available

### Read the CRS facts first

```json
describe_dataset {"connection": "austin", "dataset": "roads"}
```

`result.crs` has `code` (for example `"EPSG:3081"`), `name`, `geographic` (true/false),
`units` (`metre`, `US survey foot`, `degree`), `metres_per_unit` (null for geographic) and
`area_of_use` as [west, south, east, north] in degrees. `query_features` and `vector_op`
results carry the same facts in `layer.summary.crs`.

### Checks that concern the CRS

| Check | Severity | When |
| --- | --- | --- |
| `crs.present` | fail | No CRS recorded. Columbus has no tool to assign one: ask the owner, fix it at the source. |
| `crs.info` | info | Names the CRS and units. |
| `crs.area_of_use` | fail | Labelled geographic but the coordinates cannot be lon/lat. The fix says either "Longitude and latitude look swapped; swap X and Y" or "The data is probably in a projected CRS; find the right one and assign it (do not reproject)". |
| `crs.area_of_use` | warn | Coordinates fall outside the CRS's area of use. `detail` has `extent_lonlat` and `area_of_use`. Usually a wrong label or swapped axes. |
| `crs.measure` | refusal / info | Refusal for buffer or simplify in degrees or in Web Mercator. Info on a successful buffer or simplify, recording distance, unit and CRS. |
| `crs.agreement` | info | clip, intersect or spatial_join reprojected the second input to the first input's CRS. |
| `crs.reprojected` | info | reproject ran, from and to. |

Do not proceed past a `crs.area_of_use` fail. Tell the person the fix and stop.

### Reproject

```json
vector_op {"op": "reproject", "inputs": {"layer": "austin:roads"}, "params": {"crs": "EPSG:3081"}}
```

`crs` must be an `EPSG:` code (`"EPSG:2277"`, `"EPSG:3083"`). WKT, PROJ strings and ESRI
codes are refused. The engine does not fetch transformation grids (`PROJ_NETWORK` is off),
so a WGS 84 to NAD83 reprojection may apply no datum shift. Say so when accuracy matters.

### Buffer and simplify with units

```json
vector_op {"op": "buffer", "inputs": {"layer": "ws://<session>/results/reproject-<id>.gpkg"},
           "params": {"distance": 500, "unit": "ft"}}
vector_op {"op": "simplify", "inputs": {"layer": "ws://<session>/results/roads-<id>.gpkg"},
           "params": {"tolerance": 2, "unit": "m", "preserve_topology": true}}
```

- `unit` is required: `m`, `ft` (international, 0.3048 m), `us-ft`, `km` or `mi` (also
  `us-mi`, `metre`, `meter`, `feet`, `mile`). Columbus converts it to the layer's own unit,
  so 500 `ft` on an EPSG:3081 (metre) layer buffers 152.4 m.
- `buffer` also takes `dissolve` (true merges into one shape with `feature_count`) and
  `segments` (1 to 64, default 8).
- A layer in degrees is refused:
  `Refused (crs.measure): cannot buffer by 100 ft: the layer is in EPSG:4326 (WGS 84),
  whose units are degrees`.
- Web Mercator (EPSG:3857, 900913, 3785, 102100, 102113) is refused too: "Web Mercator
  distances are stretched away from the equator (about 15% at Austin, more further north)".

The refusal's fix suggests a CRS for the data's own area: inside Texas, EPSG:3081 for
distances statewide, EPSG:3083 for areas, or the local State Plane zone; data across several
US states, EPSG:5070 (Conus Albers); elsewhere, the UTM zone of the data's centre (NAD83,
EPSG:269xx, in the US and Canada; WGS 84, EPSG:326xx or 327xx, elsewhere). When the extent
is not plausible longitude and latitude (feet read as degrees, swapped axes) or is too wide
for one zone outside the US, it gives no code; check `crs.area_of_use` first. It is a starting point. For local Texas work a State Plane zone such as
EPSG:2277 is often better, and the context pack's `default_crs` or a delivery spec wins
over both.

### Typical sequence

```text
describe_dataset                -> crs EPSG:4326, geographic true
vector_op reproject EPSG:2277   -> crs.reprojected info
vector_op buffer 500 ft         -> crs.measure info "Buffered by 500 ft in EPSG:2277 (...)"
vector_op reproject EPSG:3081   -> to the organization's delivery CRS, if a rule says so
verify                          -> crs.info, no crs.area_of_use
```

For areas: reproject to EPSG:3083 before you dissolve or intersect polygons whose areas
you will report. Columbus has no area-calculation op today; use `sql_select` with
`ST_Area` on a PostGIS connection, and say which CRS the area was computed in.

### Bounding boxes

`query_features` takes `bbox` with `bbox_crs` (an `EPSG:` code). A lon/lat box for
downtown Austin is `"bbox": [-97.76, 30.25, -97.73, 30.28], "bbox_crs": "EPSG:4326"`,
in X, Y (lon, lat) order. Columbus moves the box into the dataset's CRS. If the dataset has
no CRS, a `bbox_crs` is refused (`bbox.crs`); give the box in the dataset's own
coordinates instead.
