"""What a database says about one of its tables, for drafting notes.

``catalog_details`` reads the catalog only: the primary key, foreign keys
(this table's columns and the table and columns they point to), indexes,
and the comments people wrote on the table and its columns (PostgreSQL
COMMENT, Oracle COMMENT ON, SQL Server MS_Description, GeoPackage
gpkg_data_columns), plus GeoPackage enumerations, which are coded-value
domains with labels. ``value_sqls`` are SELECTs the gateway has already
checked (row filter, hidden fields, limits): each counts the distinct values
of one short field, so a draft can list the codes a field holds.

Nothing here writes, and every session is the read-only one the rest of the
worker uses. The gateway removes hidden fields and names of tables the
connection does not allow before anything is written down.
"""

from __future__ import annotations

from typing import Any

import columbus_worker as w

MAX_COMMENT = 2000


def _text(v: Any) -> str | None:
    if v is None:
        return None
    s = str(v).strip()
    return s[:MAX_COMMENT] if s else None


def _split(dataset: str) -> tuple[str | None, str]:
    """``schema.table`` (quotes and brackets removed), or just the table."""
    clean = dataset.replace('"', "").replace("[", "").replace("]", "")
    if "." in clean:
        schema, table = clean.rsplit(".", 1)
        return schema, table
    return None, clean


def _group(rows, key_len: int):
    """Rows (k1..kn, value) grouped by their key, keeping order."""
    out: dict[tuple, list] = {}
    for r in rows:
        out.setdefault(tuple(r[:key_len]), []).append(r[key_len:])
    return out


