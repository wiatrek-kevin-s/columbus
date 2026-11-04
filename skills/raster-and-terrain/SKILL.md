---
name: raster-and-terrain
description: Working correctly with rasters and elevation models in GDAL, rasterio and ArcGIS, covering resampling choices, nodata, Cloud-Optimized GeoTIFF (overviews, tiling, gdal_translate -of COG), DEM slope, aspect and hillshade with the right z-factor, zonal statistics pitfalls (cell alignment, all-touched) and mosaicking tiles. Use it when asked to "make a hillshade", "calculate slope", "resample", "reproject a raster", "convert to COG", "mosaic DEM tiles", "zonal statistics by parcel", or when slope values look absurd or a raster has holes or edge stripes.
---

# Raster and terrain

A raster is a grid of numbers with a CRS, a cell size, an origin and a nodata value. Most
raster mistakes are silent: the output looks like a picture of the right thing but the
numbers are wrong. Check the four properties first, every time.

```bash
gdalinfo -stats dem.tif          # CRS, origin, pixel size, data type, nodata, min/max
```

## General practice (with or without Columbus)

### 1. Know what the cells mean

- **Continuous**: elevation, temperature, reflectance, distance. Values can be averaged.
- **Categorical**: land cover, soil class, zoning codes. Values are labels. Averaging `41`
  (deciduous forest) and `82` (crops) gives nonsense.
- Data type: `Byte` (0 to 255), `Int16`, `UInt16`, `Float32`. Compute indices and
  derivatives in `Float32`, or integer division and overflow wreck them.
- Vertical units and datum for elevation: metres or feet; NAVD88 (orthometric) or
  ellipsoid heights. In the US the two differ by tens of metres. Never mix them in one
  mosaic or difference.

### 2. Resampling

| Data | Upsampling or same size | Downsampling (coarser cells) |
| --- | --- | --- |
| Categorical (land cover, classes, IDs) | `near` (nearest neighbour) | `mode` (majority) |
| Continuous (elevation, temperature) | `bilinear` or `cubic` | `average` |
| Counts, totals (population per cell) | avoid, or redistribute | `sum` (GDAL 3.1+) |
| Imagery for display | `cubic` or `lanczos` | `average` |

- `cubic` and `lanczos` can overshoot: negative rainfall, elevations below the true minimum.
  Use `bilinear` if the range matters.
- Reprojecting is resampling. Pick the method on `gdalwarp` deliberately (`-r`), and set the
  output cell size (`-tr`) rather than letting it be guessed.
- Align to a reference grid with `-tap` (target-aligned pixels) or an exact `-te` and `-tr`,
  so later stacks and zonal statistics line up.

```bash
gdalwarp -t_srs EPSG:2277 -r bilinear -tr 3 3 -tap \
  -srcnodata -9999 -dstnodata -9999 -co COMPRESS=DEFLATE -co TILED=YES \
  dem_utm.tif dem_2277.tif
```

### 3. Nodata

- Set nodata explicitly and keep it the same through a whole workflow:
  `gdal_translate -a_nodata -9999 in.tif out.tif` (this labels; it does not change values).
- **`0` is often a real value** (sea level, no change, zero rainfall). Do not use it as nodata
  for elevation or differences.
- Esri float rasters often use `-3.4028235e+38` as nodata. If statistics show that as the
  minimum, the nodata value is not set.
- Without nodata set, bilinear resampling and slope smear the fill value into the edges,
  giving cliffs and stripes around tiles.
- Report the share of nodata cells in any statistic over an area.

### 4. Cloud-Optimized GeoTIFF (COG)

A COG is a GeoTIFF with internal tiling and overviews arranged so a client can read just the
part and zoom level it needs over HTTP. Use it for anything shared or put in cloud storage.

