"""Edits: a reviewable change set, and its application.

``edit_schema``: a dataset's key column and column types (read-only).

``edit_prepare``: reads the rows a change set touches through the
gateway's checked SELECT (the connection's row filter applied), checks
every change against the dataset's columns and geometry, and writes the
change set (``edits/<id>.json``) and a preview layer to the workspace.
Writes nothing to the source.

``edit_apply``: applies a change set either to a workspace copy (a layer
the gateway read through the connection's rules) or to the source, in one
transaction: each touched row is read again with the same checked SELECT
and must be exactly as it was when the change set was made, each write is
limited to the row's key and the connection's row filter and must touch
exactly one row, and each row written must still be inside the row filter
afterwards. Any failure rolls the whole transaction back.

Identifiers arrive quoted by the gateway (from the dataset's own column
names); values only ever travel as query parameters.
"""

from __future__ import annotations

import datetime as _dt
import decimal
import hashlib
import json
import math
import os
import sqlite3
import struct
import time
import re
from typing import Any

from columbus_plugin import ToolError, readonly_uri

INVALID_PARAMS = -32602
ENGINE_ERROR = -32002
# A refusal travels as an engine error with its message (not -32003,
# which is the worker's timeout).
REFUSED = -32002

MAX_CHANGES = 1000
# Every value must fit on the person's approval card whole.
MAX_VALUE_CHARS = 500
MAX_VERTICES = 100_000
SQLITE_KINDS = {"gpkg", "geopackage"}
PG_KINDS = {"postgis", "postgres", "postgresql"}


class EditError(ToolError):
    def __init__(self, code: int, message: str):
        super().__init__(message, code)


def _refuse(message: str) -> EditError:
    return EditError(REFUSED, message)


# ---------- values ----------


def canonical(v: Any) -> Any:
    """A value as JSON, the same way whichever database or connection
    produced it, for fingerprints and before/after display."""
    if v is None or isinstance(v, (bool, int, str)):
        return v
    if isinstance(v, float):
        return v if math.isfinite(v) else str(v)
    if isinstance(v, (bytes, bytearray, memoryview)):
        return bytes(v).hex()
    if isinstance(v, decimal.Decimal):
        return str(v)
    if isinstance(v, (_dt.datetime, _dt.date, _dt.time)):
        return v.isoformat()
    return str(v)


def fingerprint(values: list[Any]) -> str:
    text = json.dumps([canonical(v) for v in values], separators=(",", ":"), sort_keys=True)
    return hashlib.sha256(text.encode()).hexdigest()


_GEOMETRY_TYPES = re.compile(
    r"^(geometry|geography|point|linestring|polygon|multipoint|multilinestring|multipolygon|"
    r"geometrycollection|circularstring|compoundcurve|curvepolygon|multicurve|multisurface|curve|surface|"
    r"polyhedralsurface|tin|triangle)\b"
)


def _family(t: str) -> str:
    """A column type's family, by its leading word (not a substring:
    "interval" is not an int, "POINT" is a geometry)."""
    t = (t or "").strip().lower()
    if _GEOMETRY_TYPES.match(t) or t.startswith(("geometry(", "geography(")):
        return "geometry"
    if re.match(r"^(boolean|bool)\b", t):
        return "bool"
    if re.match(r"^(tiny|small|medium|big)?int(eger)?\d*\b|^(small|big)?serial\d*\b", t):
        return "int"
    if re.match(r"^(real|float\d*|double( precision)?|numeric|decimal)\b", t):
        return "float"
    if re.match(r"^(timestamp|datetime)\b", t):
        return "datetime"
    if re.match(r"^date\b", t):
        return "date"
    if re.match(r"^(text|varchar|nvarchar|character( varying)?|char|nchar|clob|string|uuid|citext)\b", t):
        return "text"
    return "other"


