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
import importlib.metadata
import json
import os
import re
import platform
import sqlite3
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
            "psycopg": _optional_version("psycopg"),
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


def _fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None):
    kind = conn["kind"].lower()
    if kind in SQLITE_KINDS:
        return _sqlite_fetch(conn, sql, max_rows, timeout_s, keep_factory)
    if kind in PG_KINDS:
        return _pg_fetch(conn, sql, max_rows, timeout_s, keep_factory)
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
    want = dataset.replace('"', "").replace("[", "").replace("]", "").lower()
    for e in entries:
        if e["name"].lower() == want:
            return e
    return None


def describe_dataset(params: dict[str, Any]) -> dict[str, Any]:
    """Fields, geometry, CRS facts and extent of one dataset, the row count
    under the connection's row filter (``count_sql``, checked by the
    gateway), and CRS checks."""
    g = _gis()
    conn = _connection(params)
    timeout_s = float(params.get("timeout_s", 60))
    entries = list_datasets({"connection": conn, "timeout_s": timeout_s})["datasets"]
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
    try:
        return g.vector_op(str(params.get("op")), inputs, params.get("params") or {}, stem)
    except g.GISError as e:
        raise _refused(e) from None


def verify_layer(params: dict[str, Any]) -> dict[str, Any]:
    g = _gis()
    path = params.get("path")
    if not isinstance(path, str):
        raise WorkerError(INVALID_PARAMS, "path is required")
    return g.verify_layer(path)


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
