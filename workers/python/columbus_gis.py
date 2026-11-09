"""GIS engine for the Columbus core worker.

Coordinate reference system facts, spatial verification checks (design
section 4), reading and writing layers, and vector operations. Built on
GeoPandas with pyogrio (GDAL), Shapely 2 (GEOS) and pyproj (PROJ), which
the managed engine environment provides.

Every operation returns ``checks`` in the verification report format: a
stable ``id``, a ``severity`` of ``fail``, ``warn`` or ``info``, a plain
``message`` with the numbers, and where possible a ``fix``.
"""

from __future__ import annotations

import math
import os
import re
from typing import Any

# Before GDAL loads: leave out the drivers whose files can point at other
# files, programs or URLs (VRT, the GDALG pipeline format), and keep PROJ
# from fetching grids over the network. Workspace inputs are also checked by
# content in read_layer; this is the second line.
_SKIP = {"VRT", "OGR_VRT", "GDALG", "GTI", "MRF", "WMS", "WMTS", "WCS", "PDS", "PDS4", "ISIS2", "ISIS3",
         "STACIT", "STACTA", "HTTP", "DAAS", "PLMOSAIC", "EEDAI", "OGCAPI", "ESRIC", "SENTINEL2",
         "SAFE", "Zarr", "netCDF", "HDF5", "HDF4", "SRP", "NITF", "RPFTOC", "ECRGTOC", "CEOS2", "XYZ"}
_skip_now = {d for d in os.environ.get("GDAL_SKIP", "").replace(",", " ").split() if d}
os.environ["GDAL_SKIP"] = " ".join(sorted(_SKIP | _skip_now))
os.environ["PROJ_NETWORK"] = "OFF"
# Read only the file named: no sidecars (.aux.xml, .ovr, world files, .msk)
# beside it may change its georeferencing or redirect a read elsewhere.
os.environ["GDAL_DISABLE_READDIR_ON_OPEN"] = "EMPTY_DIR"
os.environ["GDAL_PAM_ENABLED"] = "NO"
os.environ["GDAL_GEOREF_SOURCES"] = "INTERNAL"
# A machine-wide setting must not re-enable SQL functions in GeoPackages
# that open other datasets (gdal_get_pixel_value and the like).
os.environ["OGR_SQLITE_ALLOW_EXTERNAL_ACCESS"] = "NO"
os.environ.setdefault("CPL_VSIL_CURL_ALLOWED_EXTENSIONS", ".columbus-none")

import geopandas as gpd  # noqa: E402
import pandas as pd  # noqa: E402
import pyogrio  # noqa: E402
import pyproj  # noqa: E402
import shapely  # noqa: E402
from pyproj import CRS, Transformer  # noqa: E402

from columbus_plugin import readonly_uri  # noqa: E402

__all__ = [
    "GISError",
    "versions",
    "check",
    "crs_from_spec",
    "crs_facts",
    "crs_checks",
    "geometry_checks",
    "decode",
    "bbox_filter",
    "build_layer",
    "write_layer",
    "read_layer",
    "layer_summary",
    "vector_op",
    "verify_layer",
    "OPS",
]


class GISError(Exception):
    """A refusal with a stable check id, a plain message and a fix."""

    def __init__(self, check_id: str, message: str, fix: str | None = None):
        super().__init__(message)
        self.check_id = check_id
        self.message = message
        self.fix = fix

    def __str__(self) -> str:
        text = f"Refused ({self.check_id}): {self.message}"
        return f"{text} Fix: {self.fix}" if self.fix else text


def versions() -> dict[str, str]:
    return {
        "geopandas": gpd.__version__,
        "gdal": pyogrio.__gdal_version_string__,
        "geos": shapely.geos_version_string,
        "proj": pyproj.proj_version_str,
    }


def check(id: str, severity: str, message: str, detail: Any = None, fix: str | None = None) -> dict[str, Any]:
    c: dict[str, Any] = {"id": id, "severity": severity, "message": message}
    if detail is not None:
        c["detail"] = detail
    if fix:
        c["fix"] = fix
    return c


# ---------------------------------------------------------------------------
# Coordinate reference systems
# ---------------------------------------------------------------------------


EPSG_CODE = re.compile(r"(?i)EPSG:\d{1,6}")
AUTHORITY_CODE = re.compile(r"(?i)[A-Z][A-Z0-9_]{1,15}:\d{1,9}")


def crs_from_spec(spec: dict[str, Any] | None) -> CRS | None:
    """A CRS from what a source records: an authority code, a WKT
    definition, or a bare SRID. Never reads files."""
    if not spec:
        return None
    org = str(spec.get("org") or "").upper()
    org_id = spec.get("org_id")
    wkt = spec.get("wkt")
    code = spec.get("code")
    try:
        if code:
            # Authority codes only: PROJ strings can name init files.
            if not AUTHORITY_CODE.fullmatch(str(code).strip()):
                return None
            return CRS.from_user_input(str(code).strip().upper())
        if org == "EPSG" and isinstance(org_id, int) and org_id > 0:
            return CRS.from_epsg(org_id)
        if isinstance(wkt, str) and wkt.strip() and wkt.strip().lower() not in ("undefined", "unknown"):
            return CRS.from_wkt(wkt)
        if isinstance(org_id, int) and org_id > 0:
            return CRS.from_epsg(org_id)
    except pyproj.exceptions.CRSError:
        return None
    return None


def crs_facts(crs: CRS | None) -> dict[str, Any] | None:
    if crs is None:
        return None
    epsg = crs.to_epsg()
    axis = crs.axis_info[0] if crs.axis_info else None
    aou = crs.area_of_use
    return {
        "code": f"EPSG:{epsg}" if epsg else None,
        "name": crs.name,
        "geographic": crs.is_geographic,
        "units": axis.unit_name if axis else None,
        "metres_per_unit": axis.unit_conversion_factor if axis and not crs.is_geographic else None,
        "area_of_use": [aou.west, aou.south, aou.east, aou.north] if aou else None,
    }


def _label(crs: CRS) -> str:
    epsg = crs.to_epsg()
    return f"EPSG:{epsg} ({crs.name})" if epsg else crs.name


def _overlaps(a, b, margin: float = 0.0) -> bool:
    return not (a[2] < b[0] - margin or a[0] > b[2] + margin or a[3] < b[1] - margin or a[1] > b[3] + margin)


def crs_checks(crs: CRS | None, extent: list[float] | None, has_geometry: bool = True, what: str = "The layer") -> list[dict]:
    """crs.present, crs.info and crs.area_of_use (coordinates outside the
    area a CRS is meant for, or that are not degrees in a geographic CRS,
    or swapped X/Y)."""
    if not has_geometry:
        return []
    if crs is None:
        return [check("crs.present", "fail", f"{what} has no coordinate reference system",
                      fix="Find the source's CRS (metadata, .prj, the data owner) and assign it before measuring or overlaying")]
    out = [check("crs.info", "info", f"{what} is in {_label(crs)}, units {crs_facts(crs)['units']}")]
    if not extent or any(not math.isfinite(v) for v in extent):
        return out
    if crs.is_geographic and (abs(extent[1]) > 90.5 or abs(extent[3]) > 90.5 or abs(extent[0]) > 360 or abs(extent[2]) > 360):
        swapped_ok = abs(extent[0]) <= 90.5 and abs(extent[2]) <= 90.5 and abs(extent[1]) <= 180.5 and abs(extent[3]) <= 180.5
        out.append(check("crs.area_of_use", "fail",
                         f"{what} is labelled {_label(crs)} (degrees), but its coordinates reach beyond ±180/±90, so they cannot be longitude and latitude in that order (the extent is in the detail)",
                         detail={"extent": extent},
                         fix="Longitude and latitude look swapped; swap X and Y" if swapped_ok
                         else "The data is probably in a projected CRS; find the right one and assign it (do not reproject)"))
        return out
    aou = crs.area_of_use
    if aou is None:
        return out
    area = [aou.west, aou.south, aou.east, aou.north]
    try:
        t = Transformer.from_crs(crs, 4326, always_xy=True)
        ll = list(t.transform_bounds(*extent))
    except Exception:  # noqa: BLE001 - cannot test, say nothing
        return out
    if any(not math.isfinite(v) for v in ll):
        return out
    beyond = _beyond(ll, area)
    if beyond is None:
        swapped = [extent[1], extent[0], extent[3], extent[2]]
        fix = "Check the CRS is right; if the data was labelled with the wrong CRS, assign the right one rather than reprojecting"
        if crs.is_geographic and _beyond(swapped, area) is not None:
            fix = "Longitude and latitude look swapped; swap X and Y"
        out.append(check("crs.area_of_use", "warn",
                         f"{what}'s coordinates fall outside the area {_label(crs)} is meant for ({aou.name})",
                         detail={"extent_lonlat": [round(v, 4) for v in ll], "area_of_use": area}, fix=fix))
    elif crs.is_projected and _far_beyond(beyond, area):
        # Partly inside: fine for a geographic CRS, but a projection made
        # for a smaller area distorts distances and areas away from it.
        out.append(check("crs.area_of_use", "warn",
                         f"{what} reaches well beyond the area {_label(crs)} is meant for ({aou.name}); "
                         "distances and areas measured far from it are distorted",
                         detail={"extent_lonlat": [round(v, 4) for v in ll], "area_of_use": area,
                                 "degrees_beyond": {k: round(v, 2) for k, v in beyond.items()}},
                         fix="Use a CRS meant for the whole extent (an equal-area CRS such as EPSG:5070 for the "
                             "contiguous US), or split the data by area"))
    return out


def _lon_range(west: float, east: float) -> tuple[float, float]:
    """A longitude range as an increasing pair; one that crosses the 180°
    line (west > east, as NAD83's 167.65 to -40.73) gets east + 360."""
    return (west, east + 360.0) if east < west else (west, east)


def _beyond(ll: list[float], area: list[float], margin: float = 0.25) -> dict[str, float] | None:
    """How many degrees a lon/lat extent reaches past an area of use on each
    side, or None when the two do not meet at all (within ``margin``). Both
    may cross the 180° line; data spanning more than 300° of longitude (a
    layer that itself crosses it, such as the Aleutians) is compared by
    latitude only."""
    aw, ae = _lon_range(area[0], area[2])
    s_beyond = max(0.0, area[1] - ll[1])
    n_beyond = max(0.0, ll[3] - area[3])
    lat_meets = not (ll[3] < area[1] - margin or ll[1] > area[3] + margin)
    dw, de = _lon_range(ll[0], ll[2])
    if de - dw > 300:
        return {"west": 0.0, "east": 0.0, "south": s_beyond, "north": n_beyond} if lat_meets else None
    best = None
    for shift in (-360.0, 0.0, 360.0):
        w, e = dw + shift, de + shift
        if e < aw - margin or w > ae + margin:
            continue
        cand = {"west": max(0.0, aw - w), "east": max(0.0, e - ae)}
        if best is None or cand["west"] + cand["east"] < best["west"] + best["east"]:
            best = cand
    if best is None or not lat_meets:
        return None
    return {**best, "south": s_beyond, "north": n_beyond}


def _far_beyond(beyond: dict[str, float], area: list[float]) -> bool:
    """Reaching past the area by more than a quarter of its size (and at
    least 1°) on any side."""
    aw, ae = _lon_range(area[0], area[2])
    width, height = ae - aw, area[3] - area[1]
    lon_limit = max(1.0, width / 4)
    lat_limit = max(1.0, height / 4)
    return (beyond["west"] > lon_limit or beyond["east"] > lon_limit
            or beyond["south"] > lat_limit or beyond["north"] > lat_limit)


# ---------------------------------------------------------------------------
# Geometry
# ---------------------------------------------------------------------------


def decode(value: Any, fmt: str):
    """A Shapely geometry from a GeoPackage blob, EWKB bytes or hex EWKB."""
    if value is None:
        return None
    if isinstance(value, str):
        value = bytes.fromhex(value)
    raw = bytes(value)
    if fmt == "gpkg" or raw[:2] == b"GP":
        flags = raw[3]
        env = {0: 0, 1: 32, 2: 48, 3: 48, 4: 64}.get((flags >> 1) & 0x07, 0)
        if flags & 0x10 and len(raw) <= 8 + env:  # empty geometry flag, no body
            return shapely.Point()
        raw = raw[8 + env:]
    return shapely.from_wkb(raw)


def _family(t: str | None) -> str | None:
    if not t:
        return None
    return t[5:] if t.startswith("Multi") else t


def geometry_checks(gs: gpd.GeoSeries, what: str = "The layer") -> list[dict]:
    tally = GeometryTally()
    tally.add(gs.to_numpy())
    return tally.checks(what)


