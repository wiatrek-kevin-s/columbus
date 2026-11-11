# Data in the pictures

The README's pictures and animation show the Columbus app working on real public data for central Austin, Texas (about 11 by 13 km around downtown, IH 35 and the university). The extract is `app/desktop/e2e/demo/austin.gpkg` (2.7 MB), made by [`fetch_austin.py`](../../app/desktop/e2e/demo/fetch_austin.py) on GitHub's runners (the **README pictures** workflow, `what: data`).

| Layer | What it is | Source | Licence |
|---|---|---|---|
| `roads` | Street centerlines (name, route type, feature class) | US Census Bureau, 2024 TIGER/Line Shapefiles, roads of Travis County, Texas (`tl_2024_48453_roads`) | US government work, not subject to copyright (17 U.S.C. 105); the Census Bureau asks to be cited as the source |
| `census_blocks` | 2020 census blocks, built from TIGER faces | US Census Bureau, 2024 TIGER/Line Shapefiles, faces of Travis County (`tl_2024_48453_faces`) | As above |
| `fatal_crashes` | Fatal traffic crashes, 2020 to 2023 (date, fatalities, location) | National Highway Traffic Safety Administration (NHTSA), Fatality Analysis Reporting System (FARS), accident files 2020–2023 | US government work; listed on data.gov as "available for unrestricted public use" |

Source: U.S. Census Bureau, TIGER/Line Shapefiles; NHTSA, Fatality Analysis Reporting System. Neither agency endorses Columbus.

In the pictures, the street names and crash counts are the data's own. The answers the scripted assistant gives are worked out from this data by [`screenshots.py`](../../app/desktop/e2e/screenshots.py). The coordinate system (Texas Central State Plane, feet) is applied by the script, as an Austin GIS team would store it.

**Made up for the pictures:** the connection names, the Oracle enterprise geodatabase and the ArcGIS Online layer in the connections list (listed only, never reached), the project and the scripts and note in its folder (written by the script; the one that is run counts the real records), and the user folder `/home/analyst`.

The pictures here were taken with the street background off, the app's own default, so none shows a basemap. Taken with it on (the **README pictures** workflow does that), the basemap is © [OpenFreeMap](https://openfreemap.org), © [OpenMapTiles](https://www.openmaptiles.org), data © [OpenStreetMap contributors](https://www.openstreetmap.org/copyright).
