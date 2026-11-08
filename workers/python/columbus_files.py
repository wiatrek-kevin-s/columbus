"""``files`` connections: a folder of files (or one file) read through a cache
GeoPackage.

A ``files`` connection is read only. Each file it offers is a dataset (named
by F-1). On first use the dataset is converted into the connection's cache
GeoPackage (``cache`` in the connection handle, which the gateway sets to
``<workspaces root>/cache/files/<connection id>.gpkg``), and every read then
goes through the worker's ordinary SQLite/GeoPackage path: the gateway's
allow lists, row filters, hidden fields, read limits and SQL checks apply to
the cache tables exactly as to a GeoPackage's.

The cache is built inside one ``BEGIN IMMEDIATE`` transaction per dataset,
so two engines (two sessions) never build the same table at once: the
second waits, then finds it fresh. A dataset is rebuilt when its source's
real path, size or modification time changes, or when this module's
``BUILD`` number does; tables whose file has gone are dropped.

Formats are looked up in ``READERS`` by extension. This version reads CSV
(R-1 to R-4, R-6c; R-5 encoding); the others named in ``LATER`` are listed
under ``skipped`` until their readers land.
"""

from __future__ import annotations

import csv
import io
import json
import math
import os
import re
import sqlite3
import stat
import struct
import time
from datetime import date, datetime, timezone
from typing import Any, Callable

import columbus_worker as w

FILES_KINDS = w.FILES_KINDS

# Raise when what a build writes changes, so caches made by an older engine
# are rebuilt rather than read.
BUILD = 1

# Supported formats: extension -> reader. A reader returns a ``Table``.
READERS: dict[str, Callable[[str, dict[str, Any]], "Table"]] = {}

# Formats in F-1's list whose readers aren't in this version yet.
LATER = {".xlsx", ".geojson", ".json", ".parquet", ".geoparquet", ".kml", ".fgb"}

# Files that look like data but can't be read: extension -> reason.
REFUSED = {
    ".xls": "an old Excel workbook (.xls), which Columbus doesn't read: save as .xlsx",
}

SKIP_LINK = "link leads outside the connection folder"
SKIP_LOCK = "an Office lock file (the workbook is open in Excel), not data"
SKIP_HIDDEN = "a hidden file"
SKIP_HIDDEN_DIR = "a hidden folder"
SKIP_RESERVED = "its name is reserved by GeoPackage; rename the file"

# The cache's own table of where each dataset came from.
META = "columbus_sources"
_RESERVED = ("gpkg_", "sqlite_", "rtree_")

WKT_NAMES = ("geometry", "wkt", "the_geom", "shape")
PAIR_NAMES = (("lon", "lat"), ("longitude", "latitude"), ("x", "y"), ("point_x", "point_y"))


class Table:
    """One dataset read from a file, before it is written to the cache."""

    def __init__(self, columns: list[str], types: list[str], rows: list[list[Any]],
                 geoms: list[bytes | None] | None, geom_type: str | None, srs_id: int | None,
                 extent: list[float] | None, checks: list[dict[str, Any]]):
        self.columns = columns
        self.types = types
        self.rows = rows
        self.geoms = geoms
        self.geom_type = geom_type
        self.srs_id = srs_id
        self.extent = extent
        self.checks = checks


def _check(id: str, severity: str, message: str, detail: Any = None, fix: str | None = None) -> dict[str, Any]:
    out: dict[str, Any] = {"id": id, "severity": severity, "message": message}
    if detail is not None:
        out["detail"] = detail
    if fix:
        out["fix"] = fix
    return out


# ---------- finding the datasets (F-1, R-16) ----------


def _inside(root_real: str, p: str) -> bool:
    real = os.path.realpath(p)
    return real == root_real or real.startswith(root_real.rstrip(os.sep) + os.sep)


def _hidden(name: str, full: str) -> bool:
    if name.startswith("."):
        return True
    try:
        attrs = getattr(os.stat(full, follow_symlinks=False), "st_file_attributes", 0)
    except OSError:
        return False
    return bool(attrs & getattr(stat, "FILE_ATTRIBUTE_HIDDEN", 0))