class GeometryTally:
    """What geometry_checks reports, counted a batch at a time, so a layer
    written in batches is checked without holding it whole."""

    def __init__(self):
        self.n = 0
        self.null = 0
        self.empty = 0
        self.invalid = 0
        self.reasons: list[str] = []
        self.types: set[str] = set()
        self.has_z = 0
        self.has_m = 0
        self.extent: list[float] | None = None

    def add(self, arr) -> None:
        import numpy as np

        arr = np.asarray(arr, dtype=object)
        n = len(arr)
        if n == 0:
            return
        self.n += n
        null = shapely.is_missing(arr)
        present = ~null
        empty = present & shapely.is_empty(arr)
        invalid = present & ~empty & ~shapely.is_valid(arr)
        self.null += int(null.sum())
        self.empty += int(empty.sum())
        k = int(invalid.sum())
        if k:
            self.invalid += k
            room = 3 - len(self.reasons)
            if room > 0:
                self.reasons.extend(shapely.is_valid_reason(g) for g in arr[invalid][:room])
        solid = present & ~empty
        if solid.any():
            kinds = shapely.get_type_id(arr[solid])
            for t in np.unique(kinds):
                self.types.add(_TYPE_NAMES.get(int(t), "Geometry"))
            self.has_z += int(shapely.has_z(arr[solid]).sum())
            self.has_m += int(shapely.has_m(arr[solid]).sum())
            b = shapely.bounds(arr[solid])
            box = [float(np.nanmin(b[:, 0])), float(np.nanmin(b[:, 1])), float(np.nanmax(b[:, 2])), float(np.nanmax(b[:, 3]))]
            if all(math.isfinite(v) for v in box):
                e = self.extent
                self.extent = box if e is None else [min(e[0], box[0]), min(e[1], box[1]), max(e[2], box[2]), max(e[3], box[3])]

    def checks(self, what: str = "The layer") -> list[dict]:
        out = []
        if self.n == 0:
            return out
        if self.invalid:
            out.append(check("geometry.valid", "warn", f"{self.invalid} of {self.n} geometries are invalid",
                             detail={"invalid": self.invalid, "examples": self.reasons},
                             fix="Repair them with vector_op make_valid before overlay, clip or dissolve"))
        if self.null or self.empty:
            out.append(check("geometry.empty", "info", f"{self.null} rows have no geometry and {self.empty} have an empty one",
                             detail={"null": self.null, "empty": self.empty}))
        families = {_family(t) for t in self.types} - {None}
        if len(families) > 1:
            out.append(check("geometry.mixed_types", "warn", f"{what} mixes geometry types: {', '.join(sorted(families))}",
                             fix="Split it by geometry type before exporting to formats that allow one type per layer"))
        return out

    def gpkg_type(self) -> str:
        """The GeoPackage geometry type name for what was seen: one type,
        its multi-part form when single and multi parts are mixed (as GDAL
        writes it), else GEOMETRY."""
        families = {_family(t) for t in self.types} - {None}
        if len(families) != 1 or "GeometryCollection" in self.types:
            return "GEOMETRY"
        if len(self.types) == 1:
            return next(iter(self.types)).upper()
        return ("Multi" + next(iter(families))).upper()


_TYPE_NAMES = {0: "Point", 1: "LineString", 2: "LinearRing", 3: "Polygon", 4: "MultiPoint",
               5: "MultiLineString", 6: "MultiPolygon", 7: "GeometryCollection"}


# ---------------------------------------------------------------------------
# M (measure) and Z values
#
# Shapely 2.1+ keeps M through WKB and WKT, and GEOS keeps it through the
# operations that keep vertices (clip, union, make_valid). Three places lose
# it unless handled here: GeoPandas writes EWKB, whose M flag GDAL refuses
# ("Could not create geometry ... for WKB type 1073741826"); pyogrio reads M
# types as 2D unless it reads through Arrow; and shapely.transform (to_crs)
# drops M and turns Z into NaN when M is there too.
# ---------------------------------------------------------------------------


def _m_mask(arr):
    """Which of an object array of geometries carry M values."""
    import numpy as np

    arr = np.asarray(arr, dtype=object)
    if not len(arr):
        return np.zeros(0, dtype=bool)
    present = ~shapely.is_missing(arr)
    out = np.zeros(len(arr), dtype=bool)
    if present.any():
        out[present] = shapely.has_m(arr[present])
    return out


def has_m(geoms) -> bool:
    """Whether any of the geometries (a GeoSeries, array or list) has M."""
    import numpy as np

    arr = geoms.to_numpy() if hasattr(geoms, "to_numpy") else np.asarray(geoms, dtype=object)
    return bool(_m_mask(arr).any())


def _gpkg_has_m(path: str) -> bool | None:
    """Whether a GeoPackage declares M on a geometry column (z/m 1 or 2),
    or None when that cannot be told. Read with SQLite, read only, so the
    answer costs no GDAL open. The URI is built by ``readonly_uri``, which
    handles Windows drive letters, UNC shares and the verbatim
    (``\\\\?\\``) paths the gateway's canonical workspace paths have there."""
    import sqlite3

    try:
        db = sqlite3.connect(readonly_uri(os.path.abspath(path)), uri=True)
    except (sqlite3.Error, ValueError):
        return None
    try:
        return bool(db.execute("SELECT 1 FROM gpkg_geometry_columns WHERE m IN (1, 2) LIMIT 1").fetchone())
    except sqlite3.Error:
        return None
    finally:
        db.close()


def _read_gpkg(path: str, **kw):
    """A GeoPackage as a GeoDataFrame, keeping M where the file has it:
    pyogrio reads M through Arrow only (its plain read drops M with a
    warning). When the header cannot be read, Arrow is used too, so M is
    never dropped for want of an answer."""
    if _gpkg_has_m(path) is not False:
        import warnings

        with warnings.catch_warnings():
            # pyogrio warns about M types even when Arrow keeps them.
            warnings.filterwarnings("ignore", r"Measured \(M\) geometry types are not supported", UserWarning)
            return gpd.read_file(path, engine="pyogrio", use_arrow=True, **kw)
    return gpd.read_file(path, engine="pyogrio", **kw)


def _read_2d(path: str, **kw):
    """A GeoPackage read for display or paging, where M is not needed: M
    is left out without pyogrio's warning about it."""
    import warnings

    with warnings.catch_warnings():
        warnings.filterwarnings("ignore", r"Measured \(M\) geometry types are not supported", UserWarning)
        return gpd.read_file(path, engine="pyogrio", **kw)


def _info(path: str, **kw) -> dict:
    """pyogrio.read_info without its warning about M types (the counts,
    fields and bounds it gives do not depend on M)."""
    import warnings

    with warnings.catch_warnings():
        warnings.filterwarnings("ignore", r"Measured \(M\) geometry types are not supported", UserWarning)
        return pyogrio.read_info(path, **kw)


def _ogr_type(arr) -> tuple[str, bool]:
    """(pyogrio geometry type name, promote to multi) for a layer of
    geometries with M, as pyogrio itself would choose them without M."""
    import numpy as np

    arr = np.asarray(arr, dtype=object)
    solid = arr[~shapely.is_missing(arr)]
    solid = solid[~shapely.is_empty(solid)] if len(solid) else solid
    z = bool(len(solid)) and bool(shapely.has_z(solid).any())
    kinds = {int(k) for k in shapely.get_type_id(solid)} if len(solid) else set()
    promote = False
    if len(kinds) == 1:
        base = kinds.pop()
    elif len(kinds) == 2 and kinds in ({0, 4}, {1, 5}, {3, 6}):
        base, promote = max(kinds), True
    else:
        return ("Unknown", False)
    name = _TYPE_NAMES[base]
    if base == 0 and not z:
        return ("PointM", promote)
    return (f"Measured {'3D ' if z else ''}{name}", promote)