def coerce(field: str, ftype: str, v: Any) -> Any:
    """A JSON value for a column, checked against its type."""
    if v is None:
        return None
    fam = _family(ftype)
    if fam == "geometry":
        raise _refuse(f"{field} is a geometry column; set it with \"geometry\", which is checked")
    if isinstance(v, str) and len(v) > MAX_VALUE_CHARS:
        raise _refuse(f"{field}: at most {MAX_VALUE_CHARS} characters, so the person can read the whole value before approving")
    if fam == "bool":
        if isinstance(v, bool):
            return v
    elif fam == "int":
        if isinstance(v, int) and not isinstance(v, bool):
            return v
        if isinstance(v, float) and v.is_integer():
            return int(v)
    elif fam == "float":
        if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(float(v)):
            return float(v)
    elif fam in ("date", "datetime"):
        if isinstance(v, str):
            try:
                (_dt.date if fam == "date" else _dt.datetime).fromisoformat(v.replace("Z", "+00:00"))
                return v
            except ValueError:
                pass
    elif fam == "text":
        if isinstance(v, str):
            return v
    elif isinstance(v, (str, int, float)) and not isinstance(v, bool):
        return v
    raise _refuse(f"{field} ({ftype}) cannot take {json.dumps(v)[:60]}")


# ---------- geometry ----------


def gpkg_wkb(blob: bytes) -> tuple[bytes, int | None]:
    """WKB and srs_id from a GeoPackage geometry blob."""
    b = bytes(blob)
    if b[:2] != b"GP" or len(b) < 8:
        raise _refuse("not a GeoPackage geometry")
    flags = b[3]
    endian = "<" if flags & 1 else ">"
    (srs,) = struct.unpack(endian + "i", b[4:8])
    env = (flags >> 1) & 0x7
    size = {0: 0, 1: 32, 2: 48, 3: 48, 4: 64}.get(env)
    if size is None:
        raise _refuse("bad GeoPackage envelope")
    return b[8 + size:], srs


def gpkg_blob(geom, srs_id: int) -> bytes:
    """A GeoPackage geometry blob (little-endian, xy envelope)."""
    import shapely

    wkb = shapely.to_wkb(geom, byte_order=1, output_dimension=2)
    if geom.is_empty:
        return b"GP\x00" + bytes([0x01 | 0x10]) + struct.pack("<i", srs_id) + wkb
    minx, miny, maxx, maxy = geom.bounds
    return b"GP\x00" + bytes([0x03]) + struct.pack("<i4d", srs_id, minx, maxx, miny, maxy) + wkb


def _geom_from_db(v: Any, kind: str):
    import shapely

    if v is None:
        return None
    if kind in SQLITE_KINDS:
        wkb, _ = gpkg_wkb(v)
        return shapely.from_wkb(wkb)
    if isinstance(v, str):
        return shapely.from_wkb(bytes.fromhex(v))
    return shapely.from_wkb(bytes(v))


def _promote(geom, layer_type: str | None):
    """A single part as its multi type when the layer holds multi types."""
    import shapely

    t = (layer_type or "").upper().replace(" ", "")
    single = geom.geom_type.upper()
    if t.startswith("MULTI") and not single.startswith("MULTI"):
        cls = {"POINT": shapely.MultiPoint, "LINESTRING": shapely.MultiLineString, "POLYGON": shapely.MultiPolygon}.get(single)
        if cls is not None:
            return cls([geom])
    return geom


def _geometry(change: dict, i: int, src_crs, dst_crs, layer_type: str | None):
    """The change's GeoJSON geometry, in the dataset's CRS, checked."""
    from pyproj import Transformer
    from shapely.geometry import shape
    from shapely.ops import transform

    g = change.get("geometry")
    if not isinstance(g, dict):
        raise _refuse(f"change {i + 1}: geometry must be a GeoJSON geometry object")
    try:
        geom = shape(g)
    except Exception as e:  # noqa: BLE001 - any malformed GeoJSON
        raise _refuse(f"change {i + 1}: not a valid GeoJSON geometry: {e}") from None
    if geom.is_empty:
        raise _refuse(f"change {i + 1}: the geometry is empty")
    import shapely

    if shapely.get_num_coordinates(geom) > MAX_VERTICES:
        raise _refuse(f"change {i + 1}: at most {MAX_VERTICES} vertices in one geometry")
    if shapely.has_z(geom):
        raise _refuse(f"change {i + 1}: 3D geometries cannot be edited yet (the heights would be lost)")
    if dst_crs is None:
        raise _refuse("the dataset's coordinate system is not known, so a geometry cannot be placed in it")
    if src_crs is not None and dst_crs is not None and not src_crs.equals(dst_crs):
        t = Transformer.from_crs(src_crs, dst_crs, always_xy=True)
        geom = transform(t.transform, geom)
    if not all(math.isfinite(c) for c in geom.bounds):
        raise _refuse(f"change {i + 1}: the geometry does not fall inside the dataset's coordinate system")
    if not geom.is_valid:
        from shapely.validation import explain_validity

        raise _refuse(
            f"change {i + 1}: the geometry is invalid ({explain_validity(geom)}); fix it first (vector_op make_valid on a layer, or correct the coordinates)"
        )
    geom = _promote(geom, layer_type)
    want = (layer_type or "").upper().replace(" ", "")
    have = geom.geom_type.upper()
    if want and want not in ("GEOMETRY", "GEOMETRYCOLLECTION") and want.replace("MULTI", "") != have.replace("MULTI", ""):
        raise _refuse(f"change {i + 1}: a {geom.geom_type} cannot go in a {layer_type} layer")
    return geom


