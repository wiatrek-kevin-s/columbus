---
id: monthly-lrs-qc
kind: sop
title: Monthly LRS quality check
summary: Check the roadway inventory's events against route centerlines for gaps, overlaps and measures off the route.
when_to_use: The first week of each month, or after a recalibration.
owner: GIS Data Management
last_confirmed: 2026-10-01
status: reviewed
source: { file: sources/LRS QC procedure v4.docx, section: "2. Monthly check" }
links: [roadway-inventory, route-centerlines, state-routes]
---

1. Take active segments only (STATUS = 'A').
2. For each route, sort segments by FROM_MEAS and check that each segment starts where the previous one ended, within 0.001 mile.
3. Flag overlaps longer than 0.001 mile and gaps longer than 0.005 mile.
4. Flag any TO_MEAS beyond the route's end measure.
5. Send the findings layer and a summary table to Data Management. Do not fix measures yourself.
