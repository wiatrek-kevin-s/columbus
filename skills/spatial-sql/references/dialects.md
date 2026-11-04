# Spatial SQL by dialect

`:g`, `@g` and the numbers are placeholders. All examples assume projected data in EPSG:2277 (US survey feet) unless they say otherwise.

## Building shapes

| Task | PostGIS | Oracle Spatial | SQL Server |
| --- | --- | --- | --- |
| Point | `ST_SetSRID(ST_MakePoint(x, y), 2277)` or `ST_Point(x, y, 2277)` (PostGIS 3.2+) | `SDO_GEOMETRY(2001, 2277, SDO_POINT_TYPE(x, y, NULL), NULL, NULL)` | `geometry::Point(x, y, 2277)`; geography: `geography::Point(lat, lon, 4326)` (latitude first) |
| Box | `ST_MakeEnvelope(xmin, ymin, xmax, ymax, 2277)` | `SDO_GEOMETRY(2003, 2277, NULL, SDO_ELEM_INFO_ARRAY(1,1003,3), SDO_ORDINATE_ARRAY(xmin, ymin, xmax, ymax))` | `geometry::STGeomFromText('POLYGON((xmin ymin, xmax ymin, xmax ymax, xmin ymax, xmin ymin))', 2277)` |
| From WKT | `ST_GeomFromText('POLYGON((...))', 2277)` | `SDO_UTIL.FROM_WKTGEOMETRY('POLYGON((...))')` then set `SDO_SRID` | `geometry::STGeomFromText('POLYGON((...))', 2277)` |
| From GeoJSON | `ST_SetSRID(ST_GeomFromGeoJSON('{...}'), 4326)` | `SDO_UTIL.FROM_GEOJSON('{...}')` (12.2 and later) | No built-in; convert to WKT first |

## Predicates (WHERE clause)

| Task | PostGIS | Oracle Spatial | SQL Server |
| --- | --- | --- | --- |
| Bounding boxes meet (index only) | `geom && :g` | `SDO_FILTER(geom, :g) = 'TRUE'` | `geom.Filter(@g) = 1` |
| Intersects | `ST_Intersects(geom, :g)` | `SDO_ANYINTERACT(geom, :g) = 'TRUE'` or `SDO_RELATE(geom, :g, 'mask=ANYINTERACT') = 'TRUE'` | `geom.STIntersects(@g) = 1` |
| Inside | `ST_Within(geom, :g)` or `ST_CoveredBy(geom, :g)` | `SDO_RELATE(geom, :g, 'mask=INSIDE+COVEREDBY') = 'TRUE'` | `geom.STWithin(@g) = 1` |
| Feature contains the shape (which parcel holds this point) | `ST_Contains(geom, :g)` or `ST_Covers(geom, :g)` | `SDO_RELATE(geom, :g, 'mask=CONTAINS+COVERS') = 'TRUE'` | `geom.STContains(@g) = 1` |
| Within distance | `ST_DWithin(geom, :g, 500)` | `SDO_WITHIN_DISTANCE(geom, :g, 'distance=500 unit=FOOT') = 'TRUE'` | `geom.STDistance(@g) <= 500` |
| Within distance, metres on lon/lat | `ST_DWithin(geog, :g::geography, 100)` | `SDO_WITHIN_DISTANCE(geom, :g, 'distance=100 unit=M') = 'TRUE'` (geodetic SRID) | `geog.STDistance(@g) <= 100` (geography column) |

Oracle's index operators need the indexed column as the first argument and a spatial index on it.

## Nearest neighbours

```sql
-- PostGIS: 5 nearest signs
SELECT sign_id, ST_Distance(geom, :pt) AS ft
FROM assets.signs ORDER BY geom <-> :pt LIMIT 5;

-- Oracle: SDO_NN. When you add other filters, use sdo_batch_size rather than
-- sdo_num_res: sdo_num_res picks N candidates before the other filters run.
SELECT s.sign_id, SDO_NN_DISTANCE(1) AS dist
FROM assets.signs s
WHERE SDO_NN(s.geom, :pt, 'sdo_batch_size=10', 1) = 'TRUE' AND s.status = 'A'
ORDER BY dist FETCH FIRST 5 ROWS ONLY;

-- SQL Server: the IS NOT NULL filter lets the spatial index serve the ORDER BY.
SELECT TOP (5) sign_id, geom.STDistance(@pt) AS ft
FROM assets.signs WHERE geom.STDistance(@pt) IS NOT NULL
ORDER BY geom.STDistance(@pt);
```

