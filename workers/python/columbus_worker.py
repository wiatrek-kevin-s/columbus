"""Columbus Python engine worker.

Speaks line-delimited JSON-RPC 2.0 over stdin/stdout with the gateway, and
only with the gateway. It receives requests the gateway has already
checked: queries arrive rewritten with row filters and limits, and file
paths arrive already resolved inside the sandbox. The worker still opens
everything read-only and stops at the row limit it is given, so a gateway
bug cannot become a write or an unbounded read.

Run with ``python -I -u columbus_worker.py``. Logs go to stderr. Built on
the plugin SDK and speaking worker protocol v2 (docs/worker-protocol.md);
unlike third-party plugins, this core engine receives connection handles,
because it is the component that opens governed connections.

Engines: the standard library's sqlite3 handles GeoPackage and SQLite;
psycopg handles PostgreSQL/PostGIS, python-oracledb (thin mode) Oracle and
pyodbc with ODBC Driver 18 SQL Server, when installed. GDAL/OGR, DuckDB, PDAL
and the other database drivers are optional imports that light up when
present in the pixi environment (design section 2).

Credentials arrive inside a single request's connection handle, are used
to open that connection, and are never logged, stored or echoed back: error
messages are scrubbed of them before they leave the worker.
"""

from __future__ import annotations

import base64
import csv
import fnmatch
import importlib
import importlib.metadata
import json
import math
import os
import re
import platform
import sqlite3
import struct
import sys
import time
import traceback
from typing import Any

# The SDK lives beside this file (an administrator-installed folder); -I
# keeps the script's folder off the import path, so add it explicitly.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from columbus_plugin import PROTOCOL_VERSION, Plugin, ToolError, decode_geometry, readonly_uri  # noqa: E402

VERSION = "0.3.0"


class WorkerError(ToolError):
    """An error the gateway should pass on to the agent as a refusal."""

    def __init__(self, code: int, message: str):
        super().__init__(message, code)


# JSON-RPC error codes used by the worker.
INVALID_PARAMS = -32602
UNSUPPORTED = -32001
ENGINE_ERROR = -32002
TIMEOUT = -32003


def _optional_version(dist: str) -> str | None:
    """An installed package's version, from its metadata. Nothing is
    imported: importing geopandas or GDAL can take many seconds on a cold
    Windows start, and ping must stay fast."""
    try:
        return importlib.metadata.version(dist)
    except Exception:  # noqa: BLE001 - not installed
        return None


def ping(_params: dict[str, Any]) -> dict[str, Any]:
    return {
        "protocol": PROTOCOL_VERSION,
        "python": platform.python_version(),
        "engines": {
            "sqlite": sqlite3.sqlite_version,
            "gdal": _optional_version("GDAL"),
            "pyogrio": _optional_version("pyogrio"),
            "shapely": _optional_version("shapely"),
            "duckdb": _optional_version("duckdb"),
            "pdal": _optional_version("pdal"),
            "rasterio": _optional_version("rasterio"),
            "psycopg": _optional_version("psycopg"),
            "oracledb": _optional_version("oracledb"),
            "pyodbc": _optional_version("pyodbc"),
            "gis": _gis_versions(),
        },
    }


SQLITE_KINDS = {"gpkg", "geopackage", "sqlite", "spatialite"}


def _connection(params: dict[str, Any]) -> dict[str, Any]:
    conn = params.get("connection")
    if not isinstance(conn, dict) or not isinstance(conn.get("kind"), str):
        raise WorkerError(INVALID_PARAMS, "connection handle missing or malformed")
    return conn


def _open_sqlite(conn: dict[str, Any], timeout_s: float) -> sqlite3.Connection:
    path = conn.get("path")
    if not isinstance(path, str) or not path:
        raise WorkerError(INVALID_PARAMS, "file connection has no path")
    # Read-only at the engine: URI mode=ro plus query_only.
    uri = readonly_uri(path)
    try:
        db = sqlite3.connect(uri, uri=True, timeout=5, check_same_thread=False)
    except sqlite3.Error as e:
        raise WorkerError(ENGINE_ERROR, f"cannot open {path}: {e}") from e
    db.execute("PRAGMA query_only = ON")
    # Extensions stay off; sqlite3 disables them unless enabled explicitly.
    deadline = time.monotonic() + timeout_s

    def watchdog() -> int:
        return 1 if time.monotonic() > deadline else 0

    db.set_progress_handler(watchdog, 10_000)
    return db


def _sqlite_list(conn: dict[str, Any], timeout_s: float) -> list[dict[str, Any]]:
    db = _open_sqlite(conn, timeout_s)
    try:
        tables = {
            name: {"name": name, "type": kind}
            for name, kind in db.execute(
                "SELECT name, type FROM sqlite_master "
                "WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' "
                "AND name NOT LIKE 'gpkg_%' AND name NOT LIKE 'rtree_%' ORDER BY name"
            )
        }
        has_gpkg = db.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='gpkg_contents'"
        ).fetchone()
        if has_gpkg:
            rows = db.execute(
                "SELECT c.table_name, c.data_type, c.identifier, c.description, c.srs_id, "
                "g.column_name, g.geometry_type_name, c.min_x, c.min_y, c.max_x, c.max_y "
                "FROM gpkg_contents c LEFT JOIN gpkg_geometry_columns g "
                "ON g.table_name = c.table_name"
            )
            for (name, data_type, ident, desc, srs, gcol, gtype, x0, y0, x1, y1) in rows:
                entry = tables.setdefault(name, {"name": name, "type": "table"})
                entry.update(
                    {
                        "gpkg_type": data_type,
                        "title": ident,
                        "description": desc or None,
                        "srs_id": srs,
                        "geometry_column": gcol,
                        "geometry_type": gtype,
                        "extent": [x0, y0, x1, y1] if None not in (x0, y0, x1, y1) else None,
                    }
                )
        if has_gpkg:
            try:
                srs = {
                    r[0]: {"org": r[1], "org_id": r[2], "wkt": r[3]}
                    for r in db.execute(
                        "SELECT srs_id, organization, organization_coordsys_id, definition FROM gpkg_spatial_ref_sys"
                    )
                }
            except sqlite3.Error:
                # Not a complete GeoPackage: the srs_id is all there is.
                srs = {}
            for entry in tables.values():
                if entry.get("srs_id") is not None and entry.get("geometry_column"):
                    entry["crs_spec"] = srs.get(entry["srs_id"]) or {"org_id": entry["srs_id"]}
        for entry in tables.values():
            quoted = '"' + entry["name"].replace('"', '""') + '"'
            entry["fields"] = [
                {"name": r[1], "type": r[2] or None, "not_null": bool(r[3]), "pk": bool(r[5])}
                # table_xinfo, not table_info: it also lists generated
                # columns, which the server must know about to keep a hidden
                # one out of queries.
                for r in db.execute(f"PRAGMA table_xinfo({quoted})")
            ]
        return list(tables.values())
    finally:
        db.close()


def _cell(value: Any) -> Any:
    if isinstance(value, (bytes, bytearray, memoryview)):
        raw = bytes(value)
        if raw[:2] == b"GP":
            return {"geometry": "gpkg", "bytes": len(raw)}
        return {"blob_base64": base64.b64encode(raw[:64]).decode(), "bytes": len(raw)}
    return value


def _sqlite_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None):
    """Returns (columns, rows, hit_limit, cell_fn)."""
    db = _open_sqlite(conn, timeout_s)
    try:
        try:
            cur = db.execute(sql)
            columns = [d[0] for d in (cur.description or [])]
            keep = keep_factory(columns, "gpkg") if keep_factory else None
            rows = []
            hit = False
            for row in cur:
                if keep is not None and not keep(row):
                    continue
                if len(rows) >= max_rows:
                    hit = True
                    break
                rows.append(row)
            geoms = {}
            for i in range(len(columns)):
                first = next((r[i] for r in rows if r[i] is not None), None)
                if isinstance(first, (bytes, memoryview)) and bytes(first)[:2] == b"GP":
                    geoms[i] = "gpkg"
            return columns, rows, hit, [_cell] * len(columns), geoms
        except sqlite3.OperationalError as e:
            if "interrupted" in str(e):
                raise WorkerError(TIMEOUT, f"query exceeded {timeout_s:g} s") from e
            raise WorkerError(ENGINE_ERROR, f"query failed: {e}") from e
        except sqlite3.Error as e:
            raise WorkerError(ENGINE_ERROR, f"query failed: {e}") from e
    finally:
        db.close()


# ---------- PostgreSQL / PostGIS ----------

PG_KINDS = {"postgis", "postgres", "postgresql"}
_SEARCH_PATH_OK = re.compile(r'^[A-Za-z_][A-Za-z0-9_$]*(,[A-Za-z_][A-Za-z0-9_$]*)*$')


class _Scrub:
    """Removes a connection's secret from any text before it leaves the worker."""

    def __init__(self, conn: dict[str, Any]):
        self.secret = conn.get("password") or ""

    def __call__(self, text: str) -> str:
        return text.replace(self.secret, "***") if self.secret else text


