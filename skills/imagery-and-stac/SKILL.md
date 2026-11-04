---
name: imagery-and-stac
description: Finding and using satellite imagery through STAC APIs, covering searches by collection, bbox, datetime and eo:cloud_cover, Sentinel-2 L2A and Landsat Collection 2 Level-2, cloud masking with the SCL band and QA_PIXEL, scale factors and offsets, NDVI and other indices with the right bands per sensor, mosaics and composites, and the Planetary Computer and Earth Search endpoints. Use it when asked to "find Sentinel-2 or Landsat imagery", "search STAC", "get a cloud-free image", "calculate NDVI", "mask clouds", "make a median composite", or when index values fall outside -1 to 1.
---

# Imagery and STAC

STAC (SpatioTemporal Asset Catalog) is a standard JSON way to describe imagery: a
**collection** (Sentinel-2 L2A) holds **items** (one scene or tile at one time), and each
item has **assets** (band files, usually COGs) and properties (date, cloud cover, sensor).
A STAC API lets you search items by place, time and properties, then read only the pixels
you need from the COGs.

## General practice (with or without Columbus)

### 1. Endpoints and collections

| Catalogue | STAC API | Sentinel-2 L2A | Landsat C2 L2 | Notes |
| --- | --- | --- | --- | --- |
| Microsoft Planetary Computer | `https://planetarycomputer.microsoft.com/api/stac/v1` | `sentinel-2-l2a` | `landsat-c2-l2` | Assets need a signed URL (`planetary_computer.sign_inplace`); free |
| Element 84 Earth Search (AWS) | `https://earth-search.aws.element84.com/v1` | `sentinel-2-l2a`, `sentinel-2-c1-l2a` | `landsat-c2-l2` | Sentinel-2 COGs are public; Landsat assets are in USGS's requester-pays bucket |
| USGS LandsatLook | `https://landsatlook.usgs.gov/stac-server` | | `landsat-c2l2-sr`, `landsat-c2l2-st` | The source catalogue for Landsat |

Asset names differ: Planetary Computer's Sentinel-2 uses `B04`, `B08`, `SCL`; Earth Search
uses `red`, `nir`, `scl`. List `item.assets.keys()` before writing band code.

### 2. Search

```python
from pystac_client import Client
import planetary_computer

cat = Client.open("https://planetarycomputer.microsoft.com/api/stac/v1",
                  modifier=planetary_computer.sign_inplace)
search = cat.search(
    collections=["sentinel-2-l2a"],
    bbox=[-97.95, 30.15, -97.60, 30.45],            # lon/lat, WGS 84, west south east north
    datetime="2026-06-01/2026-08-31",
    query={"eo:cloud_cover": {"lt": 20}},
    max_items=200,
)
items = search.item_collection()
print(len(items), sorted({i.properties["s2:mgrs_tile"] for i in items}))
```

- `bbox` is always lon/lat in WGS 84, whatever the imagery's CRS. Use `intersects` with a
  GeoJSON geometry for irregular areas.
- `datetime` is an RFC 3339 interval: `"2026-06-01/2026-08-31"`, or open-ended `"2026-06-01/.."`.
- `query` is the older property filter; newer APIs also take CQL2 `filter`:
  `filter={"op": "<", "args": [{"property": "eo:cloud_cover"}, 20]}`.
- **`eo:cloud_cover` is for the whole scene or tile**, not your area. A 15% cloudy tile can
  be fully clouded over your city. Compute clear-pixel share inside your area from the mask.
- One place can fall in several Sentinel-2 MGRS tiles or Landsat path/rows on the same day.
  Group by date (`groupby="solar_day"` in odc-stac) so they mosaic rather than duplicate.

### 3. Sentinel-2 L2A

- Surface reflectance, 10 m (B02 blue, B03 green, B04 red, B08 NIR), 20 m (B05 to B07 red
  edge, B8A narrow NIR, B11 and B12 SWIR, SCL), 60 m (B01, B09). Full table:
  [references/bands-and-masks.md](references/bands-and-masks.md).
- **Scale and offset**: reflectance = (DN + offset) / 10000. Since processing baseline
  **04.00 (25 January 2022)**, the products carry an offset of **-1000**, so
  reflectance = (DN - 1000) / 10000. Earlier scenes have no offset. Mixing them without
  correcting shifts every value by 0.1, which ruins time series and NDVI.
  Check the item's `s2:processing_baseline`, or use the `raster:bands` `scale` and `offset`
  on the asset when the catalogue provides them. Collections differ in whether the offset
  is already applied (Earth Search's `sentinel-2-c1-l2a` is reprocessed and harmonised; read
  its asset metadata).
- **Cloud mask with SCL** (scene classification layer, 20 m):

| SCL | Class | Mask? |
| --- | --- | --- |
| 0 | No data | yes |
| 1 | Saturated or defective | yes |
| 2 | Dark area pixels / cast shadows | usually |
| 3 | Cloud shadows | yes |
| 4 | Vegetation | keep |
| 5 | Not vegetated | keep |
| 6 | Water | keep |
| 7 | Unclassified | judgement |
| 8 | Cloud, medium probability | yes |
| 9 | Cloud, high probability | yes |
| 10 | Thin cirrus | yes |
| 11 | Snow or ice | depends on the task |

```python
import numpy as np
bad = np.isin(scl, [0, 1, 3, 8, 9, 10])      # resample SCL to 10 m with NEAREST first
red = np.where(bad, np.nan, (b04.astype("float32") - 1000) / 10000)
```

SCL misses some thin cloud and haze and flags some bright roofs as cloud. Look at the result.

### 4. Landsat Collection 2 Level-2

