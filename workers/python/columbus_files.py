"""``files`` connections: a folder of files (or one file) read through a cache
GeoPackage.

A ``files`` connection is read only. Each file it offers is a dataset (named
by F-1); a workbook's sheets and a KML file's layers are one dataset each,
``<file>/<sheet or layer>``. On first use the dataset is converted into the
connection's cache GeoPackage (``cache`` in the connection handle, which the
gateway sets to ``<workspaces root>/cache/files/<connection id>.gpkg``), and
every read then goes through the worker's ordinary SQLite/GeoPackage path:
the gateway's allow lists, row filters, hidden fields, read limits and SQL
checks apply to the cache tables exactly as to a GeoPackage's.

Formats are looked up in ``READERS`` by extension: CSV and Excel are read
here (R-1 to R-6c; Excel with the standard library, so the first row is
always the header and date cells are told apart by their number format);
GeoJSON, KML and FlatGeobuf through GDAL (pyogrio), each only after its
content is checked to be that format and only through that format's GDAL
driver (GDAL picks a driver by content, and VRT XML can read any file);
GeoParquet through ``geopandas.read_parquet``.

Freshness (R-7): the cache records each dataset's file size, modification
time and SHA-256. A dataset whose size and time are unchanged is fresh; if
either changed the file is hashed, and only a different hash rebuilds it.
Each build happens inside one ``BEGIN IMMEDIATE`` transaction and re-checks
first, so two engines (two sessions) on a stale dataset rebuild it once: the
second waits, then finds it fresh (R-7b). ``builds`` in ``columbus_sources``
counts the rebuilds. Tables whose file has gone are dropped (R-7c). A file
that can't be read is left out and listed under ``skipped`` with the cause
(REQ-4), and its old table is dropped so stale rows are never served.
"""

from __future__ import annotations

import codecs
import csv
import hashlib
import io
import itertools
import json
import math
import os
import posixpath
import re
import sqlite3
import stat
import struct
import time
import zipfile
from datetime import date, datetime, timedelta, timezone
from typing import Any, Callable
from xml.etree import ElementTree as ET

import columbus_worker as w

FILES_KINDS = w.FILES_KINDS

# Raise when what a build writes changes, so caches made by an older engine
# are rebuilt rather than read.
BUILD = 4  # 3: M values kept (T-19); 4: R-tree spatial index (T-13)

# Supported formats: extension -> reader(path, conn, layer). A reader
# returns a ``Table`` or raises ``Unreadable``.
READERS: dict[str, Callable[[str, dict[str, Any], str | None], "Table"]] = {}

# Formats whose datasets are one per sheet or layer: ``<file>/<layer>``.
LAYERED = {".xlsx", ".kml"}

# Files that look like data but can't be read: extension -> reason.
REFUSED = {
    ".xls": "an old Excel workbook (.xls), which Columbus doesn't read: save as .xlsx",
}

SKIP_LINK = "link leads outside the connection folder"
SKIP_LOCK = "an Office lock file (the workbook is open in Excel), not data"
SKIP_HIDDEN = "a hidden file"
SKIP_HIDDEN_DIR = "a hidden folder"
SKIP_RESERVED = "its name is reserved by GeoPackage; rename the file"
SKIP_NOT_GEOJSON = "a .json file that isn't GeoJSON (no FeatureCollection, Feature or geometry), so not data Columbus reads"
SKIP_EMPTY_SHEET = "an empty sheet"

# The cache's own table of where each dataset came from, and of files whose
# conversion ran past a call's time (R-101).
META = "columbus_sources"
SLOW = "columbus_slow"
_RESERVED = ("gpkg_", "sqlite_", "rtree_")

WKT_NAMES = ("geometry", "wkt", "the_geom", "shape")
PAIR_NAMES = (("lon", "lat"), ("longitude", "latitude"), ("x", "y"), ("point_x", "point_y"))

# The GDAL drivers each format may be opened with.
_DRIVERS = {".geojson": {"GeoJSON"}, ".json": {"GeoJSON"}, ".kml": {"KML", "LIBKML"}, ".fgb": {"FlatGeobuf"}}

# SRS ids for coordinate systems without an EPSG code, numbered from here.
_CUSTOM_SRS = 1_000_000_000


class Unreadable(Exception):
    """A file that can't be read: the message says why (without the file
    name, which the caller adds)."""


class Table:
    """One dataset read from a file, before it is written to the cache.
    ``srs_id`` is an EPSG code, -1 for none, or None with ``crs_wkt`` for a
    coordinate system without one."""

    def __init__(self, columns: list[str], types: list[str], rows: list[list[Any]],
                 geoms: list[bytes | None] | None, geom_type: str | None, srs_id: int | None,
                 extent: list[float] | None, checks: list[dict[str, Any]],
                 has_z: bool = False, crs_wkt: str | None = None):
        self.columns = columns
        self.types = types
        self.rows = rows
        self.geoms = geoms
        self.geom_type = geom_type
        self.srs_id = srs_id
        self.extent = extent
        self.checks = checks
        self.has_z = has_z
        self.crs_wkt = crs_wkt
        # None: work it out from the blobs as they are written.
        self.has_m: bool | None = None

    def prepare(self, tick: Callable[[], None]) -> None:
        """Read what the table needs to know before it is written (types,
        geometry type, CRS, extent). A table read whole knows already."""

    def batches(self, size: int, tick: Callable[[], None]):
        """(rows, geometry blobs or None) in batches of ``size`` rows."""
        for at in range(0, len(self.rows), size):
            tick()
            yield self.rows[at:at + size], None if self.geoms is None else self.geoms[at:at + size]


# R-101: rows written to the cache per batch, and how often a long read
# checks the call's time.
BATCH = 50_000

# R-101: the largest file of each format Columbus converts. CSV and Excel
# are streamed into the cache in batches, so memory stays flat whatever
# their size; their limit keeps a conversion inside one call's time. For
# CSV that is 300 MB at the default 120 s (a conservative 2.5 MB a second:
# a 300 MB file of points converts in about 90 s on a slow two-core
# machine), and more where the connection allows longer calls
# (limits.seconds). GeoJSON, KML and FlatGeobuf are read whole by GDAL
# (about 6 to 8 times their size in memory), and GeoParquet whole by
# geopandas, whose memory follows its rows and columns (about 150 bytes a
# value) rather than its compressed size; their limits keep the engine
# within about 2 GB. A larger file is listed under ``skipped`` with its
# size and the fix, and the rest of the folder works. A file under its
# limit whose conversion still runs past the call's time is stopped and
# skipped the same way (``_TooSlow``). COLUMBUS_FILES_MAX_MB sets one size
# limit for every format instead (for tests and administrators).
MAX_BYTES = {
    ".csv": 256 << 20,
    ".xlsx": 64 << 20,
    ".geojson": 256 << 20,
    ".json": 256 << 20,
    ".kml": 256 << 20,
    ".fgb": 256 << 20,
    ".parquet": 256 << 20,
    ".geoparquet": 256 << 20,
}
CSV_BYTES_PER_SECOND = 5 << 19  # 2.5 MB
MAX_PARQUET_CELLS = 10_000_000


def max_bytes(ext: str, timeout_s: float = 0) -> int:
    env = os.environ.get("COLUMBUS_FILES_MAX_MB", "").strip()
    if env:
        try:
            return max(0, int(float(env) * (1 << 20)))
        except ValueError:
            pass
    limit = MAX_BYTES.get(ext, 256 << 20)
    if ext == ".csv":
        limit = max(limit, int(timeout_s * CSV_BYTES_PER_SECOND))
    return limit


