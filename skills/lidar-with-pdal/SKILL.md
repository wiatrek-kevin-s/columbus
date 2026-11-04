---
name: lidar-with-pdal
description: Processing lidar point clouds (LAS, LAZ, COPC, EPT) with PDAL, covering JSON pipelines, ASPRS classification codes (2 ground, 6 building, 7 low noise, 9 water, 18 high noise), ground filtering with filters.smrf and filters.pmf, density checks, CRS and vertical datum in LAS headers, and reading the USGS 3DEP EPT bucket. Use it when asked to "make a DTM or DSM from lidar", "classify ground", "remove noise", "check point density", "what are these classification codes", "get 3DEP lidar for my area", or "convert LAZ to COPC".
---

# Lidar with PDAL

PDAL (Point Data Abstraction Library) reads, filters and writes point clouds through
**pipelines**: a JSON list of stages run in order. Readers bring points in, filters change
or select them, writers send them out (to LAS/LAZ, COPC or a raster).

## General practice (with or without Columbus)

### 1. Look before you process

```bash
pdal info --summary tile.laz        # point count, bounds, CRS, dimensions
pdal info --metadata tile.laz       # header: LAS version, point format, scale/offset, SRS, VLRs
pdal info --boundary tile.laz       # real footprint (hexbin) and average point density
```

Answer these first:

- **LAS version and point format**: LAS 1.4 with point formats 6 to 10 is current; it
  stores the CRS as OGC WKT. LAS 1.2/1.3 (formats 0 to 5) store it as GeoTIFF keys and allow
  only classes 0 to 31.
- **Horizontal CRS and units**: metres or US survey feet. Texas data is often state plane in
  feet (EPSG:2277 NAD83 / Texas Central ftUS, or EPSG:6578 for NAD83(2011)).
- **Vertical datum and units**: usually NAVD88 (with a geoid model such as GEOID18) for US
  public lidar, as a compound CRS (EPSG:6343+5703 is NAD83(2011) / UTM 14N + NAVD88 height).
  If the header has no vertical CRS, find it in the project report. Do not guess; ellipsoid
  and NAVD88 heights differ by tens of metres in the US.
- **Classification counts**: has it been classified, and with which codes?

```json
[
  "tile.laz",
  { "type": "filters.stats", "dimensions": "Z", "count": "Classification" }
]
```
Run it with `pdal pipeline stats.json --metadata stats_out.json` and read the counts.

### 2. ASPRS classification codes (LAS 1.4)

| Code | Meaning | Code | Meaning |
| --- | --- | --- | --- |
| 0 | Created, never classified | 9 | Water |
| 1 | Unclassified (processed, not assigned) | 10 | Rail |
| 2 | **Ground** | 11 | Road surface |
| 3 | Low vegetation | 13 | Wire guard (shield) |
| 4 | Medium vegetation | 14 | Wire conductor (phase) |
| 5 | High vegetation | 15 | Transmission tower |
| 6 | **Building** | 17 | Bridge deck |
| 7 | **Low point (low noise)** | 18 | **High noise** |

The full table, flags (withheld, overlap, synthetic, key-point) and the USGS 3DEP use of 20 to
22 are in [references/las-classification.md](references/las-classification.md).

- Vegetation height classes (3 to 5) have no fixed thresholds; each vendor sets them.
- Exclude **7 and 18** (noise) and points with the **withheld** flag from everything.
- In LAS 1.4, overlap is a flag, not class 12. Older data may still use 12.

### 3. Pipelines

A pipeline is a JSON array (or `{"pipeline": [...]}`). A bare file name is a reader or writer
chosen by extension.

**DTM (bare earth) from classified data:**

```json
[
  { "type": "readers.las", "filename": "tile.laz" },
  { "type": "filters.range", "limits": "Classification[2:2]" },
  { "type": "writers.gdal", "filename": "dtm.tif", "resolution": 1.0,
    "output_type": "idw", "window_size": 3, "nodata": -9999,
    "gdalopts": "COMPRESS=DEFLATE,TILED=YES" }
]
```

