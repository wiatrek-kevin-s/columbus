"""Esri shapefiles and file geodatabases, read through GDAL (pyogrio).

A ``shapefile`` connection is a folder of shapefiles (each ``.shp`` is one
dataset) or one ``.shp`` file; a ``filegdb`` connection is a file
geodatabase (a ``.gdb`` folder), whose feature classes and tables are its
datasets. Both are read only.

SQL arrives checked and rewritten by the gateway in the SQLite dialect, as
for a GeoPackage, and runs in GDAL's SQLite dialect: real SQLite over the
layers, which never writes to them. GDAL's SQLite dialect opens any file a
query names as ``"path"."layer"`` (or ``a.b``), so the one barrier is the
gateway: it resolves every table to a single-part dataset name this module
listed, and its function allowlist keeps out GDAL's own (``ogr_*``) and
SpatiaLite's spatial functions. So no layer whose name holds a dot is
listed: ``roads.v2`` would read layer ``v2`` of a file ``roads``.

Only the expected driver may open a connection (ESRI Shapefile, or
OpenFileGDB for a geodatabase), a ``.shp`` must start with a shapefile's
own bytes (GDAL picks a driver by content, and VRT XML can read any file),
and no file of a layer may be a link leading out of the connection's
folder.

Geometry comes back as EWKB with the layer's EPSG code, as from PostGIS.
"""

from __future__ import annotations

import os
from typing import Any

import columbus_worker as w

ESRI_KINDS = w.ESRI_KINDS

# Extra caution only (it's GDAL's default, and gates functions such as
# gdal_get_pixel_value): it does not stop the SQLite dialect opening a file
# a query names. The gateway's table checks do that.
_OPEN_OPTIONS = {"OGR_SQLITE_ALLOW_EXTERNAL_ACCESS": "NO"}

# The drivers each kind may be opened with.
_DRIVERS = {"shapefile": {"ESRI Shapefile"}, "filegdb": {"OpenFileGDB", "FileGDB"}}

# A shapefile's file code (big-endian 9994), its first four bytes.
_SHP_MAGIC = b"\x00\x00\x27\x0a"

# OGR field types that GDAL's SQLite dialect hands over as text
# "YYYY/MM/DD": they don't compare as dates.
_TEXT_DATE_TYPES = {"OFTDate", "OFTDateTime", "OFTTime"}


def _pyogrio():
    try:
        import pyogrio
    except ImportError:
        raise w.WorkerError(w.UNSUPPORTED, "reading shapefiles and geodatabases needs the GIS engine (workers/python/requirements-gis.txt)") from None
    pyogrio.set_gdal_config_options(_OPEN_OPTIONS)
    return pyogrio


def _inside(root_real: str, p: str) -> bool:
    real = os.path.realpath(p)
    return real == root_real or real.startswith(root_real.rstrip(os.sep) + os.sep)


def _path(conn: dict[str, Any]) -> str:
    path = conn.get("path")
    if not isinstance(path, str) or not path:
        raise w.WorkerError(w.INVALID_PARAMS, "this connection has no path")
    kind = conn["kind"].lower()
    if kind == "filegdb":
        if not (os.path.isdir(path) and path.rstrip("/\\").lower().endswith(".gdb")):
            raise w.WorkerError(w.INVALID_PARAMS, f"{path} is not a file geodatabase (a folder ending in .gdb)")
        root = os.path.realpath(path)
        # Every file of the geodatabase is its own, not a link out of it.
        for d, dirs, files in os.walk(path):
            for n in dirs + files:
                if not _inside(root, os.path.join(d, n)):
                    raise w.WorkerError(w.INVALID_PARAMS, f"{n} in {path} leads outside the geodatabase; Columbus won't read it")
    elif os.path.isdir(path):
        pass
    elif os.path.isfile(path) and path.lower().endswith(".shp"):
        _check_shp(path, os.path.realpath(os.path.dirname(path)))
    else:
        raise w.WorkerError(w.INVALID_PARAMS, f"{path} is not a folder of shapefiles or a .shp file")
    return path


def _check_shp(shp: str, root_real: str) -> None:
    """A real shapefile: its own bytes, and its parts inside the folder."""
    folder, stem = os.path.split(shp[:-4])
    # Every file of this shapefile (any extension, any case: GDAL looks for
    # its parts without regard to case).
    for n in os.listdir(folder or "."):
        part = os.path.join(folder, n)
        if os.path.splitext(n)[0].lower() == stem.lower() and not _inside(root_real, part):
            raise w.WorkerError(w.INVALID_PARAMS, f"{n} leads outside the connection's folder; Columbus won't read it")
    try:
        with open(shp, "rb") as f:
            head = f.read(4)
    except OSError as e:
        raise w.WorkerError(w.ENGINE_ERROR, f"cannot read {shp}: {e}") from None
    if head != _SHP_MAGIC:
        raise w.WorkerError(w.INVALID_PARAMS, f"{os.path.basename(shp)} is not a shapefile (its content isn't one)")


