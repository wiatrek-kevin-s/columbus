---
name: linear-referencing
description: Work with linear referencing (LRS) data, meaning routes with measures, point and linear events, mileposts and DFO, calibration and dynamic segmentation, and run QC for gaps, overlaps, reversed measures, events past the route end and recalibration drift. Use when someone mentions route and measure, from/to measures, mile points, event tables, an LRS check or LRS QC, locating assets along routes, or segments that do not line up.
---

# Linear referencing

An LRS locates things by **route and measure** ("CR0012 at mile 3.417") instead of by coordinates. Most LRS mistakes come from three things: units (miles against feet), route ids (padding and prefixes), and calibration (measures that move when routes are recalibrated). Settle all three before you compute anything.

## 1. The parts

| Term | Meaning |
| --- | --- |
| Route | A polyline with an id and measures along it. In GIS it is usually a line with M values (`LINESTRING M`, `polyline (M)`) |
| Measure | A distance along the route from its origin: miles, feet, kilometres, or reference-marker offsets. Also called DFO (distance from origin), mile point or log mile |
| Point event | A row with `ROUTE_ID` and one `MEASURE`: a sign, a crash, a culvert |
| Linear event | A row with `ROUTE_ID`, `FROM_MEAS` and `TO_MEAS`: pavement type, lanes, speed limit, functional class |
| Calibration | Fitting the route's M values to surveyed control points, so a measure means the same place in the field and in GIS |
| Dynamic segmentation | Placing event rows on route geometry at run time, or overlaying several event tables into segments where every attribute is constant |
| LRM | A linear referencing method. One network can carry several (DFO, reference markers, control sections) |

## 2. Before you compute: units, ids, direction, calibration

- **Measure units are not geometry units.** Measures are often in miles while the CRS is in feet or metres. 1 mile = 5,280 feet = 1,609.344 metres. Never compare `ST_Length(geom)` with `TO_MEAS - FROM_MEAS` without converting.
- **Measured length and geometry length differ, and that is normal.** Calibration, curve detail and projection scale mean a route of 4.212 measured miles can have 22,300 ft (4.223 mi) of geometry. Expect differences of a fraction of a percent. Look into larger ones, and do not "fix" small ones.
- **Route ids:** learn the format (a prefix, zero padding, a direction suffix such as `-KG`/`-AG`, or a dual-carriageway code). `'CR12'` and `'CR0012'` will not join. Pad with `LPAD(number, 4, '0')`, and say when you did.
- **Direction:** measures normally increase in the digitized direction. Check with `ST_M(ST_StartPoint(geom)) < ST_M(ST_EndPoint(geom))`. A route digitized against its measures is a finding, not something to quietly reverse.
- **Calibration date:** know when routes were last recalibrated, and when each event table was last located. Events located before a recalibration may have drifted (section 6).
- **Active rows only:** many inventories keep retired segments, such as `STATUS = 'R'`. Filter them out for current-state work.

## 3. Locating events on routes (dynamic segmentation)

PostGIS, with routes carrying M values:

```sql
-- Linear events to line geometry
SELECT e.event_id, ST_LocateBetween(r.geom, e.from_meas, e.to_meas) AS geom
FROM inv.pavement_events e JOIN lrs.routes r ON r.route_id = e.route_id;

-- Point events to points
SELECT s.sign_id, ST_LocateAlong(r.geom, s.measure) AS geom
FROM assets.signs s JOIN lrs.routes r ON r.route_id = s.route_id;

-- A point back to a measure (the M value at the closest point of the route)
SELECT ST_InterpolatePoint(r.geom, c.geom) AS measure
FROM crashes c JOIN lrs.routes r ON r.route_id = c.route_id;
```

Routes without M values: `ST_LineSubstring(geom, from_meas / route_len, to_meas / route_len)` places events proportionally. That ignores calibration, so use it only when the person accepts that, and say so.

In other tools: Oracle `SDO_LRS.LOCATE_PT`, `SDO_LRS.CLIP_GEOM_SEGMENT`, `SDO_LRS.PROJECT_PT` and `SDO_LRS.FIND_MEASURE`. In ArcGIS, the Linear Referencing toolbox: Make Route Event Layer, Locate Features Along Routes, Overlay Route Events, Dissolve Route Events and Calibrate Routes.

**Overlaying two linear event tables** (functional class and lanes, for example) means intersecting their intervals on each route:

```sql
SELECT a.route_id,
       GREATEST(a.from_meas, b.from_meas) AS from_meas,
       LEAST(a.to_meas, b.to_meas)        AS to_meas,
       a.func_class, b.lanes
FROM inv.func_class_events a
JOIN inv.lane_events b
  ON a.route_id = b.route_id
 AND a.from_meas < b.to_meas
 AND b.from_meas < a.to_meas;
```

Afterwards, merge adjacent rows with identical attributes (Dissolve Route Events), or segment counts will grow with every overlay.

