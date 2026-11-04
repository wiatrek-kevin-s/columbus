# ASPRS LAS classification codes and flags

Standard classes for LAS 1.4 point data record formats 6 to 10 (R15). Formats 0 to 5 hold
classes 0 to 31 only, and in LAS 1.1 to 1.3 the meanings of 8 and 12 differed (see notes).

| Code | Meaning | Notes |
| --- | --- | --- |
| 0 | Created, never classified | Raw from the sensor or processing |
| 1 | Unclassified | Processed but not put in another class |
| 2 | Ground | Bare earth; the basis of DTMs |
| 3 | Low vegetation | Height bands are set by the vendor or spec |
| 4 | Medium vegetation | |
| 5 | High vegetation | |
| 6 | Building | |
| 7 | Low point (noise) | Below the true surface: multipath, pits |
| 8 | Reserved | Was "model key-point" (mass point) in LAS 1.1 to 1.3; now the key-point flag |
| 9 | Water | |
| 10 | Rail | |
| 11 | Road surface | |
| 12 | Reserved | Was "overlap points" in LAS 1.1 to 1.3; now the overlap flag |
| 13 | Wire, guard (shield) | |
| 14 | Wire, conductor (phase) | |
| 15 | Transmission tower | |
| 16 | Wire-structure connector (insulator) | |
| 17 | Bridge deck | |
| 18 | High noise | Above the true surface: birds, haze, clouds |
| 19 | Overhead structure | Canopies, signs, conveyors (LAS 1.4 R15) |
| 20 | Ignored ground | USGS 3DEP: ground near breaklines, kept out of the DEM |
| 21 | Snow | USGS 3DEP use |
| 22 | Temporal exclusion | USGS 3DEP: features removed because they changed between collections (for example water levels) |
| 23 to 63 | Reserved | |
| 64 to 255 | User definable | Vendor or project specific; read the project report |

## Flags (LAS 1.4, separate from the class)

| Flag | Meaning | What to do |
| --- | --- | --- |
| Synthetic | Point created by processing, not measured | Usually keep; note it |
| Key-point | Model key-point (thinned ground for contours) | Use for contours if asked |
| Withheld | Should not be used (often noise or edge points) | **Exclude** from analysis: `filters.expression` with `"Withheld == 0"` |
| Overlap | Point in the overlap of flight lines | Exclude for density and some statistics; keep for surfaces if needed |

## USGS 3DEP Lidar Base Specification, required classes (summary)

1 processed but unclassified, 2 bare earth, 7 low noise, 9 water, 17 bridge deck, 18 high
noise, 20 ignored ground. Other classes (buildings, vegetation) are optional and vary by
project.

## Quick PDAL selections

| Want | Stage |
| --- | --- |
| Ground only | `{"type": "filters.range", "limits": "Classification[2:2]"}` |
| Ground and water | `{"type": "filters.range", "limits": "Classification[2:2],Classification[9:9]"}` (same dimension, ORed) |
| Everything except noise | `{"type": "filters.expression", "expression": "Classification != 7 && Classification != 18"}` |
| First returns | `{"type": "filters.range", "limits": "ReturnNumber[1:1]"}` |
| Last returns | `{"type": "filters.expression", "expression": "ReturnNumber == NumberOfReturns"}` |
| Not withheld | `{"type": "filters.expression", "expression": "Withheld == 0"}` |
