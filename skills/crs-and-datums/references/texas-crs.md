# Texas coordinate reference systems

Names and units as the EPSG registry (via PROJ) gives them. "ftUS" is the US survey foot
(1200/3937 m); "m" is metres.

## Statewide

| Projection | NAD83 | NAD83(HARN) | NAD83(NSRS2007) | NAD83(2011) | Units |
| --- | --- | --- | --- | --- | --- |
| Texas State Mapping System (Lambert conformal conic) | 3081 | - | - | - | m |
| Texas Centric Lambert Conformal | 3082 | 3084 | 3666 | 6580 | m |
| Texas Centric Albers Equal Area | 3083 | 3085 | 3665 | 6579 | m |

Use 3081 or 3082 for statewide distances and maps, and 3083 (or 6579) for statewide areas.
EPSG:3081 and EPSG:3082 are different projections, even though both are Lambert conformal
and both are called "Texas-centric" in conversation.

## State plane zones

| Zone | NAD83 ftUS | NAD83 m | NAD83(HARN) ftUS | NAD83(2011) ftUS | NAD83(2011) m |
| --- | --- | --- | --- | --- | --- |
| North | 2275 | 32137 | 2916 | 6582 | 6581 |
| North Central | 2276 | 32138 | 2917 | 6584 | 6583 |
| Central | 2277 | 32139 | 2918 | 6578 | 6577 |
| South Central | 2278 | 32140 | 2919 | 6588 | 6587 |
| South | 2279 | 32141 | 2920 | 6586 | 6585 |

Which zone (counties per EPSG's area of use, examples only):

| Zone | Includes |
| --- | --- |
| North | Amarillo (Potter, Randall), the northern Panhandle |
| North Central | Dallas, Tarrant, Collin, Denton, Lubbock, Wichita, Bowie |
| Central | Travis (Austin), Williamson, Bell, McLennan, Brazos, El Paso, Midland, Ector, Montgomery |
| South Central | Bexar (San Antonio), Harris (Houston), Hays, Comal, Fort Bend, Galveston, Victoria |
| South | Nueces (Corpus Christi), Cameron, Hidalgo, Webb |

Hays County, just south of Austin, is in South Central, not Central. Check the county when
work crosses a zone line, or use a statewide CRS.

Compound CRSs with NAVD88 heights in feet also exist: 8779 North, 8780 North Central,
8781 Central, 8782 South Central, 8783 South (all NAD83 ftUS + NAVD88 height (ftUS)).

NAD27 zones (legacy data only, US survey feet): 32037 North, 32038 North Central,
32039 Central, 32040 South Central, 32041 South.

## UTM

| Zone | Longitudes | NAD83 | NAD83(2011) | WGS 84 |
| --- | --- | --- | --- | --- |
| 13N | 108° W to 102° W (far west Texas, El Paso) | 26913 | - | 32613 |
| 14N | 102° W to 96° W (most of Texas, including Austin and San Antonio) | 26914 | 6343 | 32614 |
| 15N | 96° W to 90° W (east Texas, Houston) | 26915 | 6344 | 32615 |

## Geographic and other

| Code | Name | Notes |
| --- | --- | --- |
| 4326 | WGS 84 | Degrees, axis order lat/lon officially; GPS, web APIs |
| 4269 | NAD83 | Degrees |
| 4152 | NAD83(HARN) | Degrees |
| 6318 | NAD83(2011) | Degrees (2D); 6319 is the 3D version |
| 3857 | WGS 84 / Pseudo-Mercator | Web tiles only; never measure in it |
| 5070 | NAD83 / Conus Albers | Equal-area, multi-state; 6350 is the NAD83(2011) version |
| 5703 | NAVD88 height | Metres; 8228 is NAVD88 height (ft) |

## Austin, one point in several CRSs

Texas State Capitol area (-97.7431, 30.2672), for checking that numbers look right:

| CRS | X | Y |
| --- | --- | --- |
| EPSG:4326 (lon, lat order) | -97.7431 | 30.2672 |
| EPSG:2277 Texas Central (ftUS) | about 3,114,226 | about 10,070,436 |
| EPSG:3081 Texas State Mapping System (m) | about 1,216,724 | about 902,700 |
| EPSG:32614 UTM 14N (m) | about 620,904 | about 3,349,062 |