def _describe(g) -> str:
    """A geometry as the person can check it: type, vertices, extent."""
    if g is None:
        return "(no geometry)"
    import shapely

    if g.is_empty:
        return f"empty {g.geom_type}"
    x0, y0, x1, y1 = g.bounds
    n = shapely.get_num_coordinates(g)

    def f(v):
        return f"{v:.6f}".rstrip("0").rstrip(".") if abs(v) < 1000 else f"{v:.1f}"

    return f"{g.geom_type}, {n} vertices, from ({f(x0)}, {f(y0)}) to ({f(x1)}, {f(y1)})"


# ---------- connections ----------


def _sqlite(conn: dict, timeout_s: float, write: bool) -> sqlite3.Connection:
    path = conn.get("path")
    if not isinstance(path, str) or not path:
        raise EditError(INVALID_PARAMS, "file connection has no path")
    try:
        if write:
            db = sqlite3.connect(path, timeout=10, isolation_level=None, check_same_thread=False)
        else:
            db = sqlite3.connect(readonly_uri(path), uri=True, timeout=5, check_same_thread=False)
            db.execute("PRAGMA query_only = ON")
    except sqlite3.Error as e:
        raise EditError(ENGINE_ERROR, f"cannot open {os.path.basename(path)}: {e}") from None
    deadline = time.monotonic() + timeout_s
    db.set_progress_handler(lambda: 1 if time.monotonic() > deadline else 0, 10_000)
    if write:
        _gpkg_functions(db)
    return db


def _gpkg_functions(db: sqlite3.Connection) -> None:
    """The functions a GeoPackage's spatial index triggers call (GDAL's
    rtree triggers use ST_IsEmpty and ST_MinX/MaxX/MinY/MaxY)."""

    def envelope(blob):
        if blob is None:
            return None
        import shapely

        wkb, _ = gpkg_wkb(blob)
        g = shapely.from_wkb(wkb)
        return None if g.is_empty else g.bounds

    def part(i):
        def f(blob):
            b = envelope(blob)
            return None if b is None else b[i]

        return f

    db.create_function("ST_IsEmpty", 1, lambda b: None if b is None else int(envelope(b) is None), deterministic=True)
    db.create_function("ST_MinX", 1, part(0), deterministic=True)
    db.create_function("ST_MinY", 1, part(1), deterministic=True)
    db.create_function("ST_MaxX", 1, part(2), deterministic=True)
    db.create_function("ST_MaxY", 1, part(3), deterministic=True)


def _open(conn: dict, timeout_s: float, write: bool):
    kind = conn.get("kind")
    if kind in SQLITE_KINDS:
        return _sqlite(conn, timeout_s, write), "?", None
    if kind in PG_KINDS:
        import columbus_worker as w

        db, scrub = w._pg_connect(conn, timeout_s, write=write)
        return db, "%s", scrub
    raise _refuse(f"edits are not supported on {kind} connections yet (GeoPackage and PostGIS are)")


def _rows(db, sql: str, args=()) -> tuple[list[str], list[tuple]]:
    cur = db.cursor()
    cur.execute(sql, args)
    cols = [d[0] for d in (cur.description or [])]
    return cols, [tuple(r) for r in cur.fetchall()]


def _db_error(e: Exception, scrub) -> EditError:
    text = str(e).strip().splitlines()[0] if str(e).strip() else type(e).__name__
    return EditError(ENGINE_ERROR, (scrub(text) if scrub else text)[:300])


# ---------- edit_schema ----------


