---
name: spatial-sql
description: Write correct, fast, read-only spatial SQL for PostGIS, Oracle Spatial (SDO_RELATE, SDO_WITHIN_DISTANCE) and SQL Server (STIntersects, geography), covering intersects, within a distance, nearest, bounding-box filters, areas and lengths. Use when asked to query a spatial database, find features inside or near something, write or fix a spatial SQL query, or when a query is slow, gives distances in the wrong units, or is refused by sql_select.
---

# Spatial SQL

The aim is spatial SQL that gives the right answer and does not hurt a shared database. Sections 1 to 6 apply to any database. Section 7 covers Columbus.

## 1. Know the table before you query it

Answer these five questions first. Most wrong answers come from guessing one of them.

1. What is the geometry column called, and what type is it (point, line, polygon, multi-, with M or Z)?
2. What is its SRID, and what are that CRS's units: feet, metres or degrees?
3. Does the geometry column have a spatial index?
4. Roughly how many rows are there?
5. What do real values look like (codes, padding, nulls, case)?

```sql
-- PostGIS
SELECT ST_SRID(geom), GeometryType(geom), COUNT(*) FROM transportation.roadway_inventory GROUP BY 1, 2;
SELECT * FROM transportation.roadway_inventory LIMIT 20;

-- Oracle
SELECT t.geom.SDO_SRID, t.geom.SDO_GTYPE, COUNT(*) FROM roads.roadway_inventory t
GROUP BY t.geom.SDO_SRID, t.geom.SDO_GTYPE;

-- SQL Server
SELECT geom.STSrid, geom.STGeometryType(), COUNT(*) FROM roads.roadway_inventory
GROUP BY geom.STSrid, geom.STGeometryType();
```

Without Columbus, the catalogs tell you about indexes: `pg_indexes` (PostgreSQL), `USER_SDO_INDEX_INFO` and `USER_SDO_GEOM_METADATA` (Oracle), `sys.spatial_indexes` (SQL Server).

## 2. Read-only etiquette

- **Count before you fetch.** Run `SELECT COUNT(*) ... WHERE ...` first. If it says 2 million rows, rethink the query instead of pulling them.
- **Sample first.** Look at 20 to 50 rows before you write filters. Codes like `'01'` and `1`, or `'CR0012'` and `'CR12'`, are the usual reason a filter returns nothing.
- **Do not `SELECT *` on big tables.** Geometry is usually most of the bytes. Name the columns you need, and leave the geometry out unless you need it.
- **Always cap the result.** Use `LIMIT n` (PostgreSQL, SQLite), `FETCH FIRST n ROWS ONLY` (Oracle 12c and later) or `TOP (n)` (SQL Server).
- **Aggregate in the database.** `SELECT district, SUM(ST_Length(geom)) ... GROUP BY district` is far cheaper than fetching every line and adding them up yourself.
- **Simplify geometry you only display.** For a map preview, `ST_AsGeoJSON(ST_Simplify(geom, 10))` is enough. Never simplify geometry you will measure.
- **Use a read-only login and a timeout.** In plain PostgreSQL, run `SET statement_timeout = '60s'` first.
- **Test on a small area first.** Try one district or one bounding box, then widen it.

## 3. Write filters the spatial index can use

A spatial index answers one question: do the bounding boxes meet? A filter can use the index when the indexed column appears bare (not wrapped in a function) and is compared with a shape.

| Use this (index) | Not this (scans every row) | Why |
| --- | --- | --- |
| `geom && ST_MakeEnvelope(3110000, 10060000, 3125000, 10075000, 2277)` | `ST_XMin(geom) > 3110000 AND ...` | `&&` compares bounding boxes through the index |
| `ST_Intersects(geom, :area)` | `ST_Intersection(geom, :area) IS NOT NULL` | `ST_Intersects` adds the `&&` step by itself |
| `ST_DWithin(geom, :pt, 500)` | `ST_Distance(geom, :pt) < 500` | `ST_Distance` is computed for every row |
| `geom && ST_Transform(:box_4326, 2277)` | `ST_Transform(geom, 4326) && :box_4326` | Transform the one constant, not the column on every row |
| `ST_DWithin(geom, :pt, 30)` | `ST_Intersects(ST_Buffer(geom, 30), :pt)` | A buffer per row is slow and has no index |
| `ORDER BY geom <-> :pt LIMIT 5` | `ORDER BY ST_Distance(geom, :pt) LIMIT 5` | `<->` is the index-assisted nearest-neighbour operator |