```bash
gdal_translate dem.tif dem_cog.tif -of COG \
  -co COMPRESS=DEFLATE -co PREDICTOR=YES -co BLOCKSIZE=512 \
  -co OVERVIEW_RESAMPLING=AVERAGE -co NUM_THREADS=ALL_CPUS -co BIGTIFF=IF_SAFER
```

- `-of COG` (GDAL 3.1+) builds internal tiles and overviews in one step. Overviews are made
  automatically (`OVERVIEWS=AUTO`).
- Overview resampling follows section 2: `AVERAGE` for continuous, `NEAREST` or `MODE` for
  categorical. The default is not always right for your data.
- Compression: `DEFLATE` or `ZSTD` (lossless) with `PREDICTOR=YES` (picks 2 for integers, 3
  for floats); `JPEG` or `WEBP` only for 8-bit RGB imagery for display, never for data values.
- Check it: `gdalinfo dem_cog.tif` should list `LAYOUT=COG` under Image Structure
  Metadata, and `Overviews:` on the band; or `rio cogeo validate dem_cog.tif`.
- For an existing GeoTIFF without the COG driver, `gdaladdo -r average in.tif` adds
  overviews, but only `-of COG` guarantees the layout.

More creation options and commands: [references/gdal-commands.md](references/gdal-commands.md).

### 5. DEM derivatives: slope, aspect, hillshade

**The z-factor.** Slope compares a vertical change to a horizontal distance. If they are in
different units, you must convert.

| Horizontal (CRS) units | Elevation units | Esri z-factor | `gdaldem -s` (scale) |
| --- | --- | --- | --- |
| metres | metres | 1 | 1 |
| US feet (e.g. EPSG:2277) | US feet | 1 | 1 |
| metres | feet | 0.3048 | 3.28084 |
| feet | metres | 3.28084 | 0.3048 |
| degrees (EPSG:4326) | metres | do not; reproject first | 111120 only near the equator |

Esri's z-factor multiplies the elevations into horizontal units; GDAL's `-s` is the inverse
ratio. A slope raster full of 89° or of 0.1° is almost always a wrong factor or a DEM in
degrees. **Reproject the DEM to a projected CRS before computing derivatives.** Pro's `Slope`
also offers `method="GEODESIC"` with a `z_unit`, which handles geographic data properly.

```bash
gdaldem slope     dem_2277.tif slope_deg.tif  -compute_edges            # degrees
gdaldem slope     dem_2277.tif slope_pct.tif  -p -compute_edges         # percent rise
gdaldem aspect    dem_2277.tif aspect.tif     -zero_for_flat -compute_edges
gdaldem hillshade dem_2277.tif hillshade.tif  -az 315 -alt 45 -z 1 -multidirectional
```

```python
from arcpy.sa import Slope, Aspect, Hillshade
Slope("dem_2277", "DEGREE", z_factor=1).save(r"C:\work\terrain.gdb\slope")
Hillshade("dem_2277", azimuth=315, altitude=45, z_factor=1).save(r"C:\work\terrain.gdb\hs")
```

- Slope in **degrees** (0 to 90) and **percent** (100% = 45°) are often confused. Label it.
- Aspect is degrees clockwise from north; flat cells are `-1` in Esri or nodata/`0` in GDAL.
  For averaging aspect, use sine and cosine, never the raw degrees.
- Hillshade's `-z` (vertical exaggeration) is for appearance only. Do not use exaggerated
  hillshade for analysis.
- Derivatives from a fine lidar DEM are noisy; smooth or resample first if the question is
  about general terrain ("slopes over 15% at parcel scale").
- `-compute_edges` stops a one-cell nodata border.

### 6. Zonal statistics

Statistics of raster cells inside polygons (mean elevation per parcel, % forest per county).

- **Same CRS.** Reproject the polygons to the raster's CRS (cheap, exact), not the raster to
  the polygons'. Many libraries (`rasterstats`) do not reproject for you.
