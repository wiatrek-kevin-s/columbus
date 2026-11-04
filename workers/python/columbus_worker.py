"""Columbus Python engine worker.

Speaks line-delimited JSON-RPC 2.0 over stdin/stdout with the gateway, and
only with the gateway. It receives requests the gateway has already
checked: queries arrive rewritten with row filters and limits, and file
paths arrive already resolved inside the sandbox. The worker still opens
everything read-only and stops at the row limit it is given, so a gateway
bug cannot become a write or an unbounded read.

Run with ``python -I -u columbus_worker.py``. Logs go to stderr.

Engines: the standard library's sqlite3 handles GeoPackage and SQLite;
psycopg handles PostgreSQL/PostGIS when installed. GDAL/OGR, DuckDB, PDAL
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
import json
import os
import re
import platform
import sqlite3
import sys
import time
import traceback
from typing import Any, Callable

PROTOCOL_VERSION = 1


class WorkerError(Exception):
    """An error the gateway should pass on to the agent as a refusal."""

    def __init__(self, code: int, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


# JSON-RPC error codes used by the worker.
INVALID_PARAMS = -32602
UNSUPPORTED = -32001
ENGINE_ERROR = -32002
TIMEOUT = -32003


def _optional_version(module: str, attr: str = "__version__") -> str | None:
    try:
        mod = importlib.import_module(module)
    except Exception:  # noqa: BLE001 - any import failure means "not available"
        return None
    if module == "osgeo.gdal":
        return mod.__version__ if hasattr(mod, "__version__") else mod.VersionInfo("RELEASE_NAME")
    return str(getattr(mod, attr, "unknown"))


def ping(_params: dict[str, Any]) -> dict[str, Any]:
    return {
        "protocol": PROTOCOL_VERSION,
        "python": platform.python_version(),
        "engines": {
            "sqlite": sqlite3.sqlite_version,
            "gdal": _optional_version("osgeo.gdal"),
            "pyogrio": _optional_version("pyogrio"),
            "shapely": _optional_version("shapely"),
            "duckdb": _optional_version("duckdb"),
            "pdal": _optional_version("pdal"),
            "psycopg": _optional_version("psycopg"),
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
    uri = "file:" + path.replace("?", "%3f").replace("#", "%23") + "?mode=ro"
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
        for entry in tables.values():
            quoted = '"' + entry["name"].replace('"', '""') + '"'
            entry["fields"] = [
                {"name": r[1], "type": r[2] or None, "not_null": bool(r[3]), "pk": bool(r[5])}
                for r in db.execute(f"PRAGMA table_info({quoted})")
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


def _sqlite_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float):
    """Returns (columns, rows, hit_limit, cell_fn)."""
    db = _open_sqlite(conn, timeout_s)
    try:
        try:
            cur = db.execute(sql)
            columns = [d[0] for d in (cur.description or [])]
            rows = []
            hit = False
            for row in cur:
                if len(rows) >= max_rows:
                    hit = True
                    break
                rows.append(row)
            return columns, rows, hit, [_cell] * len(columns)
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
    return db, scrub


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


def _pg_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float):
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
                while True:
                    chunk = cur.fetchmany(2000)
                    if not chunk:
                        break
                    room = max_rows - len(rows)
                    if len(chunk) > room:
                        rows.extend(chunk[:room])
                        hit = True
                        break
                    rows.extend(chunk)
                desc = cur.description or []
                columns = [d.name for d in desc]
                cells = [_pg_geom if d.type_code in geom_oids else _pg_cell for d in desc]
            db.rollback()
            return columns, rows, hit, cells
        except psycopg.errors.QueryCanceled as e:
            raise WorkerError(TIMEOUT, f"query exceeded {timeout_s:g} s") from e
        except psycopg.errors.ReadOnlySqlTransaction as e:
            raise WorkerError(ENGINE_ERROR, "the database refused a write: this connection is read-only") from e
        except psycopg.Error as e:
            raise WorkerError(ENGINE_ERROR, scrub(f"query failed: {e}".strip())) from None
    finally:
        db.close()


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
            entry.update({"geometry_column": col, "geometry_type": gtype, "srs_id": srid, "dimensions": dims})
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


# ---------- engine-agnostic methods ----------


def list_datasets(params: dict[str, Any]) -> dict[str, Any]:
    conn = _connection(params)
    kind = conn["kind"].lower()
    timeout_s = float(params.get("timeout_s", 30))
    if kind in SQLITE_KINDS:
        return {"datasets": _sqlite_list(conn, timeout_s)}
    if kind in PG_KINDS:
        return {"datasets": _pg_list(conn, timeout_s)}
    raise WorkerError(UNSUPPORTED, f"list_datasets does not support {conn['kind']!r} yet")


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
    if kind in SQLITE_KINDS:
        columns, all_rows, hit_limit, cells = _sqlite_fetch(conn, sql, max_rows, timeout_s)
    elif kind in PG_KINDS:
        columns, all_rows, hit_limit, cells = _pg_fetch(conn, sql, max_rows, timeout_s)
    else:
        raise WorkerError(UNSUPPORTED, f"sql_select does not support {conn['kind']!r} yet")

    # Fields organization policy hides never leave the worker: not in the
    # rows returned, and not in the workspace file, which can later be sent
    # to another provider as a file handle.
    hide = [p.lower() for p in params.get("hide_fields") or [] if isinstance(p, str)]
    keep = [not any(fnmatch.fnmatchcase(c.lower(), p) for p in hide) for c in columns]
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


METHODS: dict[str, Callable[[dict[str, Any]], dict[str, Any]]] = {
    "ping": ping,
    "list_datasets": list_datasets,
    "sql_select": sql_select,
}


def handle(line: str) -> dict[str, Any] | None:
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        return {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}}
    rid = req.get("id")
    method = METHODS.get(req.get("method", ""))
    if method is None:
        return {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "unknown method"}}
    params = req.get("params") or {}
    try:
        return {"jsonrpc": "2.0", "id": rid, "result": method(params)}
    except WorkerError as e:
        return {"jsonrpc": "2.0", "id": rid, "error": {"code": e.code, "message": e.message}}
    except Exception as e:  # noqa: BLE001 - report, never crash the loop
        traceback.print_exc(file=sys.stderr)
        return {
            "jsonrpc": "2.0",
            "id": rid,
            "error": {"code": ENGINE_ERROR, "message": f"{type(e).__name__}: {e}"},
        }


def main() -> None:
    for line in sys.stdin:
        if not line.strip():
            continue
        resp = handle(line)
        if resp is not None:
            sys.stdout.write(json.dumps(resp, separators=(",", ":"), default=str) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