def _ogr_fields(df, gname: str) -> tuple[list[str], list, list, dict]:
    """Field names, values, masks and time zone offsets for pyogrio's raw
    writer, prepared as pyogrio.write_dataframe prepares them."""
    import numpy as np

    fields, data, masks, tz = [], [], [], {}
    for name in df.columns:
        if name == gname:
            continue
        col = df[name]
        values = None
        if isinstance(col.dtype, pd.DatetimeTZDtype):
            naive = col.dt.tz_localize(None)
            values = naive.values
            offset = naive - col.dt.tz_convert("UTC").dt.tz_localize(None)
            tz[name] = (offset // pd.Timedelta("15m") + 100).values
        elif col.dtype == "object" and pd.api.types.infer_dtype(col) == "datetime":
            values = col.astype(str).where(col.notna(), None).values
        if values is None:
            values = col.values
        if isinstance(values, pd.api.extensions.ExtensionArray):
            from pandas.arrays import BooleanArray, FloatingArray, IntegerArray

            if isinstance(values, (IntegerArray, FloatingArray, BooleanArray)):
                data.append(values._data)
                masks.append(values._mask)
            else:
                data.append(np.asarray(values, dtype=object))
                masks.append(np.asarray(values.isna()))
        else:
            data.append(values)
            masks.append(None)
        fields.append(str(name))
    return fields, data, masks, tz


def write_ogr(df, path: str, driver: str, layer: str | None = None, layer_options: dict | None = None) -> None:
    """Write a GeoDataFrame with GDAL. A layer with M values is written as
    ISO WKB with an explicit measured geometry type (GDAL refuses the EWKB
    M flag GeoPandas writes); one without is written by GeoPandas as before."""
    kw: dict[str, Any] = {"driver": driver, "engine": "pyogrio"}
    if layer is not None:
        kw["layer"] = layer
    if layer_options:
        kw["layer_options"] = layer_options
    gname = df.geometry.name
    arr = df.geometry.to_numpy()
    if not _m_mask(arr).any():
        df.to_file(path, **kw)
        return
    from pyogrio.raw import write as ogr_write

    gtype, promote = _ogr_type(arr)
    wkb = shapely.to_wkb(arr, flavor="iso", output_dimension=4)
    fields, data, masks, tz = _ogr_fields(df, gname)
    crs = None
    if df.crs is not None:
        epsg = df.crs.to_epsg()
        crs = f"EPSG:{epsg}" if epsg else df.crs.to_wkt("WKT1_GDAL")
    ogr_write(path, geometry=wkb, field_data=data, fields=fields, field_mask=masks, layer=layer, driver=driver,
              geometry_type=gtype, crs=crs, promote_to_multi=promote or None,
              layer_options=layer_options or None, gdal_tz_offsets=tz or None)


def _rewrite_coords(wkb: bytes, coords, dims: int) -> bytes:
    """Little-endian ISO WKB with its coordinates replaced, in order, by
    the rows of ``coords`` (each ``dims`` wide). The structure (types,
    counts) is copied as it is."""
    import struct

    out = bytearray(wkb)
    flat = coords.astype("<f8").ravel().tobytes()
    width = 8 * dims
    pos = 0  # in flat

    def geom(o: int) -> int:
        nonlocal pos
        code = struct.unpack_from("<I", wkb, o + 1)[0] % 1000
        o += 5
        if code == 1:
            out[o:o + width] = flat[pos:pos + width]
            pos += width
            return o + width
        if code == 2:
            return line(o)
        if code == 3:
            (n,) = struct.unpack_from("<I", wkb, o)
            o += 4
            for _ in range(n):
                o = line(o)
            return o
        (n,) = struct.unpack_from("<I", wkb, o)
        o += 4
        for _ in range(n):
            o = geom(o)
        return o

    def line(o: int) -> int:
        nonlocal pos
        (n,) = struct.unpack_from("<I", wkb, o)
        o += 4
        size = n * width
        out[o:o + size] = flat[pos:pos + size]
        pos += size
        return o + size

    geom(0)
    return bytes(out)


def to_crs(df, target):
    """``df.to_crs(target)``, keeping M values (and Z beside them), which
    GeoPandas' reprojection drops. Only x and y (and z) are transformed."""
    import numpy as np

    out = df.to_crs(target)
    arr = df.geometry.to_numpy()
    measured = _m_mask(arr)
    if not measured.any():
        return out
    tf = Transformer.from_crs(df.crs, out.crs, always_xy=True)
    geoms = out.geometry.to_numpy().copy()
    zs = np.zeros(len(arr), dtype=bool)
    zs[measured] = shapely.has_z(arr[measured])
    for with_z in (False, True):
        pick = np.flatnonzero(measured & (zs == with_z))
        if not len(pick):
            continue
        part = arr[pick]
        coords = shapely.get_coordinates(part, include_z=with_z, include_m=True)
        if with_z:
            coords[:, 0], coords[:, 1], coords[:, 2] = tf.transform(coords[:, 0], coords[:, 1], coords[:, 2])
        else:
            coords[:, 0], coords[:, 1] = tf.transform(coords[:, 0], coords[:, 1])
        counts = shapely.get_num_coordinates(part)
        wkbs = shapely.to_wkb(part, flavor="iso", byte_order=1, output_dimension=4)
        dims = 4 if with_z else 3
        at = 0
        for i, w, n in zip(pick, wkbs, counts):
            geoms[i] = shapely.from_wkb(_rewrite_coords(w, coords[at:at + n], dims))
            at += n
    return out.set_geometry(gpd.GeoSeries(geoms, index=out.index, crs=out.crs))


def m_dropped_check(inputs: list, out, what: str) -> dict | None:
    """geometry.m_dropped (info) when an input had M values and the output
    has none: the operation built new vertices."""
    if not any(isinstance(d, gpd.GeoDataFrame) and has_m(d.geometry) for d in inputs):
        return None
    if isinstance(out, gpd.GeoDataFrame) and has_m(out.geometry):
        return None
    return check("geometry.m_dropped", "info",
                 f"{what} builds new vertices, so the output has no M (measure) values; the input's measures were dropped",
                 fix="Keep the measured layer for linear referencing; calibrate measures again on the new shapes if they need them")


def bbox_filter(bbox: list[float] | None, bbox_crs: CRS | None, layer_crs: CRS | None):
    """A predicate on Shapely geometries for rows whose bounds meet the
    box. The box is moved into the layer's CRS first."""
    if not bbox:
        return None
    if len(bbox) != 4 or bbox[0] > bbox[2] or bbox[1] > bbox[3]:
        raise GISError("bbox.invalid", "bbox must be [min x, min y, max x, max y]")
    box = list(bbox)
    if bbox_crs is not None and layer_crs is None:
        raise GISError("bbox.crs", "the dataset records no CRS, so a bbox in another CRS cannot be placed on it",
                       fix="Give the bbox in the dataset's own coordinates and leave out bbox_crs")
    if bbox_crs is not None and layer_crs is not None and not bbox_crs.equals(layer_crs):
        box = list(Transformer.from_crs(bbox_crs, layer_crs, always_xy=True).transform_bounds(*box))
    target = shapely.box(*box)

    shapely.prepare(target)

    def keep(geom) -> bool:
        # The envelope test is a quick pre-filter; the answer is whether the
        # geometry itself meets the box.
        return (
            geom is not None
            and not geom.is_empty
            and shapely.intersects(shapely.envelope(geom), target)
            and shapely.intersects(geom, target)
        )

    return keep


# ---------------------------------------------------------------------------
# Layers
# ---------------------------------------------------------------------------


def _plain(v: Any) -> Any:
    if isinstance(v, (bytes, bytearray, memoryview)):
        return bytes(v).hex()
    if v is None or isinstance(v, (int, float, str, bool)):
        return v
    return str(v)


def build_layer(columns: list[str], rows: list[list[Any]], geom_col: str | None, geoms: list | None, crs: CRS | None):
    """A GeoDataFrame (or DataFrame when there is no geometry)."""
    data = {c: [_plain(r[i]) for r in rows] for i, c in enumerate(columns)}
    df = pd.DataFrame(data, columns=columns)
    if geom_col is None:
        return df
    return gpd.GeoDataFrame(df, geometry=gpd.GeoSeries(geoms, crs=crs), crs=crs)


def write_layer(df, stem: str, layer: str = "layer") -> str:
    """Write a GeoPackage (with geometry) or CSV (without). Returns the path."""
    os.makedirs(os.path.dirname(stem) or ".", exist_ok=True)
    if isinstance(df, gpd.GeoDataFrame) and df.geometry.name in df.columns:
        path = stem + ".gpkg"
        if os.path.exists(path):
            os.remove(path)
        write_ogr(_fid_safe(df), path, "GPKG", layer=layer)
        return path
    path = stem + ".csv"
    pd.DataFrame(df).to_csv(path, index=False)
    return path


def _fid_safe(df):
    """GeoPackage keeps a column called "fid" as the feature id, which must be
    unique non-null integers. Keep one that is; otherwise (an empty result,
    nulls after a join, duplicates after an overlay) rename it source_fid."""
    for c in list(df.columns):
        if c.lower() != "fid" or c == df.geometry.name:
            continue
        col = df[c]
        if len(col) == 0:
            df = df.copy()
            df[c] = col.astype("int64")
        elif not (pd.api.types.is_integer_dtype(col) and col.notna().all() and col.is_unique):
            name = "source_fid"
            while name in df.columns:
                name = "_" + name
            df = df.rename(columns={c: name})
    return df


def _kind(v: Any) -> int:
    """A value's place in the type order null < boolean < integer < real <
    text, for choosing a column's type as a layer is written in batches."""
    if v is None:
        return 0
    if isinstance(v, bool):
        return 1
    if isinstance(v, int):
        return 2
    if isinstance(v, float):
        return 0 if math.isnan(v) else 3
    return 4


_DECLARED = {0: "TEXT", 1: "BOOLEAN", 2: "INTEGER", 3: "REAL", 4: "TEXT"}


def _widen(a: int, b: int) -> int:
    if a == 0 or a == b:
        return b
    if b == 0:
        return a
    if a == 4 or b == 4:
        return 4
    return max(a, b)  # boolean < integer < real


def _qi(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


class LayerStream:
    """A layer written in batches as rows arrive, so memory stays flat
    whatever the row count: a GeoPackage when there is a geometry column,
    CSV without one.

    The GeoPackage is written directly with SQLite (it is a SQLite file):
    the GeoPackage tables, then the features in batches. A column's type is
    the widest its values needed (boolean, integer, real or text); when a
    later batch needs a wider type than the first, the table is rebuilt in
    SQLite at the end, so no value is cut to fit. A source column named
    ``fid`` is kept as ``source_fid``: the GeoPackage's own ``fid`` numbers
    the features. No spatial index is written.
    """

    def __init__(self, stem: str, layer: str, attr_cols: list[str], geom_col: str | None, crs: CRS | None):
        os.makedirs(os.path.dirname(stem) or ".", exist_ok=True)
        self.layer = layer
        self.crs = crs
        self.geom_col = geom_col
        self.tally = GeometryTally()
        self.count = 0
        names: list[str] = []
        taken = {(geom_col or "").lower(), "fid"} if geom_col else set()
        for c in attr_cols:
            name = c
            if geom_col and c.lower() == "fid":
                name = "source_fid"
            while name.lower() in taken:
                name = "_" + name
            taken.add(name.lower())
            names.append(name)
        self.fields = names
        self.kinds = [0] * len(names)
        self.declared: list[int] | None = None
        self.declared_geom: str | None = None
        if geom_col is None:
            import csv

            self.path = stem + ".csv"
            if os.path.exists(self.path):
                os.remove(self.path)
            self._file = open(self.path, "w", newline="", encoding="utf-8")
            self._csv = csv.writer(self._file)
            self._csv.writerow(names)
            self.db = None
            return
        import sqlite3

        self.path = stem + ".gpkg"
        for p in (self.path, self.path + "-wal", self.path + "-shm", self.path + "-journal"):
            if os.path.exists(p):
                os.remove(p)
        self.db = sqlite3.connect(self.path)
        self.db.execute("PRAGMA journal_mode = OFF")
        self.db.execute("PRAGMA synchronous = OFF")
        self.db.execute(f"PRAGMA application_id = {GPKG_APPLICATION_ID}")
        self.db.execute("PRAGMA user_version = 10400")
        self.srs_id = self._init_gpkg(crs)

    def _init_gpkg(self, crs: CRS | None) -> int:
        db = self.db
        db.execute(
            "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT NOT NULL, srs_id INTEGER NOT NULL PRIMARY KEY, "
            "organization TEXT NOT NULL, organization_coordsys_id INTEGER NOT NULL, definition TEXT NOT NULL, description TEXT)")
        wgs84 = CRS.from_epsg(4326)
        rows = [
            ("Undefined cartesian SRS", -1, "NONE", -1, "undefined", "undefined cartesian coordinate reference system"),
            ("Undefined geographic SRS", 0, "NONE", 0, "undefined", "undefined geographic coordinate reference system"),
            ("WGS 84 geodetic", 4326, "EPSG", 4326, wgs84.to_wkt("WKT1_GDAL"), "longitude/latitude coordinates in decimal degrees on the WGS 84 spheroid"),
        ]
        srs_id = -1
        if crs is not None:
            epsg = crs.to_epsg()
            srs_id = epsg if epsg else 100000
            if srs_id != 4326:
                try:
                    wkt = crs.to_wkt("WKT1_GDAL")
                except Exception:  # noqa: BLE001 - a CRS WKT1 cannot express
                    wkt = crs.to_wkt()
                rows.append((crs.name, srs_id, "EPSG" if epsg else "NONE", epsg or srs_id, wkt, None))
        db.executemany("INSERT INTO gpkg_spatial_ref_sys VALUES (?, ?, ?, ?, ?, ?)", rows)
        db.execute(
            "CREATE TABLE gpkg_contents (table_name TEXT NOT NULL PRIMARY KEY, data_type TEXT NOT NULL, "
            "identifier TEXT UNIQUE, description TEXT DEFAULT '', "
            "last_change DATETIME NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')), "
            "min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER, "
            "CONSTRAINT fk_gc_r_srs_id FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id))")
        db.execute(
            "CREATE TABLE gpkg_geometry_columns (table_name TEXT NOT NULL, column_name TEXT NOT NULL, "
            "geometry_type_name TEXT NOT NULL, srs_id INTEGER NOT NULL, z TINYINT NOT NULL, m TINYINT NOT NULL, "
            "CONSTRAINT pk_geom_cols PRIMARY KEY (table_name, column_name), "
            "CONSTRAINT fk_gc_tn FOREIGN KEY (table_name) REFERENCES gpkg_contents(table_name), "
            "CONSTRAINT fk_gc_srs FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys (srs_id))")
        return srs_id

    def _create(self, name: str, kinds: list[int], geom_type: str) -> None:
        cols = [f"{_qi('fid')} INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL", f"{_qi(self.geom_col)} {geom_type}"]
        cols += [f"{_qi(f)} {_DECLARED[k]}" for f, k in zip(self.fields, kinds)]
        self.db.execute(f"CREATE TABLE {_qi(name)} ({', '.join(cols)})")

    def _blobs(self, shapes) -> list:
        """GeoPackage binary geometries: header, envelope, ISO WKB (with
        Z and M where the geometry has them)."""
        import struct

        import numpy as np

        arr = np.asarray(shapes, dtype=object)
        wkb = shapely.to_wkb(arr, flavor="iso", byte_order=1, output_dimension=4)
        bounds = shapely.bounds(arr)
        empty = shapely.is_empty(arr)
        out = []
        srs = struct.pack("<i", self.srs_id)
        for i, w in enumerate(wkb):
            if w is None:
                out.append(None)
            elif empty[i]:
                out.append(b"GP\x00" + bytes([0x01 | 0x10]) + srs + w)
            else:
                b = bounds[i]
                out.append(b"GP\x00\x03" + srs + struct.pack("<4d", b[0], b[2], b[1], b[3]) + w)
        return out

    def write(self, attr_rows: list[list[Any]], shapes: list | None = None) -> None:
        """One batch: attribute values in field order, and the shapes."""
        if not attr_rows:
            return
        rows = [[_plain(v) for v in r] for r in attr_rows]
        for r in rows:
            for j, v in enumerate(r):
                k = _kind(v)
                if k and k != self.kinds[j]:
                    self.kinds[j] = _widen(self.kinds[j], k)
        if self.db is None:
            self._csv.writerows([["" if v is None else v for v in r] for r in rows])
            self.count += len(rows)
            return
        self.tally.add(shapes)
        if self.declared is None:
            self.declared = list(self.kinds)
            self.declared_geom = self.tally.gpkg_type()
            self._create(self.layer, self.declared, self.declared_geom)
            marks = ", ".join("?" for _ in range(len(self.fields) + 2))
            self._insert = f"INSERT INTO {_qi(self.layer)} VALUES ({marks})"
        blobs = self._blobs(shapes)
        start = self.count + 1
        self.db.executemany(self._insert, ([start + i, blobs[i], *r] for i, r in enumerate(rows)))
        self.count += len(rows)

    def close(self) -> str:
        if self.db is None:
            self._file.close()
            return self.path
        db = self.db
        if self.declared is None:
            self.declared = list(self.kinds)
            self.declared_geom = self.tally.gpkg_type()
            self._create(self.layer, self.declared, self.declared_geom)
        final_geom = self.tally.gpkg_type()
        final = [k if k else d for k, d in zip(self.kinds, self.declared)]
        if [_DECLARED[k] for k in final] != [_DECLARED[k] for k in self.declared] or final_geom != self.declared_geom:
            # A later batch needed a wider type: rebuild the table in SQLite
            # with the final types, converting the values stored earlier.
            tmp = self.layer + "__columbus_rebuild"
            self._create(tmp, final, final_geom)
            sel = [_qi("fid"), _qi(self.geom_col)]
            for f, k in zip(self.fields, final):
                sel.append(f"CAST({_qi(f)} AS {_DECLARED[k]})" if k in (3, 4) else _qi(f))
            db.execute(f"INSERT INTO {_qi(tmp)} SELECT {', '.join(sel)} FROM {_qi(self.layer)}")
            db.execute(f"DROP TABLE {_qi(self.layer)}")
            db.execute(f"ALTER TABLE {_qi(tmp)} RENAME TO {_qi(self.layer)}")
        e = self.tally.extent
        db.execute(
            "INSERT INTO gpkg_contents (table_name, data_type, identifier, min_x, min_y, max_x, max_y, srs_id) "
            "VALUES (?, 'features', ?, ?, ?, ?, ?, ?)",
            (self.layer, self.layer, *(e if e else (None, None, None, None)), self.srs_id))
        solid = self.count - self.tally.null - self.tally.empty
        z = 0 if not self.tally.has_z else (1 if self.tally.has_z == solid else 2)
        m = 0 if not self.tally.has_m else (1 if self.tally.has_m == solid else 2)
        db.execute("INSERT INTO gpkg_geometry_columns VALUES (?, ?, ?, ?, ?, ?)",
                   (self.layer, self.geom_col, final_geom, self.srs_id, z, m))
        db.commit()
        db.close()
        self.db = None
        return self.path

    def abort(self) -> None:
        try:
            if self.db is not None:
                self.db.close()
            elif getattr(self, "_file", None) is not None:
                self._file.close()
        finally:
            if os.path.exists(self.path):
                os.remove(self.path)

    def summary(self) -> dict[str, Any]:
        if self.geom_col is None:
            return {"rows": self.count, "geometry_types": [], "crs": None, "extent": None, "fields": list(self.fields)}
        return {"rows": self.count, "geometry_types": sorted(self.tally.types), "crs": crs_facts(self.crs),
                "extent": self.tally.extent, "fields": list(self.fields)}

    def checks(self, what: str = "The result") -> list[dict]:
        if self.geom_col is None:
            return []
        return crs_checks(self.crs, self.tally.extent, what=what) + self.tally.checks(what)


GPKG_APPLICATION_ID = 0x47504B47  # "GPKG"


def _is_geopackage(path: str) -> bool:
    try:
        with open(path, "rb") as f:
            head = f.read(100)
    except OSError:
        return False
    return (
        len(head) == 100
        and head[:16] == b"SQLite format 3\x00"
        and int.from_bytes(head[68:72], "big") == GPKG_APPLICATION_ID
    )


def read_layer(path: str, check_only: bool = False):
    """Open a workspace layer: a GeoPackage (checked by its header, not its
    name) or a CSV (read as text by pandas, never by GDAL). Anything else is
    refused, so a file whose content is really a VRT, a GDAL "CSV:" or
    "/vsicurl/" path, or another format that can point at other files or
    URLs is never handed to GDAL. ``check_only``: make the checks for a
    GeoPackage and return None, for callers that read it themselves."""
    if not isinstance(path, str) or path.startswith(("/vsi", "\\vsi")) or ":" in os.path.basename(path):
        raise GISError("input.format", "input must be a file in the workspace")
    if path.lower().endswith(".gpkg"):
        if not _is_geopackage(path):
            raise GISError("input.format", f"{os.path.basename(path)} is not a GeoPackage")
        if check_only:
            return None
        return _read_gpkg(path)
    if path.lower().endswith((".tif", ".tiff")):
        raise GISError("input.format", f"{os.path.basename(path)} is a raster; vector tools take GeoPackage or CSV layers (use raster_op for rasters)")
    if not path.lower().endswith(".csv"):
        raise GISError("input.format", "inputs must be GeoPackage (.gpkg) or CSV layers")
    if path.lower().endswith(".csv"):
        # Keys stay text: "0042" must not become 42.
        return pd.read_csv(path, dtype=str, keep_default_na=False)


def layer_summary(df) -> dict[str, Any]:
    if isinstance(df, gpd.GeoDataFrame):
        gs = df.geometry
        present = gs[~gs.isna() & ~gs.is_empty]
        extent = [float(v) for v in present.total_bounds] if len(present) else None
        return {
            "rows": int(len(df)),
            "geometry_types": sorted(t for t in present.geom_type.unique() if t),
            "crs": crs_facts(df.crs),
            "extent": extent,
            "fields": [c for c in df.columns if c != gs.name],
        }
    return {"rows": int(len(df)), "geometry_types": [], "crs": None, "extent": None, "fields": list(df.columns)}


def _layer_checks(df, what: str) -> list[dict]:
    if not isinstance(df, gpd.GeoDataFrame):
        return []
    s = layer_summary(df)
    return crs_checks(df.crs, s["extent"], what=what) + geometry_checks(df.geometry, what=what)


def verify_layer(path: str) -> dict[str, Any]:
    df = read_layer(path)
    checks = _layer_checks(df, "The layer")
    if isinstance(df, gpd.GeoDataFrame):
        dup = int(df.geometry.dropna().to_wkb().duplicated().sum()) if len(df) else 0
        if dup:
            checks.append(check("geometry.duplicates", "warn", f"{dup} geometries are exact duplicates of another row's",
                                fix="Check for double-loaded features before counting or summing"))
    return {"summary": layer_summary(df), "checks": checks}


def _row_value(v: Any) -> Any:
    if v is None:
        return None
    try:
        if pd.isna(v):
            return None
    except (TypeError, ValueError):
        pass
    if isinstance(v, (bool, int, float, str)):
        return v
    if hasattr(v, "item"):  # numpy scalars
        try:
            return v.item()
        except (TypeError, ValueError):
            pass
    return str(v)


def read_rows(path: str, offset: int = 0, limit: int = 50, fields: list[str] | None = None) -> dict[str, Any]:
    """Rows of a workspace layer, a page at a time. Geometry comes back as its
    type and bounding box, not coordinates. A large GeoPackage is read a page
    at a time, never whole."""
    if path.lower().endswith(".gpkg"):
        read_layer(path, check_only=True)
        info = _info(path, force_total_bounds=True)
        if int(info.get("features") or 0) > LARGE_LAYER:
            return _read_rows_paged(path, info, offset, limit, fields)
    df = read_layer(path)
    geo = isinstance(df, gpd.GeoDataFrame)
    gname = df.geometry.name if geo else None
    cols = [c for c in df.columns if c != gname]
    if fields:
        missing = [f for f in fields if f not in cols]
        if missing:
            raise GISError("params", f"no field(s) {', '.join(missing)} in the layer")
        cols = list(fields)
    page = df.iloc[offset: offset + limit]
    rows = []
    for _, r in page.iterrows():
        row = [_row_value(r[c]) for c in cols]
        if geo:
            g = r[gname]
            row.append(None if g is None or g.is_empty else {"type": g.geom_type, "bbox": [float(x) for x in g.bounds]})
        rows.append(row)
    return {"columns": cols + ([gname] if geo else []), "rows": rows, "total": int(len(df)),
            "offset": offset, "summary": layer_summary(df)}


LARGE_LAYER = 100_000


def _read_rows_paged(path: str, info: dict, offset: int, limit: int, fields: list[str] | None) -> dict[str, Any]:
    gname = "geometry"  # as geopandas names it when reading
    cols = [str(c) for c in info.get("fields", [])]
    if fields:
        missing = [f for f in fields if f not in cols]
        if missing:
            raise GISError("params", f"no field(s) {', '.join(missing)} in the layer")
        cols = list(fields)
    total = int(info.get("features") or 0)
    rows = []
    if offset < total:
        page = _read_2d(path, skip_features=offset, max_features=limit, columns=cols)
        for _, r in page.iterrows():
            row = [_row_value(r[c]) for c in cols]
            geom = r[page.geometry.name]
            row.append(None if geom is None or geom.is_empty else {"type": geom.geom_type, "bbox": [float(x) for x in geom.bounds]})
            rows.append(row)
    crs = CRS.from_user_input(info["crs"]) if info.get("crs") else None
    bounds = info.get("total_bounds")
    summary = {
        "rows": total,
        "geometry_types": [info["geometry_type"]] if info.get("geometry_type") not in (None, "Unknown") else [],
        "crs": crs_facts(crs),
        "extent": [float(v) for v in bounds] if bounds and all(math.isfinite(v) for v in bounds) else None,
        "fields": [str(c) for c in info.get("fields", [])],
    }
    return {"columns": cols + [gname], "rows": rows, "total": total, "offset": offset, "summary": summary}


def display_geojson(path: str, max_features: int = 20000, max_coordinates: int = 750_000) -> dict[str, Any]:
    """A workspace layer as GeoJSON in EPSG:4326 for the person's map (the
    companion window), cut off at ``max_features`` features or
    ``max_coordinates`` coordinates, whichever comes first. Attributes are
    plain JSON values. A layer without a CRS cannot be placed and says so."""
    import shapely
    from shapely.geometry import mapping

    if isinstance(path, str) and path.lower().endswith(".gpkg"):
        # Read only what is shown: a large layer is never loaded whole.
        import pyogrio

        read_layer(path, check_only=True)
        total = int(_info(path).get("features") or 0)
        df = _read_2d(path, max_features=max_features)
    else:
        df = read_layer(path)
        total = int(len(df))
    if not isinstance(df, gpd.GeoDataFrame):
        return {"type": "FeatureCollection", "features": [], "total": total, "shown": 0,
                "truncated": False, "geometry": False}
    if df.crs is None:
        raise GISError("crs.present", "the layer has no coordinate reference system, so it cannot be placed on the map")
    gname = df.geometry.name
    sub = df.iloc[:max_features]
    counts = shapely.get_num_coordinates(sub.geometry.values)
    budget = 0
    keep = 0
    for c in counts:
        if keep and budget + int(c) > max_coordinates:
            break
        budget += int(c)
        keep += 1
    sub = sub.iloc[:keep]
    if not (df.crs.to_epsg() == 4326):
        sub = sub.to_crs(4326)
    cols = [c for c in sub.columns if c != gname]
    feats = []
    for i, (_, r) in enumerate(sub.iterrows()):
        g = r[gname]
        feats.append({
            "type": "Feature", "id": i,
            "geometry": None if g is None or g.is_empty else mapping(g),
            "properties": {c: _row_value(r[c]) for c in cols},
        })
    present = sub.geometry[~sub.geometry.isna() & ~sub.geometry.is_empty]
    bbox = [float(v) for v in present.total_bounds] if len(present) else None
    return {"type": "FeatureCollection", "features": feats, "total": total, "shown": len(feats),
            "truncated": len(feats) < total, "geometry": True,
            "geometry_type": layer_summary(df).get("geometry_type"), "bbox": bbox,
            "source_crs": df.crs.to_string()}


def geojson_pages_to_layer(pages: list[str], out_stem: str, max_features: int) -> dict[str, Any]:
    """Build a layer from downloaded GeoJSON pages, parsed as JSON (never
    handed to GDAL, which picks drivers by content). Coordinates are
    EPSG:4326, as GeoJSON requires."""
    import json as _json
    from shapely.geometry import shape

    rows: list[dict[str, Any]] = []
    geoms = []
    skipped = 0
    truncated = False
    for path in pages:
        with open(path, "rb") as f:
            try:
                doc = _json.load(f)
            except ValueError as e:
                raise GISError("fetch.format", f"a downloaded page is not JSON: {e}") from None
        feats = doc.get("features") if isinstance(doc, dict) else None
        if not isinstance(feats, list):
            raise GISError("fetch.format", "a downloaded page is not a GeoJSON FeatureCollection")
        for feat in feats:
            if len(rows) >= max_features:
                truncated = True
                break
            if not isinstance(feat, dict):
                skipped += 1
                continue
            g = feat.get("geometry")
            try:
                geom = shape(g) if isinstance(g, dict) else None
            except Exception:  # noqa: BLE001 - malformed geometry
                skipped += 1
                continue
            props = feat.get("properties") if isinstance(feat.get("properties"), dict) else {}
            row = {}
            for k, v in props.items():
                key = str(k)[:63]
                row[key] = v if isinstance(v, (str, int, float, bool)) or v is None else _json.dumps(v)[:10000]
            rows.append(row)
            geoms.append(geom)
        if truncated:
            break
    df = gpd.GeoDataFrame(pd.DataFrame(rows), geometry=geoms, crs="EPSG:4326")
    checks = []
    if skipped:
        checks.append(check("fetch.skipped", "warn", f"{skipped} downloaded features were malformed and left out"))
    if truncated:
        checks.append(check("fetch.truncated", "warn", f"Stopped at {max_features} features; the source has more",
                            fix="Narrow the bbox, or raise the limit within the read limit"))
    if df.empty:
        checks.append(check("fetch.empty", "warn", "The download returned no features",
                            fix="Check the bbox (lon/lat) and the layer"))
    path = write_layer(df, out_stem)
    checks.extend(_layer_checks(df, "The downloaded layer"))
    return {"output_path": path, "summary": layer_summary(df), "checks": checks, "truncated": truncated}


def _esrijson_page(path: str):
    """One ArcGIS query page (``f=json``) as a (Geo)DataFrame, read with
    GDAL's ESRIJSON driver only (the ``ESRIJSON:`` prefix: GDAL picks no
    other driver by content). Read through Arrow, so Z and M values are
    kept (AF-17)."""
    import warnings

    with warnings.catch_warnings():
        warnings.filterwarnings("ignore", r"Measured \(M\) geometry types are not supported", UserWarning)
        return gpd.read_file("ESRIJSON:" + path, engine="pyogrio", use_arrow=True)


def esrijson_pages_to_layer(pages: list[str], out_stem: str | None, layer: str, fields: list[str],
                            geometry: bool, return_rows: int) -> dict[str, Any]:
    """Build a workspace layer from an ``arcgis`` connection's query pages,
    which the gateway fetched and trimmed to the fields it allows. Only
    ``fields`` are kept, in that order, plus the geometry when ``geometry``.
    M values are written with ``write_ogr`` (ISO WKB). Without ``out_stem``
    nothing is written (sample_rows): only the first rows come back."""
    frames = []
    crs = None
    for path in pages:
        try:
            df = _esrijson_page(path)
        except Exception as e:  # noqa: BLE001 - GDAL's reason, plainly
            raise GISError("arcgis.format", f"an ArcGIS page could not be read as Esri JSON: {e}") from None
        if isinstance(df, gpd.GeoDataFrame) and df.crs is not None and crs is None:
            crs = df.crs
        frames.append(df)
    frames = [f for f in frames if len(f)] or frames[:1]
    if frames:
        df = pd.concat(frames, ignore_index=True) if len(frames) > 1 else frames[0]
    else:
        df = pd.DataFrame()
    for f in fields:
        if f not in df.columns:
            df[f] = None
    gname = df.geometry.name if isinstance(df, gpd.GeoDataFrame) and geometry else None
    keep = list(fields) + ([gname] if gname else [])
    df = df[[c for c in keep if c in df.columns]]
    if geometry:
        if gname is None:
            df = gpd.GeoDataFrame(df, geometry=gpd.GeoSeries([None] * len(df)), crs=crs)
        else:
            df = gpd.GeoDataFrame(df, geometry=gname, crs=df.crs or crs)
    else:
        df = pd.DataFrame(df)
    shown = []
    for i in range(min(return_rows, len(df))):
        row = df.iloc[i]
        vals = [_row_value(row[c]) for c in fields]
        if geometry:
            geom = row[df.geometry.name]
            if geom is None or (not isinstance(geom, shapely.Geometry)):
                vals.append(None)
            else:
                vals.append({"geometry": geom.geom_type + (" M" if shapely.has_m(geom) else ""),
                             "bounds": [round(float(v), 6) for v in geom.bounds] if not geom.is_empty else None})
        shown.append(vals)
    path = write_layer(df, out_stem, layer=layer) if out_stem else None
    what = "The result"
    checks = _layer_checks(df, what) if isinstance(df, gpd.GeoDataFrame) else []
    return {
        "columns": list(fields) + (["geometry"] if geometry else []),
        "rows": shown,
        "rows_returned": len(shown),
        "output_path": path,
        "summary": layer_summary(df),
        "checks": checks,
    }


# Delivery format names (as a context pack writes them) -> (driver, extension).
EXPORT_FORMATS = {
    "gpkg": ("GPKG", ".gpkg"), "geopackage": ("GPKG", ".gpkg"),
    "geojson": ("GeoJSON", ".geojson"),
    "fgb": ("FlatGeobuf", ".fgb"), "flatgeobuf": ("FlatGeobuf", ".fgb"),
    "shp": ("ESRI Shapefile", ".shp"), "shapefile": ("ESRI Shapefile", ".shp"),
    "csv": ("CSV", ".csv"),
    # R-8: one new file geodatabase with one feature class (OpenFileGDB).
    "gdb": ("OpenFileGDB", ".gdb"), "filegdb": ("OpenFileGDB", ".gdb"), "fgdb": ("OpenFileGDB", ".gdb"),
    "file geodatabase": ("OpenFileGDB", ".gdb"),
    # R-9: raster results only, as the workspace's Cloud-Optimized GeoTIFF.
    "tif": ("COG", ".tif"), "tiff": ("COG", ".tif"), "geotiff": ("COG", ".tif"), "cog": ("COG", ".tif"),
}

# A file geodatabase feature class name: a letter, then letters, digits and
# underscores (OpenFileGDB would otherwise change it, and R-8 names the
# layer after the export).
FGDB_LAYER_NAME = re.compile(r"[A-Za-z][A-Za-z0-9_]{0,159}")
FGDB_MAX_FIELD = 64
INT32_MIN, INT32_MAX = -(2**31), 2**31 - 1


def _gdb_int_columns(df, gname: str | None) -> tuple[Any, list[str]]:
    """Integer columns for a file geodatabase. Without
    TARGET_ARCGIS_VERSION=ARCGIS_PRO_3_2_OR_LATER, OpenFileGDB writes every
    64-bit integer field as a double, so columns whose values all fit in 32
    bits are written as 32-bit integers (readable by every ArcGIS), and the
    names of those that do not are returned: they need the Pro 3.2 option to
    stay exact."""
    big: list[str] = []
    out = df
    for c in df.columns:
        if c == gname or not pd.api.types.is_integer_dtype(df[c]) or pd.api.types.is_bool_dtype(df[c]):
            continue
        vals = df[c].dropna()
        if len(vals) and (int(vals.min()) < INT32_MIN or int(vals.max()) > INT32_MAX):
            big.append(str(c))
            continue
        if out is df:
            out = df.copy()
        nullable = isinstance(df[c].dtype, pd.api.extensions.ExtensionDtype)
        out[c] = df[c].astype("Int32" if nullable else "int32")
    return out, big


def _write_gdb(df, out: str, layer: str, gname: str, checks: list[dict]) -> None:
    if not FGDB_LAYER_NAME.fullmatch(layer):
        raise GISError("export.format",
                       f"A file geodatabase export names its feature class after the export, and {layer!r} is not a valid "
                       "feature class name (a letter, then letters, digits and _, at most 160 characters)",
                       fix="Choose a name like crash_hotspots")
    long = [str(c) for c in df.columns if c != gname and len(str(c)) > FGDB_MAX_FIELD]
    if long:
        raise GISError("export.format",
                       f"File geodatabase field names are at most {FGDB_MAX_FIELD} characters; these are longer: {', '.join(long)}",
                       fix="Rename them in the delivery spec, or deliver GeoPackage")
    if os.path.lexists(out):
        # Never written into: OpenFileGDB would add a layer to it.
        raise GISError("export.exists", f"{os.path.basename(out)} already exists; an existing file geodatabase is never changed",
                       fix="Choose another name")
    df, big = _gdb_int_columns(df, gname)
    options: dict[str, str] = {}
    if big:
        options["TARGET_ARCGIS_VERSION"] = "ARCGIS_PRO_3_2_OR_LATER"
        checks.append(check("export.needs_pro_3_2", "info",
                            f"{', '.join(big)} hold(s) whole numbers beyond the 32-bit range, kept exact as 64-bit integers; "
                            "ArcGIS Pro 3.2 or later is needed to open this file geodatabase",
                            detail={"fields": big},
                            fix="If the recipient has older ArcGIS, deliver GeoPackage, or store these values as text"))
    import warnings  # noqa: PLC0415

    try:
        # GDAL's notes on renamed fields become the check below.
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", RuntimeWarning)
            write_ogr(df, out, "OpenFileGDB", layer=layer, layer_options=options or None)
    except Exception as e:  # noqa: BLE001 - GDAL's own message says why
        raise GISError("export.format", f"could not write the file geodatabase ({e})") from None
    info = _info(out, layer=layer)
    with warnings.catch_warnings():
        warnings.filterwarnings("ignore", r"Measured \(M\) geometry types are not supported", UserWarning)
        names = pyogrio.list_layers(out)[:, 0].tolist()
    if names != [layer] or int(info["features"]) != len(df):
        raise GISError("export.format", "the file geodatabase did not come out as one feature class with every row")
    wrote = [str(f) for f in info["fields"]]
    wanted = [str(c) for c in df.columns if c != gname]
    types = dict(zip(wrote, info["ogr_types"]))
    if any(types.get(dict(zip(wanted, wrote)).get(c, c)) != "OFTInteger64" for c in big):
        # An older GDAL (before 3.9) ignores the option and writes doubles.
        raise GISError("export.format",
                       f"this GDAL ({pyogrio.__gdal_version_string__}) cannot keep 64-bit integers exact in a file geodatabase "
                       f"({', '.join(big)})", fix="Update the engine's pyogrio, or deliver GeoPackage")
    if wrote != wanted:
        changed = [f"{a} to {b}" for a, b in zip(wanted, wrote) if a != b]
        checks.append(check("export.field_names", "warn",
                            "The file geodatabase changed field names it does not allow: " + ", ".join(changed),
                            detail={"fields": dict(zip(wanted, wrote))},
                            fix="Rename the fields in the delivery spec (letters, digits and _) if the recipient expects the original names"))


def export_layer(path: str, out_stem: str, fmt: str, crs: str | None = None,
                 rename: dict[str, str] | None = None) -> dict[str, Any]:
    """Write a workspace layer in a delivery format, reprojected and with
    fields renamed as asked. Returns the files written and checks."""
    key = str(fmt).strip().lower()
    if key not in EXPORT_FORMATS:
        raise GISError("params", f"format must be one of {', '.join(sorted(set(EXPORT_FORMATS)))}")
    driver, ext = EXPORT_FORMATS[key]
    raster = isinstance(path, str) and path.lower().endswith((".tif", ".tiff"))
    if driver == "COG":
        if not raster:
            raise GISError("export.format", f"{fmt} is for raster results; this is a vector layer or table",
                           fix="Export it as gpkg, gdb, geojson, fgb, shp or csv")
        import columbus_raster  # noqa: PLC0415 - it imports this module

        return columbus_raster.export_tif(path, out_stem, crs, rename)
    if raster:
        raise GISError("export.format", f"a raster result exports as tif only, not {fmt}",
                       fix="Export it with format tif")
    df = read_layer(path)
    checks: list[dict] = []
    geo = isinstance(df, gpd.GeoDataFrame)
    if crs:
        if not isinstance(crs, str) or not EPSG_CODE.fullmatch(crs.strip()):
            raise GISError("params", "crs must be an EPSG code such as \"EPSG:3081\"")
        if not geo:
            raise GISError("export.crs", f"the layer has no geometry, so it cannot be delivered in {crs}")
        if df.crs is None:
            raise GISError("crs.present", "the layer has no coordinate reference system, so it cannot be reprojected for delivery")
        target = CRS.from_user_input(crs.strip().upper())
        if not df.crs.equals(target):
            checks.append(check("export.reprojected", "info", f"Reprojected from {_label(df.crs)} to {_label(target)} for delivery"))
            df = to_crs(df, target)
    gname = df.geometry.name if geo else None
    if rename:
        missing = [k for k in rename if k not in df.columns]
        present = {k: v for k, v in rename.items() if k in df.columns}
        if missing:
            checks.append(check("export.rename_missing", "warn",
                                f"The delivery spec renames {', '.join(missing)}, which the layer does not have",
                                fix="Check the field names against the spec; the layer may come from a different dataset"))
        if gname in present:
            raise GISError("export.rename", f"the geometry column {gname} cannot be renamed")
        # GeoPackage and dBase field names ignore case, so clashes do too.
        after = [present.get(c, c) for c in df.columns]
        seen: dict[str, str] = {}
        for name in after:
            low = str(name).lower()
            if low in seen:
                raise GISError("export.rename", f"renaming would give two fields the name {name} (field names ignore case)")
            seen[low] = name
        df = df.rename(columns=present)
        if present:
            checks.append(check("export.renamed", "info", "Renamed " + ", ".join(f"{k} to {v}" for k, v in present.items())))
    if key in ("shp", "shapefile"):
        # dBase field names are at most 10 bytes in the file's encoding (UTF-8).
        long = [c for c in df.columns if c != gname and len(str(c).encode("utf-8")) > 10]
        if long:
            raise GISError("export.format", f"Shapefile field names are at most 10 bytes; {', '.join(map(str, long))} would be cut",
                           fix="Rename them in the delivery spec, or deliver GeoPackage")
        wide = [c for c in df.columns if c != gname and df[c].dtype == object
                and df[c].dropna().astype(str).map(lambda v: len(v.encode("utf-8"))).max(initial=0) > 254]
        if wide:
            raise GISError("export.format", f"Shapefile text fields hold at most 254 bytes; {', '.join(map(str, wide))} has longer values",
                           fix="Deliver GeoPackage")
    os.makedirs(os.path.dirname(out_stem) or ".", exist_ok=True)
    out = out_stem + ext
    if driver == "CSV":
        frame = pd.DataFrame(df.drop(columns=df.geometry.name)) if geo else pd.DataFrame(df)
        if geo:
            if any(str(c).lower() == "wkt" for c in frame.columns):
                raise GISError("export.rename", "the layer already has a field named wkt, where a CSV export puts the geometry",
                               fix="Rename that field in the delivery spec")
            # Full precision: shapely's default rounds to 6 decimals.
            frame["wkt"] = df.geometry.to_wkt(rounding_precision=-1)
        frame.to_csv(out, index=False)
    elif driver == "OpenFileGDB":
        if not geo:
            raise GISError("export.format", f"{fmt} needs geometry; this layer has none (deliver it as csv)")
        _write_gdb(df, out, os.path.basename(out_stem), gname, checks)
    else:
        if not geo:
            raise GISError("export.format", f"{fmt} needs geometry; this layer has none (deliver it as csv)")
        write_ogr(_fid_safe(df), out, driver)
    files = [out]
    if driver == "ESRI Shapefile":
        base = out_stem
        files = [base + e for e in (".shp", ".shx", ".dbf", ".prj", ".cpg") if os.path.exists(base + e)]
    checks.extend(_layer_checks(df, "The delivered layer") if geo else [])
    return {"files": files, "summary": layer_summary(df), "checks": checks}


# ---------------------------------------------------------------------------
# Vector operations
# ---------------------------------------------------------------------------

UNITS = {
    "m": 1.0, "metre": 1.0, "meter": 1.0, "metres": 1.0, "meters": 1.0,
    "km": 1000.0, "ft": 0.3048, "feet": 0.3048, "us-ft": 1200 / 3937,
    "mi": 1609.344, "mile": 1609.344, "miles": 1609.344, "us-mi": 1609.3472186944,
}


def _need_geo(df, name: str) -> gpd.GeoDataFrame:
    if not isinstance(df, gpd.GeoDataFrame):
        raise GISError("input.geometry", f"input {name!r} has no geometry")
    if df.crs is None:
        raise GISError("crs.present", f"input {name!r} has no coordinate reference system",
                       fix="Assign the source's CRS before processing")
    return df


def _distance(params: dict, key: str, df: gpd.GeoDataFrame, what: str) -> float:
    """A distance in the layer's own units, refusing degrees."""
    try:
        value = float(params[key])
    except (KeyError, TypeError, ValueError) as e:
        raise GISError("params", f"{key} must be a number") from e
    unit = str(params.get("unit") or "").lower()
    if unit not in UNITS:
        raise GISError("params", f"unit must be one of {', '.join(sorted(set(UNITS)))}",
                       fix="State the unit explicitly, e.g. \"unit\": \"ft\"")
    crs = df.crs
    if crs.is_geographic:
        raise GISError("crs.measure",
                       f"cannot {what} by {value:g} {unit}: the layer is in {_label(crs)}, whose units are degrees",
                       fix=f"Reproject first to a projected CRS suited to the area ({suggest_crs(df)}), then run it again")
    if _pseudo_mercator(crs):
        raise GISError("crs.measure",
                       f"cannot {what} by {value:g} {unit} in {_label(crs)}: Web Mercator distances are stretched "
                       "away from the equator (about 15% at Austin, more further north)",
                       fix=f"Reproject first to a projected CRS suited to the area ({suggest_crs(df)}), then run it again")
    per_unit = crs.axis_info[0].unit_conversion_factor
    return value * UNITS[unit] / per_unit


# Texas's outline, coarsely (lon, lat): enough to tell Texas from Oklahoma,
# Louisiana or New Mexico for a CRS suggestion, not for analysis.
_TEXAS = shapely.Polygon([
    (-106.65, 31.90), (-106.53, 32.00), (-103.06, 32.00), (-103.04, 36.50), (-100.00, 36.50),
    (-100.00, 34.56), (-99.20, 34.40), (-97.95, 33.88), (-96.60, 33.85), (-95.30, 33.88),
    (-94.48, 33.64), (-94.04, 33.55), (-94.04, 31.99), (-93.80, 31.50), (-93.50, 30.40),
    (-93.84, 29.69), (-94.80, 29.25), (-96.20, 28.45), (-97.20, 27.60), (-97.14, 25.96),
    (-97.60, 25.90), (-99.10, 26.43), (-99.50, 27.50), (-100.40, 28.30), (-101.40, 29.77),
    (-102.40, 29.80), (-103.00, 29.00), (-104.50, 29.60), (-104.70, 30.20), (-106.00, 31.40),
])
_GENERIC = "a State Plane, UTM or equal-area CRS for the data's area"


def suggest_crs(df: gpd.GeoDataFrame) -> str:
    """A projected CRS for measuring in the layer's area: the statewide
    Texas systems inside Texas, the continental US Albers for data spanning
    several states, otherwise the UTM zone of its centre (NAD83 in the US and
    Canada, WGS 84 elsewhere). Generic advice when the extent is not a
    plausible longitude and latitude (feet read as degrees, swapped axes) or
    spans too much for one zone."""
    try:
        b = df.to_crs(4326).total_bounds if not df.crs.equals(CRS.from_epsg(4326)) else df.total_bounds
    except Exception:  # noqa: BLE001
        return _GENERIC
    if not all(math.isfinite(v) for v in b) or b[0] < -180 or b[2] > 180 or b[1] < -90 or b[3] > 90:
        return _GENERIC
    lon, lat = (b[0] + b[2]) / 2, (b[1] + b[3]) / 2
    width = b[2] - b[0]
    tx = _TEXAS.bounds
    in_tx_box = b[0] >= tx[0] - 0.1 and b[1] >= tx[1] - 0.1 and b[2] <= tx[2] + 0.1 and b[3] <= tx[3] + 0.1
    if in_tx_box and _TEXAS.contains(shapely.Point(lon, lat)):
        return ("for Texas, EPSG:3081 NAD83 / Texas State Mapping System for distances statewide, "
                "EPSG:3083 Texas Centric Albers for areas, or the local State Plane zone")
    us48 = (-125.0, 24.0, -66.5, 49.5)
    if width > 6:
        if b[0] >= us48[0] and b[1] >= us48[1] and b[2] <= us48[2] and b[3] <= us48[3]:
            return "EPSG:5070 NAD83 / Conus Albers (equal-area) for data across several states"
        return _GENERIC
    zone = min(60, max(1, int((lon + 180) // 6) + 1))
    # NAD83 UTM codes exist for zones 1 to 23 north; use them in the US and Canada.
    north_america = lat >= 14 and -172 <= lon <= -50 and not (lat < 32 and lon > -80)
    if lat >= 0 and north_america and 1 <= zone <= 23:
        return f"UTM zone {zone}N on NAD83, EPSG:{26900 + zone}"
    return f"UTM zone {zone}{'N' if lat >= 0 else 'S'}, EPSG:{(32600 if lat >= 0 else 32700) + zone}"


def _pseudo_mercator(crs) -> bool:
    if crs.to_epsg() in (3857, 900913, 3785, 102100, 102113):
        return True
    try:
        method = crs.coordinate_operation.method_name.lower() if crs.coordinate_operation else ""
    except Exception:  # noqa: BLE001
        method = ""
    return "pseudo mercator" in method or "popular visualisation" in method


def _align(base: gpd.GeoDataFrame, other: gpd.GeoDataFrame, name: str, checks: list) -> gpd.GeoDataFrame:
    if other.crs.equals(base.crs):
        return other
    checks.append(check("crs.agreement", "info", f"Reprojected {name} from {_label(other.crs)} to {_label(base.crs)} to match"))
    return other.to_crs(base.crs)


def _join_checks(left: pd.DataFrame, right: pd.DataFrame, lk: str, rk: str, out_rows: int, label: str | None = None) -> list[dict]:
    """`lk` and `rk` are the columns compared; `label` names the left key in messages."""
    out = []
    name = label or lk
    lkeys = left[lk]
    rkeys = right[rk]
    n = len(left)
    usable = _usable_keys(lkeys)
    matched = usable & lkeys.isin(set(rkeys[_usable_keys(rkeys)]))
    unmatched = int((~matched).sum())
    empty = int((~usable).sum())
    if n:
        pct = 100.0 * unmatched / n
        msg = f"{unmatched} of {n} rows ({pct:.1f}%) had no match on {name}"
        sev = "info" if unmatched == 0 else ("fail" if pct >= 40 else "warn")
        if empty:
            msg += f"; {empty} of those have an empty {name}, which never matches"
        c = check("join.unmatched", sev, msg, detail={"left": n, "matched": n - unmatched, "empty_keys": empty})
        if unmatched:
            lstr = lkeys[usable].astype(str)
            rstr = set(rkeys.dropna().astype(str))
            stripped = {s.lstrip("0") for s in rstr}
            fixable = int(lstr[~lstr.isin(rstr)].map(lambda s: s.lstrip("0") in stripped).astype(bool).sum())
            norm = lambda v: v.strip().casefold()  # noqa: E731
            rnorm = {norm(x) for x in rstr}
            loose = int(lstr[~lstr.isin(rstr)].map(lambda v: norm(v) in rnorm).astype(bool).sum())
            if fixable:
                width = int(max(lstr.str.len().max(), rkeys.dropna().astype(str).str.len().max()))
                c["fix"] = (f"{fixable} of them match once leading zeros are ignored: join again with "
                            f"\"key_as_text\": true, \"pad\": {width}")
            elif loose:
                c["fix"] = (f"{loose} of them match once spaces and letter case are ignored: join again with "
                            "\"trim\": true, \"ignore_case\": true")
        out.append(c)
    dup = int(rkeys.dropna().duplicated().sum())
    if out_rows > n:
        out.append(check("join.many_to_many", "warn",
                         f"The join produced {out_rows} rows from {n}: {dup} key values repeat on the right side",
                         detail={"left": n, "out": out_rows},
                         fix="Deduplicate or aggregate the right side first, or confirm one-to-many is intended"))
    return out


def _key_text(keys: pd.Series) -> pd.Series:
    """Keys as text: whole numbers without a trailing ".0", nulls kept null."""
    def one(v):
        try:
            if pd.isna(v):  # None, NaN, NA, NaT
                return None
        except (TypeError, ValueError):  # arrays and other non-scalars
            pass
        if isinstance(v, float) and v.is_integer():
            return str(int(v))
        return str(v)
    return keys.map(one).astype(object)


def _normalize_keys(keys: pd.Series, params: dict) -> pd.Series:
    """Apply the join's key options: key_as_text, trim, ignore_case, pad."""
    out = _key_text(keys)
    if params.get("trim"):
        out = out.map(lambda v: v.strip() if isinstance(v, str) else v)
    if params.get("ignore_case"):
        out = out.map(lambda v: v.casefold() if isinstance(v, str) else v)
    pad = params.get("pad")
    if pad is not None:
        out = out.map(lambda v: v.zfill(pad) if isinstance(v, str) and v.isdigit() else v)
    return out


def _key_options(params: dict) -> bool:
    for k in ("key_as_text", "trim", "ignore_case"):
        if k in params and not isinstance(params[k], bool):
            raise GISError("params", f"{k} must be true or false")
    pad = params.get("pad")
    if pad is not None and (isinstance(pad, bool) or not isinstance(pad, int) or not 1 <= pad <= 64):
        raise GISError("params", "pad must be a whole number of characters from 1 to 64")
    return any(params.get(k) for k in ("key_as_text", "trim", "ignore_case")) or pad is not None


def _usable_keys(keys: pd.Series) -> pd.Series:
    """Keys that can match: not null and not empty text. pandas would match
    null to null (and, after a CSV round trip, "" to "")."""
    return keys.notna() & (keys.astype(str).str.strip() != "")


def _out_limit(params: dict) -> int:
    try:
        return max(1, int(params.get("_max_rows") or 200_000))
    except (TypeError, ValueError):
        return 200_000


def _count_pairs(left, right, predicate: str, limit: int, what: str, extra: int = 0) -> None:
    """Count matching pairs a chunk at a time, refusing as soon as the total
    passes the limit, so the count itself never holds a huge result."""
    total = extra
    tree = right.sindex
    geoms = left.geometry.to_numpy()
    for i in range(0, len(geoms), 5000):
        total += tree.query(geoms[i : i + 5000], predicate=predicate).shape[1]
        _refuse_large(total, limit, what)


def _refuse_large(rows: int, limit: int, what: str) -> None:
    if rows > limit:
        raise GISError("output.too_large", f"{what} would produce {rows} rows, more than the read limit of {limit}",
                       fix="Narrow the inputs, or deduplicate or aggregate the right side first")


def _op_reproject(inp, params, checks):
    df = _need_geo(inp["layer"], "layer")
    target = params.get("crs")
    if not isinstance(target, str) or not EPSG_CODE.fullmatch(target.strip()):
        raise GISError("params", "crs must be an EPSG code such as \"EPSG:3081\"")
    out = to_crs(df, target.strip().upper())
    checks.append(check("crs.reprojected", "info", f"Reprojected from {_label(df.crs)} to {_label(out.crs)}"))
    return out


def _op_buffer(inp, params, checks):
    df = _need_geo(inp["layer"], "layer")
    d = _distance(params, "distance", df, "buffer")
    out = df.copy()
    try:
        segs = int(params.get("segments", 8))
    except (TypeError, ValueError) as e:
        raise GISError("params", "segments must be a whole number from 1 to 64") from e
    if not 1 <= segs <= 64:
        raise GISError("params", "segments must be a whole number from 1 to 64")
    out.geometry = df.geometry.buffer(d, quad_segs=segs)
    if params.get("dissolve"):
        # One shape for everything: no single feature's attributes apply.
        geom = shapely.union_all(out.geometry.to_numpy())
        out = gpd.GeoDataFrame({"feature_count": [len(df)]}, geometry=[geom], crs=df.crs)
    checks.append(check("crs.measure", "info", f"Buffered by {params['distance']} {params['unit']} in {_label(df.crs)}"))
    return out


def _op_simplify(inp, params, checks):
    df = _need_geo(inp["layer"], "layer")
    tol = _distance(params, "tolerance", df, "simplify")
    out = df.copy()
    out.geometry = df.geometry.simplify(tol, preserve_topology=bool(params.get("preserve_topology", True)))
    checks.append(check("crs.measure", "info", f"Simplified with a tolerance of {params['tolerance']} {params['unit']} in {_label(df.crs)}"))
    return out


def _op_make_valid(inp, params, checks):
    df = _need_geo(inp["layer"], "layer")
    before = int((~df.geometry.is_valid & df.geometry.notna()).sum())
    out = df.copy()
    out.geometry = shapely.make_valid(df.geometry.to_numpy())
    checks.append(check("geometry.repaired", "info" if before == 0 else "warn", f"Repaired {before} invalid geometries"))
    return out


def _op_clip(inp, params, checks):
    df = _need_geo(inp["layer"], "layer")
    mask = _align(df, _need_geo(inp["mask"], "mask"), "mask", checks)
    out = gpd.clip(df, mask).reset_index(drop=True)
    checks.append(check("clip.rows", "info", f"{len(out)} of {len(df)} features fall within the mask"))
    return out


def _op_dissolve(inp, params, checks):
    df = _need_geo(inp["layer"], "layer")
    by = params.get("by")
    if by is not None:
        by = [by] if isinstance(by, str) else list(by)
        missing = [f for f in by if f not in df.columns]
        if missing:
            raise GISError("params", f"no field(s) {', '.join(missing)} in the layer")
    agg = df.assign(feature_count=1)
    if by:
        nulls = int(df[by].isna().any(axis=1).sum())
        if nulls:
            checks.append(check("dissolve.null_groups", "warn",
                                f"{nulls} features have an empty {', '.join(by)}; they are dissolved together into one group with an empty value",
                                detail={"features": nulls},
                                fix="Fill or filter the empty values first if they should not be grouped together"))
    out = agg.dissolve(by=by, aggfunc={"feature_count": "sum"} if by else "first", dropna=False).reset_index()
    if not by:
        out = out[[out.geometry.name]].assign(feature_count=len(df))
    return out


def _op_intersect(inp, params, checks):
    a = _need_geo(inp["a"], "a")
    b = _align(a, _need_geo(inp["b"], "b"), "b", checks)
    _count_pairs(a, b, "intersects", _out_limit(params), "intersect")
    return gpd.overlay(a, b, how="intersection", keep_geom_type=True)


def _op_join(inp, params, checks):
    left = inp["left"]
    right = inp["right"]
    lk = params.get("left_key")
    rk = params.get("right_key", lk)
    how = params.get("how", "left")
    if how not in ("left", "inner"):
        raise GISError("params", "how must be \"left\" or \"inner\"")
    for df, k, side in ((left, lk, "left"), (right, rk, "right")):
        if k not in df.columns:
            raise GISError("params", f"no field {k!r} in the {side} input")
    rdf = pd.DataFrame(right.drop(columns=right.geometry.name) if isinstance(right, gpd.GeoDataFrame) else right)
    ldf = pd.DataFrame(left)
    # Compared on copies of the keys, so the output keeps the source's own
    # values whatever normalization the join used.
    LK, RK = "__columbus_left_key", "__columbus_right_key"
    if _key_options(params):
        ldf[LK] = _normalize_keys(ldf[lk], params)
        rdf[RK] = _normalize_keys(rdf[rk], params)
        checks.append(check("join.keys_normalized", "info",
                            f"Compared {lk} and {rk} as text"
                            + (", trimmed" if params.get("trim") else "")
                            + (", ignoring case" if params.get("ignore_case") else "")
                            + (f", zero-padded to {params['pad']} characters" if params.get("pad") is not None else "")
                            + "; the output keeps the original values"))
    else:
        ldf[LK] = ldf[lk]
        rdf[RK] = rdf[rk]

        def kind(series):
            vals = series[_usable_keys(series)]
            if vals.empty:
                return None  # nothing to compare: no type to disagree with
            if pd.api.types.is_bool_dtype(series):
                return "true/false"
            if pd.api.types.is_numeric_dtype(series):
                return "number"
            return "text"

        lkind, rkind = kind(ldf[lk]), kind(rdf[rk])
        if lkind and rkind and lkind != rkind:
            raise GISError("join.key_types", f"{lk} is {lkind} and {rk} is {rkind}, so no rows would match",
                           fix="Pass \"key_as_text\": true to join both as text (add \"pad\": N if codes lost their leading zeros)")
    rdf = rdf[_usable_keys(rdf[RK])]
    usable = _usable_keys(ldf[LK])
    # Rows out = for each left key, the number of right rows with it (at least
    # one for a left join). Refuse before building something huge.
    counts = rdf[RK].value_counts()
    per_left = ldf.loc[usable, LK].map(counts).fillna(0)
    estimate = int(per_left.clip(lower=1 if how == "left" else 0).sum()) + (int((~usable).sum()) if how == "left" else 0)
    _refuse_large(estimate, _out_limit(params), "join")
    rjoin = rdf.drop(columns=[rk]) if rk == lk else rdf
    merged = ldf[usable].merge(rjoin, how=how, left_on=LK, right_on=RK, suffixes=("", "_right"))
    if how == "left" and not usable.all():
        merged = pd.concat([merged, ldf[~usable]], ignore_index=False).sort_index(kind="stable")
    merged = merged.drop(columns=[c for c in (LK, RK) if c in merged.columns]).reset_index(drop=True)
    checks.extend(_join_checks(ldf, rdf, LK, RK, len(merged), label=lk))
    if isinstance(left, gpd.GeoDataFrame):
        return gpd.GeoDataFrame(merged, geometry=left.geometry.name, crs=left.crs)
    return merged


def _op_spatial_join(inp, params, checks):
    left = _need_geo(inp["left"], "left")
    right = _align(left, _need_geo(inp["right"], "right"), "right", checks)
    pred = params.get("predicate", "intersects")
    if pred not in ("intersects", "within", "contains", "touches", "crosses", "overlaps"):
        raise GISError("params", "predicate must be intersects, within, contains, touches, crosses or overlaps")
    how = params.get("how", "left")
    if how not in ("left", "inner"):
        raise GISError("params", "how must be \"left\" or \"inner\"")
    _count_pairs(left, right, pred, _out_limit(params), "spatial_join")
    out = gpd.sjoin(left, right, how=how, predicate=pred, lsuffix="left", rsuffix="right")
    # Keep the left side's own field names; only clashing right fields get "_right".
    out = out.rename(columns={c: c[: -len("_left")] for c in out.columns if c.endswith("_left") and c[: -len("_left")] not in out.columns})
    unmatched = int(out["index_right"].isna().sum()) if how == "left" else len(left) - out.index.nunique()
    n = len(left)
    pct = 100.0 * unmatched / n if n else 0.0
    checks.append(check("join.unmatched", "info" if unmatched == 0 else "warn",
                        f"{unmatched} of {n} features ({pct:.1f}%) had no match ({pred})", detail={"left": n, "matched": n - unmatched}))
    if len(out) > n:
        checks.append(check("join.many_to_many", "warn", f"The spatial join produced {len(out)} rows from {n}: some features match more than one",
                            fix="Aggregate the right side first, or use predicate \"within\" if each feature should match one"))
    return out.drop(columns=["index_right"]).reset_index(drop=True)


# name: (handler, input names, summary)
OPS: dict[str, tuple[Any, list[str], str]] = {
    "reproject": (_op_reproject, ["layer"], "params: crs (\"EPSG:…\")"),
    "buffer": (_op_buffer, ["layer"], "params: distance, unit (m, ft, us-ft, km, mi), dissolve?, segments?"),
    "simplify": (_op_simplify, ["layer"], "params: tolerance, unit, preserve_topology?"),
    "make_valid": (_op_make_valid, ["layer"], "no params"),
    "clip": (_op_clip, ["layer", "mask"], "no params"),
    "dissolve": (_op_dissolve, ["layer"], "params: by (field or list)?"),
    "intersect": (_op_intersect, ["a", "b"], "no params"),
    "join": (_op_join, ["left", "right"], "params: left_key, right_key?, how (left|inner), key_as_text?, trim?, ignore_case?, pad?"),
    "spatial_join": (_op_spatial_join, ["left", "right"], "params: predicate, how (left|inner)"),
}


def vector_op(op: str, inputs: dict[str, str], params: dict[str, Any], stem: str) -> dict[str, Any]:
    if op not in OPS:
        raise GISError("params", f"unknown op {op!r}; one of {', '.join(OPS)}")
    fn, names, _ = OPS[op]
    missing = [n for n in names if n not in inputs]
    extra = [n for n in inputs if n not in names]
    if missing or extra:
        raise GISError("params", f"{op} takes inputs {', '.join(names)}")
    loaded = {n: read_layer(p) for n, p in inputs.items()}
    checks: list[dict] = []
    for n, df in loaded.items():
        if isinstance(df, gpd.GeoDataFrame):
            checks.extend(c for c in geometry_checks(df.geometry, what=f"Input {n}") if c["id"] == "geometry.valid")
    out = fn(loaded, params or {}, checks)
    dropped = m_dropped_check(list(loaded.values()), out, op)
    if dropped:
        checks.append(dropped)
    path = write_layer(out, stem, layer=op)
    checks.extend(_layer_checks(out, "The output"))
    in_extent = [layer_summary(d)["extent"] for d in loaded.values() if isinstance(d, gpd.GeoDataFrame)]
    summary = layer_summary(out)
    if op != "reproject" and summary["extent"] and in_extent and all(e for e in in_extent):
        try:
            boxes = [list(Transformer.from_crs(loaded[n].crs, out.crs, always_xy=True).transform_bounds(*layer_summary(loaded[n])["extent"]))
                     for n in names if isinstance(loaded[n], gpd.GeoDataFrame)]
            if boxes and not any(_overlaps(summary["extent"], b, margin=0) for b in boxes):
                checks.append(check("extent.plausible", "warn", "The output does not overlap any input's extent",
                                    fix="Check the inputs' CRSs and units"))
        except Exception:  # noqa: BLE001
            pass
    return {"output_path": path, "summary": summary, "checks": checks, "engine": versions()}


# ---------- Map snapshots: a picture of layers for the AI to look at ----------

SNAPSHOT_MAX_FEATURES = 2_000_000
SNAPSHOT_PAPER = (246, 243, 236)
SNAPSHOT_FRAME = (40, 52, 70)
# Distinct on the paper background and to most colour-blind eyes.
SNAPSHOT_COLORS = [(31, 119, 180), (214, 39, 40), (44, 160, 44), (148, 103, 189),
                   (255, 127, 14), (23, 190, 207), (140, 86, 75), (227, 119, 194)]


def _wrap_antimeridian(gdf):
    """Longitudes west of 0 moved east by 360, for a layer that crosses the
    180th meridian (the Aleutians), so it is drawn in one piece."""
    return gdf.set_geometry(shapely.transform(gdf.geometry.values, lambda xy: xy + [[360.0, 0.0]] * (xy[:, :1] < 0)))


SNAPSHOT_IMAGERY_MAX_CELLS = 4_000_000


def _snapshot_imagery(path: str, crs, transform, shape) -> tuple[Any, Any, dict[str, Any]]:
    """A raster of the session's, warped onto the picture's grid: an RGB
    image and where it has data. Three or more 8-bit bands are drawn as
    red, green, blue; anything else as grey, stretched between its 2nd and
    98th percentiles. Opened as a GeoTIFF only (never a format that brings
    in other files), and read at about the picture's own resolution (from
    overviews where it has them), never cell by cell at full size."""
    import numpy as np
    import rasterio
    from rasterio.enums import Resampling
    from rasterio.warp import reproject, transform_bounds
    from rasterio.windows import from_bounds as window_from_bounds

    import columbus_raster as cr

    if not cr.is_tiff(path):
        raise GISError("input.format", f"{os.path.basename(path)} is not a GeoTIFF; imagery must be one")
    h, w = shape
    left, top = transform * (0, 0)
    right, bottom = transform * (w, h)
    with cr.open_raster(path) as src:
        if src.crs is None:
            raise GISError("params", f"{os.path.basename(path)} has no CRS, so it cannot be placed under the layers")
        rgb = src.count >= 3 and all(d == "uint8" for d in src.dtypes[:3])
        bands = [1, 2, 3] if rgb else [1]
        info = {"bands": "red, green, blue" if rgb else "one band, grey", "covers": False}
        try:
            b = transform_bounds(crs, src.crs, left, bottom, right, top, densify_pts=21)
            win = window_from_bounds(*b, transform=src.transform).intersection(
                rasterio.windows.Window(0, 0, src.width, src.height))
        except Exception:  # noqa: BLE001 - no overlap, or outside the CRS's area
            return np.zeros(shape + (3,), dtype=np.float32), np.zeros(shape, dtype=bool), info
        win = win.round_offsets().round_lengths()
        if win.width < 1 or win.height < 1:
            return np.zeros(shape + (3,), dtype=np.float32), np.zeros(shape, dtype=bool), info
        # About twice the picture's cells, at most: GDAL reads overviews for this.
        k = max(1.0, win.width / (2 * w), win.height / (2 * h),
                math.sqrt(win.width * win.height / SNAPSHOT_IMAGERY_MAX_CELLS))
        rw, rh = max(1, int(win.width / k)), max(1, int(win.height / k))
        data = src.read(bands, window=win, out_shape=(len(bands), rh, rw), masked=True,
                        resampling=Resampling.bilinear if k > 1 else Resampling.nearest)
        src_t = src.window_transform(win) * rasterio.Affine.scale(win.width / rw, win.height / rh)
        # A cell every band masks (nodata, an alpha band, an internal mask)
        # is no data; a band masked alone (a 0 in (200, 0, 0) with nodata
        # 0) is a 0.
        arr = np.ma.filled(data.astype(np.float32), 0.0)
        arr[:, np.ma.getmaskarray(data).all(axis=0)] = np.nan
        out = np.full((len(bands),) + shape, np.nan, dtype=np.float32)
        for i in range(len(bands)):
            reproject(source=arr[i], destination=out[i], src_transform=src_t, src_crs=src.crs,
                      src_nodata=np.nan, dst_transform=transform, dst_crs=crs, dst_nodata=np.nan,
                      resampling=Resampling.bilinear)
    have = ~np.isnan(out).any(axis=0)
    info["covers"] = bool(have.any())
    if not have.any():
        return np.zeros(shape + (3,), dtype=np.float32), have, info
    if rgb:
        img = np.moveaxis(np.nan_to_num(out), 0, -1)
    else:
        v = out[0]
        lo, hi = np.nanpercentile(v[have], [2, 98])
        g = np.clip((np.nan_to_num(v) - lo) / ((hi - lo) or 1.0), 0, 1) * 255
        img = np.repeat(g[:, :, None], 3, axis=2)
        info["stretch"] = [float(lo), float(hi)]
    return img.astype(np.float32), have, info


# Most labels drawn in one snapshot, and longest label text.
SNAPSHOT_MAX_LABELS = 400
SNAPSHOT_LABEL_CHARS = 40
_LABEL_HIDDEN = re.compile(
    "[\x00-\x1f\x7f-\x9f\xad\u034f\u061c\u115f\u1160\u17b4\u17b5\u180b-\u180f"
    "\u200b-\u200f\u2028-\u202e\u2060-\u206f\u3164\ufe00-\ufe0f\ufeff\uffa0\ufff9-\ufffb"
    "\U000e0000-\U000e0fff]"
)


def _label_text(v: Any) -> str | None:
    """A value as a short label: plain characters only; None when empty."""
    if v is None or (isinstance(v, float) and math.isnan(v)):
        return None
    try:
        if pd.isna(v):
            return None
    except (TypeError, ValueError):
        pass
    t = _LABEL_HIDDEN.sub("", str(v)[: SNAPSHOT_LABEL_CHARS * 4]).strip()
    if not t:
        return None
    return t if len(t) <= SNAPSHOT_LABEL_CHARS else t[: SNAPSHOT_LABEL_CHARS - 1] + "…"


def _label_points(geoms):
    """Where each feature's label goes: inside a polygon, halfway along a
    line, on a point."""
    out = []
    for g in geoms:
        if g is None or g.is_empty:
            out.append(None)
            continue
        kind = g.geom_type
        try:
            if "Polygon" in kind:
                p = g.representative_point()
            elif "Line" in kind:
                p = g.interpolate(0.5, normalized=True) if kind != "MultiLineString" else max(g.geoms, key=lambda x: x.length).interpolate(0.5, normalized=True)
            elif kind == "Point":
                p = g
            else:
                p = g.representative_point()
        except Exception:  # noqa: BLE001 - an invalid geometry gets no label
            p = None
        out.append(p)
    return out


def _draw_labels(data, placed: list[tuple[float, float, str, int]], frame_color) -> tuple[Any, dict[int, int]]:
    """Draw labels (pixel x, y, text, layer) on an RGB array: dark text with
    a pale halo, each left out where it would overlap one already drawn or
    leave the picture. The new array and how many each layer got."""
    from PIL import Image, ImageDraw, ImageFont

    img = Image.fromarray(data, "RGB")
    draw = ImageDraw.Draw(img)
    try:
        font = ImageFont.load_default(size=13)
    except TypeError:  # Pillow before 10.1
        font = ImageFont.load_default()
    h, w = data.shape[0], data.shape[1]
    taken = []
    drawn: dict[int, int] = {}
    for x, y, text, layer in placed:
        if sum(drawn.values()) >= SNAPSHOT_MAX_LABELS:
            break
        l, t, r, b = draw.textbbox((0, 0), text, font=font, stroke_width=2)
        bw, bh = r - l, b - t
        x0 = int(round(x - bw / 2))
        y0 = int(round(y - bh / 2)) - 8  # above a point's dot
        box = (x0, y0, x0 + bw, y0 + bh)
        if box[0] < 1 or box[1] < 1 or box[2] > w - 2 or box[3] > h - 2:
            continue
        if any(not (box[2] < o[0] or box[0] > o[2] or box[3] < o[1] or box[1] > o[3]) for o in taken):
            continue
        taken.append(box)
        draw.text((x0 - l, y0 - t), text, font=font, fill=tuple(int(c) for c in frame_color),
                  stroke_width=2, stroke_fill=(255, 255, 255))
        drawn[layer] = drawn.get(layer, 0) + 1
    import numpy as np
    return np.asarray(img), drawn


def map_snapshot(paths: list[str], out_path: str, width: int = 1000, height: int = 700,
                 bbox: list[float] | None = None, imagery: str | None = None,
                 labels: list[str | None] | None = None) -> dict[str, Any]:
    """Draw layers as a PNG, each layer its own colour, polygons filled
    lightly with an outline, lines and points solid, on a plain background
    with no basemap. Geometry only, unless `labels` names a field for a
    layer (one per path, or None): then features are labelled with that
    field's values (plain text, cut to 40 characters, at most 400 in all,
    none overlapping). Layers are drawn in the first layer's CRS; a
    geographic one is drawn with its longitude scaled for its latitude, in
    one piece across the 180th meridian."""
    import numpy as np
    import rasterio
    from rasterio.features import rasterize
    from rasterio.transform import from_bounds

    if not paths or len(paths) > 8:
        raise GISError("params", "give 1 to 8 layers")
    width = max(200, min(1600, int(width)))
    height = max(150, min(1200, int(height)))
    labels = list(labels or [None] * len(paths))
    if len(labels) != len(paths):
        raise GISError("params", "labels: one field (or none) per layer")
    frames, total = [], 0
    for p, field in zip(paths, labels):
        df = read_layer(p)
        if not isinstance(df, gpd.GeoDataFrame):
            raise GISError("params", f"{os.path.basename(p)} has no geometry to draw")
        total += len(df)
        if total > SNAPSHOT_MAX_FEATURES:
            raise GISError("params", f"more than {SNAPSHOT_MAX_FEATURES:,} features in all; draw fewer or smaller layers")
        g = gpd.GeoDataFrame(geometry=df.geometry, crs=df.crs)
        if field is not None:
            if field not in df.columns or field == df.geometry.name:
                raise GISError("params", f"{os.path.basename(p)} has no field {field!r} to label with")
            g["__label"] = df[field].map(_label_text)
        g = g[g.geometry.notna() & ~g.geometry.is_empty]
        frames.append(g)
    if not frames:
        raise GISError("params", "give 1 to 8 layers")
    crs = next((f.crs for f in frames if f.crs is not None), None)
    if crs is not None:
        frames = [f.to_crs(crs) if f.crs is not None and f.crs != crs else f for f in frames]
    geographic = bool(crs is not None and crs.is_geographic)
    if geographic:
        lons = [f.total_bounds for f in frames if len(f)]
        if lons and max(b[2] for b in lons) - min(b[0] for b in lons) > 180:
            frames = [_wrap_antimeridian(f) for f in frames]
    if bbox is not None:
        if len(bbox) != 4 or not (bbox[0] < bbox[2] and bbox[1] < bbox[3]):
            raise GISError("params", "bbox is [minx, miny, maxx, maxy] in the first layer's CRS")
        minx, miny, maxx, maxy = (float(v) for v in bbox)
    else:
        bounds = [f.total_bounds for f in frames if len(f)]
        if not bounds:
            raise GISError("params", "the layers have no geometry to draw")
        minx = min(b[0] for b in bounds)
        miny = min(b[1] for b in bounds)
        maxx = max(b[2] for b in bounds)
        maxy = max(b[3] for b in bounds)
        dx, dy = maxx - minx, maxy - miny
        pad = max(dx, dy) * 0.05 or 1.0
        minx, miny, maxx, maxy = minx - pad, miny - pad, maxx + pad, maxy + pad
    # The picture's shape follows the extent's (on the ground, for degrees).
    xscale = math.cos(math.radians(max(-80.0, min(80.0, (miny + maxy) / 2)))) if geographic else 1.0
    ground_w, ground_h = (maxx - minx) * xscale, maxy - miny
    if ground_w / ground_h > width / height:
        height = max(150, int(round(width * ground_h / ground_w)))
    else:
        width = max(200, int(round(height * ground_w / ground_h)))
    transform = from_bounds(minx, miny, maxx, maxy, width, height)
    px, py = (maxx - minx) / width, (maxy - miny) / height
    img = np.empty((height, width, 3), dtype=np.float32)
    img[:] = SNAPSHOT_PAPER
    shape = (height, width)
    imagery_info = None
    if imagery:
        if crs is None:
            raise GISError("params", "the layers have no CRS, so imagery cannot be placed under them")
        if geographic and maxx > 180:
            raise GISError("params", "imagery cannot be drawn across the 180th meridian; leave it out or draw one side")
        under, have, imagery_info = _snapshot_imagery(imagery, crs, transform, shape)
        img[have] = under[have]

    def burn(geoms, all_touched=True):
        geoms = [g for g in geoms if g is not None and not g.is_empty]
        if not geoms:
            return np.zeros(shape, dtype=bool)
        return rasterize(geoms, out_shape=shape, transform=transform, fill=0, default_value=1,
                         all_touched=all_touched, dtype="uint8").astype(bool)

    def thicken(m):
        out = m.copy()
        out[1:, :] |= m[:-1, :]
        out[:-1, :] |= m[1:, :]
        out[:, 1:] |= m[:, :-1]
        out[:, :-1] |= m[:, 1:]
        return out

    layers = []
    for i, f in enumerate(frames):
        color = np.array(SNAPSHOT_COLORS[i % len(SNAPSHOT_COLORS)], dtype=np.float32)
        types = f.geometry.geom_type
        polys = f.geometry[types.isin(["Polygon", "MultiPolygon"])]
        lines = f.geometry[types.isin(["LineString", "MultiLineString", "LinearRing"])]
        points = f.geometry[types.isin(["Point", "MultiPoint"])]
        others = f.geometry[types == "GeometryCollection"]
        for c in others:
            for part in getattr(c, "geoms", []):
                kind = part.geom_type
                if "Polygon" in kind:
                    polys = pd.concat([polys, gpd.GeoSeries([part], crs=f.crs)])
                elif "Line" in kind:
                    lines = pd.concat([lines, gpd.GeoSeries([part], crs=f.crs)])
                elif "Point" in kind:
                    points = pd.concat([points, gpd.GeoSeries([part], crs=f.crs)])
        if len(polys):
            fill = burn(list(polys.values), all_touched=False)
            img[fill] = img[fill] * 0.7 + color * 0.3
            edge = burn(list(polys.boundary.values))
            img[edge] = color
        if len(lines):
            m = thicken(burn(list(lines.values)))
            img[m] = color
        if len(points):
            # Straight to pixels (fast for many points), a dot 7 pixels
            # across with a dark rim.
            xy = shapely.get_coordinates(points.values)
            cols = np.floor((xy[:, 0] - minx) / px).astype(np.int64)
            rows = np.floor((maxy - xy[:, 1]) / py).astype(np.int64)
            keep = (cols >= 0) & (cols < width) & (rows >= 0) & (rows < height)
            centres = np.zeros(shape, dtype=bool)
            centres[rows[keep], cols[keep]] = True
            def disc(m, r):
                out = np.zeros_like(m)
                rr, cc = np.nonzero(m)
                for dy in range(-r, r + 1):
                    for dx in range(-r, r + 1):
                        if dx * dx + dy * dy <= r * r:
                            y, x = rr + dy, cc + dx
                            ok = (y >= 0) & (y < height) & (x >= 0) & (x < width)
                            out[y[ok], x[ok]] = True
                return out
            dots = disc(centres, 3)
            img[disc(centres, 4) & ~dots] = SNAPSHOT_FRAME
            img[dots] = color
        layers.append({"features": int(len(f)), "geometry_types": sorted(set(types)),
                       "color": "#%02x%02x%02x" % tuple(int(v) for v in color)})
    img[0, :] = img[-1, :] = SNAPSHOT_FRAME
    img[:, 0] = img[:, -1] = SNAPSHOT_FRAME
    data = np.clip(img, 0, 255).astype(np.uint8)
    # Labels last, over everything, in layer order, up to the cap.
    placed = []
    for i, f in enumerate(frames):
        layers[i]["labels"] = 0
        if "__label" not in f.columns:
            continue
        for pt, text in zip(_label_points(f.geometry.values), f["__label"].values):
            if pt is None or text is None or len(placed) >= SNAPSHOT_MAX_LABELS * 4:
                continue
            x, y = (pt.x - minx) / px, (maxy - pt.y) / py
            if 0 <= x < width and 0 <= y < height:
                placed.append((x, y, text, i))
    if placed:
        try:
            import PIL  # noqa: F401
        except ImportError:
            raise GISError("unsupported", "labels need the Pillow package in Columbus's engine; run the engine setup again to add it") from None
        data, counts = _draw_labels(data, placed, SNAPSHOT_FRAME)
        for i, n in counts.items():
            layers[i]["labels"] = n
    import warnings
    with warnings.catch_warnings(), rasterio.Env(GDAL_PAM_ENABLED="NO"):
        warnings.simplefilter("ignore", rasterio.errors.NotGeoreferencedWarning)
        with rasterio.open(out_path, "w", driver="PNG", width=width, height=height, count=3, dtype="uint8") as dst:
            for b in range(3):
                dst.write(data[:, :, b], b + 1)
    return {"path": out_path, "width": int(width), "height": int(height),
            "crs": (crs.to_string() if crs is not None else None),
            "extent": [float(minx), float(miny), float(maxx), float(maxy)],
            "wrapped_antimeridian": bool(geographic and maxx > 180),
            "imagery": imagery_info,
            "layers": layers}
