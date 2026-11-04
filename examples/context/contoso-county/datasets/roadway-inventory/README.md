---
id: roadway-inventory
kind: dataset
title: Roadway inventory
summary: One row per roadway segment on the county system, with functional class and lanes, located by route and measure.
connection: roads-prod
physical: ROADS.ROADWAY_INVENTORY
geometry: polyline (M)
crs: EPSG:3081
keys: [ROUTE_ID, FROM_MEAS, TO_MEAS]
fields:
  FUNC_CLASS: { alias: Functional class, type: smallint, domain: functional-class }
  CSJ:        { alias: Control-section-job, type: char(9), note: always 9 characters, zero-padded }
  STATUS:     { alias: Status, type: char(1), note: "A active, R retired" }
updated: nightly from the LRS edit environment
owner: GIS Data Management
last_confirmed: 2026-10-15
status: reviewed
links: [state-routes, functional-class]
---

## Gotchas

- Measures are in miles; the GIS layer's length is in feet.
- Retired segments stay in the table with STATUS = 'R'; filter them out for current-state work.
- CSJ values from spreadsheets often lose their leading zeros; pad to 9 before joining.
