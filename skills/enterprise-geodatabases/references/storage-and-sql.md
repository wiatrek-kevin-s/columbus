# Geometry storage and geodatabase SQL, by database

Read-only reference for an enterprise geodatabase (EGDB). For spatial predicates, distance
and nearest-neighbour SQL, see the spatial-sql skill ([../../spatial-sql/SKILL.md](../../spatial-sql/SKILL.md)).

## Common reads per geometry type

| Task | Esri ST_Geometry (Oracle) | Esri ST_Geometry (PostgreSQL) | Oracle SDO_GEOMETRY | SQL Server geometry | PostGIS geometry |
| --- | --- | --- | --- | --- | --- |
| As WKT | `sde.st_astext(shape)` | `sde.st_astext(shape)` | `SDO_UTIL.TO_WKTGEOMETRY(shape)` | `shape.STAsText()` | `ST_AsText(shape)` |
| SRID | `sde.st_srid(shape)` (Esri id) | `sde.st_srid(shape)` (Esri id) | `t.shape.SDO_SRID` | `shape.STSrid` | `ST_SRID(shape)` |
| Area | `sde.st_area(shape)` | `sde.st_area(shape)` | `SDO_GEOM.SDO_AREA(shape, 0.005)` | `shape.STArea()` | `ST_Area(shape)` |
| Length | `sde.st_length(shape)` | `sde.st_length(shape)` | `SDO_GEOM.SDO_LENGTH(shape, 0.005)` | `shape.STLength()` | `ST_Length(shape)` |
| Simple / valid | `sde.st_issimple(shape) = 1` (simplicity only) | `sde.st_issimple(shape)` | `SDO_GEOM.VALIDATE_GEOMETRY_WITH_CONTEXT(shape, 0.005)` returns `'TRUE'` or an error code | `shape.STIsValid()` | `ST_IsValid(shape)` |
| Intersects | `sde.st_intersects(a, b) = 1` | `sde.st_intersects(a, b)` (boolean) | `SDO_RELATE(a, b, 'mask=ANYINTERACT') = 'TRUE'` | `a.STIntersects(b) = 1` | `ST_Intersects(a, b)` |

Notes:

- Areas and lengths come out in the units of the CRS. In Texas Central state plane
  (EPSG:2277, US survey feet) `STArea()` is square US feet.
- Oracle tolerances (`0.005` above) are in the CRS's units for projected data and in metres
  for geodetic data. Pick one that suits the data's precision.
- Esri ST_Geometry SRIDs are internal ids. Map them to EPSG with
  `SELECT srid, auth_name, auth_srid FROM sde.st_spatial_references` (Oracle:
  `SDE.ST_SPATIAL_REFERENCES`). Do not assume `sde.st_srid()` returns an EPSG code.
- SQL Server's geometry methods are case-sensitive: `STAsText()` works, `stastext()` does not.

## Versioned views and setting the version (traditional versioning)

| Database | Set the version for this session | Then query |
| --- | --- | --- |
| Oracle | `EXEC sde.version_util.set_current_version('OWNER.version_name');` | `SELECT ... FROM roads.roadway_inventory_evw` |
| SQL Server | `EXEC sde.set_current_version 'OWNER.version_name';` (or `dbo.` when the geodatabase is dbo-owned) | `SELECT ... FROM roads.roadway_inventory_evw` |
| PostgreSQL | `SELECT sde.sde_set_current_version('OWNER.version_name');` | `SELECT ... FROM roads.roadway_inventory_evw` |

Without setting a version the view shows DEFAULT. Editing through versioned views is
possible (`sde.version_user_ddl.edit_version` on Oracle and equivalents) but bypasses the
same rules SQL always does. Do not do it.

## Fields ArcGIS adds

| Field(s) | Added by | Meaning |
| --- | --- | --- |
| `OBJECTID` | Registration | Unique integer id, maintained by the geodatabase |
| `GLOBALID` | Add Global IDs | GUID, required for replicas, branch versioning and many relationship classes |
| `CREATED_USER`, `CREATED_DATE`, `LAST_EDITED_USER`, `LAST_EDITED_DATE` | Editor tracking | Who changed what when. Dates may be stored in UTC; check the dataset's setting. SQL edits do not update them. |
| `GDB_FROM_DATE`, `GDB_TO_DATE`, `GDB_ARCHIVE_OID` | Archiving | History; current rows have `GDB_TO_DATE = 9999-12-31 23:59:59` |
| `GDB_ARCHIVE_OID`, `GDB_BRANCH_ID`, `GDB_FROM_DATE`, `GDB_IS_DELETE`, `GDB_DELETED_AT`, `GDB_DELETED_BY` | Branch versioning | Row versions per branch; DEFAULT is branch 0 |
| `SHAPE.AREA`, `SHAPE.LEN` (or `SHAPE_Area`, `SHAPE_Length`) | Feature class | Maintained by ArcGIS on edit; stale after SQL edits; in CRS units |

## Geodatabase system tables you may see

`GDB_ITEMS` (definitions as XML: domains, relationship classes, feature datasets) and
`GDB_ITEMRELATIONSHIPS`, plus the registry, layer, version and state tables (`TABLE_REGISTRY`,
`LAYERS`, `VERSIONS`, `STATES` in the `SDE` schema on Oracle; `sde_table_registry`,
`sde_layers`, `sde_versions`, `sde_states` on SQL Server and PostgreSQL). They belong to the geodatabase administrator. Read them only
when the DBA has granted it and a simpler route (ArcPy `arcpy.da.ListDomains`, the
organization's documentation) is not available.