def _layer_ok(kind: str, path: str, name: str, info: dict[str, Any]) -> str | None:
    """Why a layer can't be offered, or None."""
    if "." in name:
        return "its name has a dot, which SQL would read as a file and a layer; rename it without dots"
    if info.get("driver") not in _DRIVERS[kind]:
        return f"GDAL opened it as {info.get('driver')!r}, not as the {kind} it should be"
    if kind == "shapefile" and os.path.isdir(path):
        shp = next((os.path.join(path, n) for n in (name + ".shp", name + ".SHP") if os.path.exists(os.path.join(path, n))), None)
        if shp is None:
            return "its .shp file isn't directly in the folder"
        try:
            _check_shp(shp, os.path.realpath(path))
        except w.WorkerError as e:
            return str(e)
    return None


def _crs(crs: str | None):
    """(EPSG code or None, crs_spec) from GDAL's CRS text."""
    if not crs:
        return None, None
    if crs.upper().startswith("EPSG:") and crs[5:].isdigit():
        code = int(crs[5:])
        return code, {"org": "EPSG", "org_id": code}
    try:
        from pyproj import CRS

        c = CRS.from_user_input(crs)
        code = c.to_epsg()
        if code:
            return code, {"org": "EPSG", "org_id": code}
        return None, {"wkt": c.to_wkt()}
    except Exception:  # noqa: BLE001
        return None, {"wkt": crs}


def esri_list(conn: dict[str, Any], timeout_s: float, only: str | None = None) -> list[dict[str, Any]]:
    pyogrio = _pyogrio()
    path = _path(conn)
    kind = conn["kind"].lower()
    try:
        layers = pyogrio.list_layers(path)
    except Exception as e:  # noqa: BLE001
        raise w.WorkerError(w.ENGINE_ERROR, f"cannot open {path}: {e}") from None
    out = []
    for name, gtype in layers:
        name = str(name)
        if only and name.lower() != only.lower():
            continue
        try:
            info = pyogrio.read_info(path, layer=name)
        except Exception:  # noqa: BLE001
            continue  # unreadable: not offered
        if _layer_ok(kind, path, name, info) is not None:
            # Not offered at all: the gateway would otherwise put the name
            # into SQL.
            continue
        types = list(info.get("ogr_types") or [])
        fields = []
        text_dates = []
        for i, (f, t) in enumerate(zip(info["fields"], info["dtypes"])):
            fields.append({"name": str(f), "type": str(t)})
            if kind == "shapefile" and i < len(types) and types[i] in _TEXT_DATE_TYPES:
                text_dates.append(str(f))
        fid = info.get("fid_column") or ""
        if fid:
            # A geodatabase's OBJECTID: readable by name in the SQLite dialect.
            fields.insert(0, {"name": fid, "type": "int64", "not_null": True, "pk": True})
        entry: dict[str, Any] = {"name": name, "type": "table", "fields": fields}
        if text_dates:
            # Compared as text YYYY/MM/DD: the gateway refuses row filters
            # on them, and describe_dataset warns.
            entry["text_dates"] = text_dates
        if info.get("geometry_type"):
            # The SQLite dialect names a shapefile's geometry GEOMETRY.
            col = info.get("geometry_name") or "GEOMETRY"
            code, spec = _crs(info.get("crs"))
            entry.update({"geometry_column": col, "geometry_type": info.get("geometry_type") or (str(gtype) if gtype else None),
                          "srs_id": code, "crs_spec": spec})
            entry["fields"].append({"name": col, "type": "geometry"})
            bounds = info.get("total_bounds")
            if bounds is not None and all(b == b for b in bounds):  # no NaN
                entry["extent"] = [float(b) for b in bounds]
        if isinstance(info.get("features"), int) and info["features"] >= 0:
            entry["estimated_rows"] = info["features"]
        out.append(entry)
    return out


def _value(v: Any) -> Any:
    """A plain Python value for a cell (numpy scalars, dates)."""
    if v is None:
        return None
    if hasattr(v, "item"):
        try:
            v = v.item()
        except (ValueError, TypeError):
            pass
        if v is None:  # NaT: a null date
            return None
    if isinstance(v, float) and v != v:  # NaN: a null number
        return None
    if hasattr(v, "isoformat"):  # date, datetime, time
        return v.isoformat()
    if isinstance(v, (bytes, bytearray, memoryview)):
        return bytes(v)
    if isinstance(v, (int, float, str, bool)):
        return v
    return str(v)


def _geom_cell(v: Any) -> Any:
    if v is None:
        return None
    return {"geometry": "ewkb", "bytes": len(bytes(v))}


# Without pyarrow, rows are read a page at a time (GDAL runs the query again
# for each page, skipping the rows already read), so memory stays bounded.
PAGE = 50_000