def _pg_connect(conn: dict[str, Any], timeout_s: float):
    try:
        import psycopg
    except ImportError as e:
        raise WorkerError(
            UNSUPPORTED, "PostgreSQL needs the psycopg package in the engine environment"
        ) from e
    scrub = _Scrub(conn)
    search_path = (conn.get("search_path") or "public").replace(" ", "")
    if not _SEARCH_PATH_OK.match(search_path):
        raise WorkerError(INVALID_PARAMS, "search_path must be a comma-separated list of schema names")
    ms = max(1, int(timeout_s * 1000))
    # pg_catalog first, always: otherwise a function, type or operator of the
    # same name in a configured schema would be found before the built-in
    # one, and the server's checks are about the built-in ones.
    search_path = ",".join(["pg_catalog"] + [p for p in search_path.split(",") if p and p != "pg_catalog"])
    # Read-only at the server, for every transaction in this session.
    options = (
        f"-c default_transaction_read_only=on -c statement_timeout={ms} "
        f"-c idle_in_transaction_session_timeout=60000 -c search_path={search_path}"
    )
    kwargs = {
        "host": conn.get("host"),
        "port": conn.get("port"),
        "dbname": conn.get("database"),
        "user": conn.get("user"),
        "password": conn.get("password"),
        "sslmode": conn.get("sslmode") or "prefer",
        "connect_timeout": 15,
        "application_name": "columbus-worker",
        "options": options,
    }
    try:
        db = psycopg.connect(**{k: v for k, v in kwargs.items() if v not in (None, "")})
    except psycopg.Error as e:
        raise WorkerError(ENGINE_ERROR, scrub(f"cannot connect: {e}".strip())) from None
    db.read_only = True
    try:
        _pg_refuse_foreign_operators(db, [x for x in search_path.split(",") if x != "pg_catalog"])
    except WorkerError:
        db.close()
        raise
    return db, scrub


def _pg_refuse_foreign_operators(db, schemas: list[str]) -> None:
    """Refuse a session whose search path holds operators no extension
    installed. PostgreSQL picks the operator whose argument types fit best,
    whatever its schema, so `public.=(text, varchar)` written by anyone with
    CREATE on public would run inside the caller's conditions; queries name
    operators bare, so the server cannot qualify them as it does functions."""
    if not schemas:
        return
    try:
        with db.cursor() as cur:
            cur.execute(
                "SELECT n.nspname || '.' || o.oprname FROM pg_operator o "
                "JOIN pg_namespace n ON n.oid = o.oprnamespace "
                "WHERE n.nspname = ANY(%s) AND NOT EXISTS ("
                " SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_operator'::regclass"
                " AND d.objid = o.oid AND d.deptype = 'e') LIMIT 5",
                (schemas,),
            )
            found = [r[0] for r in cur.fetchall()]
        db.rollback()
    except Exception as e:  # noqa: BLE001
        db.rollback()
        raise WorkerError(ENGINE_ERROR, f"could not check the database's operators: {e}") from None
    if found:
        raise WorkerError(
            ENGINE_ERROR,
            "the database defines operators of its own on this connection's search path "
            f"({', '.join(found)}), which a query's conditions could run; ask the database "
            "administrator to move them out of these schemas, or set the connection's "
            "search_path to schemas without them",
        )


def _pg_geometry_oids(db) -> set[int]:
    try:
        with db.cursor() as cur:
            cur.execute("SELECT oid FROM pg_type WHERE typname IN ('geometry', 'geography')")
            return {r[0] for r in cur.fetchall()}
    except Exception:  # noqa: BLE001 - no PostGIS is fine
        db.rollback()
        return set()


def _pg_cell(value: Any) -> Any:
    if isinstance(value, (bytes, bytearray, memoryview)):
        return _cell(value)
    if isinstance(value, (int, float, str, bool)) or value is None:
        return value
    return str(value)  # Decimal, dates, UUID, intervals


def _pg_geom(value: Any) -> Any:
    if value is None:
        return None
    if isinstance(value, str):  # hex EWKB
        return {"geometry": "ewkb", "bytes": len(value) // 2}
    return {"geometry": "ewkb", "bytes": len(bytes(value))}


def _pg_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None):
    import psycopg

    db, scrub = _pg_connect(conn, timeout_s)
    try:
        geom_oids = _pg_geometry_oids(db)
        try:
            # A named (server-side) cursor streams rows instead of loading
            # the whole result into memory.
            with db.cursor(name="columbus_q") as cur:
                cur.itersize = 2000
                cur.execute(sql)
                rows = []
                hit = False
                keep = None
                while True:
                    chunk = cur.fetchmany(2000)
                    if not chunk:
                        break
                    if keep_factory is not None:
                        if keep is None:
                            keep = keep_factory([d.name for d in cur.description], "ewkb")
                        chunk = [r for r in chunk if keep(r)]
                    room = max_rows - len(rows)
                    if len(chunk) > room:
                        rows.extend(chunk[:room])
                        hit = True
                        break
                    rows.extend(chunk)
                desc = cur.description or []
                columns = [d.name for d in desc]
                cells = [_pg_geom if d.type_code in geom_oids else _pg_cell for d in desc]
                geoms = {i: "ewkb" for i, d in enumerate(desc) if d.type_code in geom_oids}
            db.rollback()
            return columns, rows, hit, cells, geoms
        except psycopg.errors.QueryCanceled as e:
            raise WorkerError(TIMEOUT, f"query exceeded {timeout_s:g} s") from e
        except psycopg.errors.ReadOnlySqlTransaction as e:
            raise WorkerError(ENGINE_ERROR, "the database refused a write: this connection is read-only") from e
        except psycopg.Error as e:
            raise WorkerError(ENGINE_ERROR, scrub(_pg_error_text(e))) from None
    finally:
        db.close()


# Errors whose message names only things the query itself wrote (a column,
# table or function name), so it is safe to pass on as is.
# Not 42P01 (undefined table) or 42883 (undefined function): a cast to
# regclass or a function lookup puts a data value into that message.
_PG_NAME_ERRORS = {"42703", "42601", "42803", "42702", "42P10"}


def _pg_error_text(e) -> str:
    """A database error, safe to show the agent and write to the audit log.

    PostgreSQL error messages can quote data values ("invalid input syntax
    for type integer: \"<value>\"") and their DETAIL lines often do. Only
    the primary message is kept, and unless the error is about a name in the
    query, anything quoted in it is removed."""
    diag = getattr(e, "diag", None)
    state = getattr(e, "sqlstate", None) or (diag.sqlstate if diag else None) or "unknown"
    primary = diag.message_primary if diag else None
    if not primary:
        lines = str(e).splitlines()
        primary = lines[0] if lines else "error"
    if state not in _PG_NAME_ERRORS:
        primary = re.sub(r'"[^"]*"|\'[^\']*\'', '"…"', primary)
    return f"query failed: {primary} (SQLSTATE {state})"


def _pg_list(conn: dict[str, Any], timeout_s: float) -> list[dict[str, Any]]:
    import psycopg

    db, scrub = _pg_connect(conn, timeout_s)
    try:
        with db.cursor() as cur:
            # information_schema only lists tables this login may use, so the
            # database's own grants shape the answer too.
            cur.execute(
                "SELECT table_schema, table_name, table_type FROM information_schema.tables "
                "WHERE table_schema NOT IN ('pg_catalog', 'information_schema') "
                "AND table_schema NOT LIKE 'pg\\_%' ORDER BY 1, 2"
            )
            tables = {
                f"{s}.{t}": {"name": f"{s}.{t}", "type": "view" if kind == "VIEW" else "table"}
                for s, t, kind in cur.fetchall()
            }
            cur.execute(
                "SELECT table_schema, table_name, column_name, udt_name, is_nullable "
                "FROM information_schema.columns "
                "WHERE table_schema NOT IN ('pg_catalog', 'information_schema') "
                "ORDER BY table_schema, table_name, ordinal_position"
            )
            for s, t, col, typ, nullable in cur.fetchall():
                entry = tables.get(f"{s}.{t}")
                if entry is not None:
                    entry.setdefault("fields", []).append(
                        {"name": col, "type": typ, "not_null": nullable == "NO"}
                    )
            cur.execute(
                "SELECT n.nspname, c.relname, c.reltuples::bigint FROM pg_class c "
                "JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p')"
            )
            for s, t, n in cur.fetchall():
                entry = tables.get(f"{s}.{t}")
                if entry is not None and n is not None and n >= 0:
                    entry["estimated_rows"] = n
        try:
            with db.transaction(), db.cursor() as cur:
                cur.execute(
                    "SELECT f_table_schema, f_table_name, f_geometry_column, type, srid, "
                    "coord_dimension FROM geometry_columns"
                )
                geoms = cur.fetchall()
        except psycopg.Error:
            geoms = []  # no PostGIS
        for s, t, col, gtype, srid, dims in geoms:
            entry = tables.get(f"{s}.{t}")
            if entry is None:
                continue
            entry.update({"geometry_column": col, "geometry_type": gtype, "srs_id": srid, "dimensions": dims,
                          "crs_spec": {"org": "EPSG", "org_id": srid} if srid else None})
            try:
                with db.transaction(), db.cursor() as cur:
                    cur.execute("SELECT ST_EstimatedExtent(%s, %s, %s)::box2d::text", (s, t, col))
                    box = cur.fetchone()[0]
                if box:
                    nums = [float(x) for x in re.findall(r"-?[0-9.]+(?:e-?[0-9]+)?", box)]
                    if len(nums) == 4:
                        entry["extent"] = nums
            except psycopg.Error:
                pass  # no statistics yet
        db.rollback()
        return list(tables.values())
    except psycopg.Error as e:
        raise WorkerError(ENGINE_ERROR, scrub(f"listing failed: {e}".strip())) from None
    finally:
        db.close()