**DSM (first surface):** keep first returns, drop noise:

```json
[
  "tile.laz",
  { "type": "filters.expression",
    "expression": "Classification != 7 && Classification != 18 && ReturnNumber == 1" },
  { "type": "writers.gdal", "filename": "dsm.tif", "resolution": 1.0,
    "output_type": "max", "nodata": -9999 }
]
```

In `filters.range`, limits joined by commas are ANDed across different dimensions but
**ORed for the same dimension**, so `Classification![7:7],Classification![18:18]` keeps
everything. Use `filters.expression` for "not this and not that".

**Ground classification on unclassified data:**

```json
[
  "raw.laz",
  { "type": "filters.assign", "value": "Classification = 0" },
  { "type": "filters.elm" },
  { "type": "filters.outlier", "method": "statistical", "mean_k": 12, "multiplier": 2.5 },
  { "type": "filters.smrf", "ignore": "Classification[7:7]",
    "slope": 0.15, "window": 18.0, "threshold": 0.5, "scalar": 1.25, "cell": 1.0 },
  { "type": "writers.las", "filename": "classified.laz", "compression": "true",
    "minor_version": 4, "dataformat_id": 6, "forward": "all" }
]
```

- `filters.elm` (extended local minimum) and `filters.outlier` mark noise as class 7, and
  `ignore` keeps it out of the ground search.
- **`filters.smrf`** (simple morphological filter) is the usual first choice. Raise `slope`
  for steep terrain; lower `threshold` for flatter, cleaner ground; `window` should exceed the
  largest building footprint in metres or feet (it is in CRS units).
- **`filters.pmf`** (progressive morphological filter) is the alternative: `max_window_size`,
  `slope`, `initial_distance`, `max_distance`, `cell_size`. It is slower and more sensitive to
  its window on large buildings.
- **Units matter**: the defaults above assume metres. In US feet, scale `window`, `cell` and
  `threshold` (for example `window` 60, `cell` 3, `threshold` 1.6).
- Always look at the result: a hillshade of the DTM shows buildings left as bumps (window
  too small) or cut-off hilltops (slope too low).
- `"compression": "true"` writes LAZ. Without it, a `.laz` name holds uncompressed LAS.
- `"forward": "all"` keeps the source header's scale, offset and VLRs, including the CRS.

**Height above ground** (vegetation or building heights):

```json
[ "classified.laz",
  { "type": "filters.hag_nn" },
  { "type": "filters.range", "limits": "HeightAboveGround[2:60]" },
  { "type": "writers.gdal", "filename": "canopy_height.tif", "resolution": 1.0,
    "dimension": "HeightAboveGround", "output_type": "max" } ]
```

**Reproject** (horizontal and, where the PROJ grids are installed, vertical):

```json
{ "type": "filters.reprojection", "in_srs": "EPSG:6343+5703", "out_srs": "EPSG:6578+6360" }
```
Set `in_srs` only when the header is missing or wrong. Note the output units change (here
metres to US feet). Check vertical units in the output.

### 4. COPC and EPT

- **COPC** (Cloud-Optimized Point Cloud) is one LAZ 1.4 file arranged as an octree. Clients
  read only the area and detail level they need over HTTP. Write it with
  `{ "type": "writers.copc", "filename": "tile.copc.laz" }`; read with `readers.copc`, which
  takes `bounds`, `polygon` and `resolution`.
- **EPT** (Entwine Point Tile) is a folder of tiles plus `ept.json`. Read with `readers.ept`.
- Prefer COPC for sharing single tiles or projects; use EPT where a provider already serves it.

### 5. USGS 3DEP lidar (EPT on AWS)

