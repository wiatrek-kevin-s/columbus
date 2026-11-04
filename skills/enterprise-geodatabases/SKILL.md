---
name: enterprise-geodatabases
description: Reading and safely working with Esri enterprise geodatabases (SDE) on Oracle, SQL Server and PostgreSQL, covering versions, reconcile and post, branch vs traditional versioning, a protected DEFAULT, domains and subtypes, relationship classes, attribute rules, archiving, ST_Geometry vs SDO_GEOMETRY vs SQL Server geometry, and versioned views vs base tables. Use it when asked to "query the SDE", "edit the enterprise geodatabase", "reconcile and post", "what do these codes mean", "why does SQL show different rows than ArcGIS Pro", or before touching any versioned or registered table.
---

# Enterprise geodatabases

An enterprise geodatabase (EGDB, "SDE") is an ordinary Oracle, SQL Server or PostgreSQL
database with Esri's system tables and rules on top. The rules (versions, domains,
relationship classes, attribute rules, editor tracking) live in the geodatabase, not in
the database, so **SQL sees the tables but not the rules**. Most mistakes come from
forgetting that.

## General practice (with or without Columbus)

### 1. The one rule: never edit DEFAULT directly

DEFAULT is the published, trusted state of the data. Edit in a **named child version**,
check it, then reconcile and post. Even if you have the rights to edit DEFAULT, do not:
there is no review step, no undo for other users, and a bad edit is live at once.

- DEFAULT's access is usually **protected**: everyone can read it, only its owner (often
  `SDE` or the data owner) can edit it or post to it. A "protected" error when posting means
  you need the owner or a QA role to post. That is the design working, not a bug.
- Do not edit registered tables with SQL (`INSERT`, `UPDATE`, `DELETE`) or with tools that
  bypass the geodatabase. SQL skips domains, subtypes, attribute rules, relationship class
  behaviour (composite deletes, messaging), editor tracking and GlobalID generation, and on
  versioned data it can write rows that ArcGIS never shows.
- If someone asks for a bulk fix, propose it as a reviewed edit in a named version.

### 2. Traditional vs branch versioning

| | Traditional | Branch |
| --- | --- | --- |
| How edits are stored | Delta tables: adds `A<registration_id>`, deletes `D<registration_id>`; base table holds only compressed state | One table; every edit adds a row with `GDB_ARCHIVE_OID`, `GDB_BRANCH_ID`, `GDB_FROM_DATE`, `GDB_IS_DELETE`, `GDB_DELETED_AT`, `GDB_DELETED_BY` |
| Version tree | Any depth (DEFAULT > QA > editor) | One level: named versions are children of DEFAULT |
| Where you edit | Direct database connection in ArcGIS Pro, or services | Through feature services with the Version Management capability |
| Maintenance | Reconcile all, then **compress** regularly, or the state tree and delta tables grow and everything slows | No compress; history is kept by design |
| Required by | Legacy geometric networks, some older workflows | Utility Network and Parcel Fabric multi-user editing, web editing with versions |

Find which one a dataset uses: in Pro, the dataset's Properties (Versioning), or ask the
data owner. In ArcPy, `arcpy.Describe(fc).isVersioned` says whether it is versioned at all.
In SQL, branch versioned tables carry the `GDB_BRANCH_ID` field; traditional ones have
`A<id>`/`D<id>` delta tables and usually an `_evw` view.

### 3. Reconcile and post

1. **Create a version** from DEFAULT (or from the QA version in a traditional tree):
   `arcpy.management.CreateVersion(sde_conn, "SDE.DEFAULT", "roads_fix_1104", "PRIVATE")`.
2. **Edit in that version**, inside an edit session (see the arcpy-scripting skill).
3. **Reconcile**: pull DEFAULT's newer changes into your version. Conflicts appear when the
   same feature (by object) or the same field (by attribute) changed in both.
4. **Review conflicts** in Pro's Conflicts pane. Choose, per conflict, the edit version's
   value or the target's. Never resolve conflicts in bulk without reading them.
5. **Post**: push your reconciled version into DEFAULT. Post only after a reconcile with no
   unresolved conflicts.
6. **Delete the version** when done. In traditional versioning, the DBA compresses later.

```python
arcpy.management.ReconcileVersions(
    input_database=sde_conn,
    reconcile_mode="ALL_VERSIONS",          # or "BLOCKING_VERSIONS"
    target_version="SDE.DEFAULT",
    edit_versions="ANALYST.roads_fix_1104",
    acquire_locks="LOCK_ACQUIRED",
    abort_if_conflicts="ABORT_CONFLICTS",   # stop and review rather than auto-resolve
    conflict_definition="BY_OBJECT",        # or "BY_ATTRIBUTE"
    conflict_resolution="FAVOR_TARGET_VERSION",
    with_post="NO_POST",                    # post only after a person has reviewed
    with_delete="KEEP_VERSION",
    out_log=r"C:\work\reconcile.log",
)
```