# ---------- shared by the Oracle and SQL Server engines ----------


def _ewkb(wkb, srid) -> bytes | None:
    """WKB as the database wrote it, plus its SRID, as EWKB: what the GIS
    engine and the plugin SDK read (the same as PostGIS hands over). ISO
    Z/M type codes (1000s) become EWKB flags, so every reader agrees."""
    if wkb is None:
        return None
    raw = bytes(wkb)
    if len(raw) < 5:
        return raw
    endian = "<" if raw[0] == 1 else ">"
    (code,) = struct.unpack_from(endian + "I", raw, 1)
    base = code & 0x0FFFFFFF
    flags = code & 0xE0000000
    if base >= 3000:
        flags |= 0xC0000000
        base -= 3000
    elif base >= 2000:
        flags |= 0x40000000
        base -= 2000
    elif base >= 1000:
        flags |= 0x80000000
        base -= 1000
    if isinstance(srid, (int, float)) and int(srid) > 0 and not flags & 0x20000000:
        return raw[:1] + struct.pack(endian + "II", base | flags | 0x20000000, int(srid)) + raw[5:]
    return raw[:1] + struct.pack(endian + "I", base | flags) + raw[5:]


def _like_patterns(globs) -> list[str] | None:
    """Dataset allow patterns' schema parts (`ROADS`, `GIS_*`) as LIKE
    patterns, or None when one of them could match any schema."""
    if not isinstance(globs, list) or not globs:
        return None
    out = []
    for g in globs:
        if not isinstance(g, str) or not g or g in ("*", "%"):
            return None
        p = g.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        out.append(p.replace("*", "%").replace("?", "_").upper())
    return out


def _scrub_quoted(text: str) -> str:
    return re.sub(r'"[^"]*"|\'[^\']*\'', '"…"', text)


def _quote_dq(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _quote_br(name: str) -> str:
    return "[" + name.replace("]", "]]") + "]"


def _end(db) -> None:
    """Roll back and close. A session a timeout already broke cannot roll
    back; the database rolls back when it goes, so that error is not the
    one to report."""
    try:
        db.rollback()
    except Exception:  # noqa: BLE001
        pass
    try:
        db.close()
    except Exception:  # noqa: BLE001
        pass


def _fetch_rows(cur, max_rows: int, convert, keep_factory, columns, fmt):
    """Rows from an open cursor, at most ``max_rows`` (after the bbox
    filter, when there is one), and whether there were more."""
    rows: list = []
    keep = keep_factory(columns, fmt) if keep_factory is not None else None
    while True:
        chunk = cur.fetchmany(2000)
        if not chunk:
            return rows, False
        chunk = [convert(r) for r in chunk]
        if keep is not None:
            chunk = [r for r in chunk if keep(r)]
        room = max_rows - len(rows)
        if len(chunk) > room:
            rows.extend(chunk[:room])
            return rows, True
        rows.extend(chunk)


# ---------- Oracle (python-oracledb, thin mode) ----------

ORACLE_KINDS = {"oracle", "sde-oracle"}
DB_MISSING = "install workers/python/requirements-db.txt in the engine environment"

# Errors that mean the database refused a write: a read-only transaction or
# session, or PL/SQL that tried to commit inside a query.
_ORA_WRITE_REFUSED = {1456, 28193, 34, 6519, 14551, 14552, 16000}
# Errors whose message names only things the query itself wrote.
_ORA_NAME_ERRORS = {904, 918, 923, 933, 936, 907, 979, 937, 942}
_ORA_TIMEOUT = {"DPY-4024", "ORA-01013", "ORA-03156", "DPY-4011"}


def _ora():
    try:
        import oracledb
    except ImportError as e:
        raise WorkerError(UNSUPPORTED, f"Oracle needs the oracledb package (python-oracledb); {DB_MISSING}") from e
    # Thin mode only (init_oracle_client is never called, so no Oracle
    # Client libraries load). LOBs come back as bytes and text.
    oracledb.defaults.fetch_lobs = False
    return oracledb


def _ora_error(e) -> tuple[int, str, str]:
    err = e.args[0] if getattr(e, "args", None) else None
    message = getattr(err, "message", None) or str(e)
    code = getattr(err, "code", 0) or 0
    full = getattr(err, "full_code", "") or ""
    return int(code), full, message


def _ora_error_text(e) -> str:
    """A database error, safe to show the agent: the first line of the
    primary message only (Oracle 23 adds help links and, in ORA-01722 and
    others, the value that failed), with quoted values removed unless the
    error is about a name in the query."""
    code, full, message = _ora_error(e)
    primary = (message.strip().splitlines() or ["error"])[0]
    primary = re.sub(r"^(ORA|DPY|DPI|PLS)-\d+:\s*", "", primary)
    if code not in _ORA_NAME_ERRORS:
        primary = _scrub_quoted(primary)
    return f"query failed: {primary} ({full or 'Oracle error'})"


def _ora_raise(e, timeout_s: float, scrub):
    code, full, _ = _ora_error(e)
    if full in _ORA_TIMEOUT:
        raise WorkerError(TIMEOUT, f"query exceeded {timeout_s:g} s") from None
    if code in _ORA_WRITE_REFUSED:
        raise WorkerError(ENGINE_ERROR, f"the database refused a write: this connection is read-only ({full})") from None
    raise WorkerError(ENGINE_ERROR, scrub(_ora_error_text(e))) from None


def _ora_connect(conn: dict[str, Any], timeout_s: float):
    """A thin-mode session that cannot write. The session is set
    read-only where the database supports it (READ_ONLY, Oracle 23ai and
    later), PL/SQL cannot commit in it (so an autonomous transaction in a
    function a query calls cannot write either), and its one transaction is
    SET TRANSACTION READ ONLY. Every call has a timeout."""
    oracledb = _ora()
    scrub = _Scrub(conn)
    user, password = conn.get("user"), conn.get("password")
    if not user or not password:
        raise WorkerError(INVALID_PARAMS, "an Oracle connection needs a user and a stored password (columbus-mcp secret set <id>): thin mode has no integrated sign-in")
    params: dict[str, Any] = {"user": user, "password": password, "tcp_connect_timeout": 15.0}
    sslmode = str(conn.get("sslmode") or "").lower()
    if conn.get("host"):
        if not conn.get("database"):
            raise WorkerError(INVALID_PARAMS, "an Oracle connection needs database: its service name (for example FREEPDB1)")
        params.update(host=conn["host"], port=int(conn.get("port") or 1521), service_name=conn["database"])
        if sslmode in ("require", "verify-ca", "verify-full"):
            params.update(protocol="tcps", ssl_server_dn_match=sslmode == "verify-full")
    elif conn.get("dsn"):
        params["dsn"] = conn["dsn"]
    else:
        raise WorkerError(INVALID_PARAMS, "an Oracle connection needs host (with database, the service name) or dsn")
    try:
        db = oracledb.connect(**params)
    except oracledb.Error as e:
        raise WorkerError(ENGINE_ERROR, scrub(f"cannot connect: {_ora_error(e)[2].splitlines()[0]}")) from None
    try:
        db.call_timeout = max(1, int(timeout_s * 1000))
        with db.cursor() as cur:
            try:
                cur.execute("ALTER SESSION SET READ_ONLY = TRUE")
            except oracledb.DatabaseError:
                pass  # before 23ai; the transaction below still applies
            cur.execute("ALTER SESSION DISABLE COMMIT IN PROCEDURE")
            cur.execute("SET TRANSACTION READ ONLY")
    except oracledb.Error as e:
        db.close()
        raise WorkerError(ENGINE_ERROR, scrub(f"cannot open a read-only session: {_ora_error_text(e)}")) from None
    return db, scrub


def _ora_object_type(oracledb, info):
    """(schema, name) of an object-typed result column, else None."""
    t = getattr(info, "type", None)
    if not isinstance(t, oracledb.DbObjectType):
        return None
    return (t.schema or "").upper(), (t.name or "").upper()


def _ora_describe(cur, sql: str):
    try:
        cur.parse(sql)
        if cur.description:
            return cur.description
    except Exception:  # noqa: BLE001 - the real execution reports it
        return None
    return None


def _ora_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None):
    oracledb = _ora()
    db, scrub = _ora_connect(conn, timeout_s)
    try:
        try:
            cur = db.cursor()
            cur.arraysize = 2000
            cur.prefetchrows = 2001
            # SDO_GEOMETRY comes back as WKB (with its SRID) converted by the
            # database; other object types (Esri ST_Geometry and the like)
            # as a placeholder, never as objects. The query is the gateway's,
            # checked; this outer SELECT is the worker's own.
            desc = _ora_describe(cur, sql)
            plan = []  # (kind, name) per output column
            if desc:
                for d in desc:
                    ot = _ora_object_type(oracledb, d)
                    if ot == ("MDSYS", "SDO_GEOMETRY"):
                        plan.append(("sdo", d.name))
                    elif ot is not None:
                        plan.append(("object", d.name, f"{ot[0]}.{ot[1]}"))
                    else:
                        plan.append(("plain", d.name))
            run = sql
            if any(p[0] != "plain" for p in plan):
                items = []
                for i, p in enumerate(plan):
                    col = "columbus_g." + _quote_dq(p[1])
                    if p[0] == "sdo":
                        items.append(f"CASE WHEN {col} IS NULL THEN NULL ELSE MDSYS.SDO_UTIL.TO_WKBGEOMETRY({col}) END AS {_quote_dq(p[1])}")
                        items.append(f"{col}.SDO_SRID AS \"COLUMBUS_SRID_{i}\"")
                    elif p[0] == "object":
                        label = f"unsupported: {p[2]}".replace("'", "")
                        items.append(f"CASE WHEN {col} IS NULL THEN NULL ELSE '{label}' END AS {_quote_dq(p[1])}")
                    else:
                        items.append(f"{col} AS {_quote_dq(p[1])}")
                run = f"SELECT {', '.join(items)} FROM ({sql}) columbus_g"
            cur.execute(run)
            if cur.description is None:
                raise WorkerError(ENGINE_ERROR, "the statement returned no rows; only a SELECT can run")
            raw_cols = [d.name for d in cur.description]
            if any(p[0] == "sdo" for p in plan):
                # Output column -> (WKB index, SRID index)
                out_idx, geoms, j = [], {}, 0
                for i, p in enumerate(plan):
                    if p[0] == "sdo":
                        geoms[len(out_idx)] = (j, j + 1)
                        out_idx.append(j)
                        j += 2
                    else:
                        out_idx.append(j)
                        j += 1
                columns = [p[1] for p in plan]

                def convert(r):
                    out = []
                    for k, idx in enumerate(out_idx):
                        if k in geoms:
                            w, s = geoms[k]
                            out.append(_ewkb(r[w], r[s]))
                        else:
                            out.append(r[idx])
                    return out

                geom_cols = {k: "ewkb" for k in geoms}
            else:
                columns = raw_cols
                geom_cols = {}

                def convert(r):
                    return list(r)

            rows, hit = _fetch_rows(cur, max_rows, convert, keep_factory, columns, "ewkb")
            cells = [_pg_geom if i in geom_cols else _pg_cell for i in range(len(columns))]
            return columns, rows, hit, cells, geom_cols
        except oracledb.Error as e:
            _ora_raise(e, timeout_s, scrub)
    finally:
        _end(db)