def edit_schema(params: dict[str, Any]) -> dict[str, Any]:
    """The key column and column types of a table."""
    conn = params.get("connection") or {}
    table = params.get("table_sql")
    if not isinstance(table, str) or not table:
        raise EditError(INVALID_PARAMS, "table_sql is required")
    timeout_s = float(params.get("timeout_s", 60))
    db, mark, scrub = _open(conn, timeout_s, write=False)
    try:
        if conn.get("kind") in SQLITE_KINDS:
            _c, rows = _rows(db, f"PRAGMA table_info({table})")
            types = {r[1]: (r[2] or "") for r in rows}
            notnull = [r[1] for r in rows if r[3]]
            keys = [r[1] for r in sorted(rows, key=lambda r: r[5]) if r[5]]
        else:
            _c, rows = _rows(
                db,
                "SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull FROM pg_catalog.pg_attribute a "
                "WHERE a.attrelid = %s::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
                (table,),
            )
            types = {r[0]: r[1] for r in rows}
            notnull = [r[0] for r in rows if r[2]]
            _c, k = _rows(
                db,
                "SELECT a.attname FROM pg_catalog.pg_index i JOIN pg_catalog.pg_attribute a "
                "ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) WHERE i.indrelid = %s::regclass AND i.indisprimary",
                (table,),
            )
            keys = [r[0] for r in k]
            db.rollback()
    except EditError:
        raise
    except Exception as e:  # noqa: BLE001 - database errors of either driver
        raise _db_error(e, scrub) from None
    finally:
        db.close()
    return {"columns": types, "not_null": notnull, "primary_key": keys}


# ---------- edit_prepare ----------


def _check_key(change: dict, i: int, key_type: str):
    k = change.get("key")
    if isinstance(k, bool) or not isinstance(k, (int, str)):
        raise _refuse(f"change {i + 1}: \"key\" must be the row's {key_type} key value")
    return coerce("key", key_type, k)