## 4. Quality checks on event tables

Run these checks on the active rows, one route at a time, sorted by `FROM_MEAS`:

| Problem | Test | Typical cause |
| --- | --- | --- |
| Missing measure | `FROM_MEAS IS NULL OR TO_MEAS IS NULL` | Incomplete edit |
| Reversed | `FROM_MEAS > TO_MEAS + tol` | Data entry error, or events captured against the route's direction |
| Zero length | `ABS(TO_MEAS - FROM_MEAS) <= tol` on a linear event | A point event stored as linear |
| Out of range | `FROM_MEAS < route_begin - tol` or `TO_MEAS > route_end + tol` | Route shortened or recalibrated; feet stored as miles (values in the thousands) |
| Unknown route | `ROUTE_ID` not in the route table | Padding, retired route, or a typo |
| Gap | The next event starts more than `gap_tol` after the furthest end so far | Missing inventory |
| Overlap | The next event starts more than `overlap_tol` before the furthest end so far | Double entry, or a split not trimmed |

**Compare with the furthest end so far, not just the previous row.** A long event can overlap several later short ones:

```sql
SELECT route_id, from_meas, to_meas, reach,
       CASE WHEN from_meas > reach + 0.005 THEN 'gap'
            WHEN from_meas < reach - 0.001 THEN 'overlap' END AS problem
FROM (
  SELECT route_id, from_meas, to_meas,
         MAX(to_meas) OVER (PARTITION BY route_id ORDER BY from_meas, to_meas
                            ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING) AS reach
  FROM roads.roadway_inventory
  WHERE status = 'A'
) x
WHERE from_meas > reach + 0.005 OR from_meas < reach - 0.001;
```

## 5. Tolerances

- **Use the organization's numbers.** Gap and overlap tolerances are often different. For example, one county flags overlaps over 0.001 mile but gaps only over 0.005 mile.
- **A tolerance is in measure units.** 0.001 mile is 5.28 ft. If measures are in feet, the same tolerance is 5.28, not 0.001.
- **Match the tolerance to the stored precision.** Measures stored to 3 decimal places in miles cannot resolve less than 0.001 mile. A tolerance of 0.0001 would flag rounding.
- **Write the tolerances in every report** ("gaps > 0.005 mi, overlaps > 0.001 mi"), so a finding can be reproduced.

## 6. Recalibration drift

When routes are recalibrated, the same measure can move along the ground. Events located before the recalibration keep their old measures. They drift, usually by a little and sometimes by a lot near realignments.

- Compare each event table's last-located date with the route calibration date.
- To measure drift on point events that have their own geometry, re-derive the measure from the geometry and compare: `ABS(ST_InterpolatePoint(r.geom, s.geom) - s.measure) > tol`.
- Do not "correct" measures by hand or by a blanket shift. Re-locating events is the LRS owner's job (from reference markers, field locations or the vendor's tools). Report the drift, with counts and the largest values.

## 7. Report, do not fix

Measures and route ids usually belong to an LRS team. Produce findings: a layer at the problem locations, and a table of check, route, measures and message. Hand them to the owner. Change event measures only if the person asks, the procedure allows it, and the tool permits edits.

## 8. If the Columbus tools are available

Columbus enforces permissions, row filters, hidden fields and approvals in the server. This section explains how to use its tools for LRS work. It is not a control.

### Read the organization's LRS context first

- `whoami` returns the pack's always-on rules. The Contoso sample says: "Route measures are in miles; GIS geometry lengths are in feet. Convert before comparing."
- `context_search {"query": "route measures", "kind": "lrs"}` finds the `lrs` entries. `context_get {"id": "state-routes"}` returns the body: route id format (a 2-letter prefix plus a 4-digit zero-padded number), measures in miles to 3 decimals, and quarterly recalibration that can shift events up to 0.01 mile.
- `context_search {"query": "lrs qc", "kind": "sop"}` finds the procedure (`monthly-lrs-qc`), with its tolerances and the rule "Do not fix measures yourself."
- `describe_dataset` attaches the dataset's entry, including keys such as `[ROUTE_ID, FROM_MEAS, TO_MEAS]` and gotchas. `describe_data_group {"id": "live-grid-linework"}` lists the datasets, joins and documents for the job, and whether each dataset is `usable_here`.
- Act on the warnings: `context.stale` (entry past the review interval), `context.draft` and `context.retired`. In the sample, `asset-signs` was last confirmed in 2025-06 and says measures "can lag route recalibration by up to a month".

### Run the LRS checker plugin

The checker is an engine plugin, called through `run_tool` as `contoso.lrs-check.check_events`. A person must have approved it (`columbus-mcp plugins approve contoso.lrs-check`), and any change to it switches it off until it is approved again.

```json
{"name": "contoso.lrs-check.check_events",
 "args": {
   "events": {"connection": "roads-prod", "dataset": "ROADS.ROADWAY_INVENTORY", "where": "STATUS = 'A'"},
   "routes": "roads-prod:ROADS.ROUTE_CENTERLINES",
   "overlap_tolerance": 0.001, "gap_tolerance": 0.005, "measure_unit": "mile"}}
```