def _ora_srid_to_epsg(cur, srids) -> dict[int, int]:
    """Oracle SRIDs mapped to EPSG codes (8307 -> 4326 and so on); most
    Oracle SRIDs are EPSG codes already."""
    out = {}
    for s in srids:
        try:
            cur.execute("SELECT MDSYS.SDO_CS.MAP_ORACLE_SRID_TO_EPSG(:s) FROM DUAL", s=s)
            r = cur.fetchone()
            if r and r[0]:
                out[s] = int(r[0])
        except Exception:  # noqa: BLE001 - unknown SRID: no EPSG code
            pass
    return out


_SDO_GTYPES = {
    "POINT": "POINT", "LINE": "LINESTRING", "CURVE": "LINESTRING", "POLYGON": "POLYGON",
    "SURFACE": "POLYGON", "MULTIPOINT": "MULTIPOINT", "MULTILINE": "MULTILINESTRING",
    "MULTICURVE": "MULTILINESTRING", "MULTIPOLYGON": "MULTIPOLYGON", "MULTISURFACE": "MULTIPOLYGON",
    "COLLECTION": "GEOMETRYCOLLECTION",
}


def _ora_list(conn: dict[str, Any], timeout_s: float, schemas=None, only: str | None = None) -> list[dict[str, Any]]:
    """Tables and views the login can see (ALL_TABLES, ALL_VIEWS) in the
    schemas the connection's allow list names, never Oracle's own schemas.
    The worker reads the catalog here; the agent's queries never may."""
    oracledb = _ora()
    db, scrub = _ora_connect(conn, timeout_s)
    try:
        cur = db.cursor()
        cur.arraysize = 2000
        binds: dict[str, Any] = {}
        where = "owner NOT IN (SELECT username FROM all_users WHERE oracle_maintained = 'Y')"
        if only:
            owner, _, table = only.replace('"', "").rpartition(".")
            if not owner:
                return []
            where += " AND UPPER(owner) = UPPER(:o) AND UPPER(%s) = UPPER(:t)"
            binds.update(o=owner, t=table)
        else:
            pats = _like_patterns(schemas)
            if pats:
                ors = []
                for i, p in enumerate(pats):
                    binds[f"s{i}"] = p
                    ors.append(f"UPPER(owner) LIKE :s{i} ESCAPE '\\'")
                where += " AND (" + " OR ".join(ors) + ")"
        tw = where.replace("%s", "table_name")
        vw = where.replace("%s", "view_name")
        cur.execute(
            f"SELECT owner, table_name, 'table', num_rows FROM all_tables WHERE {tw} "
            "AND nested = 'NO' AND secondary = 'N' AND dropped = 'NO' AND (iot_type IS NULL OR iot_type = 'IOT') "
            f"UNION ALL SELECT owner, view_name, 'view', NULL FROM all_views WHERE {vw} ORDER BY 1, 2",
            binds,
        )
        tables: dict[str, dict[str, Any]] = {}
        for owner, name, kind, nrows in cur.fetchall():
            if name.startswith("MDRT_") or name.startswith("BIN$"):
                continue  # spatial index tables, the recycle bin
            e = {"name": f"{owner}.{name}", "type": kind}
            if nrows is not None:
                e["estimated_rows"] = int(nrows)
            tables[e["name"]] = e
        cw = where.replace("%s", "table_name")
        cur.execute(
            "SELECT owner, table_name, column_name, data_type, data_type_owner, nullable "
            f"FROM all_tab_columns WHERE {cw} ORDER BY owner, table_name, column_id",
            binds,
        )
        for owner, table, col, dtype, downer, nullable in cur.fetchall():
            entry = tables.get(f"{owner}.{table}")
            if entry is None:
                continue
            typ = f"{downer}.{dtype}" if downer and downer not in ("PUBLIC",) else dtype
            entry.setdefault("fields", []).append({"name": col, "type": typ, "not_null": nullable == "N"})
            if downer == "MDSYS" and dtype == "SDO_GEOMETRY":
                entry.setdefault("geometry_column", col)
                entry.setdefault("geometry_type", "GEOMETRY")
            elif dtype == "ST_GEOMETRY" or (downer and dtype.startswith("ST_")):
                entry.setdefault("unsupported_geometry", []).append({"column": col, "type": typ})
        # Spatial metadata (absent without Oracle Spatial).
        srids = {}
        try:
            cur.execute(f"SELECT owner, table_name, column_name, srid FROM all_sdo_geom_metadata WHERE {cw}", binds)
            for owner, table, col, srid in cur.fetchall():
                entry = tables.get(f"{owner}.{table}")
                if entry is not None and entry.get("geometry_column") == col and srid is not None:
                    srids[entry["name"]] = int(srid)
        except oracledb.DatabaseError:
            pass
        try:
            iw = where.replace("owner", "i.table_owner").replace("%s", "i.table_name")
            cur.execute(
                "SELECT i.table_owner, i.table_name, i.column_name, m.sdo_layer_gtype "
                "FROM all_sdo_index_info i JOIN all_sdo_index_metadata m "
                f"ON m.sdo_index_owner = i.index_owner AND m.sdo_index_name = i.index_name WHERE {iw}",
                binds,
            )
            for owner, table, col, gtype in cur.fetchall():
                entry = tables.get(f"{owner}.{table}")
                col = str(col or "").strip('"')
                if entry is not None and entry.get("geometry_column") == col and gtype:
                    entry["geometry_type"] = _SDO_GTYPES.get(str(gtype).upper(), "GEOMETRY")
        except oracledb.DatabaseError:
            pass
        if only:
            # Describing one dataset with no type on its spatial index: take
            # it from one row (only SDO_GTYPE is read: its last digit is the
            # type).
            for entry in tables.values():
                if entry.get("geometry_column") and entry.get("geometry_type") == "GEOMETRY":
                    s, _, t = entry["name"].partition(".")
                    try:
                        cur.execute(
                            f"SELECT g.{_quote_dq(entry['geometry_column'])}.SDO_GTYPE FROM {_quote_dq(s)}.{_quote_dq(t)} g "
                            f"WHERE g.{_quote_dq(entry['geometry_column'])} IS NOT NULL AND ROWNUM = 1"
                        )
                        r = cur.fetchone()
                    except oracledb.DatabaseError:
                        continue
                    kind = {1: "POINT", 2: "LINESTRING", 3: "POLYGON", 4: "GEOMETRYCOLLECTION",
                            5: "MULTIPOINT", 6: "MULTILINESTRING", 7: "MULTIPOLYGON"}
                    if r and r[0]:
                        entry["geometry_type"] = kind.get(int(r[0]) % 10, "GEOMETRY")
        epsg = _ora_srid_to_epsg(cur, sorted(set(srids.values())))
        for name, srid in srids.items():
            e = tables[name]
            e["srs_id"] = srid
            code = epsg.get(srid)
            e["crs_spec"] = {"org": "EPSG", "org_id": code} if code else {"org_id": srid}
        return list(tables.values())
    except oracledb.Error as e:
        raise WorkerError(ENGINE_ERROR, scrub(_ora_error_text(e).replace("query failed", "listing failed"))) from None
    finally:
        _end(db)