What this looks like in PostgreSQL 16 with PostGIS on a 400,000-polygon table:

```text
EXPLAIN SELECT fid FROM roads.parcels p WHERE p.geom && ST_MakeEnvelope(1000,1000,3000,3000,2277);
  Bitmap Heap Scan on parcels p
    ->  Bitmap Index Scan on parcels_geom_idx          <- index used

EXPLAIN SELECT fid FROM roads.parcels p WHERE ST_Transform(p.geom, 4326) && ST_MakeEnvelope(-97.8,30.2,-97.7,30.3,4326);
  Parallel Seq Scan on parcels p                       <- every row reprojected
```

**Filter on the box first, then test exactly.** `ST_Intersects`, `ST_Contains`, `ST_Within`, `ST_Covers` and `ST_DWithin` already do this. For `ST_Relate(a, b, 'T*F**F***')` or a `ST_Distance` comparison, add `a.geom && b.geom` (or `ST_DWithin`) yourself.

**"Not inside" cannot use an index.** `NOT ST_Intersects(...)` and `ST_Disjoint(...)` test every pair. Write an anti-join instead:

```sql
SELECT s.sign_id
FROM assets.signs s
LEFT JOIN transportation.roadway_inventory r ON ST_DWithin(s.geom, r.geom, 50)
WHERE r.fid IS NULL;          -- signs more than 50 ft from any roadway
```

**Oracle:** `SDO_RELATE`, `SDO_ANYINTERACT`, `SDO_FILTER`, `SDO_WITHIN_DISTANCE` and `SDO_NN` are index operators. The indexed column goes first, and you compare the result with `'TRUE'`. Without a spatial index they fail with `ORA-13226: interface not supported without a spatial index`. `SDO_GEOM.RELATE` and `SDO_GEOM.SDO_DISTANCE` are plain functions with no index, so keep them out of the WHERE clause on big tables.

**SQL Server:** the index is used only when the method is called on the indexed column and compared in the WHERE clause, as in `WHERE t.geom.STIntersects(@area) = 1` or `WHERE t.geom.STDistance(@pt) <= 500`. `t.geom.Filter(@area) = 1` is the index-only (bounding-box) test. If the optimizer ignores the index, try `WITH (INDEX(ix_roadway_geom))`.

## 4. Geometry or geography, units, and the cost of ST_Transform

- **Units come from the SRID.** EPSG:4326 is in degrees. EPSG:2277 (NAD83 / Texas Central) is in US survey feet. EPSG:3081 (Texas Centric Lambert Conformal) and UTM zones are in metres. `ST_Length` on EPSG:4326 lines returns degrees, which mean nothing.
- **Failure example:** `ST_DWithin(geom_4326, :pt, 100)` means "within 100 degrees", which covers most of a hemisphere. Either query in the data's projected CRS, or use geography: `ST_DWithin(geom::geography, :pt::geography, 100)` is 100 metres.
- **Geography is accurate over long distances, in metres, and slower.** Casting the column (`geom::geography`) in a WHERE clause cannot use a geometry index. That needs an index on the expression or a geography column.
- **Transform constants, not columns.** Put `ST_Transform` on the search shape in the WHERE clause. Transform output only in the SELECT list, after filtering, on the rows you return.
- **Mixed SRIDs fail loudly in PostGIS:** `ST_Intersects: Operation on mixed SRID geometries (Polygon, 2277) != (Point, 4326)`. If the constant has no SRID but you know its coordinates are in the column's CRS, use `ST_SetSRID`. If they are in another CRS, use `ST_Transform`. `ST_SetSRID` only relabels a geometry. It never moves it, so it cannot fix data in the wrong CRS.
- **SQL Server has no reprojection** in the database. Reproject before loading, or outside SQL. Note that `geography::Point(lat, lon, 4326)` takes latitude first, while WKT is always `x y` (longitude latitude).
- **Oracle** takes units in the parameter string (`'distance=500 unit=FOOT'`). Without a unit, it uses the CRS's unit, which is metres for geodetic data. `SDO_GEOM` functions need a tolerance in the data's units, such as `0.005` (5 mm) for geodetic data.