The connection and table names come from the Contoso sample pack. Use the names `list_datasets` shows on your connection.

- **Dataset arguments** take a string `"<connection>:<dataset>"`, or an object `{connection, dataset, where}`. The gateway reads them through the connection's permissions, row filter and hidden fields, and hands the plugin a read-only extract. `where` uses the simple filter grammar: comparisons, AND/OR/NOT, IN, LIKE, BETWEEN, IS NULL and simple functions, with no subqueries.
- **The read limit is a hard stop.** If a dataset has more rows than the connection's limit (200,000 by default), the call is refused: "... a plugin is never given a cut-off dataset. Narrow it with "where"". Split the job by route prefix or district.
- **Tolerances are in miles**, from 0 to 1, whatever the data's measure unit; `measure_unit` (`mile` default, `foot`, `kilometer`, `meter`) says what the measures are in, and the plugin converts. So with measures in feet, pass `"measure_unit": "foot"` and still write 0.001 for 0.001 mile (5.28 ft). A value outside 0 to 1 is refused ("tolerance is out of range"), and so is an unknown argument ("... has no parameter ...").
- **`tolerance`** (default 0.001) applies to reversed and out-of-range checks, and to gaps and overlaps unless `gap_tolerance` or `overlap_tolerance` is given. Use the procedure's own numbers, for example `"overlap_tolerance": 0.001, "gap_tolerance": 0.005`. The result's `tolerances_mi` echoes what was used; quote it in the report.
- **Column names are matched case-insensitively**, from fixed lists, or named with `route_field`, `from_field` and `to_field` (the events dataset's columns). Events need a route id (`route_id`, `rte_id`, `routeid`, `route`), a from measure (`from_meas`, `from_measure`, `begin_meas`, `bmp`, `frm_meas`) and a to measure (`to_meas`, `to_measure`, `end_meas`, `emp`). Routes need a route id and an end measure (`to_meas`, `end_meas`, `max_meas`, `length_mi`, `route_length`, `emp`); the begin measure is optional (`from_meas`, `begin_meas`, `min_meas`, `bmp`) and defaults to 0. A table that does not match fails with, for example, "no route end measure column; expected one of ...". Routes that carry measures only as M values in their geometry do not work.
- **A route on several rows** (one per section) spans from its lowest begin to its highest end measure. Gaps between its sections are not checked; the events are.
- **When the plugin does not fit** (other column names, or routes measured only by M), the gap and overlap query in section 4 passes `sql_select {connection, sql}` as written, and the server adds the row filter. Out-of-range checks need the route end measures, which you get with `ST_M(ST_EndPoint(geom))` on PostGIS.
- **Reading Oracle and SQL Server connections is not supported yet.** The call is refused with "reading oracle connections is not supported yet". Today it works on PostGIS, GeoPackage and SQLite connections.

### What it returns

- `result`: `{provider: "contoso.lrs-check", tool: "check_events", status: "done", summary: {events_checked, routes, findings}, saved: [...]}`. Each saved file has a `ws://` handle, `bytes`, `kind` and `path`.
- **`findings`** is a GeoJSON point layer, one feature per problem, at the first vertex of the event's geometry (no geometry if the events have none). Its properties are `check`, `route_id`, `from_meas`, `to_meas` and `message`.
- **`report`** is a CSV table with columns `check, severity, route_id, from_meas, to_meas, message`. For `lrs.gap` rows, `from_meas` and `to_meas` are where the gap starts and ends.
- **Checks**, one per kind found, with `detail: {count}`:

| Check id | Severity | Meaning |
| --- | --- | --- |
| `lrs.missing_measure` | fail | From or to measure is null |
| `lrs.reversed` | fail | From measure after to measure (beyond tolerance) |
| `lrs.unknown_route` | fail | The route id is not in the routes dataset |
| `lrs.out_of_range` | fail | Outside the route's begin and end measures |
| `lrs.overlap` | warn | Starts before the furthest end of earlier events on the route |
| `lrs.gap` | warn | Starts after the furthest end of earlier events on the route |
| `flow.labels` | info | The outputs carry data labels (such as `connection:roads-prod`), so sending them elsewhere may need approval |
| `verify.pending` | info | The plugin's declared checks (`geometry.valid`, `lrs.measures`) are not yet run by Columbus |

- `provenance` is the id of a provenance record: inputs, the exact SQL executed for each extract, the plugin version and hash, and the tolerance.

### After the run

- `verify {"layer": "<findings handle>"}` runs Columbus's spatial checks on the findings layer.
- Summarize by check and route, with the tolerances used. Hand the layer and the table to the data's owner, following the SOP. Do not edit measures. Sending files to a connected server is subject to the organization's flow rules and may need the person's approval.