Scripted reconcile with `with_post="POST"` and `FAVOR_EDIT_VERSION` silently overwrites other
people's work. Use it only for a documented, approved batch process.

### 4. Domains and subtypes

- **Coded value domains** map stored codes to labels (`1` = `Interstate`). **Range domains**
  give a min and max. A domain is assigned to a field, and can differ per subtype.
- **Subtypes** split one table into groups by an integer field (for example `SUBTYPE_CD`).
  Each subtype can have its own default values and domains.
- **SQL returns codes, not labels.** To report labels, join to the domain definition, which is
  stored as XML in the geodatabase's `GDB_ITEMS` table. Prefer reading domains with
  `arcpy.da.ListDomains(sde_conn)` (each has `name`, `domainType` `CodedValue` or `Range`,
  `codedValues` dict, `range`), and subtypes with `arcpy.da.ListSubtypes(fc)`.
- Domains are enforced only by ArcGIS clients when editing. Old or SQL-loaded rows may hold
  values outside the domain. When you QA a field, count values not in the domain.
- Code field types matter: a code stored as text `"01"` will not match the number `1`.

### 5. Relationship classes

- A relationship class links an **origin** table's primary key to a **destination** table's
  foreign key (often `GlobalID` to a `GUID` field). Cardinality is 1:1, 1:M or M:N; M:N and
  attributed relationships use an intermediate table.
- **Composite** relationships delete destination rows when the origin is deleted, but only
  when the delete happens through ArcGIS. A SQL delete leaves orphans.
- In SQL, follow a relationship with an ordinary join on its keys. Find the keys with
  `arcpy.Describe(rel_class)`: `originClassNames`, `destinationClassNames`, `cardinality`,
  `isComposite`, `originClassKeys`.
- QA check: destination rows whose foreign key matches no origin row (orphans).

### 6. Attribute rules

Arcade expressions the geodatabase runs on edits:

- **Calculation** rules fill fields (an ID from a sequence, a district from a spatial
  intersect). Immediate ones run on insert/update/delete; batch ones run later.
- **Constraint** rules reject an edit that breaks a condition.
- **Validation** rules flag existing features as errors for review (batch).

They run only for edits made through ArcGIS (Pro, services, ArcPy edit sessions). SQL edits
skip them, so a calculated field can be stale or empty. Batch calculation and validation
rules have extra requirements in an EGDB (typically branch versioning and services); check
before relying on them. Export the rules to read them:
`arcpy.management.ExportAttributeRules(fc, r"C:\work\rules.csv")`.

### 7. Archiving

- **Traditional versioned + archiving**: history lives in an archive class `<name>_H` with
  `GDB_FROM_DATE`, `GDB_TO_DATE` and `GDB_ARCHIVE_OID`. Current rows have
  `GDB_TO_DATE = 9999-12-31 23:59:59`.
- **Non-versioned + archiving**: the same fields are added to the base table itself. A plain
  `SELECT` returns old and current rows together; filter on `GDB_TO_DATE` for the current state.
- **Branch versioning** keeps history by design (see section 2).
- Counting rows in an archived table without the filter overcounts. It is a common reason a
  SQL count disagrees with Pro.

### 8. Geometry storage

| Storage | Database | Read geometry as WKT | Notes |
| --- | --- | --- | --- |
| Esri `ST_Geometry` | Oracle, PostgreSQL | `sde.st_astext(shape)` | Its SRID is Esri's own id from `ST_SPATIAL_REFERENCES`; the EPSG code is in that table's `auth_srid`. On Oracle, many functions need the `st_shapelib` external procedure; ORA-28595/ORA-06520 mean it is not configured. |
| `SDO_GEOMETRY` (Oracle Spatial) | Oracle | `SDO_UTIL.TO_WKTGEOMETRY(shape)` | SRIDs may be Oracle legacy codes (8307 for WGS 84, 8265 for NAD83) rather than EPSG. Spatial operators need a spatial index and a tolerance. |
| `geometry` / `geography` | SQL Server | `shape.STAsText()` | Methods are case-sensitive. `geography::Point(lat, lon, srid)` takes latitude first; WKT is lon lat. `geography` needs correct ring orientation. |
| PostGIS `geometry` | PostgreSQL | `ST_AsText(shape)` | Optional storage for EGDBs on PostgreSQL. |
| `SDEBINARY` / LOB | Oracle, legacy | not readable in SQL | Only through ArcGIS. Ask the DBA. |

More functions per type are in [references/storage-and-sql.md](references/storage-and-sql.md),
and spatial predicates in the spatial-sql skill ([../spatial-sql/SKILL.md](../spatial-sql/SKILL.md)).
Find a feature class's storage from the shape column's database type, or ask the DBA.
ArcGIS uses the configuration keyword (`GEOMETRY_STORAGE` in the `DBTUNE` settings) chosen
when the class was created.

### 9. Reading through views vs base tables