# ---------- SQL Server (pyodbc, ODBC Driver 18) ----------

MSSQL_KINDS = {"sqlserver", "mssql", "sde-sqlserver", "azure-sql"}
MSSQL_DRIVER = "ODBC Driver 18 for SQL Server"
# Errors whose message names only things the query itself wrote.
_MS_NAME_ERRORS = {102, 156, 207, 208, 209, 4104, 8120, 1038, 4145, 116}


def _odbc_value(v: Any) -> str:
    """One connection-string value, braced so `;` or `=` in it cannot add
    another keyword."""
    return "{" + str(v).replace("}", "}}") + "}"


def _ms_encrypt(sslmode: str | None) -> dict[str, str]:
    """`sslmode` in ODBC Driver 18's terms. The default, as the driver's,
    is encrypted with the certificate verified."""
    mode = str(sslmode or "verify-full").lower()
    if mode == "disable":
        return {"Encrypt": "no"}
    if mode in ("allow", "prefer", "require"):
        return {"Encrypt": "yes", "TrustServerCertificate": "yes"}
    if mode == "strict":
        return {"Encrypt": "strict"}
    return {"Encrypt": "yes", "TrustServerCertificate": "no"}  # verify-ca, verify-full


def _ms_datetimeoffset(raw):
    import datetime

    t = struct.unpack("<6hI2h", raw)
    tz = datetime.timezone(datetime.timedelta(hours=t[7], minutes=t[8]))
    return datetime.datetime(t[0], t[1], t[2], t[3], t[4], t[5], t[6] // 1000, tz).isoformat()


def _ms_connect(conn: dict[str, Any], timeout_s: float):
    """A session whose every statement runs in a transaction that is always
    rolled back. SQL Server has no read-only session, so this is a second
    line of defence: the guarantee is a login with SELECT only."""
    try:
        import pyodbc
    except ImportError as e:
        raise WorkerError(UNSUPPORTED, f"SQL Server needs the pyodbc package and {MSSQL_DRIVER}; {DB_MISSING}") from e
    scrub = _Scrub(conn)
    host = conn.get("host")
    if not host:
        raise WorkerError(INVALID_PARAMS, "a SQL Server connection needs host (and database)")
    server = f"tcp:{host},{int(conn['port'])}" if conn.get("port") else f"tcp:{host}"
    parts: dict[str, Any] = {"DRIVER": MSSQL_DRIVER, "SERVER": server, "ApplicationIntent": "ReadOnly", "APP": "columbus-worker"}
    if conn.get("database"):
        parts["DATABASE"] = conn["database"]
    parts.update(_ms_encrypt(conn.get("sslmode")))
    if conn.get("password"):
        parts.update(UID=conn.get("user") or "", PWD=conn["password"])
    else:
        parts["Trusted_Connection"] = "yes"  # integrated sign-in
    text = ";".join(f"{k}={_odbc_value(v)}" for k, v in parts.items())
    try:
        db = pyodbc.connect(text, autocommit=False, timeout=15)
    except pyodbc.Error as e:
        raise WorkerError(ENGINE_ERROR, scrub(f"cannot connect: {_ms_error_text(e, prefix='')}")) from None
    db.timeout = max(1, int(math.ceil(timeout_s)))  # per statement, seconds
    for sql_type in (-150, -151):  # sql_variant, CLR types (hierarchyid)
        db.add_output_converter(sql_type, lambda v: v)
    db.add_output_converter(-155, lambda v: None if v is None else _ms_datetimeoffset(v))
    return db, scrub


def _ms_error_parts(e) -> tuple[str, int | None, str]:
    args = getattr(e, "args", ()) or ()
    state = str(args[0]) if args else "unknown"
    msg = str(args[1]) if len(args) > 1 else str(e)
    first = msg.split("; [")[0]
    first = re.sub(r"^(\s*\[[^\]]*\])+", "", first).strip()
    first = re.sub(r"\s*\((SQL\w+)\)\s*$", "", first)
    m = re.search(r"\((\d+)\)\s*$", first)
    number = int(m.group(1)) if m else None
    if m:
        first = first[: m.start()].rstrip()
    return state, number, first


def _ms_error_text(e, prefix: str = "query failed: ") -> str:
    """A database error, safe to show the agent: the first message only,
    and quoted values removed unless the error is about a name in the
    query ("Conversion failed when converting the varchar value '<value>'")."""
    state, number, primary = _ms_error_parts(e)
    if number not in _MS_NAME_ERRORS:
        primary = _scrub_quoted(primary)
    tag = f"SQL Server error {number}" if number is not None else f"SQLSTATE {state}"
    return f"{prefix}{primary or 'error'} ({tag})"


def _ms_geometry_columns(cur, sql: str):
    """[(name, kind)] of the result's columns, kind 'geometry'/'geography'
    or None, from the server's description of the query (not run)."""
    try:
        cur.execute(
            "SELECT name, system_type_name FROM sys.dm_exec_describe_first_result_set(?, NULL, 0) "
            "WHERE is_hidden = 0 ORDER BY column_ordinal",
            sql,
        )
        rows = cur.fetchall()
    except Exception:  # noqa: BLE001 - the real execution reports it
        return None
    out = []
    for name, typ in rows:
        t = str(typ or "").lower()
        out.append((name, t if t in ("geometry", "geography") else None))
    return out


def _ms_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None):
    import pyodbc

    db, scrub = _ms_connect(conn, timeout_s)
    try:
        try:
            cur = db.cursor()
            cols = _ms_geometry_columns(cur, sql)
            names = [c[0] for c in cols or []]
            wrap = (
                cols
                and any(k for _, k in cols)
                and all(isinstance(n, str) and n for n in names)
                and len({n.lower() for n in names}) == len(names)
            )
            run = sql
            if wrap:
                items = []
                for i, (n, k) in enumerate(cols):
                    c = "columbus_g." + _quote_br(n)
                    if k:
                        items += [f"{c}.STAsBinary() AS {_quote_br(n)}", f"{c}.STSrid AS [columbus_srid_{i}]"]
                    else:
                        items.append(f"{c} AS {_quote_br(n)}")
                run = f"SELECT {', '.join(items)} FROM ({sql}) AS columbus_g"
            cur.execute(run)
            if cur.description is None:
                raise WorkerError(ENGINE_ERROR, "the statement returned no rows; only a SELECT can run (nothing it did was kept)")
            if wrap:
                out_idx, geoms, j = [], {}, 0
                for i, (n, k) in enumerate(cols):
                    if k:
                        geoms[i] = (j, j + 1)
                        j += 2
                    else:
                        out_idx.append((i, j))
                        j += 1
                columns = names
                plain = dict(out_idx)

                def convert(r):
                    return [_ewkb(r[geoms[i][0]], r[geoms[i][1]]) if i in geoms else r[plain[i]] for i in range(len(columns))]

                geom_cols = {i: "ewkb" for i in geoms}
            else:
                columns = [d[0] for d in cur.description]
                geom_cols = {}

                def convert(r):
                    return list(r)

            rows, hit = _fetch_rows(cur, max_rows, convert, keep_factory, columns, "ewkb")
            if hit:
                cur.cancel()
            cells = [_pg_geom if i in geom_cols else _pg_cell for i in range(len(columns))]
            return columns, rows, hit, cells, geom_cols
        except pyodbc.Error as e:
            state, _, _ = _ms_error_parts(e)
            if state == "HYT00":
                raise WorkerError(TIMEOUT, f"query exceeded {timeout_s:g} s") from None
            raise WorkerError(ENGINE_ERROR, scrub(_ms_error_text(e))) from None
    finally:
        _end(db)  # always rolled back: nothing a statement did is kept


