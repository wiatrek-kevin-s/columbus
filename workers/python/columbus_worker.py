"""Columbus Python engine worker.

Speaks line-delimited JSON-RPC 2.0 over stdin/stdout with the gateway, and
only with the gateway. It receives requests the gateway has already
checked: queries arrive rewritten with row filters and limits, and file
paths arrive already resolved inside the sandbox. The worker still opens
everything read-only and stops at the row limit it is given, so a gateway
bug cannot become a write or an unbounded read.

Run with ``python -I -u columbus_worker.py``. Logs go to stderr.

Engines: the standard library's sqlite3 handles GeoPackage and SQLite
today. GDAL/OGR, DuckDB, PDAL and the database drivers are optional imports
that light up when present in the pixi environment (design section 2).
"""

from __future__ import annotations

import base64
import csv
import importlib
import os
import json
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


def list_datasets(params: dict[str, Any]) -> dict[str, Any]:
    conn = _connection(params)
    if conn["kind"].lower() not in SQLITE_KINDS:
        raise WorkerError(UNSUPPORTED, f"list_datasets does not support {conn['kind']!r} yet")
    db = _open_sqlite(conn, float(params.get("timeout_s", 30)))
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
        return {"datasets": list(tables.values())}
    finally:
        db.close()


def _cell(value: Any) -> Any:
    if isinstance(value, (bytes, bytearray, memoryview)):
        raw = bytes(value)
        if raw[:2] == b"GP":
            return {"geometry": "gpkg", "bytes": len(raw)}
        return {"blob_base64": base64.b64encode(raw[:64]).decode(), "bytes": len(raw)}
    return value


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
    if conn["kind"].lower() not in SQLITE_KINDS:
        raise WorkerError(UNSUPPORTED, f"sql_select does not support {conn['kind']!r} yet")
    timeout_s = float(params.get("timeout_s", 120))
    started = time.monotonic()
    db = _open_sqlite(conn, timeout_s)
    try:
        try:
            cur = db.execute(sql)
            columns = [d[0] for d in (cur.description or [])]
            all_rows = []
            hit_limit = False
            for row in cur:
                if len(all_rows) >= max_rows:
                    hit_limit = True
                    break
                all_rows.append(row)
        except sqlite3.OperationalError as e:
            if "interrupted" in str(e):
                raise WorkerError(TIMEOUT, f"query exceeded {timeout_s:g} s") from e
            raise WorkerError(ENGINE_ERROR, f"query failed: {e}") from e
        except sqlite3.Error as e:
            raise WorkerError(ENGINE_ERROR, f"query failed: {e}") from e
    finally:
        db.close()

    written = None
    if len(all_rows) > return_rows and isinstance(output_path, str) and output_path:
        os.makedirs(os.path.dirname(output_path) or ".", exist_ok=True)
        with open(output_path, "w", newline="", encoding="utf-8") as f:
            w = csv.writer(f)
            w.writerow(columns)
            for row in all_rows:
                w.writerow([json.dumps(_cell(v)) if isinstance(v, (bytes, bytearray, memoryview)) else v for v in row])
        written = output_path
    return {
        "columns": columns,
        "rows": [[_cell(v) for v in row] for row in all_rows[:return_rows]],
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