def size_text(n: int) -> str:
    """1.2 GB, 340 MB, 12 KB: sizes as an analyst reads them."""
    for unit, scale in (("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10)):
        if n >= scale:
            v = n / scale
            return f"{v:.1f} {unit}" if v < 10 else f"{v:.0f} {unit}"
    return f"{n} bytes"


_FIX_BIG = "split it into smaller files, or convert it to a GeoPackage and connect that (a GeoPackage is read directly, at any size)"
_RAISE = "; an administrator can also raise limits.seconds"


def _too_big(path: str, ext: str, size: int, timeout_s: float) -> str | None:
    """Why a file is too large to convert (R-14: the cause and the fix), or
    None."""
    limit = max_bytes(ext, timeout_s)
    by_time = ext == ".csv" and not os.environ.get("COLUMBUS_FILES_MAX_MB", "").strip()
    if size > limit:
        within = f" within one call's {timeout_s:g} s" if by_time and timeout_s else ""
        return (f"it is {size_text(size)}, more than the {size_text(limit)} Columbus converts from one {ext} file{within}; "
                f"{_FIX_BIG}{_RAISE if by_time else ''}")
    if ext in (".parquet", ".geoparquet") and not os.environ.get("COLUMBUS_FILES_MAX_MB", "").strip():
        try:
            import pyarrow.parquet as pq

            meta = pq.read_metadata(path)
        except Exception:  # noqa: BLE001 - a broken file is reported when it is read
            return None
        if meta.num_rows * max(1, meta.num_columns) > MAX_PARQUET_CELLS:
            return (f"it is {size_text(size)} holding {meta.num_rows:,} rows of {meta.num_columns} columns, more than the "
                    f"{MAX_PARQUET_CELLS:,} values Columbus converts from one GeoParquet file; {_FIX_BIG}")
    return None


class _TooSlow(Exception):
    """A conversion that ran past the call's time limit."""


def _check(id: str, severity: str, message: str, detail: Any = None, fix: str | None = None) -> dict[str, Any]:
    out: dict[str, Any] = {"id": id, "severity": severity, "message": message}
    if detail is not None:
        out["detail"] = detail
    if fix:
        out["fix"] = fix
    return out


def _no_crs(what: str) -> dict[str, Any]:
    """R-1, R-2, A-5: geometry with no coordinate system is a warning."""
    return _check("crs.present", "warn", f"{what} has no coordinate reference system",
                  fix="Set crs on the connection to the file's EPSG code (ask the data's owner if unsure)")


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


def _clean(part: str) -> str:
    return re.sub(r"[.\\:]", "_", part)


def dataset_name(rel_parts: list[str], ext: str, layer: str | None = None) -> str:
    """F-1: the path relative to the connection folder, with forward slashes
    and no extension, then ``/<layer>`` for a sheet or layer; every ``.``,
    ``\\`` or ``:`` left becomes ``_``."""
    parts = list(rel_parts)
    if ext and parts[-1].lower().endswith(ext):
        parts[-1] = parts[-1][: -len(ext)]
    if layer is not None:
        parts.append(layer)
    return "/".join(_clean(p) for p in parts)


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


# ---------- what a file holds, by its bytes ----------


def _head(path: str, n: int = 65536) -> bytes:
    with open(path, "rb") as f:
        return f.read(n)


_GEOJSON_TYPE = re.compile(
    rb'"type"\s*:\s*"(FeatureCollection|Feature|Point|MultiPoint|LineString|MultiLineString|Polygon|MultiPolygon|GeometryCollection)"')


def _json_start(head: bytes) -> bytes:
    if head.startswith(b"\xef\xbb\xbf"):
        head = head[3:]
    return head.lstrip()


def is_geojson(path: str) -> bool:
    """A .json file is offered only when it holds GeoJSON: an object with a
    GeoJSON ``type`` near its start."""
    head = _json_start(_head(path))
    return head.startswith(b"{") and _GEOJSON_TYPE.search(head) is not None


def _sniff(path: str, ext: str) -> None:
    """Refuse a file whose content isn't its format, before any library
    that picks a reader by content opens it."""
    head = _head(path, 4096)
    if ext == ".xlsx":
        if head.startswith(b"\xd0\xcf\x11\xe0"):
            raise Unreadable("it is not an .xlsx workbook (password-protected and old .xls workbooks can't be read; save it as an unprotected .xlsx)")
        ok = head.startswith(b"PK\x03\x04")
    elif ext in (".geojson", ".json"):
        ok = _json_start(head).startswith(b"{")
    elif ext == ".kml":
        text = head.lstrip(b"\xef\xbb\xbf").lstrip()
        ok = text.startswith(b"<") and re.search(rb"<(\w+:)?kml[\s>]", head) is not None and b"OGRVRTDataSource" not in head
    elif ext == ".fgb":
        ok = head[:4] == b"fgb\x03"
    elif ext in (".parquet", ".geoparquet"):
        ok = head[:4] == b"PAR1"
    else:
        ok = True
    if not ok:
        raise Unreadable(f"its content isn't {ext} data")


# Layers of multi-layer files, by (real path, size, modification time), so a
# folder is listed without re-reading unchanged workbooks.
_LAYERS: dict[tuple[str, int, int], list[tuple[str, bool]]] = {}


def _layers(path: str, ext: str) -> list[tuple[str, bool]]:
    """(layer name, has data) for each sheet or layer."""
    st = os.stat(path)
    key = (os.path.realpath(path), st.st_size, st.st_mtime_ns)
    if key in _LAYERS:
        return _LAYERS[key]
    _sniff(path, ext)
    if ext == ".xlsx":
        with _Workbook(path) as book:
            out = [(name, has) for name, _part, has in book.sheets(check_data=True)]
    else:
        out = [(str(name), True) for name, _type in _ogr_layers(path, ext)]
    if len(_LAYERS) > 256:
        _LAYERS.clear()
    _LAYERS[key] = out
    return out


def scan(conn: dict[str, Any], timeout_s: float = 0) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """The datasets a connection offers and the files it skips, each with
    its would-be dataset ``name``, its ``file`` (relative, forward slashes)
    and, for skipped ones, the ``reason``."""
    root, only = _root(conn)
    root_real = os.path.realpath(root)
    recursive = bool(conn.get("recursive")) and only is None
    found: list[dict[str, Any]] = []
    skipped: list[dict[str, Any]] = []
    seen_dirs = {root_real}

    def skip(parts: list[str], ext: str, reason: str, layer: str | None = None) -> None:
        skipped.append({"name": dataset_name(parts, ext, layer), "file": "/".join(parts), "reason": reason})

    def offer(parts: list[str], ext: str, full: str, layer: str | None) -> None:
        name = dataset_name(parts, ext, layer)
        if name.lower().startswith(_RESERVED) or name.lower() in (META, SLOW):
            skip(parts, ext, SKIP_RESERVED, layer)
            return
        found.append({"name": name, "file": "/".join(parts), "path": full, "ext": ext, "layer": layer})

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
            known = ext in READERS or ext in REFUSED
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
            elif ext == ".json" and not _quiet(lambda: is_geojson(full), True):
                skip(parts, ext, SKIP_NOT_GEOJSON)
            elif big := _too_big(full, ext, _quiet(lambda: os.stat(full).st_size, 0), timeout_s):
                # R-101: refused before anything reads it.
                skip(parts, ext, big)
            elif ext in LAYERED:
                try:
                    layers = _layers(full, ext)
                except Exception as e:  # noqa: BLE001 - any failure to open is the file's
                    skip(parts, ext, f"it could not be read ({_cause(e)})")
                    continue
                if not layers:
                    skip(parts, ext, "it has no sheets or layers")
                for layer, has_data in layers:
                    if has_data:
                        offer(parts, ext, full, layer)
                    else:
                        skip(parts, ext, SKIP_EMPTY_SHEET, layer)
            else:
                offer(parts, ext, full, None)

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
            others = ", ".join(_where(o) for o in same if o is not d)
            skipped.append({"name": d["name"], "file": d["file"],
                            "reason": f"its dataset name {d['name']} is also the name of {others}; rename one of them"})
        else:
            datasets.append(d)
    return datasets, skipped


def _where(d: dict[str, Any]) -> str:
    return d["file"] if d.get("layer") is None else f"{d['layer']!r} in {d['file']}"


def _quiet(fn, default):
    try:
        return fn()
    except OSError:
        return default


def _cause(e: BaseException) -> str:
    if isinstance(e, Unreadable):
        return str(e)
    if isinstance(e, PermissionError):
        return "permission denied; it may be open in another program"
    if isinstance(e, OSError):
        return e.strerror or str(e)
    text = str(e).strip() or type(e).__name__
    # GDAL messages can carry the full path; the file is named by the caller.
    return text.splitlines()[0][:300]


# ---------- CSV (R-1 to R-4, R-5, R-6c) ----------

_LEADING_ZERO = re.compile(r"^[+-]?0\d")
_THOUSANDS = re.compile(r"^[+-]?\d{1,3}(,\d{3})+(\.\d*)?$")
_INTEGER = re.compile(r"^[+-]?\d+$")
_REAL = re.compile(r"^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$")
_DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_DATETIME = re.compile(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}(:?\d{2})?)?$")
_INT64 = 2**63 - 1


def _all_lines(pattern: re.Pattern) -> re.Pattern:
    """Lines separated by newlines, every one matching ``pattern``."""
    body = pattern.pattern.lstrip("^").rstrip("$")
    return re.compile(f"(?:{body})(?:\\n(?:{body}))*")


# The same tests as above over many values at once (one per line).
_ANY_TEXT = re.compile(r"^[+-]?0\d|^[+-]?\d{1,3}(,\d{3})+(\.\d*)?$", re.M)
_ALL_INTEGER = _all_lines(_INTEGER)
_ALL_REAL = _all_lines(_REAL)
_ALL_DATE = _all_lines(_DATE)
_ALL_DATETIME = _all_lines(_DATETIME)


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


class _Typer:
    """R-4 for one column, a batch of values at a time, so a file is typed
    without holding its cells: TEXT if any value has a leading zero before
    another digit or a thousands separator; else INTEGER if every value is a
    whole number, REAL if every value is a number, DATE or DATETIME if every
    value is ISO 8601; else TEXT. Empty cells don't count (they are null).
    Each test is an "any" or an "every" over all the values, so batches give
    the same answer as the whole column."""

    __slots__ = ("seen", "text", "can_int", "can_real", "can_date", "can_dt")

    def __init__(self) -> None:
        self.seen = self.text = False
        self.can_int = self.can_real = self.can_date = self.can_dt = True

    def done(self) -> bool:
        """Nothing more can change the answer (it is TEXT)."""
        return self.text or not (self.can_int or self.can_real or self.can_date or self.can_dt)

    def add(self, values) -> None:
        if self.done():
            return
        vals = {v.strip() for v in values if v is not None}
        vals.discard("")
        if not vals:
            return
        self.seen = True
        if any("\n" in v or "\r" in v for v in vals):
            # A value over more than one line: no number or date has one, so
            # the column is TEXT (unless a value has a leading zero, which is
            # TEXT too).
            self.can_int = self.can_real = self.can_date = self.can_dt = False
            return
        # Each test on all the values at once, one per line, so the regular
        # expressions run over one string (the same tests, much faster).
        joined = "\n".join(vals)
        if _ANY_TEXT.search(joined):
            self.text = True
            return
        if self.can_int and not (_ALL_INTEGER.fullmatch(joined)
                                 and all(abs(int(v)) <= _INT64 for v in vals if len(v) > 18)):
            self.can_int = False
        if self.can_real and not _ALL_REAL.fullmatch(joined):
            self.can_real = False
        if self.can_date and not (_ALL_DATE.fullmatch(joined) and all(_is_date(v) for v in vals)):
            self.can_date = False
        if self.can_dt and not (_ALL_DATETIME.fullmatch(joined) and all(_is_datetime(v) for v in vals)):
            self.can_dt = False

    def type(self) -> str:
        if not self.seen or self.text:
            return "TEXT"
        if self.can_int:
            return "INTEGER"
        if self.can_real:
            return "REAL"
        if self.can_date:
            return "DATE"
        if self.can_dt:
            return "DATETIME"
        return "TEXT"


def column_type(values: list[str | None]) -> str:
    """R-4 for a whole column (see ``_Typer``)."""
    t = _Typer()
    t.add(values)
    return t.type()


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


def unique_headers(raw: list[str], what: str = "Headers") -> tuple[list[str], list[dict[str, Any]]]:
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
                             f"{what} renamed: " + ", ".join(f"{r['was']!r} is {r['now']}" for r in renamed),
                             detail={"columns": [r["now"] for r in renamed], "renamed": renamed}))
    return out, checks


