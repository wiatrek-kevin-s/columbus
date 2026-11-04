# Bands, scale factors and masks

## Sentinel-2 MSI, Level-2A (surface reflectance)

| Band | Planetary Computer asset | Earth Search asset | Centre (nm) | Resolution | Use |
| --- | --- | --- | --- | --- | --- |
| B01 | `B01` | `coastal` | 443 | 60 m | Aerosol |
| B02 | `B02` | `blue` | 490 | 10 m | Blue |
| B03 | `B03` | `green` | 560 | 10 m | Green |
| B04 | `B04` | `red` | 665 | 10 m | Red |
| B05 | `B05` | `rededge1` | 705 | 20 m | Red edge |
| B06 | `B06` | `rededge2` | 740 | 20 m | Red edge |
| B07 | `B07` | `rededge3` | 783 | 20 m | Red edge |
| B08 | `B08` | `nir` | 842 | 10 m | NIR (broad) |
| B8A | `B8A` | `nir08` | 865 | 20 m | NIR (narrow) |
| B09 | `B09` | `nir09` | 945 | 60 m | Water vapour |
| B11 | `B11` | `swir16` | 1610 | 20 m | SWIR 1 |
| B12 | `B12` | `swir22` | 2190 | 20 m | SWIR 2 |
| SCL | `SCL` | `scl` | | 20 m | Scene classification |
| TCI | `visual` | `visual` | | 10 m | True colour, 8-bit, display only |

B10 (cirrus) is in Level-1C only. Asset names can change between catalogue versions:
always list `item.assets` first.

**Scaling**: reflectance = (DN + BOA_ADD_OFFSET) / 10000 (QUANTIFICATION_VALUE). For processing
baseline 04.00 and later, BOA_ADD_OFFSET = −1000; before it, 0. Nodata is DN 0.

## Landsat Collection 2, Level-2

| Quantity | Landsat 8/9 (OLI/TIRS) | Landsat 4, 5, 7 (TM/ETM+) |
| --- | --- | --- |
| Coastal/aerosol | SR_B1 | none |
| Blue | SR_B2 | SR_B1 |
| Green | SR_B3 | SR_B2 |
| Red | SR_B4 | SR_B3 |
| NIR | SR_B5 | SR_B4 |
| SWIR 1 | SR_B6 | SR_B5 |
| SWIR 2 | SR_B7 | SR_B7 |
| Thermal (surface temperature) | ST_B10 | ST_B6 |
| Pixel QA | QA_PIXEL | QA_PIXEL |
| Saturation | QA_RADSAT | QA_RADSAT |

Planetary Computer and Earth Search name Landsat assets by colour (`blue`, `green`, `red`,
`nir08`, `swir16`, `swir22`, `lwir11`, `qa_pixel`); USGS uses the SR_/ST_ names.

| Product | Scale | Offset | Valid DN range | Fill |
| --- | --- | --- | --- | --- |
| Surface reflectance (SR_B*) | 0.0000275 | −0.2 | 7273 to 43636 | 0 |
| Surface temperature (ST_B*) | 0.00341802 | 149.0 (gives kelvin) | 1 to 65535 | 0 |

## QA_PIXEL bits (Landsat Collection 2)

| Bit(s) | Flag | Notes |
| --- | --- | --- |
| 0 | Fill | |
| 1 | Dilated cloud | Cloud buffered outward |
| 2 | Cirrus | Landsat 8 and 9 only |
| 3 | Cloud | |
| 4 | Cloud shadow | |
| 5 | Snow | |
| 6 | Clear | Set when bits 1 to 4 (and 5 for snow) are not |
| 7 | Water | |
| 8-9 | Cloud confidence | 0 none, 1 low, 2 medium, 3 high |
| 10-11 | Cloud shadow confidence | |
| 12-13 | Snow/ice confidence | |
| 14-15 | Cirrus confidence | Landsat 8 and 9 |

Reading a two-bit field: `conf = (qa >> 8) & 0b11`.

## Sentinel-2 SCL classes

| Value | Class |
| --- | --- |
| 0 | No data |
| 1 | Saturated or defective |
| 2 | Dark area pixels (topographic cast shadows in recent baselines) |
| 3 | Cloud shadows |
| 4 | Vegetation |
| 5 | Not vegetated |
| 6 | Water |
| 7 | Unclassified |
| 8 | Cloud, medium probability |
| 9 | Cloud, high probability |
| 10 | Thin cirrus |
| 11 | Snow or ice |

## Other indices

| Index | Formula | Notes |
| --- | --- | --- |
| SAVI | 1.5 (NIR − Red) / (NIR + Red + 0.5) | Sparse vegetation; L = 0.5 |
| MNDWI | (Green − SWIR1) / (Green + SWIR1) | Water in built-up areas |
| NDBI | (SWIR1 − NIR) / (SWIR1 + NIR) | Built-up; confused with bare soil |
| NDRE | (NIR − RedEdge) / (NIR + RedEdge) | Sentinel-2: (B8A − B05)/(B8A + B05) |
| dNBR | NBR(pre) − NBR(post) | Burn severity from two composites |