def dataset_name(rel_parts: list[str], ext: str) -> str:
    """F-1: the path relative to the connection folder, with forward slashes
    and no extension; every ``.``, ``\\`` or ``:`` left becomes ``_``."""
    parts = list(rel_parts)
    if ext and parts[-1].lower().endswith(ext):
        parts[-1] = parts[-1][: -len(ext)]
    return "/".join(re.sub(r"[.\\:]", "_", p) for p in parts)


def _root(conn: dict[str, Any]) -> tuple[str, list[str] | None]:
    """(connection folder, the one file's name or None for a folder)."""
    path = conn.get("path")
    if not isinstance(path, str) or not path:
        raise w.WorkerError(w.INVALID_PARAMS, "this files connection has no path")
    if not os.path.isabs(path):
        raise w.WorkerError(w.INVALID_PARAMS, f"{path} is not an absolute path; give the full path to the folder or file")
    if os.path.isdir(path):
        return path, None
    if os.path.isfile(path):
        return os.path.dirname(path), [os.path.basename(path)]
    raise w.WorkerError(w.INVALID_PARAMS, f"{path} was not found; check the connection's path")


def scan(conn: dict[str, Any]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """The datasets a connection offers and the files it skips, each with
    its would-be dataset ``name``, its ``file`` (relative, forward slashes)
    and, for skipped ones, the ``reason``."""
    root, only = _root(conn)
    root_real = os.path.realpath(root)
    recursive = bool(conn.get("recursive")) and only is None
    found: list[dict[str, Any]] = []
    skipped: list[dict[str, Any]] = []
    seen_dirs = {root_real}

    def skip(parts: list[str], ext: str, reason: str) -> None:
        skipped.append({"name": dataset_name(parts, ext), "file": "/".join(parts), "reason": reason})

    def walk(folder: str, rel: list[str]) -> None:
        try:
            names = sorted(only if only is not None and not rel else os.listdir(folder))
        except OSError as e:
            raise w.WorkerError(w.ENGINE_ERROR, f"cannot read the folder {folder}: {e}") from None
        for n in names:
            full = os.path.join(folder, n)
            parts = rel + [n]
            if os.path.isdir(full):
                if not recursive:
                    continue
                if _hidden(n, full):
                    skip(parts, "", SKIP_HIDDEN_DIR)
                elif not _inside(root_real, full):
                    skip(parts, "", SKIP_LINK)
                elif os.path.realpath(full) not in seen_dirs:
                    seen_dirs.add(os.path.realpath(full))
                    walk(full, parts)
                continue
            ext = os.path.splitext(n)[1].lower()
            known = ext in READERS or ext in LATER or ext in REFUSED
            if n.startswith("~$"):
                skip(parts, ext, SKIP_LOCK)
            elif not known:
                continue
            elif _hidden(n, full):
                skip(parts, ext, SKIP_HIDDEN)
            elif not _inside(root_real, full):
                skip(parts, ext, SKIP_LINK)
            elif ext in REFUSED:
                skip(parts, ext, REFUSED[ext])
            elif ext not in READERS:
                skip(parts, ext, f"this version of Columbus doesn't read {ext} files yet")
            elif dataset_name(parts, ext).lower().startswith(_RESERVED) or dataset_name(parts, ext).lower() == META:
                skip(parts, ext, SKIP_RESERVED)
            else:
                found.append({"name": dataset_name(parts, ext), "file": "/".join(parts),
                              "path": full, "ext": ext})

    walk(root, [])
    # Names that collide once the dots are gone (compared without case) are
    # refused, both files named.
    by_name: dict[str, list[dict[str, Any]]] = {}
    for d in found:
        by_name.setdefault(d["name"].lower(), []).append(d)
    datasets = []
    for d in found:
        same = by_name[d["name"].lower()]
        if len(same) > 1:
            others = ", ".join(o["file"] for o in same if o is not d)
            skipped.append({"name": d["name"], "file": d["file"],
                            "reason": f"its dataset name {d['name']} is also the name of {others}; rename one of them"})
        else:
            datasets.append(d)
    return datasets, skipped


# ---------- CSV (R-1 to R-4, R-5, R-6c) ----------

_LEADING_ZERO = re.compile(r"^[+-]?0\d")
_THOUSANDS = re.compile(r"^[+-]?\d{1,3}(,\d{3})+(\.\d*)?$")
_INTEGER = re.compile(r"^[+-]?\d+$")
_REAL = re.compile(r"^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$")
_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_DATETIME = re.compile(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}(:?\d{2})?)?$")
_INT64 = 2**63 - 1


