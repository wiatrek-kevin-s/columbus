"""Esri shapefiles and file geodatabases, read through GDAL (pyogrio).

A ``shapefile`` connection is a folder of shapefiles (each ``.shp`` is one
dataset) or one ``.shp`` file; a ``filegdb`` connection is a file
geodatabase (a ``.gdb`` folder), whose feature classes and tables are its
datasets. Both are read only.

SQL arrives checked and rewritten by the gateway in the SQLite dialect, as
for a GeoPackage, and runs in GDAL's SQLite dialect: real SQLite over the
layers, which never writes to them. The gateway resolves every table to a
dataset this module listed, so a query can't name another file (GDAL's
SQLite dialect would otherwise open ``"other.shp"."layer"`` by path), and
its function allowlist keeps out GDAL's own functions (``ogr_*``).

Geometry comes back as EWKB with the layer's EPSG code, as from PostGIS.
"""

from __future__ import annotations

import os
from typing import Any

import columbus_worker as w

ESRI_KINDS = w.ESRI_KINDS

# GDAL's SQLite dialect may otherwise open other files a query names; the
# gateway already refuses any table it didn't list, and this says the same
# to GDAL.
_OPEN_OPTIONS = {"OGR_SQLITE_ALLOW_EXTERNAL_ACCESS": "NO"}


def _pyogrio():
    try:
        import pyogrio
    except ImportError:
        raise w.WorkerError(w.UNSUPPORTED, "reading shapefiles and geodatabases needs the GIS engine (workers/python/requirements-gis.txt)") from None
    pyogrio.set_gdal_config_options(_OPEN_OPTIONS)
    return pyogrio


def _path(conn: dict[str, Any]) -> str:
    path = conn.get("path")
    if not isinstance(path, str) or not path:
        raise w.WorkerError(w.INVALID_PARAMS, "this connection has no path")
    kind = conn["kind"].lower()
    if kind == "filegdb":
        if not (os.path.isdir(path) and path.rstrip("/\\").lower().endswith(".gdb")):
            raise w.WorkerError(w.INVALID_PARAMS, f"{path} is not a file geodatabase (a folder ending in .gdb)")
    elif not (os.path.isdir(path) or (os.path.isfile(path) and path.lower().endswith(".shp"))):
        raise w.WorkerError(w.INVALID_PARAMS, f"{path} is not a folder of shapefiles or a .shp file")
    return path


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
        except Exception as e:  # noqa: BLE001
            out.append({"name": name, "type": "table", "fields": [], "problem": str(e)})
            continue
        fields = [{"name": str(f), "type": str(t)} for f, t in zip(info["fields"], info["dtypes"])]
        fid = info.get("fid_column") or ""
        if fid:
            # A geodatabase's OBJECTID: readable by name in the SQLite dialect.
            fields.insert(0, {"name": fid, "type": "int64", "not_null": True, "pk": True})
        entry: dict[str, Any] = {"name": name, "type": "table", "fields": fields}
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
    if isinstance(v, float) and v != v:  # NaN: a null number
        return None
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
        names = [n for n in reader.schema.names if n != geom_name]
        has_geom = geom_name in reader.schema.names
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