def _ms_list(conn: dict[str, Any], timeout_s: float, schemas=None, only: str | None = None) -> list[dict[str, Any]]:
    """Tables and views the login may use (INFORMATION_SCHEMA), in the
    schemas the connection's allow list names."""
    import pyodbc

    db, scrub = _ms_connect(conn, timeout_s)
    try:
        cur = db.cursor()
        where, binds = "TABLE_SCHEMA NOT IN ('sys', 'INFORMATION_SCHEMA')", []
        if only:
            schema, _, table = only.replace("[", "").replace("]", "").rpartition(".")
            if not schema:
                return []
            where += " AND TABLE_SCHEMA = ? AND TABLE_NAME = ?"
            binds += [schema, table]
        else:
            pats = _like_patterns(schemas)
            if pats:
                where += " AND (" + " OR ".join("UPPER(TABLE_SCHEMA) LIKE ? ESCAPE '\\'" for _ in pats) + ")"
                binds += pats
        cur.execute(f"SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE FROM INFORMATION_SCHEMA.TABLES WHERE {where} ORDER BY 1, 2", binds)
        tables = {
            f"{s}.{t}": {"name": f"{s}.{t}", "type": "view" if kind == "VIEW" else "table"}
            for s, t, kind in cur.fetchall()
        }
        cur.execute(
            "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, DATA_TYPE, IS_NULLABLE FROM INFORMATION_SCHEMA.COLUMNS "
            f"WHERE {where} ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION",
            binds,
        )
        for s, t, col, typ, nullable in cur.fetchall():
            entry = tables.get(f"{s}.{t}")
            if entry is None:
                continue
            entry.setdefault("fields", []).append({"name": col, "type": typ, "not_null": nullable == "NO"})
            if str(typ).lower() in ("geometry", "geography") and "geometry_column" not in entry:
                entry.update(geometry_column=col, geometry_type=str(typ).upper())
        try:
            cur.execute(
                "SELECT s.name, o.name, SUM(p.rows) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id "
                "JOIN sys.partitions p ON p.object_id = o.object_id AND p.index_id IN (0, 1) "
                "WHERE o.type = 'U' GROUP BY s.name, o.name"
            )
            for s, t, n in cur.fetchall():
                entry = tables.get(f"{s}.{t}")
                if entry is not None and n is not None:
                    entry["estimated_rows"] = int(n)
        except pyodbc.Error:
            pass  # no VIEW DEFINITION: no estimates
        # SQL Server keeps no SRID per column: take it from one row (only
        # the SRID is read), for at most 100 tables.
        for entry in [e for e in tables.values() if e.get("geometry_column")][:100]:
            s, _, t = entry["name"].partition(".")
            col = _quote_br(entry["geometry_column"])
            try:
                cur.execute(f"SELECT TOP (1) {col}.STSrid FROM {_quote_br(s)}.{_quote_br(t)} WHERE {col} IS NOT NULL")
                r = cur.fetchone()
            except pyodbc.Error:
                continue
            if r and r[0]:
                entry["srs_id"] = int(r[0])
                entry["crs_spec"] = {"org": "EPSG", "org_id": int(r[0])}
        return list(tables.values())
    except pyodbc.Error as e:
        raise WorkerError(ENGINE_ERROR, scrub(_ms_error_text(e, prefix="listing failed: "))) from None
    finally:
        _end(db)


# ---------- engine-agnostic methods ----------


def list_datasets(params: dict[str, Any]) -> dict[str, Any]:
    conn = _connection(params)
    kind = conn["kind"].lower()
    timeout_s = float(params.get("timeout_s", 30))
    if kind in SQLITE_KINDS:
        return {"datasets": _sqlite_list(conn, timeout_s)}
    if kind in PG_KINDS:
        out = {"datasets": _pg_list(conn, timeout_s)}
        if params.get("extension_functions"):
            out["extension_functions"] = _pg_extension_functions(conn, timeout_s)
        return out
    only = params.get("only") if isinstance(params.get("only"), str) else None
    if kind in ORACLE_KINDS:
        return {"datasets": _ora_list(conn, timeout_s, params.get("schemas"), only)}
    if kind in MSSQL_KINDS:
        return {"datasets": _ms_list(conn, timeout_s, params.get("schemas"), only)}
    raise WorkerError(UNSUPPORTED, f"list_datasets does not support {conn['kind']!r} yet")


POSTGIS_EXTENSIONS = ("postgis", "postgis_raster", "postgis_sfcgal", "postgis_topology")


def _pg_extension_functions(conn: dict[str, Any], timeout_s: float) -> dict[str, str]:
    """Functions the PostGIS extensions installed (upper-case name -> schema),
    from pg_depend: only these may be called as spatial functions in free SQL,
    not whatever else in the database happens to be named st_..."""
    db, scrub = _pg_connect(conn, timeout_s)
    try:
        with db.cursor() as cur:
            cur.execute(
                "SELECT DISTINCT upper(p.proname), n.nspname FROM pg_depend d "
                "JOIN pg_extension e ON d.refclassid = 'pg_extension'::regclass AND d.refobjid = e.oid "
                "JOIN pg_proc p ON d.classid = 'pg_proc'::regclass AND d.objid = p.oid "
                "JOIN pg_namespace n ON n.oid = p.pronamespace "
                "WHERE e.extname = ANY(%s)",
                (list(POSTGIS_EXTENSIONS),),
            )
            return {name: schema for name, schema in cur.fetchall()}
    except Exception as e:  # noqa: BLE001
        raise WorkerError(ENGINE_ERROR, scrub(f"could not list PostGIS functions: {e}")) from None
    finally:
        db.close()


def _fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None):
    kind = conn["kind"].lower()
    if kind in SQLITE_KINDS:
        return _sqlite_fetch(conn, sql, max_rows, timeout_s, keep_factory)
    if kind in PG_KINDS:
        return _pg_fetch(conn, sql, max_rows, timeout_s, keep_factory)
    if kind in ORACLE_KINDS:
        return _ora_fetch(conn, sql, max_rows, timeout_s, keep_factory)
    if kind in MSSQL_KINDS:
        return _ms_fetch(conn, sql, max_rows, timeout_s, keep_factory)
    raise WorkerError(UNSUPPORTED, f"reading {conn['kind']!r} connections is not supported yet")


def _hidden_mask(columns: list[str], patterns) -> list[bool]:
    hide = [p.lower() for p in patterns or [] if isinstance(p, str)]
    return [not any(fnmatch.fnmatchcase(c.lower(), p) for p in hide) for c in columns]


def extract(params: dict[str, Any]) -> dict[str, Any]:
    """Write a governed read to an extract for an engine plugin.

    The SQL arrives checked and rewritten by the gateway. Every row read is
    written, raw geometry included, to a SQLite file with the table ``data``
    and the metadata table ``columbus_extract`` (docs/worker-protocol.md).
    Hidden fields are dropped. Stops at ``max_rows`` and says so, so the
    gateway can refuse to hand a plugin a silently truncated dataset.
    """
    conn = _connection(params)
    sql = params.get("sql")
    max_rows = params.get("max_rows")
    out = params.get("output_path")
    if not isinstance(sql, str) or not isinstance(max_rows, int) or max_rows < 1 or not isinstance(out, str):
        raise WorkerError(INVALID_PARAMS, "sql, a positive max_rows and output_path are required")
    timeout_s = float(params.get("timeout_s", 120))
    columns, rows, hit, _cells, geoms = _fetch(conn, sql, max_rows, timeout_s)
    keep = _hidden_mask(columns, params.get("hide_fields"))
    hidden = [c for c, k in zip(columns, keep) if not k]
    kept = [i for i, k in enumerate(keep) if k]
    geom_idx = next((i for i in kept if i in geoms), None)

    def raw(i, v):
        if v is None:
            return None
        if i in geoms:
            return bytes.fromhex(v) if isinstance(v, str) else bytes(v)
        if isinstance(v, (int, float, str, bytes)):
            return v
        if isinstance(v, memoryview):
            return bytes(v)
        return str(v)

    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    if os.path.exists(out):
        os.remove(out)
    db = sqlite3.connect(out)
    try:
        cols_sql = ", ".join('"' + columns[i].replace('"', '""') + '"' for i in kept)
        db.execute(f'CREATE TABLE "data" ({cols_sql})')
        marks = ", ".join("?" for _ in kept)
        db.executemany(f'INSERT INTO "data" VALUES ({marks})', ([raw(i, r[i]) for i in kept] for r in rows))
        srid = ""
        if geom_idx is not None:
            sample = next((r[geom_idx] for r in rows if r[geom_idx] is not None), None)
            if sample is not None:
                try:
                    g = decode_geometry(raw(geom_idx, sample), geoms[geom_idx])
                    srid = "" if g is None or g.srid is None else str(g.srid)
                except ValueError:
                    pass
        meta = {
            "connection": str(params.get("connection_id") or ""),
            "dataset": str(params.get("dataset") or ""),
            "rows": str(len(rows)),
            "geometry_column": columns[geom_idx] if geom_idx is not None else "",
            "geometry_format": geoms.get(geom_idx, "") if geom_idx is not None else "",
            "srid": srid,
            "labels": json.dumps(params.get("labels") or []),
        }
        db.execute("CREATE TABLE columbus_extract (key TEXT PRIMARY KEY, value TEXT)")
        db.executemany("INSERT INTO columbus_extract VALUES (?, ?)", meta.items())
        db.commit()
    finally:
        db.close()
    return {
        "rows": len(rows),
        "hit_read_limit": hit,
        "columns": [columns[i] for i in kept],
        "hidden_columns": hidden,
        "geometry_column": meta["geometry_column"] or None,
        "srid": meta["srid"] or None,
    }


def sql_select(params: dict[str, Any]) -> dict[str, Any]:
    """Run a checked SELECT read-only.

    Reads at most ``max_rows``. Returns the first ``return_rows`` rows; when
    more were read, writes every row read to ``output_path`` (CSV, inside the
    session workspace, already resolved by the gateway) so large results stay
    on the machine instead of going to the AI provider.
    """
    conn = _connection(params)
    sql = params.get("sql")
    max_rows = params.get("max_rows")
    return_rows = params.get("return_rows", 500)
    output_path = params.get("output_path")
    if not isinstance(sql, str) or not isinstance(max_rows, int) or max_rows < 1:
        raise WorkerError(INVALID_PARAMS, "sql and a positive max_rows are required")
    if not isinstance(return_rows, int) or return_rows < 0:
        raise WorkerError(INVALID_PARAMS, "return_rows must be a non-negative integer")
    timeout_s = float(params.get("timeout_s", 120))
    kind = conn["kind"].lower()
    started = time.monotonic()
    columns, all_rows, hit_limit, cells, _ = _fetch(conn, sql, max_rows, timeout_s)

    # Fields organization policy hides never leave the worker: not in the
    # rows returned, and not in the workspace file, which can later be sent
    # to another provider as a file handle.
    keep = _hidden_mask(columns, params.get("hide_fields"))
    hidden = [c for c, k in zip(columns, keep) if not k]
    columns = [c for c, k in zip(columns, keep) if k]

    def convert(row):
        return [f(v) for f, v, k in zip(cells, row, keep) if k]

    written = None
    if len(all_rows) > return_rows and isinstance(output_path, str) and output_path:
        os.makedirs(os.path.dirname(output_path) or ".", exist_ok=True)
        with open(output_path, "w", newline="", encoding="utf-8") as f:
            w = csv.writer(f)
            w.writerow(columns)
            for row in all_rows:
                w.writerow([json.dumps(v) if isinstance(v, dict) else v for v in convert(row)])
        written = output_path
    return {
        "columns": columns,
        "hidden_columns": hidden,
        "rows": [convert(row) for row in all_rows[:return_rows]],
        "rows_read": len(all_rows),
        "rows_returned": min(len(all_rows), return_rows),
        "hit_read_limit": hit_limit,
        "output_path": written,
        "seconds": round(time.monotonic() - started, 3),
    }