def _is_date(v: str) -> bool:
    try:
        date.fromisoformat(v)
        return True
    except ValueError:
        return False


def _is_datetime(v: str) -> bool:
    try:
        datetime.fromisoformat(v.replace("Z", "+00:00"))
        return True
    except ValueError:
        return False


def column_type(values: list[str]) -> str:
    """R-4: TEXT if any value has a leading zero before another digit or a
    thousands separator; else INTEGER if every value is a whole number, REAL
    if every value is a number, DATE or DATETIME if every value is ISO 8601;
    else TEXT. Empty cells don't count (they are null)."""
    vals = [v.strip() for v in values if v is not None and v.strip() != ""]
    if not vals:
        return "TEXT"
    if any(_LEADING_ZERO.match(v) or _THOUSANDS.match(v) for v in vals):
        return "TEXT"
    if all(_INTEGER.match(v) and abs(int(v)) <= _INT64 for v in vals):
        return "INTEGER"
    if all(_REAL.match(v) for v in vals):
        return "REAL"
    if all(_DATE.match(v) and _is_date(v) for v in vals):
        return "DATE"
    if all(_DATETIME.match(v) and _is_datetime(v) for v in vals):
        return "DATETIME"
    return "TEXT"


def _convert(v: str | None, t: str) -> Any:
    if v is None:
        return None
    s = v.strip()
    if s == "":
        return None
    if t == "INTEGER":
        return int(s)
    if t == "REAL":
        return float(s)
    if t in ("DATE", "DATETIME"):
        return s
    return v


def unique_headers(raw: list[str]) -> tuple[list[str], list[dict[str, Any]]]:
    """R-6c: a blank header becomes ``field_<n>``; repeated names (without
    regard to case, as SQL compares them) get ``_2``, ``_3``."""
    out: list[str] = []
    seen: set[str] = set()
    renamed = []
    for i, h in enumerate(raw, start=1):
        name = h.strip() or f"field_{i}"
        base, n = name, 1
        while name.lower() in seen:
            n += 1
            name = f"{base}_{n}"
        seen.add(name.lower())
        out.append(name)
        if name != h:
            renamed.append({"was": h, "now": name})
    checks = []
    if renamed:
        checks.append(_check("csv.headers", "info",
                             "Headers renamed: " + ", ".join(f"{r['was']!r} is {r['now']}" for r in renamed),
                             detail={"columns": [r["now"] for r in renamed], "renamed": renamed}))
    return out, checks


def _decode(raw: bytes) -> tuple[str, list[dict[str, Any]]]:
    """R-5: UTF-8 (a BOM is dropped), else Windows-1252 with a warning."""
    try:
        return raw.decode("utf-8-sig"), []
    except UnicodeDecodeError:
        return raw.decode("cp1252", errors="replace"), [_check(
            "file.encoding", "warn", "The file isn't UTF-8; it was read as Windows-1252",
            detail={"encoding": "cp1252"},
            fix="If letters look wrong, save the file as CSV UTF-8 and query it again")]


def _point_blob(x: float, y: float, srs_id: int) -> bytes:
    """A GeoPackage point: header without an envelope, then little-endian WKB."""
    return b"GP\x00\x01" + struct.pack("<i", srs_id) + struct.pack("<BIdd", 1, 1, x, y)


def _geom_blob(wkb: bytes, bounds, srs_id: int) -> bytes:
    if any(math.isnan(b) for b in bounds):
        return b"GP\x00" + bytes([0x01 | 0x10]) + struct.pack("<i", srs_id) + wkb
    minx, miny, maxx, maxy = bounds
    return b"GP\x00\x03" + struct.pack("<i4d", srs_id, minx, maxx, miny, maxy) + wkb