def edit_prepare(params: dict[str, Any]) -> dict[str, Any]:
    import geopandas as gpd

    import columbus_gis as cg

    conn = params.get("connection") or {}
    kind = conn.get("kind")
    key = params.get("key")
    fields: dict[str, str] = params.get("fields") or {}
    geom_col = params.get("geometry_column")
    changes = params.get("changes")
    out = params.get("change_path")
    stem = params.get("preview_stem")
    if not isinstance(key, str) or not isinstance(changes, list) or not isinstance(out, str) or not isinstance(stem, str):
        raise EditError(INVALID_PARAMS, "key, changes, change_path and preview_stem are required")
    if not changes:
        raise _refuse("no changes given")
    if len(changes) > MAX_CHANGES:
        raise _refuse(f"at most {MAX_CHANGES} changes at a time; split the edit")
    timeout_s = float(params.get("timeout_s", 120))
    dst = cg.crs_from_spec(params.get("crs_spec"))
    if params.get("input_crs"):
        src = cg.crs_from_spec(params.get("input_crs"))
        if src is None:
            raise _refuse(f"unknown coordinate system {json.dumps(params.get('input_crs'))[:80]}; give an EPSG code such as EPSG:4326")
    else:
        src = cg.crs_from_spec({"code": "EPSG:4326"})
    if (params.get("dimensions") or 2) > 2 and any(isinstance(c, dict) and "geometry" in c for c in changes):
        raise _refuse("this dataset holds 3D geometries, which cannot be edited yet (the heights would be lost)")
    layer_type = params.get("geometry_type")
    key_type = params.get("key_type") or ""

    # The rows that updates and deletions touch, as the connection's rules
    # let this session read them.
    before: dict[Any, dict] = {}
    columns: list[str] = []
    sql = params.get("select_sql")
    if isinstance(sql, str) and sql:
        db, _mark, scrub = _open(conn, timeout_s, write=False)
        try:
            columns, rows = _rows(db, sql)
            if kind in PG_KINDS:
                db.rollback()
        except Exception as e:  # noqa: BLE001
            raise _db_error(e, scrub) from None
        finally:
            db.close()
        ki = columns.index(key)
        for r in rows:
            before[canonical(r[ki])] = {"row": r, "fingerprint": fingerprint(list(r))}

    seen: set = set()
    out_changes: list[dict] = []
    diff: list[dict] = []
    preview: list[dict] = []
    counts = {"update": 0, "insert": 0, "delete": 0}
    for i, c in enumerate(changes):
        if not isinstance(c, dict) or c.get("op") not in counts:
            raise _refuse(f"change {i + 1}: \"op\" must be update, insert or delete")
        op = c["op"]
        rec: dict[str, Any] = {"op": op}
        values = c.get("values") or {}
        if not isinstance(values, dict):
            raise _refuse(f"change {i + 1}: \"values\" must be an object of field: value")
        if op == "delete" and (values or "geometry" in c):
            raise _refuse(f"change {i + 1}: a deletion takes only a key")
        if op in ("update", "delete"):
            k = _check_key(c, i, key_type)
            if canonical(k) in seen:
                raise _refuse(f"change {i + 1}: row {json.dumps(k)} is changed twice; combine the changes")
            seen.add(canonical(k))
            b = before.get(canonical(k))
            if b is None:
                raise _refuse(
                    f"change {i + 1}: no row with {key} = {json.dumps(k)[:60]} among the rows this connection may read"
                )
            rec["key"] = k
            rec["fingerprint"] = b["fingerprint"]
            rec["before"] = {col: canonical(v) for col, v in zip(columns, b["row"]) if col != geom_col}
        elif "key" in c:
            raise _refuse(f"change {i + 1}: an insertion gets its key from the database; leave \"key\" out")
        elif kind in SQLITE_KINDS and _family(key_type) != "int":
            raise _refuse(f"change {i + 1}: rows can be inserted only where the key is a whole number the database assigns")
        if op == "update" and not values and "geometry" not in c:
            raise _refuse(f"change {i + 1}: an update needs values or a geometry")
        if op == "insert" and not values and not geom_col:
            raise _refuse(f"change {i + 1}: an insertion needs values")
        setv = {}
        for f, v in values.items():
            if f not in fields or (geom_col and f.lower() == geom_col.lower()):
                raise _refuse(f"change {i + 1}: no field {f!r} you may edit (see describe_dataset)")
            if f == key:
                raise _refuse(f"change {i + 1}: the key {key} cannot be changed")
            setv[f] = coerce(f, fields[f], v)
        rec["values"] = setv
        geom = None
        if "geometry" in c:
            if not geom_col:
                raise _refuse(f"change {i + 1}: this dataset has no geometry")
            geom = _geometry(c, i, src, dst, layer_type)
            rec["geometry"] = geom.wkb_hex
        elif op == "insert" and geom_col:
            raise _refuse(f"change {i + 1}: an insertion needs a geometry")
        counts[op] += 1
        out_changes.append(rec)
        # What the person reviews: every field that changes.
        if op == "update":
            for f, v in setv.items():
                if rec["before"].get(f) != v:
                    diff.append({"op": op, "key": rec["key"], "field": f, "before": rec["before"].get(f), "after": v})
            if geom is not None:
                old = _geom_from_db(before[canonical(rec["key"])]["row"][columns.index(geom_col)], kind)
                diff.append({"op": op, "key": rec["key"], "field": geom_col, "before": _describe(old), "after": _describe(geom)})
        elif op == "insert":
            after = dict(setv)
            if geom is not None:
                after[geom_col] = _describe(geom)
            diff.append({"op": op, "key": None, "field": None, "before": None, "after": after})
        else:
            diff.append({"op": op, "key": rec["key"], "field": None, "before": rec["before"], "after": None})
        # The preview layer: each row as it will be, deletions as they were.
        prow = dict(rec.get("before") or {})
        prow.update(setv)
        pg = geom
        if pg is None and op != "insert" and geom_col:
            b = before[canonical(rec["key"])]
            pg = _geom_from_db(b["row"][columns.index(geom_col)], kind)
        prow["_change"] = op
        prow["_geometry"] = pg
        preview.append(prow)

    change_set = {
        "version": 1,
        "connection": params.get("connection_id"),
        "dataset": params.get("dataset"),
        "key": key,
        "geometry_column": geom_col,
        "counts": counts,
        "changes": out_changes,
    }
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    # The hash of exactly the bytes written, not of a re-read.
    data = json.dumps(change_set, ensure_ascii=False, indent=1).encode("utf-8")
    digest = hashlib.sha256(data).hexdigest()
    with open(out, "wb") as f:
        f.write(data)

    preview_path = None
    summary = None
    if geom_col and preview:
        geoms = [p.pop("_geometry") for p in preview]
        df = gpd.GeoDataFrame(preview, geometry=geoms, crs=dst)
        for c in list(df.columns):
            if c != "geometry" and df[c].map(lambda v: isinstance(v, (dict, list))).any():
                df[c] = df[c].map(lambda v: json.dumps(v) if isinstance(v, (dict, list)) else v)
        preview_path = cg.write_layer(df, stem)
        summary = cg.layer_summary(df)
    return {
        "change_path": out,
        "sha256": digest,
        "counts": counts,
        "diff": diff[:200],
        "diff_total": len(diff),
        "preview_path": preview_path,
        "preview_summary": summary,
    }