# ---------- GIS tools (GDAL, GEOS, PROJ through columbus_gis) ----------

GIS_MISSING = "needs the GIS engine (geopandas with pyogrio/GDAL, shapely and pyproj) in the engine environment"


def _gis():
    try:
        import columbus_gis

        return columbus_gis
    except ImportError as e:
        raise WorkerError(UNSUPPORTED, f"{GIS_MISSING} ({e})") from None


def _gis_versions():
    """Versions of the GIS engine's packages, or None when it is not
    installed. The libraries' own versions (GDAL, GEOS, PROJ) are added once
    the engine has been loaded by a GIS call."""
    names = ("geopandas", "pyogrio", "shapely", "pyproj")
    found = {n: _optional_version(n) for n in names}
    if not all(found.values()):
        return None
    loaded = sys.modules.get("columbus_gis")
    if loaded is not None:
        try:
            found.update(loaded.versions())
        except Exception:  # noqa: BLE001 - still loading
            pass
    return found


def _refused(e) -> WorkerError:
    return WorkerError(INVALID_PARAMS, str(e))


def _find(entries, dataset: str):
    """The dataset a GIS tool names: an exact match, else the one entry that
    matches without regard to case. Two entries differing only by case
    (roads."County" and roads.county) are refused, not guessed."""
    want = dataset.replace('"', "").replace("[", "").replace("]", "")
    for e in entries:
        if e["name"] == want:
            return e
    folded = [e for e in entries if e["name"].lower() == want.lower()]
    if len(folded) > 1:
        raise WorkerError(INVALID_PARAMS, f"{dataset!r} matches more than one dataset that differ only by case; use sql_select with a quoted name")
    return folded[0] if folded else None


def describe_dataset(params: dict[str, Any]) -> dict[str, Any]:
    """Fields, geometry, CRS facts and extent of one dataset, the row count
    under the connection's row filter (``count_sql``, checked by the
    gateway), and CRS checks."""
    g = _gis()
    conn = _connection(params)
    timeout_s = float(params.get("timeout_s", 60))
    # The one dataset only, where the engine can list it alone (Oracle and
    # SQL Server catalogs can be very large).
    entries = list_datasets({"connection": conn, "timeout_s": timeout_s, "only": str(params.get("dataset") or "")})["datasets"]
    entry = _find(entries, str(params.get("dataset") or ""))
    if entry is None:
        raise WorkerError(INVALID_PARAMS, f"no dataset {params.get('dataset')!r} on this connection")
    keep = _hidden_mask([f["name"] for f in entry.get("fields", [])], params.get("hide_fields"))
    entry["fields"] = [f for f, k in zip(entry.get("fields", []), keep) if k]
    rows = None
    if isinstance(params.get("count_sql"), str):
        _, r, _, _, _ = _fetch(conn, params["count_sql"], 2, timeout_s)
        rows = int(r[0][0]) if r else 0
    crs = g.crs_from_spec(entry.get("crs_spec")) if entry.get("geometry_column") else None
    checks = g.crs_checks(crs, entry.get("extent"), has_geometry=bool(entry.get("geometry_column")), what="The dataset")
    visible = {f["name"] for f in entry["fields"]}
    for u in entry.pop("unsupported_geometry", None) or []:
        if u["column"] not in visible:
            continue
        checks.append(g.check(
            "geometry.unsupported", "warn",
            f"Column {u['column']} is {u['type']}, a geometry type Columbus cannot read here: its values come back as a placeholder, not shapes",
            detail={"column": u["column"], "type": u["type"]},
            fix="Read this layer through ArcGIS (the ArcPy worker, when available), or ask the DBA for a view or copy with SDO_GEOMETRY; the Esri ST_ functions are database code and are not called"))
    entry.update({"rows": rows, "crs": g.crs_facts(crs), "checks": checks})
    return entry


def query_layer(params: dict[str, Any]) -> dict[str, Any]:
    """Run a checked query and save the result as a layer in the workspace
    (GeoPackage with geometry, CSV without). Optional ``bbox`` keeps only
    rows whose geometry meets the box (moved into the layer's CRS)."""
    g = _gis()
    conn = _connection(params)
    sql = params.get("sql")
    max_rows = params.get("max_rows")
    if not isinstance(sql, str) or not isinstance(max_rows, int) or max_rows < 1:
        raise WorkerError(INVALID_PARAMS, "sql and a positive max_rows are required")
    return_rows = int(params.get("return_rows", 500))
    timeout_s = float(params.get("timeout_s", 120))
    geom_col = params.get("geometry_column")
    layer_crs = g.crs_from_spec(params.get("crs_spec"))
    bbox_crs = layer_crs
    if params.get("bbox_crs"):
        bbox_crs = g.crs_from_spec({"code": params.get("bbox_crs")})
        if bbox_crs is None:
            raise WorkerError(INVALID_PARAMS, 'bbox_crs must be an EPSG code such as "EPSG:4326"')
    try:
        keep_geom = g.bbox_filter(params.get("bbox"), bbox_crs, layer_crs)
    except g.GISError as e:
        raise _refused(e) from None

    keep_factory = None
    if keep_geom is not None:
        if not geom_col:
            raise WorkerError(INVALID_PARAMS, "bbox needs a dataset with geometry")

        def keep_factory(columns, fmt):
            i = columns.index(geom_col)
            return lambda row: keep_geom(g.decode(row[i], fmt))

    columns, rows, hit, cells, geoms = _fetch(conn, sql, max_rows, timeout_s, keep_factory)
    keep = _hidden_mask(columns, params.get("hide_fields"))
    hidden = [c for c, k in zip(columns, keep) if not k]
    gi = columns.index(geom_col) if geom_col in columns else None
    fmt = geoms.get(gi, "gpkg") if gi is not None else None
    kept_idx = [i for i, k in enumerate(keep) if k and i != gi]
    attr_cols = [columns[i] for i in kept_idx]
    attr_rows = [[r[i] for i in kept_idx] for r in rows]
    shapes = [g.decode(r[gi], fmt) for r in rows] if gi is not None else None
    layer = g.build_layer(attr_cols, attr_rows, geom_col if gi is not None else None, shapes, layer_crs)
    stem = params.get("output_stem")
    path = g.write_layer(layer, stem, layer=str(params.get("layer_name") or "query")) if isinstance(stem, str) and stem else None
    checks = []
    if gi is not None:
        summary = g.layer_summary(layer)
        checks = g.crs_checks(layer_crs, summary["extent"], what="The result") + g.geometry_checks(layer.geometry, what="The result")
    else:
        summary = g.layer_summary(layer)
    out_cols = attr_cols + ([geom_col] if gi is not None else [])
    shown = []
    for r in rows[:return_rows]:
        vals = [cells[i](r[i]) for i in kept_idx]
        if gi is not None:
            vals.append(cells[gi](r[gi]))
        shown.append(vals)
    return {
        "columns": out_cols,
        "rows": shown,
        "rows_read": len(rows),
        "rows_returned": len(shown),
        "hit_read_limit": hit,
        "hidden_columns": hidden,
        "output_path": path,
        "summary": summary,
        "checks": checks,
    }


def vector_op(params: dict[str, Any]) -> dict[str, Any]:
    g = _gis()
    inputs = params.get("inputs")
    stem = params.get("output_stem")
    if not isinstance(inputs, dict) or not isinstance(stem, str):
        raise WorkerError(INVALID_PARAMS, "inputs and output_stem are required")
    op_params = params.get("params") or {}
    if not isinstance(op_params, dict):
        raise WorkerError(INVALID_PARAMS, "params must be an object")
    # Names starting with "_" are set by the server, never by the caller.
    op_params = {k: v for k, v in op_params.items() if not str(k).startswith("_")}
    op_params["_max_rows"] = params.get("max_rows")
    try:
        return g.vector_op(str(params.get("op")), inputs, op_params, stem)
    except g.GISError as e:
        raise _refused(e) from None


def verify_layer(params: dict[str, Any]) -> dict[str, Any]:
    g = _gis()
    path = params.get("path")
    if not isinstance(path, str):
        raise WorkerError(INVALID_PARAMS, "path is required")
    try:
        return g.verify_layer(path)
    except g.GISError as e:
        raise _refused(e) from None