def _epsg(conn: dict[str, Any]) -> int | None:
    crs = conn.get("crs")
    if not crs:
        return None
    m = re.fullmatch(r"(?i)EPSG:(\d{1,9})", str(crs).strip())
    if not m:
        raise w.WorkerError(w.INVALID_PARAMS, f"the connection's crs {crs!r} must be an EPSG code such as EPSG:4326")
    return int(m.group(1))


def _find_col(columns: list[str], want: str) -> int | None:
    return next((i for i, c in enumerate(columns) if c.lower() == want.lower()), None)


def read_csv(path: str, conn: dict[str, Any]) -> Table:
    with open(path, "rb") as f:
        text, checks = _decode(f.read())
    reader = csv.reader(io.StringIO(text, newline=""))
    try:
        header = next(reader)
    except StopIteration:
        header = []
    columns, more = unique_headers(header)
    checks += more
    width = len(columns)
    rows: list[list[str | None]] = []
    long_rows = 0
    for r in reader:
        if not r:
            continue
        if len(r) > width:
            long_rows += 1
        rows.append((r + [""] * width)[:width])
    if long_rows:
        checks.append(_check("csv.extra_cells", "warn",
                             f"{long_rows} rows have more cells than the header; the extra cells were left out",
                             fix="Check the file for unquoted commas"))

    srs_conn = _epsg(conn)
    wkt_i = next((i for n in WKT_NAMES if (i := _find_col(columns, n)) is not None), None)
    pair: tuple[int, int] | None = None
    if conn.get("x") and conn.get("y"):
        xi, yi = _find_col(columns, str(conn["x"])), _find_col(columns, str(conn["y"]))
        if xi is None or yi is None:
            checks.append(_check("csv.xy_missing", "warn",
                                 f"The connection names coordinate columns {conn['x']} and {conn['y']}, but this file has no such columns",
                                 detail={"columns": [conn["x"], conn["y"]]}))
        else:
            pair = (xi, yi)
    else:
        for xn, yn in PAIR_NAMES:
            xi, yi = _find_col(columns, xn), _find_col(columns, yn)
            if xi is not None and yi is not None:
                pair = (xi, yi)
                break

    geoms: list[bytes | None] | None = None
    geom_type = srs_id = extent = None
    if wkt_i is not None:
        import shapely

        if pair is not None:
            # R-3: WKT wins.
            ignored = [columns[pair[0]], columns[pair[1]]]
            checks.append(_check("csv.geometry_source", "info",
                                 f"Geometry is read from the WKT column {columns[wkt_i]}; the coordinate columns {', '.join(ignored)} are kept as fields only",
                                 detail={"columns": [columns[wkt_i]] + ignored}))
        texts = [(r[wkt_i] or "").strip() or None for r in rows]
        shapes = shapely.from_wkt(texts, on_invalid="ignore")
        bad = sum(1 for t, s in zip(texts, shapes) if t is not None and s is None)
        if bad:
            checks.append(_check("csv.bad_wkt", "warn", f"{bad} rows have geometry text that isn't valid WKT; their geometry is empty",
                                 detail={"columns": [columns[wkt_i]], "rows": bad}))
        srs_id = srs_conn if srs_conn is not None else -1
        wkbs = shapely.to_wkb(shapes, byte_order=1, output_dimension=2)
        bounds = shapely.bounds(shapes)
        geoms = [None if s is None else _geom_blob(b, bd, srs_id) for s, b, bd in zip(shapes, wkbs, bounds)]
        kinds = {shapely.get_type_id(s) for s in shapes if s is not None and not s.is_empty}
        names = {0: "POINT", 1: "LINESTRING", 3: "POLYGON", 4: "MULTIPOINT", 5: "MULTILINESTRING",
                 6: "MULTIPOLYGON", 7: "GEOMETRYCOLLECTION"}
        geom_type = names.get(kinds.pop(), "GEOMETRY") if len(kinds) == 1 else "GEOMETRY"
        real = [s for s in shapes if s is not None and not s.is_empty]
        if real:
            b = shapely.total_bounds(real)
            extent = [float(v) for v in b]
        drop = {wkt_i}
    elif pair is not None:
        xi, yi = pair
        coords = []
        for r in rows:
            try:
                x, y = float((r[xi] or "").strip()), float((r[yi] or "").strip())
                coords.append((x, y) if math.isfinite(x) and math.isfinite(y) else None)
            except ValueError:
                coords.append(None)
        valid = [c for c in coords if c is not None]
        if srs_conn is not None:
            srs_id = srs_conn
        elif valid and all(abs(x) <= 180 and abs(y) <= 90 for x, y in valid):
            srs_id = 4326
        else:
            srs_id = -1
        geoms = [None if c is None else _point_blob(c[0], c[1], srs_id) for c in coords]
        geom_type = "POINT"
        if valid:
            extent = [min(x for x, _ in valid), min(y for _, y in valid),
                      max(x for x, _ in valid), max(y for _, y in valid)]
        missing = len(coords) - len(valid)
        if missing:
            checks.append(_check("csv.no_coordinates", "warn",
                                 f"{missing} rows have no usable coordinates in {columns[xi]}/{columns[yi]}; their geometry is empty",
                                 detail={"columns": [columns[xi], columns[yi]], "rows": missing}))
        drop = set()
    else:
        checks.append(_check("crs.present", "info",
                             "No geometry: the file has no WKT column (geometry, wkt, the_geom, shape) and no coordinate pair (lon/lat, longitude/latitude, x/y, point_x/point_y), so it is a table",
                             fix="If it has coordinates under other names, set x and y on the connection"))
        drop = set()

    keep = [i for i in range(width) if i not in drop]
    out_cols = [columns[i] for i in keep]
    types = [column_type([r[i] for r in rows]) for i in keep]
    out_rows = [[_convert(r[i], t) for i, t in zip(keep, types)] for r in rows]
    return Table(out_cols, types, out_rows, geoms, geom_type, srs_id, extent, checks)