## 5. The same question in three dialects

Within 500 feet of a point (data in EPSG:2277):

```sql
-- PostGIS
SELECT sign_id FROM assets.signs
WHERE ST_DWithin(geom, ST_SetSRID(ST_MakePoint(3117500, 10067200), 2277), 500)
LIMIT 100;

-- Oracle
SELECT s.sign_id FROM assets.signs s
WHERE SDO_WITHIN_DISTANCE(s.geom,
        SDO_GEOMETRY(2001, 2277, SDO_POINT_TYPE(3117500, 10067200, NULL), NULL, NULL),
        'distance=500 unit=FOOT') = 'TRUE'
FETCH FIRST 100 ROWS ONLY;

-- SQL Server
DECLARE @pt geometry = geometry::Point(3117500, 10067200, 2277);
SELECT TOP (100) sign_id FROM assets.signs WHERE geom.STDistance(@pt) <= 500;
```

For intersects, nearest, area and length, validity, WKT and GeoJSON output, row limits and SQLite/GeoPackage notes, see [references/dialects.md](references/dialects.md).

## 6. Failures and what they usually mean

| Symptom | Likely cause | Fix |
| --- | --- | --- |
| Zero rows from a spatial filter | Search shape in another CRS (degrees against feet), or X and Y swapped | Check `ST_SRID` on both sides, and transform the constant |
| Distances like `0.0021` | Measuring in degrees | Use a projected CRS or geography |
| Areas about 10.76 times too big or too small | Square feet against square metres | Read the CRS units, then convert |
| Query runs for minutes | Function on the indexed column, a `NOT ST_Intersects`, or a cross join without `&&`/`ST_DWithin` | See section 3 |
| `ORA-13226` | Oracle index operator on a column with no spatial index | Use an indexed column, or ask the DBA |
| `ORA-13295` / mixed SRID error | Geometries in different coordinate systems | Transform the constant to the column's SRID |
| SQL Server: "An expression of non-boolean type specified in a context where a condition is expected" | `WHERE geom.STIntersects(@g)` without `= 1` | Write `WHERE geom.STIntersects(@g) = 1` |
| SQL Server: zero rows, no error | `@g` has a different SRID from the column, so the method returns NULL | Build `@g` with the column's SRID (`STSrid`) |
| `TopologyException` / invalid geometry errors | Self-intersecting polygons | Find them with `ST_IsValid`. Fix them in a copy with `ST_MakeValid`, never silently |

## 7. If the Columbus tools are available

Columbus is a governed MCP server. **The server enforces the rules; this section only explains them** so you can work with them. Nothing here is a control. If the server refuses something, do not look for a way around it. Tell the person what was refused and why.

### Pick the right tool

1. `whoami` shows your permissions, connections and engines. `list_datasets {connection}` and `describe_dataset {connection, dataset}` give fields, geometry column, CRS facts (code, units, projected or not), extent, the row count under the row filter, and the context pack's entry for the dataset.
2. `sample_rows {connection, dataset, n}` returns up to 50 real rows. Use it instead of `TABLESAMPLE` or `ORDER BY random()`.
3. **`query_features` is usually the better choice** for "features from one dataset that match a filter and/or a box". Its arguments are `{connection, dataset, fields, where, bbox, bbox_crs, order_by, limit}`. `bbox` is `[min x, min y, max x, max y]`, and `bbox_crs` (for example `"EPSG:4326"`) lets you give the box in lon/lat while Columbus moves it into the data's CRS. `where` allows only comparisons, AND/OR/NOT, IN, LIKE, BETWEEN, IS NULL and simple functions (UPPER, TRIM, LPAD, COALESCE, TO_DATE, and so on). The result is saved as a layer, and you get a `ws://` handle for `vector_op` and `verify`, with CRS and geometry checks attached.
4. `vector_op` covers buffer, clip, intersect, dissolve, join and `spatial_join` (`params: {predicate: intersects|within|contains|touches|crosses|overlaps, how}`). It refuses to buffer or simplify in degrees (check `crs.measure`) and names the projected CRS to use instead.
5. `sql_select {connection, sql}` is for what the others cannot express: aggregates, joins across tables, window functions, distance and nearest-neighbour queries.