## Measurement and output (SELECT list)

| Task | PostGIS | Oracle Spatial | SQL Server |
| --- | --- | --- | --- |
| Area | `ST_Area(geom)` (CRS units squared); `ST_Area(geom::geography)` m² | `SDO_GEOM.SDO_AREA(geom, 0.005, 'unit=SQ_FOOT')` | `geom.STArea()` |
| Length | `ST_Length(geom)`; `ST_Length(geom::geography)` m | `SDO_GEOM.SDO_LENGTH(geom, 0.005, 'unit=FOOT')` | `geom.STLength()` |
| Distance | `ST_Distance(a, b)` | `SDO_GEOM.SDO_DISTANCE(a, b, 0.005)` | `a.STDistance(b)` |
| Reproject | `ST_Transform(geom, 4326)` | `SDO_CS.TRANSFORM(geom, 4326)` | Not available; reproject outside SQL |
| Valid? | `ST_IsValid(geom)`, `ST_IsValidReason(geom)` | `SDO_GEOM.VALIDATE_GEOMETRY_WITH_CONTEXT(geom, 0.005)` | `geom.STIsValid()`, `geom.IsValidDetailed()` |
| Repair (in a copy) | `ST_MakeValid(geom)` | `SDO_UTIL.RECTIFY_GEOMETRY(geom, 0.005)` | `geom.MakeValid()` |
| WKT out | `ST_AsText(geom)` | `SDO_UTIL.TO_WKTGEOMETRY(geom)` or `t.geom.Get_WKT()` | `geom.STAsText()` |
| GeoJSON out | `ST_AsGeoJSON(geom)` | `SDO_UTIL.TO_GEOJSON(geom)` (12.2 and later) | No built-in |
| Extent | `ST_Extent(geom)` (aggregate) | `SDO_AGGR_MBR(geom)` | `geometry::EnvelopeAggregate(geom)` |
| SRID | `ST_SRID(geom)` | `t.geom.SDO_SRID` | `geom.STSrid` |
| Type | `GeometryType(geom)` / `ST_GeometryType(geom)` | `t.geom.SDO_GTYPE` (2001 point, 2002 line, 2003 polygon, 2005-2007 multi) | `geom.STGeometryType()` |

The Oracle tolerance (`0.005` above) is in the data's units. For projected data, use the precision of the data, such as `0.01` feet.

## Row limits

| Dialect | Limit | Notes |
| --- | --- | --- |
| PostgreSQL / SQLite / DuckDB | `LIMIT 50` | `LIMIT 50 OFFSET 100` for paging |
| Oracle 12c and later | `FETCH FIRST 50 ROWS ONLY` | Older: `WHERE ROWNUM <= 50`, applied before ORDER BY unless you nest the query |
| SQL Server | `SELECT TOP (50) ...` | `OFFSET 0 ROWS FETCH NEXT 50 ROWS ONLY` needs an ORDER BY |

## SQLite, SpatiaLite and GeoPackage

- A GeoPackage stores geometry as a GeoPackage binary blob. Plain SQLite cannot read it. Spatial functions exist only when SpatiaLite is loaded, or when GDAL opens the file (`ogr2ogr`/`ogrinfo` with `-dialect SQLite`, which provides `ST_MinX`, `ST_Intersects` and others).
- The spatial index of a GeoPackage is an R*Tree table named `rtree_<table>_<geometry column>`, with columns `id, minx, maxx, miny, maxy`. Filter on it by bounding box and join back on `fid`.
- In SpatiaLite, use the `SpatialIndex` virtual table: `WHERE ROWID IN (SELECT ROWID FROM SpatialIndex WHERE f_table_name = 'roads' AND search_frame = :g)`.
