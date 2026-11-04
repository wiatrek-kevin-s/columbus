# GDAL commands for raster work

## Inspect

| Command | Shows |
| --- | --- |
| `gdalinfo in.tif` | CRS, origin, pixel size, size, data type, nodata, overviews, block size, metadata |
| `gdalinfo -stats in.tif` | Adds min, max, mean, std dev (writes a `.aux.xml` beside the file) |
| `gdalinfo -json in.tif` | The same as JSON for scripts |
| `gdalsrsinfo -o epsg in.tif` | The EPSG code GDAL can match, if any |
| `gdallocationinfo -wgs84 in.tif -97.74 30.27` | Value at a lon/lat |

## COG driver creation options (`-of COG`)

| Option | Values | Use |
| --- | --- | --- |
| `COMPRESS` | `DEFLATE`, `ZSTD`, `LZW`, `LERC`, `LERC_ZSTD`, `JPEG`, `WEBP`, `NONE` | `DEFLATE`/`ZSTD` for data; `JPEG`/`WEBP` for 8-bit RGB display only; `LERC` with `MAX_Z_ERROR` for controlled lossy elevation |
| `LEVEL` | 1 to 9 (DEFLATE), 1 to 22 (ZSTD) | Higher is smaller and slower |
| `PREDICTOR` | `YES`, `NO`, `STANDARD`, `FLOATING_POINT` | `YES` picks the right one for the data type |
| `BLOCKSIZE` | 256, 512 (default 512) | Internal tile size |
| `OVERVIEWS` | `AUTO`, `IGNORE_EXISTING`, `FORCE_USE_EXISTING`, `NONE` | `AUTO` builds them if the source has none |
| `OVERVIEW_RESAMPLING` | `NEAREST`, `AVERAGE`, `MODE`, `BILINEAR`, `CUBIC`, ... | Match the data (continuous `AVERAGE`, categorical `NEAREST`/`MODE`) |
| `RESAMPLING` | as above | Used when the COG driver also warps (`TILING_SCHEME`, `TARGET_SRS`) |
| `BIGTIFF` | `YES`, `NO`, `IF_NEEDED`, `IF_SAFER` | `IF_SAFER` for large or compressed outputs |
| `NUM_THREADS` | number or `ALL_CPUS` | Faster compression |
| `TILING_SCHEME` | `GoogleMapsCompatible`, ... | Web-tile aligned COGs (reprojects to EPSG:3857; do not measure on it) |

## gdalwarp flags you will use

| Flag | Meaning |
| --- | --- |
| `-t_srs EPSG:xxxx` | Target CRS |
| `-s_srs EPSG:xxxx` | Source CRS, only when the file's own is missing or wrong |
| `-r <method>` | Resampling: `near`, `bilinear`, `cubic`, `lanczos`, `average`, `mode`, `sum`, `min`, `max`, `med` |
| `-tr xres yres` | Output cell size in target units |
| `-tap` | Align the output grid to multiples of `-tr` |
| `-te xmin ymin xmax ymax` (`-te_srs`) | Output extent |
| `-srcnodata v` / `-dstnodata v` | Nodata in and out |
| `-cutline aoi.gpkg -crop_to_cutline` | Clip to a polygon |
| `-wo NUM_THREADS=ALL_CPUS -multi` | Parallel warping |
| `-ot Float32` | Output data type |
| `-overwrite` | Replace an existing output |

## Raster arithmetic

```bash
# Difference of two aligned DEMs (same grid), nodata kept
gdal_calc.py -A dem_2026.tif -B dem_2018.tif --outfile=change.tif \
  --calc="A-B" --NoDataValue=-9999 --type=Float32
```

`gdal_calc.py` needs inputs on the same grid. Warp one onto the other first (`-te`, `-tr`
and `-tap` from the reference).

## Rasterize and polygonize

```bash
gdal_rasterize -a ZONE_ID -tr 3 3 -tap -a_nodata 0 -ot Int32 zones.gpkg zones.tif
gdal_polygonize.py classes.tif -f GPKG classes.gpkg classes CLASS
```

## Common elevation sources and their vertical datums (check each product's metadata)

| Source | Typical CRS | Vertical |
| --- | --- | --- |
| USGS 3DEP 1/3 arc-second, 1 m DEM | NAD83 geographic (seamless), UTM NAD83 (1 m) | NAVD88, metres |
| Copernicus GLO-30 | WGS 84 geographic | EGM2008, metres |
| State or county lidar DEMs (Texas: TNRIS / TxGIO) | State plane or UTM, often feet | NAVD88 with a GEOID model, check units |