READERS[".csv"] = read_csv


# ---------- the cache GeoPackage ----------


def _q(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _open_cache(conn: dict[str, Any], timeout_s: float) -> sqlite3.Connection:
    path = conn.get("cache")
    if not isinstance(path, str) or not path:
        raise w.WorkerError(w.INVALID_PARAMS, "files connection has no cache path")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    try:
        db = sqlite3.connect(path, timeout=max(5.0, timeout_s), isolation_level=None, check_same_thread=False)
    except sqlite3.Error as e:
        raise w.WorkerError(w.ENGINE_ERROR, f"cannot open the cache {path}: {e}") from None
    db.execute("BEGIN IMMEDIATE")
    try:
        if db.execute("PRAGMA application_id").fetchone()[0] != 0x47504B47:
            _init_gpkg(db)
        db.execute("COMMIT")
    except BaseException:
        db.execute("ROLLBACK")
        db.close()
        raise
    return db


def _init_gpkg(db: sqlite3.Connection) -> None:
    """The tables a GeoPackage must have, and the cache's own."""
    db.execute("PRAGMA application_id = 1196444487")
    db.execute("PRAGMA user_version = 10400")
    db.execute("CREATE TABLE IF NOT EXISTS gpkg_spatial_ref_sys (srs_name TEXT NOT NULL, srs_id INTEGER PRIMARY KEY, "
               "organization TEXT NOT NULL, organization_coordsys_id INTEGER NOT NULL, definition TEXT NOT NULL, description TEXT)")
    db.executemany("INSERT OR IGNORE INTO gpkg_spatial_ref_sys VALUES (?,?,?,?,?,?)", [
        ("Undefined cartesian SRS", -1, "NONE", -1, "undefined", "undefined cartesian coordinate reference system"),
        ("Undefined geographic SRS", 0, "NONE", 0, "undefined", "undefined geographic coordinate reference system"),
        ("WGS 84 geodetic", 4326, "EPSG", 4326,
         'GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],'
         'AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,'
         'AUTHORITY["EPSG","9122"]],AXIS["Latitude",NORTH],AXIS["Longitude",EAST],AUTHORITY["EPSG","4326"]]',
         "longitude/latitude coordinates in decimal degrees on the WGS 84 spheroid"),
    ])
    db.execute("CREATE TABLE IF NOT EXISTS gpkg_contents (table_name TEXT NOT NULL PRIMARY KEY, data_type TEXT NOT NULL, "
               "identifier TEXT UNIQUE, description TEXT DEFAULT '', last_change DATETIME NOT NULL DEFAULT "
               "(strftime('%Y-%m-%dT%H:%M:%fZ','now')), min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, "
               "srs_id INTEGER, CONSTRAINT fk_gc_r_srs_id FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id))")
    db.execute("CREATE TABLE IF NOT EXISTS gpkg_geometry_columns (table_name TEXT NOT NULL, column_name TEXT NOT NULL, "
               "geometry_type_name TEXT NOT NULL, srs_id INTEGER NOT NULL, z TINYINT NOT NULL, m TINYINT NOT NULL, "
               "CONSTRAINT pk_geom_cols PRIMARY KEY (table_name, column_name))")
    db.execute(f"CREATE TABLE IF NOT EXISTS {META} (dataset TEXT PRIMARY KEY, source TEXT NOT NULL, size INTEGER NOT NULL, "
               "mtime_ns INTEGER NOT NULL, build INTEGER NOT NULL, checks TEXT NOT NULL, geometry_column TEXT)")


def _stamp(d: dict[str, Any]) -> tuple[str, int, int]:
    st = os.stat(d["path"])
    return os.path.realpath(d["path"]), st.st_size, st.st_mtime_ns


def _fresh(db: sqlite3.Connection, d: dict[str, Any], stamp: tuple[str, int, int]) -> bool:
    row = db.execute(f"SELECT source, size, mtime_ns, build FROM {META} WHERE dataset = ?", (d["name"],)).fetchone()
    return row is not None and tuple(row) == (*stamp, BUILD)


def _drop(db: sqlite3.Connection, name: str) -> None:
    db.execute(f"DROP TABLE IF EXISTS {_q(name)}")
    db.execute("DELETE FROM gpkg_geometry_columns WHERE table_name = ?", (name,))
    db.execute("DELETE FROM gpkg_contents WHERE table_name = ?", (name,))
    db.execute(f"DELETE FROM {META} WHERE dataset = ?", (name,))


def _srs(db: sqlite3.Connection, srs_id: int) -> None:
    if db.execute("SELECT 1 FROM gpkg_spatial_ref_sys WHERE srs_id = ?", (srs_id,)).fetchone():
        return
    from pyproj import CRS
    from pyproj.exceptions import CRSError

    try:
        crs = CRS.from_epsg(srs_id)
    except CRSError:
        raise w.WorkerError(w.INVALID_PARAMS, f"EPSG:{srs_id} (the connection's crs) is not a known coordinate system") from None
    db.execute("INSERT INTO gpkg_spatial_ref_sys VALUES (?,?,?,?,?,?)",
               (crs.name, srs_id, "EPSG", srs_id, crs.to_wkt("WKT1_GDAL"), None))


def _build(db: sqlite3.Connection, d: dict[str, Any], conn: dict[str, Any], stamp: tuple[str, int, int]) -> None:
    """Read the file and write its table, in the caller's transaction."""
    try:
        t = READERS[d["ext"]](d["path"], conn)
    except OSError as e:
        raise w.WorkerError(w.ENGINE_ERROR, f"cannot read {d['file']}: {e}") from None
    except csv.Error as e:
        raise w.WorkerError(w.ENGINE_ERROR, f"{d['file']} is not a readable CSV file: {e}") from None
    name = d["name"]
    _drop(db, name)
    taken = {c.lower() for c in t.columns}
    fid = next(n for n in ("fid", "fid_", "columbus_fid") if n not in taken)
    gcol = None
    if t.geoms is not None:
        gcol = next(n for n in ("geometry", "geom", "geometry_") if n not in taken)
    cols = [f"{_q(fid)} INTEGER PRIMARY KEY AUTOINCREMENT"] + [f"{_q(c)} {ty}" for c, ty in zip(t.columns, t.types)]
    if gcol:
        cols.append(f"{_q(gcol)} {t.geom_type or 'GEOMETRY'}")
    db.execute(f"CREATE TABLE {_q(name)} ({', '.join(cols)})")
    names = [_q(c) for c in t.columns] + ([_q(gcol)] if gcol else [])
    insert = f"INSERT INTO {_q(name)} ({', '.join(names)}) VALUES ({', '.join('?' for _ in names)})"
    if gcol:
        db.executemany(insert, (r + [g] for r, g in zip(t.rows, t.geoms)))
    else:
        db.executemany(insert, t.rows)
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    ext = t.extent or [None] * 4
    if gcol:
        if t.srs_id not in (-1, 0):
            _srs(db, t.srs_id)
        db.execute("INSERT INTO gpkg_contents VALUES (?,?,?,?,?,?,?,?,?,?)",
                   (name, "features", name, "", now, *ext, t.srs_id))
        db.execute("INSERT INTO gpkg_geometry_columns VALUES (?,?,?,?,0,0)", (name, gcol, t.geom_type or "GEOMETRY", t.srs_id))
    else:
        db.execute("INSERT INTO gpkg_contents VALUES (?,?,?,?,?,NULL,NULL,NULL,NULL,NULL)", (name, "attributes", name, "", now))
    db.execute(f"INSERT INTO {META} VALUES (?,?,?,?,?,?,?)", (name, *stamp, BUILD, json.dumps(t.checks), gcol))


def sync(conn: dict[str, Any], timeout_s: float) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Bring the cache up to date with the folder: build what is missing or
    stale, drop what has gone. (datasets, skipped)."""
    datasets, skipped = scan(conn)
    deadline = time.monotonic() + timeout_s
    db = _open_cache(conn, timeout_s)
    try:
        for d in datasets:
            if time.monotonic() > deadline:
                raise w.WorkerError(w.TIMEOUT, f"converting the files took more than {timeout_s:g} s")
            try:
                stamp = _stamp(d)
            except OSError:
                continue
            if _fresh(db, d, stamp):
                continue
            db.execute("BEGIN IMMEDIATE")
            try:
                # Another engine may have built it while this one waited.
                if not _fresh(db, d, stamp):
                    _build(db, d, conn, stamp)
                db.execute("COMMIT")
            except BaseException:
                db.execute("ROLLBACK")
                raise
        current = {d["name"] for d in datasets}
        gone = [r[0] for r in db.execute(f"SELECT dataset FROM {META}") if r[0] not in current]
        if gone:
            db.execute("BEGIN IMMEDIATE")
            try:
                for name in gone:
                    _drop(db, name)
                db.execute("COMMIT")
            except BaseException:
                db.execute("ROLLBACK")
                raise
        notes = {r[0]: json.loads(r[1]) for r in db.execute(f"SELECT dataset, checks FROM {META}")}
    finally:
        db.close()
    for d in datasets:
        d["checks"] = notes.get(d["name"], [])
    return datasets, skipped


def cache_conn(conn: dict[str, Any]) -> dict[str, Any]:
    """The handle the worker's GeoPackage path reads the cache through."""
    return {"kind": "gpkg", "path": conn["cache"]}


def files_list(conn: dict[str, Any], timeout_s: float, only: str | None = None,
               with_checks: bool = False) -> dict[str, Any]:
    datasets, skipped = sync(conn, timeout_s)
    by_name = {d["name"]: d for d in datasets}
    entries = []
    for e in w._sqlite_list(cache_conn(conn), timeout_s):
        d = by_name.get(e["name"])
        if d is None or (only and e["name"].lower() != only.lower()):
            continue
        e["file"] = d["file"]
        if with_checks:
            e["file_checks"] = d["checks"]
        entries.append(e)
    return {"datasets": entries, "skipped": skipped}


def files_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None, sink=None):
    sync(conn, timeout_s)
    return w._sqlite_fetch(cache_conn(conn), sql, max_rows, timeout_s, keep_factory, sink)


def skipped_reason(conn: dict[str, Any], dataset: str) -> str | None:
    """Why a dataset name the person used isn't offered, if a skipped file
    would have had it."""
    _datasets, skipped = scan(conn)
    for s in skipped:
        if s["name"].lower() == dataset.lower():
            return f"{s['file']} is not offered: {s['reason']}"
    return None