# ---------- edit_apply ----------


def _load(path: str, sha: str) -> dict:
    with open(path, "rb") as f:
        data = f.read()
    if hashlib.sha256(data).hexdigest() != sha:
        raise _refuse("the change set changed after it was proposed; propose it again")
    return json.loads(data)


def edit_apply(params: dict[str, Any]) -> dict[str, Any]:
    cs = _load(params.get("change_path", ""), params.get("sha256", ""))
    if params.get("target") == "copy":
        return _apply_copy(params, cs)
    if params.get("target") == "source":
        return _apply_source(params, cs)
    raise EditError(INVALID_PARAMS, "target must be copy or source")


def _apply_copy(params: dict, cs: dict) -> dict:
    """The change set applied to a workspace copy: every row this
    connection may read (the gateway's checked SELECT, key included), with
    the changes made, as a new layer. The source is not written."""
    import geopandas as gpd
    import pandas as pd
    import shapely

    import columbus_gis as cg

    conn = params.get("connection") or {}
    kind = conn.get("kind")
    max_rows = int(params.get("max_rows") or 0)
    db, _m, scrub = _open(conn, float(params.get("timeout_s", 120)), write=False)
    try:
        cols, rows = _rows(db, params["select_all_sql"])
        if kind in PG_KINDS:
            db.rollback()
    except Exception as e:  # noqa: BLE001
        raise _db_error(e, scrub) from None
    finally:
        db.close()
    if max_rows and len(rows) > max_rows:
        raise _refuse(f"the dataset has more rows than this connection's read limit ({max_rows}); a cut-off copy would be wrong")
    key = cs["key"]
    geom_col = cs.get("geometry_column")
    ki = cols.index(key)
    gi = cols.index(geom_col) if geom_col in cols else None
    table: dict = {}
    for r in rows:
        rec = {c: canonical(v) for n, (c, v) in enumerate(zip(cols, r)) if n != gi}
        rec["__geom"] = _geom_from_db(r[gi], kind) if gi is not None else None
        table[canonical(r[ki])] = rec
    next_key = max((k for k in table if isinstance(k, int)), default=0) + 1
    for c in cs["changes"]:
        op = c["op"]
        k = canonical(c.get("key"))
        if op in ("update", "delete") and k not in table:
            raise _refuse(f"row {key} = {json.dumps(c['key'])[:60]} is not in the rows this connection may read")
        if op == "delete":
            del table[k]
            continue
        if op == "update":
            rec = table[k]
        else:
            rec = {c2: None for c2 in cols if c2 != geom_col}
            rec[key] = next_key
            table[next_key] = rec
            next_key += 1
        rec.update(c["values"])
        if "geometry" in c:
            rec["__geom"] = shapely.from_wkb(bytes.fromhex(c["geometry"]))
    recs = list(table.values())
    geoms = [r.pop("__geom") for r in recs]
    df = pd.DataFrame(recs, columns=[c for c in cols if c != geom_col])
    if gi is not None:
        df = gpd.GeoDataFrame(df, geometry=geoms, crs=cg.crs_from_spec(params.get("crs_spec")))
    path = cg.write_layer(df, params["out_stem"])
    return {"target": "copy", "path": path, "summary": cg.layer_summary(df), "counts": cs["counts"], "rows": len(df)}