The `bbox` in `query_features` is applied by Columbus's engine to the rows it reads. The database's spatial index does not do it. On a very large table, add a `where` on an attribute such as district or county as well.

### What sql_select accepts

- The SQL must parse, in the connection's dialect, as **exactly one `SELECT`**. `WITH`, `SELECT INTO`, `FOR UPDATE`, writes and second statements are refused.
- **Every table must be on the connection's allow list**, including tables in subqueries. Name tables as `list_datasets` shows them. A bare name that exists in two schemas is refused with "could mean more than one table; qualify it with its schema".
- **System catalogs are refused**: `pg_catalog`, `information_schema`, `sys`, Oracle's `DBA_`/`ALL_`/`V$` views and `sqlite_master`. Use `list_datasets` and `describe_dataset` instead.
- **FROM takes plain tables, subqueries and joins only.** `FROM generate_series(...)` and `FROM ST_Dump(...)` are refused. Put set-returning functions in the select list instead: `SELECT fid, (ST_Dump(geom)).geom FROM ...`.
- **Functions come from an allow list**: ordinary scalar, aggregate and window functions (COUNT, SUM, LAG, ROW_NUMBER, COALESCE, LPAD, DATE_TRUNC, STRING_AGG, and so on), plus PostGIS functions. On PostgreSQL, a spatial function counts only if the PostGIS extensions installed it. A function in another schema, or one the database owner wrote, is refused even if it is called `st_something`. `ST_EstimatedExtent`, `ST_Find_Extent`, `postgis_*` and `_postgis_*` are refused. Use `SELECT ST_Extent(geom) ...` or the extent from `describe_dataset` instead.
- **Operators and casts:** the standard operators are fine, including `&&`, `<->` and `~`. Custom operators defined in the database are refused. Casts may name built-in types plus `geometry`, `geography`, `box2d`, `box3d` and `regclass`, so `geom::geography` and `geom::geometry(Polygon, 2277)` both work. `CAST(x AS roads.my_type)` is refused.
- **Positional `GROUP BY 1` and `ORDER BY 2 DESC` are fine.**

### The server adds the row filter, and what that costs

Columbus rewrites every table reference that has a row filter or hidden fields into a derived table, `FROM (SELECT <visible fields> FROM t WHERE <row filter> OFFSET 0) t`; the `OFFSET 0` is added only when there is a row filter. It also wraps the whole query with its own row limit plus one sentinel row, so that a truncated result is reported rather than returned as if it were complete.