def esri_fetch(conn: dict[str, Any], sql: str, max_rows: int, timeout_s: float, keep_factory=None, sink=None):
    """Run a checked SELECT: (columns, rows, cut off, cell functions,
    geometry formats by column), as the other engines' fetches."""
    pyogrio = _pyogrio()
    path = _path(conn)
    try:
        # Each layer's driver is the source's; ask by the first layer's name
        # (asking with none warns when there are several).
        layers = pyogrio.list_layers(path)
        driver = pyogrio.read_info(path, layer=str(layers[0][0])).get("driver") if len(layers) else None
    except Exception as e:  # noqa: BLE001
        raise w.WorkerError(w.ENGINE_ERROR, f"cannot open {path}: {e}") from None
    if driver not in _DRIVERS[conn["kind"].lower()]:
        raise w.WorkerError(w.INVALID_PARAMS, f"GDAL opened {path} as {driver!r}, not as a {conn['kind']}")
    collect = sink is None
    sink = w._Collect() if sink is None else sink
    try:
        try:
            import pyarrow  # noqa: F401

            streamed = True
        except ImportError:
            streamed = False
        if streamed:
            columns, hit = _fetch_arrow(pyogrio, path, sql, max_rows, keep_factory, sink)
        else:
            columns, hit = _fetch_pages(pyogrio, path, sql, max_rows, keep_factory, sink)
    except w.WorkerError:
        raise
    except Exception as e:  # noqa: BLE001
        raise w.WorkerError(w.ENGINE_ERROR, f"query failed: {e}") from None
    cells, geoms = _shape(columns)
    return columns, sink.rows if collect else [], hit, cells, geoms


def _shape(columns):
    geoms = {len(columns) - 1: "ewkb"} if columns and isinstance(columns[-1], _GeomName) else {}
    cells = [(_geom_cell if i in geoms else w._cell) for i in range(len(columns))]
    return cells, geoms


class _GeomName(str):
    """The name of the geometry column, told apart from a field of that name."""


class _Starter:
    """Starts the sink on the first chunk (the columns are known by then)."""

    def __init__(self, sink, columns, keep_factory):
        self.sink, self.columns = sink, columns
        cells, geoms = _shape(columns)
        self.sink.start(columns, cells, geoms)
        self.keep = keep_factory(columns, "ewkb") if keep_factory else None


def _fetch_arrow(pyogrio, path, sql, max_rows, keep_factory, sink):
    with pyogrio.raw.open_arrow(path, sql=sql, sql_dialect="SQLITE", batch_size=w.FETCH_CHUNK, use_pyarrow=True) as (meta, reader):
        geom_name = meta.get("geometry_name") or "wkb_geometry"
        srid, _ = _crs(meta.get("crs"))
        # A geometry only where GDAL says the result has one (a field may
        # be called wkb_geometry).
        has_geom = bool(meta.get("geometry_type")) and geom_name in reader.schema.names
        names = [n for n in reader.schema.names if not (has_geom and n == geom_name)]
        label = _GeomName(geom_name if meta.get("geometry_name") else "GEOMETRY")
        columns = names + ([label] if has_geom else [])
        st = _Starter(sink, columns, keep_factory)
        batches = iter(reader)

        def fetch():
            for batch in batches:
                cols = [batch.column(n).to_pylist() for n in names]
                gcol = batch.column(geom_name).to_pylist() if has_geom else None
                rows = []
                for i in range(batch.num_rows):
                    r = [_value(c[i]) for c in cols]
                    if has_geom:
                        r.append(w._ewkb(gcol[i], srid))
                    rows.append(r)
                if rows:
                    return rows
            return []

        _n, hit = w._drain(fetch, max_rows, None, st.keep, sink)
        return columns, hit


def _geometry_name(pyogrio, path, sql) -> str | None:
    """The name GDAL gives the result's geometry (``read`` doesn't say): from
    the Arrow stream's description, which needs no pyarrow to read."""
    try:
        with pyogrio.raw.open_arrow(path, sql=sql, sql_dialect="SQLITE", batch_size=1, use_pyarrow=False) as (meta, _reader):
            return meta.get("geometry_name") or None
    except Exception:  # noqa: BLE001
        return None


def _fetch_pages(pyogrio, path, sql, max_rows, keep_factory, sink):
    state: dict[str, Any] = {"skip": 0, "done": False, "columns": None, "starter": None}

    def page():
        if state["done"]:
            return []
        meta, _fids, geom, fields = pyogrio.raw.read(path, sql=sql, sql_dialect="SQLITE", skip_features=state["skip"], max_features=PAGE)
        names = [str(n) for n in meta["fields"]]
        if state["columns"] is None:
            label = _GeomName(_geometry_name(pyogrio, path, sql) or "GEOMETRY")
            state["columns"] = names + ([label] if geom is not None else [])
            state["srid"], _ = _crs(meta.get("crs"))
        n = len(geom) if geom is not None else (len(fields[0]) if fields else 0)
        state["skip"] += n
        if n < PAGE:
            state["done"] = True
        rows = []
        for i in range(n):
            r = [_value(f[i]) for f in fields]
            if geom is not None:
                r.append(w._ewkb(geom[i], state["srid"]))
            rows.append(r)
        return rows

    first = page()
    columns = state["columns"] or []
    st = _Starter(sink, columns, keep_factory)
    pending = [first]

    def fetch():
        while pending:
            chunk = pending.pop()
            if chunk:
                return chunk
        return page()

    _n, hit = w._drain(fetch, max_rows, None, st.keep, sink)
    return columns, hit