def _point_blob(x: float, y: float, srs_id: int) -> bytes:
    """A GeoPackage point: header without an envelope, then little-endian WKB."""
    return b"GP\x00\x01" + struct.pack("<i", srs_id) + struct.pack("<BIdd", 1, 1, x, y)


def _geom_blob(wkb: bytes, bounds, srs_id: int) -> bytes:
    if any(math.isnan(b) for b in bounds):
        return b"GP\x00" + bytes([0x01 | 0x10]) + struct.pack("<i", srs_id) + wkb
    minx, miny, maxx, maxy = bounds
    return b"GP\x00\x03" + struct.pack("<i4d", srs_id, minx, maxx, miny, maxy) + wkb


_TYPE_NAMES = {0: "POINT", 1: "LINESTRING", 3: "POLYGON", 4: "MULTIPOINT", 5: "MULTILINESTRING",
               6: "MULTIPOLYGON", 7: "GEOMETRYCOLLECTION"}


def _blobs(shapes, srs_id: int) -> list[bytes | None]:
    """GeoPackage blobs for shapely geometries (None for no geometry)."""
    import shapely

    # Dimension 4: each geometry keeps its own Z and M (a 2D one stays 2D).
    wkbs = shapely.to_wkb(shapes, byte_order=1, output_dimension=4, flavor="iso")
    bounds = shapely.bounds(shapes)
    return [None if s is None else _geom_blob(b, bd, srs_id) for s, b, bd in zip(shapes, wkbs, bounds)]


def _shapes_to_blobs(shapes, srs_id: int):
    """(GeoPackage blobs, geometry type name, extent, has Z) for shapely
    geometries (None for no geometry). The blobs are written with srs id
    ``srs_id``; a custom one is patched in by the build."""
    import shapely

    real = [s for s in shapes if s is not None and not s.is_empty]
    has_z = bool(real) and bool(shapely.has_z(real).any())
    geoms = _blobs(shapes, srs_id)
    kinds = {int(shapely.get_type_id(s)) for s in real}
    geom_type = _TYPE_NAMES.get(kinds.pop(), "GEOMETRY") if len(kinds) == 1 else "GEOMETRY"
    extent = [float(v) for v in shapely.total_bounds(real)] if real else None
    return geoms, geom_type, extent, has_z


def _blob_has_m(blob: bytes | None) -> bool:
    """Whether a GeoPackage blob's (little-endian ISO) WKB has M."""
    if not blob or len(blob) < 8:
        return False
    env = {0: 0, 1: 32, 2: 48, 3: 48, 4: 64}.get((blob[3] >> 1) & 0x07, 0)
    at = 8 + env
    if len(blob) < at + 5:
        return False
    code = struct.unpack_from("<I" if blob[at] == 1 else ">I", blob, at + 1)[0]
    return bool(code & 0x40000000) or (code & 0x0FFFFFFF) // 1000 in (2, 3)


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


def _convert_column(values: list[Any], t: str) -> list[Any]:
    """``_convert`` for a column of cells at once."""
    if t == "INTEGER":
        return [None if v is None or not (s := v.strip()) else int(s) for v in values]
    if t == "REAL":
        return [None if v is None or not (s := v.strip()) else float(s) for v in values]
    if t in ("DATE", "DATETIME"):
        return [None if v is None or not (s := v.strip()) else s for v in values]
    return [None if v is None or not v.strip() else v for v in values]


def _csv_encoding(path: str) -> tuple[str, str, list[dict[str, Any]]]:
    """R-5 without holding the file: (encoding, errors, checks). UTF-8 if
    every byte decodes as UTF-8 (a BOM is dropped), else Windows-1252 with
    a warning."""
    dec = codecs.getincrementaldecoder("utf-8")()
    with open(path, "rb") as f:
        try:
            for block in iter(lambda: f.read(1 << 20), b""):
                dec.decode(block)
            dec.decode(b"", final=True)
        except UnicodeDecodeError:
            return "cp1252", "replace", [_check(
                "file.encoding", "warn", "The file isn't UTF-8; it was read as Windows-1252",
                detail={"encoding": "cp1252"},
                fix="If letters look wrong, save the file as CSV UTF-8 and query it again")]
    return "utf-8", "strict", []


class _CsvSource:
    """A CSV file's rows, read from the file each time they are asked for:
    the header is the first row; blank rows are left out; each row is cut or
    padded with "" to the header's width."""

    def __init__(self, path: str):
        self.path = path
        self.long_rows = 0
        self.checks: list[dict[str, Any]] = []

    def _open(self):
        raw = open(self.path, "rb")
        try:
            if self.encoding == "utf-8" and raw.read(3) != b"\xef\xbb\xbf":
                raw.seek(0)  # a BOM is dropped (R-5); without one, read from the start
            return io.TextIOWrapper(raw, encoding=self.encoding, errors=self.errors, newline="")
        except BaseException:
            raw.close()
            raise

    def open(self, tick: Callable[[], None]) -> list[str]:
        self.encoding, self.errors, self.checks = _csv_encoding(self.path)
        with self._open() as fh:
            try:
                return next(csv.reader(fh), [])
            except csv.Error as e:
                raise Unreadable(f"it is not a readable CSV file: {e}") from None

    def rows(self, width: int, first: bool, size: int, tick: Callable[[], None]):
        """The rows after the header, in lists of up to ``size``."""
        long_rows = 0
        pad = [""] * width
        with self._open() as fh:
            reader = csv.reader(fh)
            try:
                next(reader, None)
                while chunk := list(itertools.islice(reader, size)):
                    tick()
                    if not width or any(len(r) != width for r in chunk):
                        long_rows += sum(1 for r in chunk if len(r) > width)
                        chunk = [r if len(r) == width else (r + pad)[:width] for r in chunk if r]
                    if chunk:
                        yield chunk
            except csv.Error as e:
                raise Unreadable(f"it is not a readable CSV file: {e}") from None
        if first:
            self.long_rows = long_rows

    def final_checks(self) -> list[dict[str, Any]]:
        checks = list(self.checks)
        if self.long_rows:
            checks.append(_check("csv.extra_cells", "warn",
                                 f"{self.long_rows} rows have more cells than the header; the extra cells were left out",
                                 fix="Check the file for unquoted commas"))
        return checks


def read_csv(path: str, conn: dict[str, Any], _layer: str | None = None) -> Table:
    """R-1 to R-6c. The file is read when the table is written, twice (types
    first, then the rows), a batch at a time (R-101)."""
    return _TextTable(_CsvSource(path), conn, "Headers")


def _batched(it, size: int, tick: Callable[[], None]):
    batch: list[Any] = []
    for item in it:
        batch.append(item)
        if len(batch) >= size:
            tick()
            yield batch
            batch = []
    if batch:
        tick()
        yield batch