- **Do not repeat the row filter** in your query. The result's `query.row_filter` check shows the filter that was applied.
- **With a row filter, `OFFSET 0` blocks the database from merging the filtered table into your query** (on purpose: your conditions only ever see allowed rows). As a result, on PostgreSQL **your own spatial conditions cannot use the spatial index** on a row-filtered table. The plan reads every row the row filter allows (through the filter column's index, if there is one) and then tests your `&&`, `ST_Intersects` or `<->` on each row. Tested on PostgreSQL 16: the same `&&` query took about 1 ms unwrapped and about 60 to 90 ms wrapped on 400,000 rows. Keep writing index-friendly SQL anyway, because it is still cheaper per row and correct everywhere. Just expect cost to grow with the size of the row-filtered set. If a query hits the timeout, narrow it with attributes, or ask the person whether a narrower connection or data group exists.
- When a connection has no row filter, the table is read directly (by its full name, or as a plain column list when fields are hidden) and the database can merge it into your query, so indexes work as usual.

### Hidden fields

- **`SELECT *` is fine.** Hidden fields are simply not in the derived table, and the result's `policy.hidden_fields` check names them.
- **Naming a hidden field anywhere is refused**, whether in the select list, WHERE, ORDER BY, a join or a subquery: `field owner_name is hidden by organization policy and cannot be used in a query`. Leave it out. Do not try to reach it another way, and tell the person if the task needs it.

### Results and checks

- At most 500 rows come back per call (organization policy can set fewer). Larger results are saved as a CSV in the session workspace, and its path is returned.
- Your own `LIMIT` is kept. The server's limit is the connection's read limit (`limits.rows`, 200,000 by default).
- Read the checks: `query.rows` (rows read and returned), `query.row_filter`, `filter.zero_rows` (often wrong case, lost zero padding or the row filter), `query.read_limit` (stopped at the limit, so the result is incomplete and must not be presented as whole), and `policy.hidden_fields`.

### Engines today

- **PostGIS** runs everything above.
- **GeoPackage and SQLite** files are opened with plain SQLite with extensions off, so `sql_select` refuses spatial functions on them: `function ST_AREA is not available: spatial SQL functions do not run on GeoPackage or SQLite connections`. Use `sql_select` for attribute questions on these files, and `query_features` (bbox) and `vector_op` for spatial ones.
- **Oracle** runs everything above in Oracle's own SQL. Names are upper case unless quoted (`ROADS.ROADS`, as `list_datasets` shows them); `FETCH FIRST n ROWS ONLY` caps results. Only Oracle's built-in functions are allowed: a name Oracle lacks, such as `SPLIT_PART` or `MD5`, is refused, because Oracle would resolve it to whatever function or synonym someone created. Oracle Spatial works through `SDO_RELATE`, `SDO_FILTER`, `SDO_WITHIN_DISTANCE`, `SDO_GEOM.*` (`SDO_GEOM.SDO_LENGTH(geom, 0.005)`, `SDO_GEOM.RELATE`), `SDO_UTIL.TO_WKTGEOMETRY` and the `SDO_GEOMETRY` constructors; the server calls them in `MDSYS`. `ST_*` names are refused ("spatial SQL there is Oracle Spatial's"). On a row-filtered table the index operators fail with `ORA-13226` (the filtered table is not the indexed one): use `SDO_GEOM.RELATE` or `SDO_GEOM.SDO_DISTANCE`, or `query_features` with a bbox. Object methods such as `t.geom.GET_WKT()` are refused; use `SDO_UTIL.TO_WKTGEOMETRY(geom)`.
- **SQL Server** runs attribute SQL in T-SQL: `TOP (n)`, `CAST(x AS INT)`, `IIF`, `STRING_AGG` and the other built-ins. Geometry methods (`t.geom.STIntersects(...)`, `geom.STLength()`) and static calls (`geometry::Point(...)`) are refused: the parser cannot tell `col.Method()` from a call to the database's own `schema.function()`. Geometry still comes back in results (as shapes in `query_features` layers). Plan spatial work on SQL Server through `query_features` (bbox) and `vector_op`.

### Refused forms and what to do instead

| You wrote | Server says | Do instead |
| --- | --- | --- |
| `WITH a AS (SELECT ...) SELECT ... FROM a` | WITH (common table expressions) is not supported; use a subquery in FROM | `SELECT ... FROM (SELECT ...) a` |
| `SELECT ...; SELECT ...` | expected exactly one statement, found 2 | One call per statement |
| `SELECT now()` with no table | query reads no tables | Select from a dataset |
| `SELECT * FROM information_schema.columns` | dataset "information_schema.columns" is not in this connection's allowed datasets | `describe_dataset` |
| `SELECT owner_name FROM ...` (a hidden field) | field owner_name is hidden by organization policy ... | Leave it out; `SELECT *` is fine |
| `SELECT roads.st_area(geom) ...` | function ROADS.ST_AREA is not allowed ... | Call `ST_Area` unqualified |
| `... TABLESAMPLE SYSTEM (1)` | Refused: `TABLESAMPLE is not allowed` | `sample_rows` |

More refusals, with exact messages, are in [references/columbus-sql-refusals.md](references/columbus-sql-refusals.md).