def _sqlite(conn: dict[str, Any], table: str, timeout_s: float) -> dict[str, Any]:
    db = w._open_sqlite(conn, timeout_s)
    try:
        q = '"' + table.replace('"', '""') + '"'
        cols = list(db.execute(f"PRAGMA table_info({q})"))
        pk = [r[1] for r in sorted((r for r in cols if r[5]), key=lambda r: r[5])]
        fks = []
        for (fid,), items in _group(
            [(r[0], r[2], r[3], r[4]) for r in db.execute(f"PRAGMA foreign_key_list({q})")], 1
        ).items():
            fks.append({
                "name": f"fk{fid}",
                "columns": [i[1] for i in items],
                "references": {"dataset": items[0][0], "columns": [i[2] for i in items]},
            })
        indexes = []
        for r in db.execute(f"PRAGMA index_list({q})"):
            name, unique = r[1], bool(r[2])
            iq = '"' + str(name).replace('"', '""') + '"'
            indexes.append({"name": name, "unique": unique,
                            "columns": [c[2] for c in db.execute(f"PRAGMA index_info({iq})")]})
        comments: dict[str, Any] = {"table": None, "columns": {}}
        domains: dict[str, dict[str, Any]] = {}
        has = {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type IN ('table','view')")}
        if "gpkg_contents" in has:
            row = db.execute("SELECT description FROM gpkg_contents WHERE table_name = ?", (table,)).fetchone()
            comments["table"] = _text(row[0]) if row else None
        if "gpkg_data_columns" in has:
            for column, title, desc, constraint in db.execute(
                "SELECT column_name, title, description, constraint_name FROM gpkg_data_columns WHERE table_name = ?",
                (table,),
            ):
                text = _text(" — ".join(x for x in (title, desc) if x))
                if text:
                    comments["columns"][column] = text
                if constraint and "gpkg_data_column_constraints" in has:
                    values = {}
                    kind = None
                    for ctype, value, vdesc, vmin, vmax in db.execute(
                        "SELECT constraint_type, value, description, min, max FROM gpkg_data_column_constraints "
                        "WHERE constraint_name = ? ORDER BY value", (constraint,)
                    ):
                        kind = ctype
                        if ctype == "enum" and value is not None:
                            values[str(value)[:200]] = _text(vdesc) or ""
                        elif ctype == "range":
                            domains[column] = {"name": constraint, "type": "range", "min": vmin, "max": vmax}
                    if kind == "enum" and values:
                        domains[column] = {"name": constraint, "type": "coded", "values": values}
        return {"primary_key": pk, "foreign_keys": fks, "indexes": indexes, "comments": comments, "domains": domains}
    finally:
        db.close()


def _pg(conn: dict[str, Any], dataset: str, timeout_s: float) -> dict[str, Any]:
    db, scrub = w._pg_connect(conn, timeout_s)
    schema, table = _split(dataset)
    rel = ('"' + (schema or "public").replace('"', '""') + '".') + '"' + table.replace('"', '""') + '"'
    try:
        with db.cursor() as cur:
            cur.execute(
                "SELECT a.attname FROM pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid "
                "AND a.attnum = ANY(i.indkey) WHERE i.indrelid = %s::regclass AND i.indisprimary "
                "ORDER BY array_position(i.indkey, a.attnum)", (rel,))
            pk = [r[0] for r in cur.fetchall()]
            cur.execute(
                "SELECT c.conname, k.n, a.attname, rn.nspname, rr.relname, ra.attname "
                "FROM pg_constraint c "
                "CROSS JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(att, ratt, n) "
                "JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.att "
                "JOIN pg_class rr ON rr.oid = c.confrelid JOIN pg_namespace rn ON rn.oid = rr.relnamespace "
                "JOIN pg_attribute ra ON ra.attrelid = c.confrelid AND ra.attnum = k.ratt "
                "WHERE c.contype = 'f' AND c.conrelid = %s::regclass ORDER BY c.conname, k.n", (rel,))
            fks = [
                {"name": name, "columns": [i[1] for i in items],
                 "references": {"dataset": f"{items[0][2]}.{items[0][3]}", "columns": [i[4] for i in items]}}
                for (name,), items in _group(cur.fetchall(), 1).items()
            ]
            cur.execute(
                "SELECT ic.relname, i.indisunique, a.attname FROM pg_index i "
                "JOIN pg_class ic ON ic.oid = i.indexrelid "
                "JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) "
                "WHERE i.indrelid = %s::regclass ORDER BY ic.relname, array_position(i.indkey, a.attnum)", (rel,))
            indexes = [
                {"name": name, "unique": bool(unique), "columns": [i[0] for i in items]}
                for (name, unique), items in _group(cur.fetchall(), 2).items()
            ]
            cur.execute("SELECT obj_description(%s::regclass, 'pg_class')", (rel,))
            row = cur.fetchone()
            cur.execute(
                "SELECT a.attname, col_description(a.attrelid, a.attnum) FROM pg_attribute a "
                "WHERE a.attrelid = %s::regclass AND a.attnum > 0 AND NOT a.attisdropped", (rel,))
            cols = {n: _text(d) for n, d in cur.fetchall() if _text(d)}
        return {"primary_key": pk, "foreign_keys": fks, "indexes": indexes,
                "comments": {"table": _text(row[0]) if row else None, "columns": cols}, "domains": {}}
    except Exception as e:  # psycopg errors, scrubbed of connection details
        raise w.WorkerError(w.ENGINE_ERROR, scrub(f"reading the catalog failed: {w._pg_error_text(e)}")) from None
    finally:
        db.close()


def _ora(conn: dict[str, Any], dataset: str, timeout_s: float) -> dict[str, Any]:
    oracledb = w._ora()
    db, scrub = w._ora_connect(conn, timeout_s)
    owner, table = _split(dataset)
    if owner is None:
        owner = str(conn.get("user") or "").upper()
    b = {"o": owner, "t": table}
    try:
        with db.cursor() as cur:
            cur.execute(
                "SELECT cc.column_name FROM all_constraints c JOIN all_cons_columns cc "
                "ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name "
                "WHERE c.owner = :o AND c.table_name = :t AND c.constraint_type = 'P' ORDER BY cc.position", b)
            pk = [r[0] for r in cur.fetchall()]
            cur.execute(
                "SELECT c.constraint_name, cc.column_name, r.owner, r.table_name, rc.column_name "
                "FROM all_constraints c "
                "JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name "
                "JOIN all_constraints r ON r.owner = c.r_owner AND r.constraint_name = c.r_constraint_name "
                "JOIN all_cons_columns rc ON rc.owner = r.owner AND rc.constraint_name = r.constraint_name "
                "AND rc.position = cc.position "
                "WHERE c.owner = :o AND c.table_name = :t AND c.constraint_type = 'R' "
                "ORDER BY c.constraint_name, cc.position", b)
            fks = [
                {"name": name, "columns": [i[0] for i in items],
                 "references": {"dataset": f"{items[0][1]}.{items[0][2]}", "columns": [i[3] for i in items]}}
                for (name,), items in _group(cur.fetchall(), 1).items()
            ]
            cur.execute(
                "SELECT i.index_name, i.uniqueness, ic.column_name FROM all_indexes i "
                "JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name "
                "WHERE i.table_owner = :o AND i.table_name = :t ORDER BY i.index_name, ic.column_position", b)
            indexes = [
                {"name": name, "unique": uniq == "UNIQUE", "columns": [i[0] for i in items]}
                for (name, uniq), items in _group(cur.fetchall(), 2).items()
            ]
            cur.execute("SELECT comments FROM all_tab_comments WHERE owner = :o AND table_name = :t", b)
            row = cur.fetchone()
            cur.execute("SELECT column_name, comments FROM all_col_comments WHERE owner = :o AND table_name = :t", b)
            cols = {n: _text(d) for n, d in cur.fetchall() if _text(d)}
        return {"primary_key": pk, "foreign_keys": fks, "indexes": indexes,
                "comments": {"table": _text(row[0]) if row else None, "columns": cols}, "domains": {}}
    except oracledb.Error as e:
        raise w.WorkerError(w.ENGINE_ERROR, scrub(f"reading the catalog failed: {w._ora_error_text(e)}")) from None
    finally:
        db.close()


def _ms(conn: dict[str, Any], dataset: str, timeout_s: float) -> dict[str, Any]:
    import pyodbc

    db, scrub = w._ms_connect(conn, timeout_s)
    schema, table = _split(dataset)
    obj = "[" + (schema or "dbo").replace("]", "]]") + "].[" + table.replace("]", "]]") + "]"
    try:
        cur = db.cursor()
        cur.execute(
            "SELECT c.name FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id "
            "AND ic.index_id = i.index_id JOIN sys.columns c ON c.object_id = ic.object_id "
            "AND c.column_id = ic.column_id WHERE i.object_id = OBJECT_ID(?) AND i.is_primary_key = 1 "
            "ORDER BY ic.key_ordinal", obj)
        pk = [r[0] for r in cur.fetchall()]
        cur.execute(
            "SELECT fk.name, pc.name, SCHEMA_NAME(rt.schema_id), rt.name, rc.name FROM sys.foreign_keys fk "
            "JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id "
            "JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id "
            "JOIN sys.tables rt ON rt.object_id = fkc.referenced_object_id "
            "JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id "
            "WHERE fk.parent_object_id = OBJECT_ID(?) ORDER BY fk.name, fkc.constraint_column_id", obj)
        fks = [
            {"name": name, "columns": [i[0] for i in items],
             "references": {"dataset": f"{items[0][1]}.{items[0][2]}", "columns": [i[3] for i in items]}}
            for (name,), items in _group([tuple(r) for r in cur.fetchall()], 1).items()
        ]
        cur.execute(
            "SELECT i.name, i.is_unique, c.name FROM sys.indexes i JOIN sys.index_columns ic "
            "ON ic.object_id = i.object_id AND ic.index_id = i.index_id JOIN sys.columns c "
            "ON c.object_id = ic.object_id AND c.column_id = ic.column_id "
            "WHERE i.object_id = OBJECT_ID(?) AND i.index_id > 0 ORDER BY i.name, ic.key_ordinal", obj)
        indexes = [
            {"name": name, "unique": bool(unique), "columns": [i[0] for i in items]}
            for (name, unique), items in _group([tuple(r) for r in cur.fetchall()], 2).items()
        ]
        cur.execute(
            "SELECT ep.minor_id, COALESCE(c.name, ''), CAST(ep.value AS nvarchar(2000)) FROM sys.extended_properties ep "
            "LEFT JOIN sys.columns c ON c.object_id = ep.major_id AND c.column_id = ep.minor_id "
            "WHERE ep.class = 1 AND ep.major_id = OBJECT_ID(?) AND ep.name = 'MS_Description'", obj)
        table_comment, cols = None, {}
        for minor, column, text in cur.fetchall():
            if minor == 0:
                table_comment = _text(text)
            elif column and _text(text):
                cols[column] = _text(text)
        return {"primary_key": pk, "foreign_keys": fks, "indexes": indexes,
                "comments": {"table": table_comment, "columns": cols}, "domains": {}}
    except pyodbc.Error as e:
        raise w.WorkerError(w.ENGINE_ERROR, scrub(f"reading the catalog failed: {w._ms_error_text(e, prefix='')}")) from None
    finally:
        try:
            db.rollback()
        finally:
            db.close()


def _esri(conn: dict[str, Any], dataset: str, timeout_s: float) -> dict[str, Any]:
    """A shapefile or geodatabase layer: its key is a geodatabase's OBJECTID;
    neither keeps foreign keys or comments GDAL can read here."""
    import columbus_esri

    entries = columbus_esri.esri_list(conn, timeout_s, dataset)
    pk = [f["name"] for f in (entries[0]["fields"] if entries else []) if f.get("pk")]
    return {"primary_key": pk, "foreign_keys": [], "indexes": [],
            "comments": {"table": None, "columns": {}}, "domains": {}}


def catalog_details(params: dict[str, Any]) -> dict[str, Any]:
    conn = w._connection(params)
    dataset = params.get("dataset")
    if not isinstance(dataset, str) or not dataset:
        raise w.WorkerError(w.INVALID_PARAMS, "dataset is required")
    timeout_s = float(params.get("timeout_s", 60))
    kind = conn["kind"].lower()
    if kind in w.SQLITE_KINDS:
        out = _sqlite(conn, dataset, timeout_s)
    elif kind in w.PG_KINDS:
        out = _pg(conn, dataset, timeout_s)
    elif kind in w.ORACLE_KINDS:
        out = _ora(conn, dataset, timeout_s)
    elif kind in w.MSSQL_KINDS:
        out = _ms(conn, dataset, timeout_s)
    elif kind in w.ESRI_KINDS:
        out = _esri(conn, dataset, timeout_s)
    elif kind in w.FILES_KINDS:
        # The cache GeoPackage, brought up to date first.
        import columbus_files

        columbus_files.sync(conn, timeout_s)
        out = _sqlite(columbus_files.cache_conn(conn), dataset, timeout_s)
    else:
        raise w.WorkerError(w.UNSUPPORTED, f"catalog_details does not support {conn['kind']!r}")
    # Distinct values of short fields, by the gateway's checked SELECTs:
    # [value, count] pairs, most common first.
    values: dict[str, Any] = {}
    for item in params.get("value_sqls") or []:
        field, sql = item.get("field"), item.get("sql")
        if not isinstance(field, str) or not isinstance(sql, str):
            continue
        limit = int(item.get("max_rows") or 31)
        try:
            _, rows, _, cells, _ = w._fetch(conn, sql, limit, timeout_s)
        except w.WorkerError as e:
            values[field] = {"error": str(e)}
            continue
        values[field] = [[cells[0](r[0]), int(r[1])] for r in rows]
    out["values"] = values
    return out