USGS 3DEP point clouds are published as EPT in the public `usgs-lidar-public` bucket
(`https://s3-us-west-2.amazonaws.com/usgs-lidar-public/<PROJECT>/ept.json`). The project list
and footprints are in the `resources.geojson` of the `hobuinc/usgs-lidar` repository on GitHub.
They are also in Microsoft Planetary Computer's `3dep-lidar-copc` STAC collection as COPC.

```json
[
  { "type": "readers.ept",
    "filename": "https://s3-us-west-2.amazonaws.com/usgs-lidar-public/<PROJECT>/ept.json",
    "bounds": "([-10882000, -10878000], [3551000, 3555000])",
    "resolution": 1.0 },
  { "type": "filters.reprojection", "out_srs": "EPSG:6343" },
  { "type": "filters.range", "limits": "Classification[2:2]" },
  { "type": "writers.gdal", "filename": "dtm_utm14.tif", "resolution": 1.0, "output_type": "idw" }
]
```

- The EPT copies are in **Web Mercator (EPSG:3857)**, so `bounds` are in EPSG:3857 metres
  (the box above is about 4 km square over central Austin).
  Convert your area of interest first. Web Mercator lengths are stretched (about 16% at
  Austin), so **reproject before gridding or measuring**, as above.
- Read the project's metadata for the vertical datum, the quality level and the acquisition
  date; several projects can cover one place. Pick by date and quality level.
- `resolution` asks EPT for a coarser level of detail; omit it for full density.
- Start with a small `bounds` box. A whole county pulls billions of points.

### 6. Density checks

- `pdal info --boundary` reports average points per square unit over the real footprint. In
  US feet, multiply points per square foot by 10.764 to get points per square metre.
- Specifications use **pulse density** (first or last returns) or **aggregate nominal pulse
  spacing**, not all points. Count first returns: `filters.range` with `ReturnNumber[1:1]`.
- USGS Lidar Base Specification quality levels: **QL2** at least 2 pulses/m² (spacing ≤ 0.71
  m), **QL1** and **QL0** at least 8 pulses/m² (spacing ≤ 0.35 m), with different accuracy.
- Density varies: water returns little, flight-line overlap doubles it. Map density on a grid
  (`writers.gdal` with `"output_type": "count"`) to see voids, and report the share of cells
  below specification, not only the mean.

### 7. Before you hand over lidar products

1. CRS (horizontal and vertical) and units stated, and correct in the output's header or raster.
2. Classes used (ground only? noise excluded? withheld excluded?).
3. Cell size and interpolation (`idw`, `min`, `max`, `mean`) for rasters.
4. Source project names, acquisition dates and quality levels.
5. Voids shown, not silently interpolated.

## If the Columbus tools are available

**Columbus cannot process point clouds yet.** The point-cloud tool (`pointcloud_pipeline`,
running PDAL pipelines) is planned and not yet available. Write the pipeline for the person
to run with `pdal pipeline <file>.json`, and say so.

What helps today:

- **Find lidar sources** for an area in the geospatial registry:
  `registry_search {"query": "lidar point cloud", "kind": "data", "bbox": [-98.0, 30.0, -97.5, 30.5]}`
  and `registry_get {"id": "..."}` for coverage, licence and access links. Results are
  community-written and marked untrusted; entries with no recorded extent come last and are
  marked unchecked. Fetching through the registry (`registry_fetch`) is planned, not available.
  `registry_search {"query": "PDAL", "kind": "tool"}` finds tool entries.
- **Check the organization's own lidar notes** (vertical datum, tile scheme, vendor class
  conventions): `context_search {"query": "lidar"}`, then `context_get {"id": "..."}`.
- **Prepare an area of interest or tile selection** with the vector tools when the tile index
  is a governed dataset: `query_features` with a `bbox` and `bbox_crs`, then
  `vector_op {"op": "reproject", ..., "params": {"crs": "EPSG:3857"}}` to get bounds for
  `readers.ept`, and `verify` on the result.