What you see depends on what you query:

| You query | You get |
| --- | --- |
| Base table of a **traditional versioned** class | The state as of the last compress. Edits since then (in delta tables) are missing, and deleted rows may still be there. **Wrong for current data.** |
| Versioned view `<name>_evw` (traditional) | DEFAULT, including delta-table edits, unless the session set another version first |
| Base table of a **branch versioned** class | Every row version ever written, across branches. Esri does not support SQL against these; use the feature service or a view the data owner built. |
| Base table of a **non-versioned, archived** class | Current and historical rows; filter `GDB_TO_DATE` |
| Base table of a **non-versioned, non-archived** class | Current data |
| A database view or Esri-registered view | Whatever its owner defined. Read its definition or its context entry before trusting it. |

Reading a specific version through a versioned view needs a call first: Oracle
`EXEC sde.version_util.set_current_version('ANALYST.roads_fix_1104');`, SQL Server
`EXEC sde.set_current_version 'ANALYST.roads_fix_1104';`, PostgreSQL
`SELECT sde.sde_set_current_version('ANALYST.roads_fix_1104');`. Without it the view shows DEFAULT.

### 10. Before you answer anything from an EGDB

1. Which table or view, and is the class versioned (which kind), archived, or neither?
2. Which version does the answer describe? Say "DEFAULT as of <time>" in the answer.
3. Are coded fields shown as codes or labels? Say which.
4. Are qualified names in play (`ROADS.ROADWAY_INVENTORY`, field names like
   `ROADS.ROADWAY_INVENTORY.FUNC_CLASS` after a join)? Use the owner-qualified name.
5. Is geometry in a type your tool can read? If not, read attributes in SQL and geometry
   through ArcGIS or the feature service.

## If the Columbus tools are available

Columbus enforces its rules in the server. Nothing here is a control; if it refuses
something, tell the person what and why, and do not look for a way around it.

**What works today and what does not.**

- `whoami`, `list_connections` and `describe_connection` show each connection's type and
  effective mode. **Every connection is read-only** until organization policy turns edits
  on, whatever the connection file says. There is no editing tool yet: `propose_edits` and
  `apply_edits` (edits in a named version, never DEFAULT) are planned and not available.
  Write proposed edits as a list for a person to make in Pro.
- An EGDB's tables on **Oracle** (`oracle`, `sde-oracle`) and **SQL Server** (`sqlserver`,
  `sde-sqlserver`, `azure-sql`) connections are read like any other table: `list_datasets`,
  `describe_dataset`, `sample_rows`, `query_features` and `sql_select` all work, read-only.
  `SDO_GEOMETRY` and SQL Server `geometry`/`geography` come back as shapes. **Esri
  `ST_Geometry` columns cannot be read**: `describe_dataset` raises `geometry.unsupported` and
  their values come back as a placeholder (`unsupported: SDE.ST_GEOMETRY`); the `sde.st_*`
  functions are database code and are refused. Say so, and suggest reading that layer in
  ArcGIS Pro or asking the DBA for an SDO_GEOMETRY view. The same holds for versions:
  `sql_select` cannot call `sde.version_util`, so a versioned view shows DEFAULT.
- Connections of type `sde` (through ArcPy) are configured and checked, but **that engine
  is not built yet**; the tools answer that reading it is not supported yet. Say so rather
  than guessing at the data.
- An EGDB on **PostgreSQL** can be read today through a `postgis` connection if the person
  has added one. Then sections 7 to 9 matter directly: check whether the allowed dataset is
  a base table or an `_evw` view. `sql_select` cannot set a version (the setter calls are
  not SELECTs, and `sde.*` functions such as `sde.st_astext` are not on the function allow
  list), so a versioned view shows DEFAULT. PostGIS-stored geometry works with `ST_*`
  functions; Esri `ST_Geometry` columns cannot be converted or tested through `sql_select`.

**Use the context pack for meaning.** The organization's pack says what tables, codes and
versions mean, which SQL cannot:

```json
describe_dataset {"connection": "<connection id>", "dataset": "<schema.table>"}
```

attaches the **dataset entry** whose `connection` and `physical` match (keys, field aliases, each field's domain id,
gotchas such as "retired segments stay with STATUS = 'R'"). For code labels, read the
**domain entry** the field names:

```json
context_search {"query": "functional class", "kind": "domain"}
context_get {"id": "functional-class"}
```

A domain entry has `type` (`coded` or `range`) and `values` (code: label) or `min`/`max`.
Use it to label results and to QA values outside the domain. If an entry comes back
`stale`, say so, and do not present it as current. If the pack has no entry, say the codes
are unlabelled rather than inventing labels. `context_search {"kind": "dataset"}` also
finds datasets on connections whose engine is not built yet, so you can still explain what
a table holds and how to read it.

Columbus does not yet draft context entries from a geodatabase
(`draft_context_from_geodatabase` is planned), and it does not enforce domains in `verify`.
