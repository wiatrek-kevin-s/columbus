# sql_select: what is refused, the message, and what to do

These messages come from Columbus's query checker (`crates/shared/src/sqlcheck.rs`), checked against a PostGIS connection with a row filter and hidden fields. The server prefixes them with `query refused:` or reports `dataset "..." is not in this connection's allowed datasets`. A refusal is final for that query. Change the query, and do not try to get around the rule behind it.

## Statement shape

| You wrote | Message | Do instead |
| --- | --- | --- |
| `WITH a AS (SELECT * FROM roads.parcels) SELECT * FROM a` | WITH (common table expressions) is not supported; use a subquery in FROM | `SELECT * FROM (SELECT ... FROM roads.parcels) a` |
| `SELECT fid FROM roads.parcels; SELECT 1 FROM roads.signs` | expected exactly one statement, found 2 | Two calls |
| `SELECT 1` | query reads no tables | Every query reads at least one allowed table |
| `SELECT * INTO roads.copy FROM roads.parcels` | SELECT INTO is not allowed | Large results are saved to the workspace for you |
| `SELECT * FROM roads.parcels FOR UPDATE` | locking clauses (FOR UPDATE) are not allowed | Drop the clause |
| `UPDATE` / `DELETE` / `CREATE` | only SELECT is allowed, not UPDATE | Columbus is read-only here; edits are not an agent action |
| Text that does not parse in the dialect | could not parse SQL: ... | Write it in the connection's dialect. Nothing unparsed is ever passed through |

## Tables

| You wrote | Message | Do instead |
| --- | --- | --- |
| `FROM information_schema.columns`, `FROM pg_catalog.pg_class`, `sqlite_master`, Oracle `ALL_TABLES` | dataset "information_schema.columns" is not in this connection's allowed datasets | `list_datasets`, `describe_dataset` |
| `FROM geometry_columns` (unless the connection lists it) | not in this connection's allowed datasets | `describe_dataset` gives geometry column, type and CRS |
| `FROM county` when `roads.county` and `public.county` both exist | county could mean more than one table; qualify it with its schema | `FROM roads.county` |
| `FROM gis.roads.roadway_inventory` (a three-part name) | not in this connection's allowed datasets | Name it exactly as `list_datasets` shows it |
| A table that is denied (`*_audit`) | not in this connection's allowed datasets | Quoting or changing case does not change the answer. The rule applies to the table the database would read |
| `FROM generate_series(1, 10)`, `FROM ST_Dump(geom)` | dataset "generate_series" is not in this connection's allowed datasets (FROM takes plain tables, subqueries and joins) | Put the function in the select list: `SELECT fid, (ST_Dump(geom)).geom FROM roads.parcels` |
| `FROM unnest(ARRAY[1,2])` | `UNNEST(ARRAY[1, 2])` is not allowed in FROM; use plain tables | `SELECT unnest(...) FROM <table>` in the select list |
| `FROM t TABLESAMPLE SYSTEM (1)` | TABLESAMPLE is not allowed; use sample_rows, or LIMIT with ORDER BY | `sample_rows {connection, dataset, n}` (up to 50 rows) |

## Functions

Allowed: ordinary scalar, aggregate and window functions on the checker's list (counts, sums, averages, percentiles, `LAG`/`LEAD`/`ROW_NUMBER`, text functions such as `LPAD`, `TRIM`, `SPLIT_PART`, `REGEXP_REPLACE`, date functions such as `DATE_TRUNC` and `EXTRACT`, and JSON builders such as `JSON_BUILD_OBJECT` and `TO_JSONB`), plus PostGIS functions the PostGIS extensions installed.

| You wrote | Message | Do instead |
| --- | --- | --- |
| `ST_EstimatedExtent('roads','parcels','geom')` | function ST_ESTIMATEDEXTENT is not allowed (...) | `SELECT ST_Extent(geom) FROM roads.parcels` (exact, and under the row filter), or the extent in `describe_dataset` |
| `postgis_full_version()`, `_postgis_stats(...)` | function ... is not allowed | Ask the person, or check `whoami` |
| `roads.st_area(geom)` | function ROADS.ST_AREA is not allowed | `ST_Area(geom)`. Columbus calls it from the PostGIS schema itself |
| `st_helper_summary()` written by the database owner | function ST_HELPER_SUMMARY is not allowed | No workaround. Only the extension's own functions count |
| `roads.log_visit()` or any schema-qualified function outside `pg_catalog` | function ROADS.LOG_VISIT is not allowed | None |
| `query_to_xml(...)`, `table_to_xml(...)`, `ts_stat(...)`, `dblink(...)`, `pg_read_file(...)`, `current_setting(...)`, `load_extension(...)` | function ... is not allowed | None. These read outside every check |
| Oracle `SDO_RELATE(...)`, `SDO_WITHIN_DISTANCE(...)`, `SDO_GEOM.SDO_AREA(...)` | function SDO_RELATE is not allowed (...) | `query_features` with `bbox`; `vector_op` (`spatial_join`, `buffer`, `clip`) |
| SQL Server `p.geom.STIntersects(@g) = 1`, `p.geom.STArea()` | function P.GEOM.STINTERSECTS is not allowed (...) | Same as Oracle |
| SQL Server `geography::Point(30.27, -97.74, 4326)` | could not parse SQL: ... | Same as Oracle |

## Operators and casts

| You wrote | Result |
| --- | --- |
| `geom && box`, `geom <-> pt`, `route_id ~ '^CR'`, `route_id ILIKE 'cr%'` | Allowed |
| `route_id ~~~ 'x'` (a custom operator) | operator in `...` is not allowed; use the standard operators |
| `geom::geography`, `CAST(geom AS geography)`, `geom::geometry(Polygon, 2277)`, `geom::box2d`, `x::text`, `x::numeric(12,2)` | Allowed |
| `CAST(route_id AS roads.secret_domain)`, `CAST(x AS my_type)` | cast to roads.secret_domain is not allowed; cast to a built-in type |

## Hidden fields

| You wrote | Result |
| --- | --- |
| `SELECT * FROM roads.roadway_inventory` | Allowed. Hidden columns are absent, and `policy.hidden_fields` names them |
| `SELECT to_jsonb(r) FROM roads.roadway_inventory r`, UNION tricks, `USING (owner_name)`, `NATURAL JOIN` | Allowed, and still no hidden column. The derived table never contains it |
| `SELECT owner_name ...`, `WHERE emp_ssn LIKE '1%'`, `ORDER BY owner_name`, or naming it in a subquery | field owner_name is hidden by organization policy and cannot be used in a query |
| Every column of a table hidden | every field of ... is hidden by organization policy |

## Things that are fine and often assumed not to be

- `GROUP BY 1, 2` and `ORDER BY 2 DESC`
- Subqueries in FROM, WHERE (`IN`, `EXISTS`, `NOT IN`) and the select list. Each table in them gets the row filter too
- `LATERAL (SELECT ...)` subqueries
- `UNION ALL`
- Window functions, `COUNT(*) FILTER (WHERE ...)`, `DISTINCT ON`, `CASE`
- Your own `LIMIT` (kept inside the server's limit)