- Landsat 8 and 9 (OLI): SR_B2 blue, SR_B3 green, SR_B4 red, **SR_B5 NIR**, SR_B6 SWIR1,
  SR_B7 SWIR2, ST_B10 thermal. Landsat 4, 5 and 7 (TM/ETM+) number them differently:
  SR_B1 blue, SR_B2 green, **SR_B3 red, SR_B4 NIR**, SR_B5 SWIR1, SR_B7 SWIR2, ST_B6 thermal.
- **Scale factors**: surface reflectance = DN × **0.0000275** − **0.2**; surface temperature
  (K) = DN × **0.00341802** + **149.0**. Reflectance outside roughly −0.2 to 1.6 is fill or error.
- **Cloud mask with QA_PIXEL** (bit flags):

| Bit | Meaning |
| --- | --- |
| 0 | Fill |
| 1 | Dilated cloud |
| 2 | Cirrus (Landsat 8 and 9) |
| 3 | Cloud |
| 4 | Cloud shadow |
| 5 | Snow |
| 6 | Clear |
| 7 | Water |

```python
mask_bits = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4)   # = 31
bad = (qa_pixel & mask_bits) != 0
nir = np.where(bad, np.nan, sr_b5 * 0.0000275 - 0.2)
```

- Landsat 7 after May 2003 has scan-line-corrector gaps (stripes of nodata). Mask them; do
  not interpolate silently.

### 5. Spectral indices

Compute in `float32`, on **scaled reflectance**, after masking.

| Index | Formula | Sentinel-2 | Landsat 8/9 | Landsat 4 to 7 |
| --- | --- | --- | --- | --- |
| NDVI (vegetation) | (NIR − Red) / (NIR + Red) | (B08 − B04)/(B08 + B04) | (B5 − B4)/(B5 + B4) | (B4 − B3)/(B4 + B3) |
| NDWI (open water, McFeeters) | (Green − NIR) / (Green + NIR) | (B03 − B08)/(...) | (B3 − B5)/(...) | (B2 − B4)/(...) |
| NDMI (moisture) | (NIR − SWIR1) / (NIR + SWIR1) | (B08 − B11)/(...) | (B5 − B6)/(...) | (B4 − B5)/(...) |
| NBR (burn) | (NIR − SWIR2) / (NIR + SWIR2) | (B08 − B12)/(...) | (B5 − B7)/(...) | (B4 − B7)/(...) |
| EVI | 2.5 (NIR − Red) / (NIR + 6 Red − 7.5 Blue + 1) | B08, B04, B02 | B5, B4, B2 | B4, B3, B1 |

- NDVI on raw Sentinel-2 DNs after baseline 04.00 is biased (the −1000 offset does not
  cancel). EVI needs reflectance in 0 to 1; on DNs it is meaningless.
- Values outside −1 to 1 mean unscaled data, integer arithmetic or unmasked fill.
- Mixing sensors in one time series (Sentinel-2 with Landsat) needs harmonised products
  (for example NASA HLS); band responses differ.

### 6. Loading, mosaics and composites

```python
import odc.stac
ds = odc.stac.load(items, bands=["B02", "B04", "B08", "SCL"],
                   crs="EPSG:32614", resolution=10, groupby="solar_day",
                   bbox=[-97.95, 30.15, -97.60, 30.45], chunks={})
```

- Choose the output CRS on purpose: the scene's UTM zone (Austin is 14N, EPSG:32614) for
  analysis, not EPSG:4326 or 3857.
- **Mosaic** (one date, several tiles): `groupby="solar_day"` merges same-day tiles.
- **Composite** (many dates, one clear image): mask each date, then take the per-pixel
  **median** over time. Median resists leftover cloud and shadow; mean does not. A
  max-NDVI composite favours green-season pixels, which suits vegetation but not land cover.
- Report the dates used and the number of clear observations per pixel; a composite pixel
  from one observation is weaker than one from twenty.
- Resample categorical bands (SCL, QA_PIXEL) with nearest; reflectance with bilinear or
  average (see the raster-and-terrain skill).
- Save results as COGs with the CRS, scale and nodata set.

### 7. Licences and citations

Sentinel-2 (Copernicus) data are free and open with attribution ("Contains modified
Copernicus Sentinel data [year]"). Landsat data are public domain from USGS; cite them.
Catalogue terms (Planetary Computer, AWS) apply to access.

## If the Columbus tools are available

**Columbus has no imagery or raster processing yet.** The raster tool (`raster_op`) is
planned and not yet available, so Columbus cannot read, mask or composite imagery today.
Write the pystac-client and odc-stac (or rasterio) code for the person to run, and say so.

What helps today:

- **Find imagery sources and STAC catalogues** in the geospatial registry:
  `registry_search {"query": "sentinel-2 stac", "kind": "data"}`, then
  `registry_get {"id": "..."}` for the endpoint, licence and access. Entries are community
  written and marked untrusted. Running an entry's access recipe (`registry_fetch`) is planned,
  not available.
- **Build the search area from governed data**: pull the boundary with `query_features`,
  reproject it with
  `vector_op {"op": "reproject", "inputs": {"layer": "<handle>"}, "params": {"crs": "EPSG:4326"}}`
  to get a lon/lat `bbox` for the STAC search, and `verify` it.
- **A STAC server connected to Columbus**: if the organization has connected and approved
  one, `list_providers` shows it and `run_tool {"name": "<provider>.<tool>", "args": {...}}`
  calls it. Its results are wrapped as untrusted data. While governed-data labels are active,
  sending a derived area to a connected server can need the person's approval; that prompt
  goes to the person, not to you. Do not assume such a server exists.
- **Organization notes** on imagery (licensed vendor imagery, preferred collections):
  `context_search {"query": "imagery"}`.