- **Cell centre vs all touched.** By default a cell counts if its centre is inside the
  polygon. Small or thin polygons can get **zero cells** and return null.
  `all_touched=True` includes every cell the boundary touches, which overcounts edges and
  biases small polygons. For accurate results on small polygons, use fractional coverage
  (`exactextract`), which weights each cell by the share inside.
- **Alignment.** Zones are rasterized on the value raster's grid. In ArcGIS set
  `arcpy.env.snapRaster` and `arcpy.env.cellSize` to the value raster so zones do not shift
  half a cell.
- **Overlapping polygons.** A tool that rasterizes all zones into one grid gives each cell
  to one zone only. Check your tool's documentation, or process overlapping zones separately.
- **Categorical rasters**: report counts or shares per class (`categorical=True` in
  `rasterstats`, or `TabulateArea` in ArcGIS), not the mean.
- **Area per cell** is not constant in a geographic CRS. Use a projected, equal-area CRS
  (for Texas, EPSG:3083) to report areas.
- Always report `count` (valid cells) and the nodata share with each mean.

```python
from rasterstats import zonal_stats
stats = zonal_stats("parcels_32614.gpkg", "dem_32614.tif",
                    stats=["count", "mean", "min", "max"], nodata=-9999, all_touched=False)
```

### 7. Mosaicking

Before joining tiles, confirm they share CRS, cell size, data type, band count, nodata and
(for elevation) the vertical datum and units. Then:

```bash
gdalbuildvrt -srcnodata -9999 -vrtnodata -9999 -resolution highest mosaic.vrt tiles/*.tif
gdal_translate mosaic.vrt dem_mosaic.tif -of COG -co COMPRESS=DEFLATE -co PREDICTOR=YES
```

- In overlaps, **the last file listed wins** in `gdalbuildvrt` and `gdal_merge.py`. Order the
  list on purpose (newest or best last).
- Tiles of different dates or sensors leave visible seams. For elevation, that may mean a
  real difference (new construction) or a datum mismatch. Investigate before blending.
- In ArcGIS: `arcpy.management.MosaicToNewRaster(..., mosaic_method="LAST")`, or a mosaic
  dataset for many tiles that change over time.
- Clip to the area of interest after mosaicking:
  `gdalwarp -cutline aoi.gpkg -crop_to_cutline -dstnodata -9999 mosaic.vrt aoi_dem.tif`.

### 8. Before you hand over a raster result

1. CRS, cell size, extent and nodata as intended (`gdalinfo`).
2. Value range is plausible (no `-3.4e38` minimum, slope in 0 to 90).
3. Units stated: metres or feet, degrees or percent, vertical datum.
4. Resampling method and source dates written down.
5. A COG if it will be shared.

## If the Columbus tools are available

**Columbus has no raster processing yet.** The raster tool (`raster_op`, through GDAL) is
planned and not yet available, so Columbus cannot resample, warp, compute slope or zonal
statistics today. Write the GDAL, rasterio or ArcPy steps for the person to run, and say so.

What helps today:

- **Find elevation and raster sources** in the geospatial registry:
  `registry_search {"query": "lidar DEM", "kind": "data", "theme": "elevation", "bbox": [-98.0, 30.0, -97.5, 30.5]}`,
  then `registry_get {"id": "..."}` for licence, coverage and access. Entries are community
  written and marked untrusted; check the licence yourself. Downloading through the registry
  (`registry_fetch`) is planned and not available.
- **Read the organization's notes** on its rasters and terrain products:
  `context_search {"query": "DEM vertical datum"}`.
- **Prepare zones** for zonal statistics with the vector tools: pull them with
  `query_features`, reproject them to the raster's CRS with
  `vector_op {"op": "reproject", "inputs": {"layer": "<handle>"}, "params": {"crs": "EPSG:32614"}}`,
  and run `verify` on the result to catch invalid or empty polygons before they give null
  statistics. `list_outputs` lists the resulting workspace files and their provenance.