def _apply_source(params: dict, cs: dict) -> dict:
    """The change set written to the source, in one transaction."""
    import shapely

    conn = params.get("connection") or {}
    if not conn.get("write"):
        raise _refuse("this connection is not open for writing")
    kind = conn.get("kind")
    table = params["table_sql"]
    qkey = params["key_sql"]
    qcols: dict[str, str] = params["columns_sql"]
    qgeom = params.get("geometry_sql")
    srid = params.get("srid")
    gpkg_srs = params.get("gpkg_srs_id")
    filt = " AND ".join(f"({f})" for f in params.get("filters") or [])
    scope = f" AND {filt}" if filt else ""
    timeout_s = float(params.get("timeout_s", 120))
    db, m, scrub = _open(conn, timeout_s, write=True)
    key = cs["key"]

    def geom_value(hexwkb: str):
        g = shapely.from_wkb(bytes.fromhex(hexwkb))
        if kind in SQLITE_KINDS:
            return gpkg_blob(g, int(gpkg_srs if gpkg_srs is not None else 0)), m
        return bytes.fromhex(hexwkb), f"ST_SetSRID(ST_GeomFromWKB({m}), {int(srid or 0)})"

    written = {"update": 0, "insert": 0, "delete": 0}
    inserted = []
    try:
        if kind in SQLITE_KINDS:
            db.execute("BEGIN IMMEDIATE")
        # The rows as they are now, locked where the database can: each must
        # be exactly as it was when the change set was made.
        sql = params.get("select_sql")
        now: dict = {}
        if sql:
            lock = " FOR UPDATE" if kind in PG_KINDS else ""
            cols, rows = _rows(db, sql + lock)
            ki = cols.index(key)
            now = {canonical(r[ki]): fingerprint(list(r)) for r in rows}
        for c in cs["changes"]:
            if c["op"] in ("update", "delete") and now.get(canonical(c["key"])) != c["fingerprint"]:
                raise _refuse(
                    f"row {key} = {json.dumps(c['key'])[:60]} changed (or left the rows this connection may read) since the change set was made; nothing was written. Propose the edit again"
                )
        cur = db.cursor()
        for c in cs["changes"]:
            op = c["op"]
            if op == "delete":
                cur.execute(f"DELETE FROM {table} WHERE {qkey} = {m}{scope}", (c["key"],))
                if cur.rowcount != 1:
                    raise _refuse(f"deleting row {json.dumps(c['key'])[:60]} touched {cur.rowcount} rows; nothing was written")
                written["delete"] += 1
                continue
            sets, args = [], []
            for f, v in c["values"].items():
                if f not in qcols or f.lower() == (cs.get("geometry_column") or "").lower():
                    raise _refuse(f"field {f!r} may not be edited")
                sets.append((qcols[f], m))
                args.append(v)
            if "geometry" in c:
                if not qgeom:
                    raise _refuse("this dataset has no geometry")
                val, expr = geom_value(c["geometry"])
                sets.append((qgeom, expr))
                args.append(val)
            if op == "update":
                assign = ", ".join(f"{col} = {expr}" for col, expr in sets)
                cur.execute(f"UPDATE {table} SET {assign} WHERE {qkey} = {m}{scope}", (*args, c["key"]))
                if cur.rowcount != 1:
                    raise _refuse(f"updating row {json.dumps(c['key'])[:60]} touched {cur.rowcount} rows; nothing was written")
                k = c["key"]
                written["update"] += 1
            else:
                names = ", ".join(col for col, _ in sets)
                marks = ", ".join(expr for _, expr in sets)
                if kind in PG_KINDS:
                    cur.execute(f"INSERT INTO {table} ({names}) VALUES ({marks}) RETURNING {qkey}", args)
                    k = cur.fetchone()[0]
                else:
                    cur.execute(f"INSERT INTO {table} ({names}) VALUES ({marks})", args)
                    k = cur.lastrowid
                inserted.append(canonical(k))
                written["insert"] += 1
            # Still inside the rows this connection may write.
            cur.execute(f"SELECT COUNT(*) FROM {table} WHERE {qkey} = {m}{scope}", (k,))
            if cur.fetchone()[0] != 1:
                raise _refuse(
                    f"row {key} = {json.dumps(canonical(k))[:60]} would fall outside the connection's row filter after this change; nothing was written"
                )
        if kind in SQLITE_KINDS:
            try:
                cur.execute(
                    "UPDATE gpkg_contents SET last_change = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE lower(table_name) = lower(?)",
                    (params.get("table_name") or "",),
                )
            except sqlite3.Error:
                pass
            db.execute("COMMIT")
        else:
            db.commit()
    except EditError:
        _rollback(db, kind)
        raise
    except Exception as e:  # noqa: BLE001 - database errors of either driver
        _rollback(db, kind)
        raise _db_error(e, scrub) from None
    finally:
        db.close()
    return {"target": "source", "written": written, "inserted_keys": inserted}


def _rollback(db, kind) -> None:
    try:
        if kind in SQLITE_KINDS:
            if db.in_transaction:
                db.execute("ROLLBACK")
        else:
            db.rollback()
    except Exception:  # noqa: BLE001 - a broken session cannot roll back; closing it does
        pass