def read_rows(params: dict[str, Any]) -> dict[str, Any]:
    g = _gis()
    path = params.get("path")
    if not isinstance(path, str):
        raise WorkerError(INVALID_PARAMS, "path is required")
    try:
        offset = max(0, int(params.get("offset") or 0))
        limit = max(1, min(500, int(params.get("limit") or 50)))
    except (TypeError, ValueError):
        raise WorkerError(INVALID_PARAMS, "offset and limit must be whole numbers") from None
    fields = params.get("fields")
    if fields is not None and (not isinstance(fields, list) or not all(isinstance(f, str) for f in fields)):
        raise WorkerError(INVALID_PARAMS, "fields must be a list of names")
    try:
        return g.read_rows(path, offset, limit, fields)
    except g.GISError as e:
        raise _refused(e) from None


def geojson_layer(params: dict[str, Any]) -> dict[str, Any]:
    g = _gis()
    pages, stem = params.get("pages"), params.get("out_stem")
    if not isinstance(pages, list) or not all(isinstance(x, str) for x in pages) or not isinstance(stem, str):
        raise WorkerError(INVALID_PARAMS, "pages and out_stem are required")
    try:
        cap = max(1, int(params.get("max_features") or 200_000))
    except (TypeError, ValueError):
        raise WorkerError(INVALID_PARAMS, "max_features must be a whole number") from None
    try:
        return g.geojson_pages_to_layer(pages, stem, cap)
    except g.GISError as e:
        raise _refused(e) from None


def export_layer(params: dict[str, Any]) -> dict[str, Any]:
    g = _gis()
    path, stem = params.get("path"), params.get("out_stem")
    if not isinstance(path, str) or not isinstance(stem, str):
        raise WorkerError(INVALID_PARAMS, "path and out_stem are required")
    rename = params.get("rename") or None
    if rename is not None and (not isinstance(rename, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in rename.items())):
        raise WorkerError(INVALID_PARAMS, "rename must map field names to field names")
    try:
        return g.export_layer(path, stem, str(params.get("format") or "gpkg"), params.get("crs"), rename)
    except g.GISError as e:
        raise _refused(e) from None


# ---------- Rasters (columbus_raster: rasterio and GDAL) ----------

RASTER_MISSING = "needs the raster engine (rasterio, with the GIS engine) in the engine environment"


def _raster():
    try:
        import columbus_raster

        return columbus_raster
    except ImportError as e:
        raise WorkerError(UNSUPPORTED, f"{RASTER_MISSING} ({e})") from None


def describe_raster(params: dict[str, Any]) -> dict[str, Any]:
    """A raster file a rasters connection allows, resolved and checked by
    the gateway (inside the connection's folder, TIFF by content); checked
    again here before rasterio opens it."""
    r = _raster()
    path = params.get("path")
    if not isinstance(path, str):
        raise WorkerError(INVALID_PARAMS, "path is required")
    try:
        return r.describe_raster(path)
    except r.GISError as e:
        raise _refused(e) from None


def raster_op(params: dict[str, Any]) -> dict[str, Any]:
    r = _raster()
    inputs = params.get("inputs")
    stem = params.get("output_stem")
    if not isinstance(inputs, dict) or not all(isinstance(v, str) for v in inputs.values()) or not isinstance(stem, str):
        raise WorkerError(INVALID_PARAMS, "inputs and output_stem are required")
    op_params = params.get("params") or {}
    if not isinstance(op_params, dict):
        raise WorkerError(INVALID_PARAMS, "params must be an object")
    # Names starting with "_" are set by the server, never by the caller.
    op_params = {k: v for k, v in op_params.items() if not str(k).startswith("_")}
    max_pixels = params.get("max_pixels")
    try:
        return r.raster_op(str(params.get("op")), inputs, op_params, stem, max_pixels if isinstance(max_pixels, int) else None)
    except r.GISError as e:
        raise _refused(e) from None


# ---------- Registry (design section 6) ----------

REGISTRY_LIMIT = 50


def _registry_db(params: dict[str, Any]):
    path = params.get("db")
    if not isinstance(path, str) or not os.path.isfile(path):
        raise WorkerError(UNSUPPORTED, "the registry index is not installed (columbus-mcp registry update)")
    from columbus_plugin import readonly_uri

    return sqlite3.connect(readonly_uri(path), uri=True)


def _fts_query(text: str) -> str:
    """Words as an FTS5 query: each word quoted (so no FTS syntax gets
    through), all of them required, the last one as a prefix."""
    words = re.findall(r"[\w][\w.-]*", text.lower())[:12]
    if not words:
        return ""
    quoted = ['"' + w.replace('"', '""') + '"' for w in words]
    quoted[-1] += "*"
    return " ".join(quoted)


def _bbox_meets(a, b) -> bool:
    return a[0] <= b[2] and b[0] <= a[2] and a[1] <= b[3] and b[1] <= a[3]


def registry_search(params: dict[str, Any]) -> dict[str, Any]:
    """Ranked registry cards for words, optionally limited to a kind, a theme
    and a bounding box (lon/lat) the entry's coverage must meet."""
    query = str(params.get("query") or "")
    kind = params.get("kind")
    theme = (params.get("theme") or "").lower() or None
    bbox = params.get("bbox")
    limit = max(1, min(int(params.get("limit") or 10), REGISTRY_LIMIT))
    if bbox is not None and (not isinstance(bbox, list) or len(bbox) != 4):
        raise WorkerError(INVALID_PARAMS, "bbox must be [min lon, min lat, max lon, max lat]")
    db = _registry_db(params)
    try:
        meta = dict(db.execute("SELECT key, value FROM meta").fetchall())
        fts = _fts_query(query)
        if fts:
            rows = db.execute(
                "SELECT e.card, e.entry FROM entries_fts f JOIN entries e ON e.id = f.id "
                "WHERE entries_fts MATCH ? ORDER BY bm25(entries_fts, 0, 10.0, 4.0, 2.0, 2.0, 1.0) LIMIT 500",
                (fts,),
            ).fetchall()
        else:
            rows = db.execute("SELECT card, entry FROM entries ORDER BY name LIMIT 2000").fetchall()
    finally:
        db.close()
    cards = []
    unchecked = []
    for card_text, entry_text in rows:
        card = json.loads(card_text)
        entry = json.loads(entry_text)
        if kind and card.get("kind") != kind:
            continue
        if theme and theme not in [str(t).lower() for t in card.get("themes") or []]:
            continue
        coverage = (entry.get("coverage") or {}).get("bbox")
        card["coverage"] = entry.get("coverage")
        card["last_verified"] = entry.get("last_verified")
        if bbox is not None and card.get("kind") == "tool":
            # An area says nothing about software: tools are left out of a
            # search by place unless tools were asked for.
            if kind != "tool":
                continue
        elif bbox is not None:
            if not (isinstance(coverage, list) and len(coverage) == 4):
                # No recorded extent: it may cover the area or not. Listed
                # after the entries known to, and marked.
                card["coverage_checked"] = False
                unchecked.append(card)
                continue
            if not _bbox_meets([float(v) for v in bbox], [float(v) for v in coverage]):
                continue
            card["coverage_checked"] = True
        cards.append(card)
        if len(cards) >= limit:
            break
    cards.extend(unchecked[: max(0, limit - len(cards))])
    return {
        "cards": cards,
        "registry": {"generated_at": meta.get("generated_at"), "count": meta.get("count"), "format_version": meta.get("format_version")},
    }


def registry_get(params: dict[str, Any]) -> dict[str, Any]:
    entry_id = params.get("id")
    if not isinstance(entry_id, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,127}", entry_id):
        raise WorkerError(INVALID_PARAMS, "id must be a registry entry id (lower-case letters, digits and -)")
    db = _registry_db(params)
    try:
        row = db.execute("SELECT entry FROM entries WHERE id = ?", (entry_id,)).fetchone()
    finally:
        db.close()
    if row is None:
        raise WorkerError(INVALID_PARAMS, f"no registry entry {entry_id!r}")
    return {"entry": json.loads(row[0])}


plugin = Plugin("columbus.core", VERSION)

for _name, _fn in (
    ("ping", ping),
    ("list_datasets", list_datasets),
    ("sql_select", sql_select),
    ("extract", extract),
    ("describe_dataset", describe_dataset),
    ("query_layer", query_layer),
    ("vector_op", vector_op),
    ("verify_layer", verify_layer),
    ("read_rows", read_rows),
    ("export_layer", export_layer),
    ("geojson_layer", geojson_layer),
    ("describe_raster", describe_raster),
    ("raster_op", raster_op),
    ("registry_search", registry_search),
    ("registry_get", registry_get),
):
    plugin.tool(_name)(lambda ctx, fn=_fn: fn(ctx.params))


def _preload_gis() -> None:
    """Load the GIS engine in the background at start-up, so the first GIS
    call does not pay for importing GDAL and PROJ (many seconds on a cold
    Windows machine)."""
    try:
        import columbus_gis  # noqa: F401
    except Exception:  # noqa: BLE001 - reported when a GIS tool is called
        pass


if __name__ == "__main__":
    if os.environ.get("COLUMBUS_GIS_PRELOAD", "1") != "0":
        import threading

        threading.Thread(target=_preload_gis, name="gis-preload", daemon=True).start()
    plugin.serve()