def _guarded(gen):
    """A file's rows, with whatever a library raises while reading them
    said as the file being unreadable (as ``_read`` does)."""
    it = iter(gen)
    while True:
        try:
            item = next(it)
        except StopIteration:
            return
        except (Unreadable, _TooSlow, w.WorkerError, MemoryError):
            raise
        except Exception as e:  # noqa: BLE001 - whatever a library raises, the file is unreadable
            raise Unreadable(_cause(e)) from None
        yield item


class _TextTable(Table):
    """A table of text cells (CSV, or a sheet's cells as text): headers
    (R-6c), geometry (R-1 to R-3) and typing (R-4), read from its source in
    two passes of ``BATCH`` rows, so memory stays flat whatever the file's
    size (R-101): the first decides the types, geometry type, CRS and
    extent; the second writes the rows."""

    def __init__(self, source, conn: dict[str, Any], what: str):
        super().__init__([], [], [], None, None, None, None, [])
        self.source = source
        self.conn = conn
        self.what = what

    def prepare(self, tick: Callable[[], None]) -> None:
        try:
            self._prepare(tick)
        except (Unreadable, _TooSlow, w.WorkerError, MemoryError):
            raise
        except Exception as e:  # noqa: BLE001 - whatever a library raises, the file is unreadable
            raise Unreadable(_cause(e)) from None

    def _prepare(self, tick: Callable[[], None]) -> None:
        conn = self.conn
        header = self.source.open(tick)
        columns, header_checks = unique_headers(header, self.what)
        geo_checks: list[dict[str, Any]] = []
        width = self.width = len(columns)
        srs_conn = _epsg(conn)
        wkt_i = next((i for n in WKT_NAMES if (i := _find_col(columns, n)) is not None), None)
        pair: tuple[int, int] | None = None
        if conn.get("x") and conn.get("y"):
            xi, yi = _find_col(columns, str(conn["x"])), _find_col(columns, str(conn["y"]))
            if xi is None or yi is None:
                geo_checks.append(_check("csv.xy_missing", "warn",
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
        self.wkt_i = wkt_i
        self.pair = pair if wkt_i is None else None
        self.keep = [i for i in range(width) if i != wkt_i]
        typers = [_Typer() for _ in self.keep]

        # The first pass: types, and what the geometry is.
        bad = missing = valid = 0
        kinds: set[int] = set()
        has_z = False
        boxes: list[Any] = []
        lonlat = True
        x0 = y0 = math.inf
        x1 = y1 = -math.inf
        if wkt_i is not None:
            import numpy as np
            import shapely
        for batch in _guarded(self.source.rows(width, True, BATCH, tick)):
            cols = list(zip(*batch))
            for j, i in enumerate(self.keep):
                if not typers[j].done():
                    typers[j].add(cols[i])
            if wkt_i is not None:
                texts = [(v or "").strip() or None for v in cols[wkt_i]]
                shapes = shapely.from_wkt(texts, on_invalid="ignore")
                given = np.array([t is not None for t in texts], dtype=bool)
                absent = shapely.is_missing(shapes)
                bad += int(np.count_nonzero(given & absent))
                real = shapes[~absent & ~shapely.is_empty(shapes)]
                if len(real):
                    kinds.update(int(k) for k in np.unique(shapely.get_type_id(real)))
                    has_z = has_z or bool(shapely.has_z(real).any())
                    boxes.append(shapely.total_bounds(real))
            elif self.pair is not None:
                xi, yi = self.pair
                for xs, ys in zip(cols[xi], cols[yi]):
                    try:
                        x, y = float((xs or "").strip()), float((ys or "").strip())
                    except ValueError:
                        missing += 1
                        continue
                    if not (math.isfinite(x) and math.isfinite(y)):
                        missing += 1
                        continue
                    valid += 1
                    if lonlat and not (abs(x) <= 180 and abs(y) <= 90):
                        lonlat = False
                    if x < x0:
                        x0 = x
                    if x > x1:
                        x1 = x
                    if y < y0:
                        y0 = y
                    if y > y1:
                        y1 = y

        if wkt_i is not None:
            if pair is not None:
                # R-3: WKT wins.
                ignored = [columns[pair[0]], columns[pair[1]]]
                geo_checks.append(_check("csv.geometry_source", "info",
                                         f"Geometry is read from the WKT column {columns[wkt_i]}; the coordinate columns {', '.join(ignored)} are kept as fields only",
                                         detail={"columns": [columns[wkt_i]] + ignored}))
            if bad:
                geo_checks.append(_check("csv.bad_wkt", "warn", f"{bad} rows have geometry text that isn't valid WKT; their geometry is empty",
                                         detail={"columns": [columns[wkt_i]], "rows": bad}))
            self.srs_id = srs_conn if srs_conn is not None else -1
            if srs_conn is None:
                geo_checks.append(_no_crs("The WKT geometry"))
            self.geom_type = _TYPE_NAMES.get(kinds.pop(), "GEOMETRY") if len(kinds) == 1 else "GEOMETRY"
            self.has_z = has_z
            if boxes:
                import warnings

                b = np.array(boxes, dtype=float)
                with warnings.catch_warnings():
                    warnings.simplefilter("ignore", RuntimeWarning)  # an all-NaN column stays NaN
                    self.extent = [float(np.nanmin(b[:, 0])), float(np.nanmin(b[:, 1])),
                                   float(np.nanmax(b[:, 2])), float(np.nanmax(b[:, 3]))]
            self.geoms = []  # geometry: the blobs come with each batch
        elif self.pair is not None:
            xi, yi = self.pair
            if srs_conn is not None:
                self.srs_id = srs_conn
            elif valid and lonlat:
                self.srs_id = 4326
            else:
                self.srs_id = -1
                geo_checks.append(_no_crs(f"The points in {columns[xi]}/{columns[yi]}"))
            self.geom_type = "POINT"
            self.has_m = False
            if valid:
                self.extent = [x0, y0, x1, y1]
            if missing:
                geo_checks.append(_check("csv.no_coordinates", "warn",
                                         f"{missing} rows have no usable coordinates in {columns[xi]}/{columns[yi]}; their geometry is empty",
                                         detail={"columns": [columns[xi], columns[yi]], "rows": missing}))
            self.geoms = []
        else:
            geo_checks.append(_check("crs.present", "info",
                                     "No geometry: the file has no WKT column (geometry, wkt, the_geom, shape) and no coordinate pair (lon/lat, longitude/latitude, x/y, point_x/point_y), so it is a table",
                                     fix="If it has coordinates under other names, set x and y on the connection"))
        self.columns = [columns[i] for i in self.keep]
        self.types = [t.type() for t in typers]
        self.checks = self.source.final_checks() + header_checks + geo_checks

    def batches(self, size: int, tick: Callable[[], None]):
        """The second pass: the rows, converted, with their geometry."""
        if self.wkt_i is not None:
            import shapely
        for batch in _guarded(self.source.rows(self.width, False, size, tick)):
            cells = list(zip(*batch))
            try:
                cols = [_convert_column(cells[i], t) for i, t in zip(self.keep, self.types)]
            except (ValueError, OverflowError):
                # The first pass typed it: the file changed in between.
                raise Unreadable("it changed while it was being read; query it again") from None
            rows = list(zip(*cols)) if cols else [() for _ in batch]
            if self.wkt_i is not None:
                texts = [(v or "").strip() or None for v in cells[self.wkt_i]]
                geoms = _blobs(shapely.from_wkt(texts, on_invalid="ignore"), self.srs_id)
            elif self.pair is not None:
                xi, yi = self.pair
                srs_id = self.srs_id
                geoms = []
                for xs, ys in zip(cells[xi], cells[yi]):
                    try:
                        x, y = float((xs or "").strip()), float((ys or "").strip())
                    except ValueError:
                        geoms.append(None)
                        continue
                    geoms.append(_point_blob(x, y, srs_id) if math.isfinite(x) and math.isfinite(y) else None)
            else:
                geoms = None
            yield rows, geoms


READERS[".csv"] = read_csv


# ---------- Excel (R-6, R-6b) ----------

_NS_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
_CELL = re.compile(r"^([A-Z]{1,3})(\d+)$")
# Built-in number formats that show dates or times.
_DATE_FORMATS = set(range(14, 18)) | {22} | set(range(27, 32)) | set(range(36, 37)) | set(range(50, 59))
_TIME_FORMATS = set(range(18, 22)) | {45, 46, 47} | set(range(32, 36))


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _fmt_kind(code: str) -> str | None:
    """'date', 'time' or None for a custom number format code."""
    c = re.sub(r'"[^"]*"|\\.|_.|\*.', "", code)
    c = re.sub(r"\[(?![hms]+\])[^\]]*\]", "", c, flags=re.I).lower()
    if "general" in c:
        return None
    if "y" in c or "d" in c:
        return "date"
    if "h" in c or "s" in c:
        return "time"
    return None


class _Workbook:
    """An .xlsx workbook read with the standard library: its sheets, shared
    strings and which cell styles are dates."""

    def __init__(self, path: str):
        self.path = path
        try:
            self.zip = zipfile.ZipFile(path)
        except (zipfile.BadZipFile, OSError) as e:
            raise Unreadable(f"it is not a readable .xlsx workbook ({_cause(e)})") from None
        try:
            self._open()
        except BaseException:
            # Windows locks an open file: never leave the person's workbook
            # held open, or they can't save it in Excel.
            self.zip.close()
            raise

    def __enter__(self) -> "_Workbook":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    def close(self) -> None:
        self.zip.close()

    def _open(self) -> None:
        self.names = set(self.zip.namelist())
        root_rels = self._rels("_rels/.rels", "")
        self.book = next((t for typ, t in root_rels if typ.endswith("/officeDocument")), "xl/workbook.xml")
        if self.book not in self.names:
            raise Unreadable("it is not a readable .xlsx workbook (it has no workbook part)")
        self.base = posixpath.dirname(self.book)
        rels_path = posixpath.join(self.base, "_rels", posixpath.basename(self.book) + ".rels")
        self.book_rels = {rid: t for rid, t in self._rels(rels_path, self.base, ids=True)}
        self._sst: list[str] | None = None
        self._styles: list[str | None] | None = None
        self.date1904 = False

    def _xml(self, name: str):
        with self.zip.open(name) as f:
            return ET.parse(f).getroot()

    def _rels(self, name: str, base: str, ids: bool = False):
        if name not in self.names:
            return []
        out = []
        for r in self._xml(name):
            target = r.get("Target") or ""
            full = target.lstrip("/") if target.startswith("/") else posixpath.normpath(posixpath.join(base, target))
            out.append((r.get("Id"), full) if ids else (r.get("Type") or "", full))
        return out

    def sheets(self, check_data: bool = False) -> list[tuple[str, str, bool]]:
        """(name, part, has a value) for each worksheet, in workbook order."""
        root = self._xml(self.book)
        out = []
        for el in root.iter():
            if _local(el.tag) == "workbookPr":
                self.date1904 = (el.get("date1904") or "").lower() in ("1", "true")
            if _local(el.tag) != "sheet":
                continue
            rid = el.get(f"{{{_NS_REL}}}id") or next((v for k, v in el.attrib.items() if _local(k) == "id"), None)
            part = self.book_rels.get(rid)
            if part is None or part not in self.names:
                continue  # a chart sheet or a broken link: no cells
            name = el.get("name") or part
            out.append((name, part, self._has_data(part) if check_data else True))
        return out

    def _has_data(self, part: str) -> bool:
        with self.zip.open(part) as f:
            for _ev, el in ET.iterparse(f, events=("end",)):
                tag = _local(el.tag)
                if tag in ("v", "t") and (el.text or "").strip():
                    return True
                if tag == "row":
                    el.clear()
        return False

    def shared(self) -> list[str]:
        if self._sst is None:
            self._sst = []
            part = next((t for t in self.book_rels.values() if t.endswith("sharedStrings.xml")), None)
            if part and part in self.names:
                with self.zip.open(part) as f:
                    for _ev, el in ET.iterparse(f, events=("end",)):
                        if _local(el.tag) == "si":
                            self._sst.append(_text(el))
                            el.clear()
        return self._sst

    def styles(self) -> list[str | None]:
        """For each cell style index: 'date', 'time' or None."""
        if self._styles is None:
            self._styles = []
            part = next((t for t in self.book_rels.values() if t.endswith("styles.xml")), None)
            if part and part in self.names:
                root = self._xml(part)
                custom = {}
                for el in root.iter():
                    if _local(el.tag) == "numFmt":
                        try:
                            custom[int(el.get("numFmtId") or -1)] = el.get("formatCode") or ""
                        except ValueError:
                            pass
                xfs = next((el for el in root if _local(el.tag) == "cellXfs"), None)
                for xf in (xfs if xfs is not None else []):
                    try:
                        fid = int(xf.get("numFmtId") or 0)
                    except ValueError:
                        fid = 0
                    if fid in custom:
                        self._styles.append(_fmt_kind(custom[fid]))
                    elif fid in _DATE_FORMATS:
                        self._styles.append("date")
                    elif fid in _TIME_FORMATS:
                        self._styles.append("time")
                    else:
                        self._styles.append(None)
        return self._styles


def _text(el) -> str:
    """The text of a string item or inline string: its runs, without
    phonetic hints."""
    out = []
    for child in el:
        tag = _local(child.tag)
        if tag == "t":
            out.append(child.text or "")
        elif tag == "r":
            out.extend(t.text or "" for t in child if _local(t.tag) == "t")
    return "".join(out)


class _Tags(dict):
    """Element tags without their namespace, worked out once per tag."""

    def __missing__(self, tag: str) -> str:
        self[tag] = name = _local(tag)
        return name


class _Columns(dict):
    """Column letters (A, B, ..., XFD) to 0-based indexes, worked out once."""

    def __missing__(self, letters: str) -> int:
        n = 0
        for ch in letters:
            n = n * 26 + ord(ch) - 64
        self[letters] = n - 1
        return n - 1


_COLUMNS = _Columns()


class _Serial:
    """A date-formatted cell, kept as its serial until the column is known
    (a column with any time of day is written as date-times)."""

    __slots__ = ("value", "kind")

    def __init__(self, value: float, kind: str):
        self.value = value
        self.kind = kind


def read_xlsx(path: str, conn: dict[str, Any], layer: str | None) -> Table:
    """R-6: one sheet; the first row with a value is the header, formulas
    give their saved values; R-6b: date-formatted cells become ISO dates and
    General serial numbers stay numbers; then as CSV (R-1 to R-4, R-6c).
    The sheet is read when the table is written, a row at a time (R-101)."""
    _sniff(path, ".xlsx")
    return _TextTable(_SheetSource(path, layer), conn, "Column headers")


class _SheetSource:
    """A sheet's rows with a value, as text, read from the workbook each
    time they are asked for (the workbook is open only while it is read:
    Windows locks an open file). The first read finds the width and which
    date columns have a time of day."""

    def __init__(self, path: str, layer: str | None):
        self.path = path
        self.layer = layer
        self.unsaved = self.errors = 0

    def _cells(self):
        """Each row with a value: {column index: str or _Serial}."""
        with _Workbook(self.path) as book:
            part = next((p for n, p, _ in book.sheets() if n == self.layer), None)
            if part is None:
                raise Unreadable(f"it has no sheet {self.layer!r}")
            self.date1904 = book.date1904
            sst, styles = book.shared(), book.styles()
            unsaved = errors = 0
            tags = _Tags()
            with book.zip.open(part) as f:
                # A read row is cleared; its empty element stays in the
                # sheet's tree, a few dozen bytes for each of at most
                # 1,048,576 rows (Excel's limit).
                for _ev, el in ET.iterparse(f, events=("end",)):
                    if tags[el.tag] != "row":
                        continue
                    cells: dict[int, Any] = {}
                    i = -1
                    for c in el:
                        if tags[c.tag] != "c":
                            continue
                        i += 1
                        m = _CELL.match(c.get("r") or "")
                        col = (_COLUMNS[m.group(1)] if m else max(cells) + 1 if cells else i)
                        t = c.get("t") or "n"
                        v = is_ = None
                        has_v = has_formula = False
                        for x in c:
                            n = tags[x.tag]
                            if n == "v":
                                if not has_v:
                                    v, has_v = x.text, True
                            elif n == "f":
                                has_formula = True
                            elif n == "is" and is_ is None:
                                is_ = x
                        if t == "inlineStr":
                            val: Any = _text(is_) if is_ is not None else None
                        elif v is None:
                            if has_formula:
                                unsaved += 1
                            continue
                        elif t == "s":
                            try:
                                val = sst[int(v)]
                            except (ValueError, IndexError):
                                val = None
                        elif t in ("str", "d"):
                            val = v
                        elif t == "b":
                            val = "TRUE" if v.strip() in ("1", "true") else "FALSE"
                        elif t == "e":
                            errors += 1
                            continue
                        else:
                            try:
                                s = int(c.get("s") or 0)
                            except ValueError:
                                s = 0
                            kind = styles[s] if 0 <= s < len(styles) else None
                            val = v.strip()
                            if kind and _REAL.match(val):
                                val = _Serial(float(val), kind)
                        if val is None or (isinstance(val, str) and val == ""):
                            continue
                        cells[col] = val
                    el.clear()
                    if cells:
                        yield cells
            self.unsaved, self.errors = unsaved, errors

    def open(self, tick: Callable[[], None]) -> list[str]:
        width = -1
        with_time: set[int] = set()
        first: dict[int, Any] | None = None
        n = 0
        for cells in self._cells():
            if first is None:
                first = cells
            width = max(width, max(cells))
            for col, v in cells.items():
                if isinstance(v, _Serial) and v.kind == "date" and v.value != int(v.value):
                    with_time.add(col)
            n += 1
            if n % BATCH == 0:
                tick()
        if first is None:
            raise Unreadable(f"the sheet {self.layer!r} is empty")
        self.width = width + 1
        self.with_time = with_time
        self.base = datetime(1904, 1, 1) if self.date1904 else datetime(1899, 12, 30)
        return ["" if v is None else str(v) for v in self._row(first)]

    def _row(self, cells: dict[int, Any]) -> list[Any]:
        out = []
        for col in range(self.width):
            v = cells.get(col)
            if isinstance(v, _Serial):
                try:
                    when = self.base + timedelta(seconds=round(v.value * 86400))
                except OverflowError:
                    v = repr(v.value)
                else:
                    if v.kind == "time":
                        v = when.strftime("%H:%M:%S")
                    elif col in self.with_time:
                        v = when.strftime("%Y-%m-%dT%H:%M:%S")
                    else:
                        v = when.date().isoformat()
            out.append(v)
        return out

    def rows(self, width: int, first: bool, size: int, tick: Callable[[], None]):
        """The rows after the header, in lists of up to ``size``."""
        cells = self._cells()
        next(cells, None)  # the header
        yield from _batched((self._row(c) for c in cells), size, tick)

    def final_checks(self) -> list[dict[str, Any]]:
        checks: list[dict[str, Any]] = []
        if self.unsaved:
            checks.append(_check("xlsx.formulas", "warn",
                                 f"{self.unsaved} formula cells have no saved value (the workbook was never recalculated in Excel); they are empty",
                                 fix="Open the workbook in Excel, save it, and query it again"))
        if self.errors:
            checks.append(_check("xlsx.errors", "warn", f"{self.errors} cells hold an Excel error (such as #DIV/0! or #N/A); they are empty"))
        return checks


READERS[".xlsx"] = read_xlsx


# ---------- GeoJSON, KML, FlatGeobuf (GDAL) and GeoParquet ----------


def _pyogrio():
    w._gis()  # the GIS engine's own import first (see columbus_esri)
    import pyogrio

    pyogrio.set_gdal_config_options({"OGR_SQLITE_ALLOW_EXTERNAL_ACCESS": "NO"})
    return pyogrio


def _ogr_layers(path: str, ext: str):
    pyogrio = _pyogrio()
    layers = list(pyogrio.list_layers(path))
    if layers:
        driver = pyogrio.read_info(path, layer=str(layers[0][0])).get("driver")
        if driver not in _DRIVERS[ext]:
            raise Unreadable(f"GDAL opened it as {driver!r}, not as {ext} data")
    return layers


def _crs_of(crs, conn: dict[str, Any], checks: list[dict[str, Any]], what: str) -> tuple[int, str | None]:
    """(srs id, WKT for a CRS without an EPSG code): the file's own CRS,
    else the connection's ``crs``, else none with a ``crs.present`` warning."""
    if crs is not None:
        from pyproj import CRS

        c = CRS.from_user_input(crs)
        epsg = c.to_epsg()
        if epsg is None and c.to_authority() == ("OGC", "CRS84"):
            epsg = 4326  # GeoPackage stores 4326 as longitude, latitude
        if epsg is not None:
            return epsg, None
        return 0, c.to_wkt("WKT1_GDAL") or c.to_wkt()
    epsg = _epsg(conn)
    if epsg is not None:
        return epsg, None
    checks.append(_no_crs(what))
    return -1, None


def _arrow_columns(table, skip: set[str]) -> tuple[list[str], list[str], list[list[Any]]]:
    """(names, SQLite types, rows) from an Arrow table, types kept."""
    import pyarrow as pa

    names, types, cols = [], [], []
    for field, column in zip(table.schema, table.columns):
        if field.name in skip:
            continue
        t = field.type
        values = column.to_pylist()
        if pa.types.is_boolean(t):
            ty, conv = "BOOLEAN", lambda v: int(v)
        elif pa.types.is_integer(t) and not (pa.types.is_uint64(t)):
            ty, conv = "INTEGER", int
        elif pa.types.is_integer(t) or pa.types.is_floating(t) or pa.types.is_decimal(t):
            ty, conv = "REAL", lambda v: None if isinstance(v, float) and math.isnan(v) else float(v)
        elif pa.types.is_date(t):
            ty, conv = "DATE", lambda v: v.isoformat()
        elif pa.types.is_timestamp(t):
            ty, conv = "DATETIME", lambda v: v.isoformat()
        elif pa.types.is_binary(t) or pa.types.is_large_binary(t) or pa.types.is_fixed_size_binary(t):
            ty, conv = "BLOB", bytes
        elif pa.types.is_string(t) or pa.types.is_large_string(t):
            ty, conv = "TEXT", str
        else:
            ty, conv = "TEXT", lambda v: v if isinstance(v, str) else json.dumps(v, default=str)
        names.append(field.name)
        types.append(ty)
        cols.append([None if v is None else conv(v) for v in values])
    rows = [list(r) for r in zip(*cols)] if cols else [[] for _ in range(table.num_rows)]
    return names, types, rows


def _features(names: list[str], types: list[str], rows: list[list[Any]], shapes, crs,
              conn: dict[str, Any], checks: list[dict[str, Any]], what: str) -> Table:
    columns, more = unique_headers(names, "Field names")
    checks = checks + more
    if shapes is None:
        checks.append(_check("crs.present", "info", f"No geometry: {what} is a table"))
        return Table(columns, types, rows, None, None, None, None, checks)
    srs_id, wkt = _crs_of(crs, conn, checks, what)
    geoms, geom_type, extent, has_z = _shapes_to_blobs(shapes, srs_id)
    return Table(columns, types, rows, geoms, geom_type, srs_id if wkt is None else None, extent, checks,
                 has_z=has_z, crs_wkt=wkt)


# Columns LIBKML adds to every KML layer; left out when no feature uses them.
_KML_EXTRAS = {"id", "description", "timestamp", "begin", "end", "altitudeMode", "tessellate", "extrude",
               "visibility", "drawOrder", "icon", "snippet"}
_KML_DEFAULTS = {"tessellate": -1, "extrude": 0, "visibility": -1, "drawOrder": None}


def read_ogr(path: str, conn: dict[str, Any], layer: str | None) -> Table:
    ext = os.path.splitext(path)[1].lower()
    _sniff(path, ext)
    import shapely

    pyogrio = _pyogrio()
    try:
        import pyarrow  # noqa: F401
    except ImportError:
        raise Unreadable(f"reading {ext} files needs pyarrow (workers/python/requirements-gis.txt)") from None
    info = pyogrio.read_info(path, layer=layer) if layer is not None else pyogrio.read_info(path)
    if info.get("driver") not in _DRIVERS[ext]:
        raise Unreadable(f"GDAL opened it as {info.get('driver')!r}, not as {ext} data")
    meta, table = pyogrio.read_arrow(path, layer=layer) if layer is not None else pyogrio.read_arrow(path)
    gname = meta.get("geometry_name") or "wkb_geometry"
    shapes = None
    if gname in table.column_names and info.get("geometry_type"):
        shapes = list(shapely.from_wkb(table.column(gname).to_pylist()))
    names, types, rows = _arrow_columns(table, {gname})
    if ext == ".kml":
        keep = [i for i, n in enumerate(names)
                if n not in _KML_EXTRAS or any(r[i] not in (None, "", _KML_DEFAULTS.get(n)) for r in rows)]
        names, types = [names[i] for i in keep], [types[i] for i in keep]
        rows = [[r[i] for i in keep] for r in rows]
    what = "The layer" if layer is not None else "The file"
    return _features(names, types, rows, shapes, meta.get("crs"), conn, [], what)


def read_parquet(path: str, conn: dict[str, Any], _layer: str | None = None) -> Table:
    """GeoParquet through geopandas.read_parquet, which takes the CRS from
    the file's ``geo`` metadata (A-5); a Parquet file without that metadata
    is a table."""
    _sniff(path, ".parquet")
    g = w._gis()  # noqa: F841 - loads geopandas the engine's way
    import geopandas as gpd
    import pandas as pd
    import pyarrow as pa
    import pyarrow.parquet as pq

    schema = pq.read_schema(path)
    if b"geo" not in (schema.metadata or {}):
        names, types, rows = _arrow_columns(pq.read_table(path), set())
        return _features(names, types, rows, None, None, conn, [], "the Parquet file (it has no GeoParquet metadata)")
    df = gpd.read_parquet(path)
    gname = df.geometry.name
    others = [c for c in df.columns if c != gname and isinstance(df[c].dtype, gpd.array.GeometryDtype)]
    attrs = pd.DataFrame(df.drop(columns=[gname]))
    for c in others:
        attrs[c] = gpd.GeoSeries(df[c]).to_wkt()
    names, types, rows = _arrow_columns(pa.Table.from_pandas(attrs, preserve_index=False), set())
    shapes = [None if s is None else s for s in df.geometry.array]
    return _features(names, types, rows, shapes, df.crs, conn, [], "The file")


READERS[".geojson"] = read_ogr
READERS[".json"] = read_ogr
READERS[".kml"] = read_ogr
READERS[".fgb"] = read_ogr
READERS[".parquet"] = read_parquet
READERS[".geoparquet"] = read_parquet


# ---------- the cache GeoPackage ----------


def _q(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


_META_COLUMNS = ["dataset", "source", "layer", "size", "mtime_ns", "sha256", "build", "builds", "checks", "geometry_column"]


def _open_cache(conn: dict[str, Any], timeout_s: float) -> sqlite3.Connection:
    path = conn.get("cache")
    if not isinstance(path, str) or not path:
        raise w.WorkerError(w.INVALID_PARAMS, "files connection has no cache path")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    try:
        db = sqlite3.connect(path, timeout=max(5.0, timeout_s), isolation_level=None, check_same_thread=False)
    except sqlite3.Error as e:
        raise w.WorkerError(w.ENGINE_ERROR, f"cannot open the cache {path}: {e}") from None
    try:
        # A larger page cache (64 MB, a fixed bound) for writing big tables
        # and their R-trees in one transaction.
        db.execute("PRAGMA cache_size = -65536")
        have = [r[1] for r in db.execute(f"PRAGMA table_info({META})")]
        if db.execute("PRAGMA application_id").fetchone()[0] != 0x47504B47 or have != _META_COLUMNS:
            db.execute("BEGIN IMMEDIATE")
            try:
                _init_gpkg(db)
                db.execute("COMMIT")
            except BaseException:
                db.execute("ROLLBACK")
                raise
    except BaseException:
        db.close()
        raise
    return db


def _init_gpkg(db: sqlite3.Connection) -> None:
    """The tables a GeoPackage must have, and the cache's own (a cache made
    by an older engine, with other columns, is emptied)."""
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
    have = [r[1] for r in db.execute(f"PRAGMA table_info({META})")]
    if have and have != _META_COLUMNS:
        for (name,) in db.execute(f"SELECT dataset FROM {META}").fetchall():
            _drop(db, name, meta=False)
        db.execute(f"DROP TABLE {META}")
    db.execute(f"CREATE TABLE IF NOT EXISTS {META} (dataset TEXT PRIMARY KEY, source TEXT NOT NULL, layer TEXT, "
               "size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL, sha256 TEXT NOT NULL, build INTEGER NOT NULL, "
               "builds INTEGER NOT NULL, checks TEXT NOT NULL, geometry_column TEXT)")


def _stamp(d: dict[str, Any]) -> tuple[str, int, int]:
    st = os.stat(d["path"])
    return os.path.realpath(d["path"]), st.st_size, st.st_mtime_ns


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def _state(db: sqlite3.Connection, d: dict[str, Any], stamp: tuple[str, int, int],
           hashes: dict[str, str] | None) -> str:
    """R-7: 'fresh'; 'touched' (size or time changed, same bytes); or
    'stale'. With ``hashes`` None, a changed stamp is 'stale' without
    hashing (the caller re-checks under the lock)."""
    row = db.execute(f"SELECT source, layer, size, mtime_ns, sha256, build FROM {META} WHERE dataset = ?",
                     (d["name"],)).fetchone()
    if row is None or row[0] != stamp[0] or row[1] != d.get("layer") or row[5] != BUILD:
        return "stale"
    if (row[2], row[3]) == stamp[1:]:
        return "fresh"
    if hashes is None:
        return "stale"
    if d["path"] not in hashes:
        hashes[d["path"]] = sha256_file(d["path"])
    return "touched" if hashes[d["path"]] == row[4] else "stale"


def _drop(db: sqlite3.Connection, name: str, meta: bool = True) -> None:
    row = db.execute("SELECT column_name FROM gpkg_geometry_columns WHERE table_name = ?", (name,)).fetchone()
    if row is not None:
        db.execute(f"DROP TABLE IF EXISTS {_q(f'rtree_{name}_{row[0]}')}")
        if db.execute("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'gpkg_extensions'").fetchone():
            db.execute("DELETE FROM gpkg_extensions WHERE table_name = ?", (name,))
    db.execute(f"DROP TABLE IF EXISTS {_q(name)}")
    db.execute("DELETE FROM gpkg_geometry_columns WHERE table_name = ?", (name,))
    db.execute("DELETE FROM gpkg_contents WHERE table_name = ?", (name,))
    if meta:
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


def _custom_srs(db: sqlite3.Connection, wkt: str) -> int:
    """An srs id for a coordinate system without an EPSG code."""
    row = db.execute("SELECT srs_id FROM gpkg_spatial_ref_sys WHERE definition = ? AND srs_id >= ?",
                     (wkt, _CUSTOM_SRS)).fetchone()
    if row:
        return int(row[0])
    top = db.execute("SELECT MAX(srs_id) FROM gpkg_spatial_ref_sys").fetchone()[0] or 0
    srs_id = max(int(top) + 1, _CUSTOM_SRS)
    m = re.match(r'^\w+\["([^"]*)"', wkt)
    db.execute("INSERT INTO gpkg_spatial_ref_sys VALUES (?,?,?,?,?,?)",
               (m.group(1) if m else "Custom", srs_id, "NONE", srs_id, wkt, "from the file"))
    return srs_id


def _read(d: dict[str, Any], conn: dict[str, Any]) -> Table:
    """The file's table, or ``Unreadable`` with the cause."""
    try:
        return READERS[d["ext"]](d["path"], conn, d.get("layer"))
    except (w.WorkerError, MemoryError):
        raise
    except Exception as e:  # noqa: BLE001 - whatever a library raises, the file is unreadable
        raise Unreadable(_cause(e)) from None


def _build(db: sqlite3.Connection, d: dict[str, Any], t: Table, stamp: tuple[str, int, int], sha: str,
           tick: Callable[[], None] = lambda: None) -> None:
    """Write the table, in the caller's transaction, ``BATCH`` rows at a
    time (R-101)."""
    name = d["name"]
    t.prepare(tick)
    old = db.execute(f"SELECT builds FROM {META} WHERE dataset = ?", (name,)).fetchone()
    _drop(db, name)
    taken = {c.lower() for c in t.columns}
    fid = next(n for n in ("fid", "fid_", "columbus_fid") if n not in taken)
    gcol = None
    srs_id = t.srs_id
    custom = False
    if t.geoms is not None:
        gcol = next(n for n in ("geometry", "geom", "geometry_") if n not in taken)
        if t.crs_wkt is not None:
            srs_id = _custom_srs(db, t.crs_wkt)
            custom = True
        elif srs_id not in (-1, 0):
            _srs(db, srs_id)
    cols = [f"{_q(fid)} INTEGER PRIMARY KEY AUTOINCREMENT"] + [f"{_q(c)} {ty}" for c, ty in zip(t.columns, t.types)]
    if gcol:
        cols.append(f"{_q(gcol)} {t.geom_type or 'GEOMETRY'}")
    db.execute(f"CREATE TABLE {_q(name)} ({', '.join(cols)})")
    names = [_q(c) for c in t.columns] + ([_q(gcol)] if gcol else [])
    # Keys are given (1, 2, ... as AUTOINCREMENT gives a new table), so each
    # row's R-tree entry is written with it, in the same pass.
    insert = f"INSERT INTO {_q(name)} ({', '.join([_q(fid)] + names)}) VALUES ({', '.join('?' for _ in range(len(names) + 1))})"
    rtree = _rtree(db, name, gcol) if gcol else None
    # M is looked for in the blobs unless the table knows (CSV points have none).
    look_for_m = t.has_m is None
    has_m = bool(t.has_m)
    first = 1
    for rows, geoms in t.batches(BATCH, tick):
        if not names:
            continue
        if gcol:
            if custom:
                geoms = [None if g is None else g[:4] + struct.pack("<i", srs_id) + g[8:] for g in geoms]
            if look_for_m and not has_m:
                has_m = any(_blob_has_m(g) for g in geoms)
            db.executemany(insert, ((i, *r, g) for i, r, g in zip(itertools.count(first), rows, geoms)))
            if rtree:
                db.executemany(rtree, ((i, *env) for i, g in zip(itertools.count(first), geoms)
                                       if (env := _blob_envelope(g)) is not None))
        else:
            db.executemany(insert, ((i, *r) for i, r in zip(itertools.count(first), rows)))
        first += len(rows)
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    ext = t.extent or [None] * 4
    if gcol:
        db.execute("INSERT INTO gpkg_contents VALUES (?,?,?,?,?,?,?,?,?,?)",
                   (name, "features", name, "", now, *ext, srs_id))
        db.execute("INSERT INTO gpkg_geometry_columns VALUES (?,?,?,?,?,?)",
                   (name, gcol, t.geom_type or "GEOMETRY", srs_id, 1 if t.has_z else 0, 1 if has_m else 0))
        if rtree:
            db.execute("CREATE TABLE IF NOT EXISTS gpkg_extensions (table_name TEXT, column_name TEXT, extension_name TEXT NOT NULL, "
                       "definition TEXT NOT NULL, scope TEXT NOT NULL, "
                       "CONSTRAINT ge_tce UNIQUE (table_name, column_name, extension_name))")
            db.execute("INSERT OR REPLACE INTO gpkg_extensions VALUES (?,?,?,?,?)",
                       (name, gcol, "gpkg_rtree_index", "http://www.geopackage.org/spec120/#extension_rtree", "write-only"))
    else:
        db.execute("INSERT INTO gpkg_contents VALUES (?,?,?,?,?,NULL,NULL,NULL,NULL,NULL)", (name, "attributes", name, "", now))
    db.execute(f"INSERT INTO {META} VALUES (?,?,?,?,?,?,?,?,?,?)",
               (name, stamp[0], d.get("layer"), stamp[1], stamp[2], sha, BUILD, (old[0] if old else 0) + 1,
                json.dumps(t.checks), gcol))


def _blob_envelope(blob: bytes | None) -> tuple[float, float, float, float] | None:
    """(min x, max x, min y, max y) of a GeoPackage blob, from its header's
    envelope, or from the point it holds; None for no or empty geometry."""
    if not blob or len(blob) < 8 or blob[3] & 0x10:
        return None
    flags = blob[3]
    endian = "<" if flags & 0x01 else ">"
    if not (flags >> 1) & 0x07 and len(blob) == 29 and blob[8] == 1 and blob[9:13] == b"\x01\x00\x00\x00":
        # A 2D point in little-endian WKB (every CSV point): read directly.
        x, y = struct.unpack_from("<2d", blob, 13)
        if math.isnan(x) or math.isnan(y):
            return None
        return x, x, y, y
    if (flags >> 1) & 0x07:
        minx, maxx, miny, maxy = struct.unpack_from(endian + "4d", blob, 8)
    else:
        import shapely

        g = shapely.from_wkb(bytes(blob[8:]))
        if g is None or g.is_empty:
            return None
        minx, miny, maxx, maxy = g.bounds
    if any(math.isnan(v) for v in (minx, maxx, miny, maxy)):
        return None
    return minx, maxx, miny, maxy


def _rtree(db: sqlite3.Connection, name: str, gcol: str) -> str | None:
    """The table's R-tree (GeoPackage's RTree Spatial Index extension), so
    a bbox query reads only the rows whose envelope meets the box (T-13):
    made empty here, and the INSERT for its entries returned, which the
    build runs for each batch of rows. The cache is written only by the
    build, whole, so the R-tree needs none of the extension's triggers. An
    SQLite without the R-tree module goes without (None): queries then
    read every row, as before."""
    rtree = f"rtree_{name}_{gcol}"
    try:
        db.execute(f"CREATE VIRTUAL TABLE {_q(rtree)} USING rtree(id, minx, maxx, miny, maxy)")
    except sqlite3.OperationalError:
        return None
    return f"INSERT INTO {_q(rtree)} VALUES (?,?,?,?,?)"


def _tx(db: sqlite3.Connection, fn) -> Any:
    db.execute("BEGIN IMMEDIATE")
    try:
        out = fn()
        db.execute("COMMIT")
        return out
    except BaseException:
        db.execute("ROLLBACK")
        raise


def _slow_get(db: sqlite3.Connection, stamp: tuple[str, int, int]) -> float | None:
    """R-101: the time limit this version of the file's conversion ran past
    before, if it did. It is skipped at once while the limit is no longer,
    rather than tried again on every call."""
    try:
        row = db.execute(f"SELECT seconds FROM {SLOW} WHERE source = ? AND size = ? AND mtime_ns = ?", stamp).fetchone()
    except sqlite3.OperationalError:
        return None  # a cache from before R-101
    return None if row is None else float(row[0])


def _slow_put(db: sqlite3.Connection, stamp: tuple[str, int, int], seconds: float) -> None:
    def put() -> None:
        db.execute(f"CREATE TABLE IF NOT EXISTS {SLOW} (source TEXT PRIMARY KEY, size INTEGER NOT NULL, "
                   "mtime_ns INTEGER NOT NULL, seconds REAL NOT NULL)")
        db.execute(f"INSERT OR REPLACE INTO {SLOW} VALUES (?,?,?,?)", (*stamp, seconds))

    _tx(db, put)


def _slow_reason(size: int, timeout_s: float) -> str:
    return (f"converting it ({size_text(size)}) takes longer than the {timeout_s:g} s one call may run; "
            f"{_FIX_BIG}{_RAISE}")


def sync(conn: dict[str, Any], timeout_s: float) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Bring the cache up to date with the folder: build what is missing or
    stale, drop what has gone or can't be read. (datasets, skipped).

    Smaller files are built first, and a build that runs past the call's
    time is stopped and its file skipped, so one huge file can't hold up
    the rest of the folder (R-101)."""
    datasets, skipped = scan(conn, timeout_s)
    deadline = time.monotonic() + timeout_s
    db = _open_cache(conn, timeout_s)
    hashes: dict[str, str] = {}
    failed: dict[str, str] = {}

    def tick() -> None:
        if time.monotonic() > deadline:
            raise _TooSlow()

    def size(d: dict[str, Any]) -> int:
        return _quiet(lambda: os.stat(d["path"]).st_size, 0)

    try:
        for d in sorted(datasets, key=size):
            try:
                stamp = _stamp(d)
            except OSError:
                failed[d["name"]] = "it could not be read (it disappeared while being read)"
                continue
            if _state(db, d, stamp, None) == "fresh":
                continue
            slow = _slow_get(db, stamp)
            if slow is not None and slow >= timeout_s:
                failed[d["name"]] = _slow_reason(stamp[1], slow)
                continue
            if time.monotonic() > deadline:
                failed[d["name"]] = ("it was not converted: converting the folder's other files used all of this call's "
                                     f"{timeout_s:g} s; list the connection again to convert it")
                continue

            def bring_up_to_date(d=d, stamp=stamp):
                # Another engine may have built it while this one waited.
                state = _state(db, d, stamp, hashes)
                if state == "touched":
                    db.execute(f"UPDATE {META} SET size = ?, mtime_ns = ? WHERE dataset = ?",
                               (stamp[1], stamp[2], d["name"]))
                elif state == "stale":
                    if d["path"] not in hashes:
                        hashes[d["path"]] = sha256_file(d["path"])
                    _build(db, d, _read(d, conn), stamp, hashes[d["path"]], tick)

            try:
                _tx(db, bring_up_to_date)
            except (Unreadable, OSError) as e:
                failed[d["name"]] = f"it could not be read ({_cause(e)})"
            except _TooSlow:
                _slow_put(db, stamp, timeout_s)
                failed[d["name"]] = _slow_reason(stamp[1], timeout_s)
        current = {d["name"] for d in datasets if d["name"] not in failed}
        gone = [r[0] for r in db.execute(f"SELECT dataset FROM {META}") if r[0] not in current]
        if gone:
            _tx(db, lambda: [_drop(db, name) for name in gone])
        notes = {r[0]: json.loads(r[1]) for r in db.execute(f"SELECT dataset, checks FROM {META}")}
    finally:
        db.close()
    out = []
    for d in datasets:
        if d["name"] in failed:
            skipped.append({"name": d["name"], "file": d["file"], "reason": failed[d["name"]]})
            continue
        d["checks"] = notes.get(d["name"], [])
        out.append(d)
    return out, skipped


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
        if d.get("layer") is not None:
            e["layer"] = d["layer"]
        if with_checks:
            e["file_checks"] = d["checks"]
        entries.append(e)
    return {"datasets": entries, "skipped": skipped}


def files_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None, sink=None):
    sync(conn, timeout_s)
    try:
        return w._sqlite_fetch(cache_conn(conn), sql, max_rows, timeout_s, keep_factory, sink)
    except w.WorkerError as e:
        # A dataset whose file went after the gateway last listed the
        # connection (R-7c): its table was just dropped.
        m = re.search(r"no such table: (\S+)", str(e))
        if m:
            raise w.WorkerError(w.INVALID_PARAMS, f"no dataset {m.group(1).strip(chr(34))} on this connection") from None
        raise


def skipped_reason(skipped: list[dict[str, Any]], dataset: str) -> str | None:
    """Why a dataset name the person used isn't offered, if a skipped file
    would have had it."""
    for s in skipped:
        if s["name"].lower() == dataset.lower():
            return f"{s['file']} is not offered: {s['reason']}"
    return None


def no_crs_is_a_warning(checks: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """R-1, R-2: on a files dataset, geometry without a coordinate system is
    a ``crs.present`` warning, not a fail (the file never had one; the
    connection's ``crs`` can supply it)."""
    out = []
    for c in checks:
        if c.get("id") == "crs.present" and c.get("severity") == "fail":
            c = dict(c, severity="warn",
                     fix="Set crs on the connection to the file's EPSG code (ask the data's owner if unsure); "
                         "until then, distances, areas and overlays with other layers can't be trusted")
        out.append(c)
    return out
