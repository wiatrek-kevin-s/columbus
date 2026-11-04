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
_SKIP = {"VRT", "OGR_VRT", "GDALG"}
_skip_now = {d for d in os.environ.get("GDAL_SKIP", "").replace(",", " ").split() if d}
os.environ["GDAL_SKIP"] = " ".join(sorted(_SKIP | _skip_now))
os.environ["PROJ_NETWORK"] = "OFF"
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
                         f"{what} is labelled {_label(crs)} (degrees), but its coordinates reach {extent}, which cannot be longitude and latitude in that order",
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
    if not _overlaps(ll, area, margin=1.0):
        swapped = [extent[1], extent[0], extent[3], extent[2]]
        fix = "Check the CRS is right; if the data was labelled with the wrong CRS, assign the right one rather than reprojecting"
        if crs.is_geographic and _overlaps(swapped, area, margin=1.0):
            fix = "Longitude and latitude look swapped; swap X and Y"
        out.append(check("crs.area_of_use", "warn",
                         f"{what}'s coordinates fall outside the area {_label(crs)} is meant for ({aou.name})",
                         detail={"extent_lonlat": [round(v, 4) for v in ll], "area_of_use": area}, fix=fix))
    return out


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
    out = []
    n = len(gs)
    if n == 0:
        return out
    null = gs.isna().to_numpy()
    arr = gs.to_numpy()
    present = ~null
    empty = present & shapely.is_empty(arr)
    valid = shapely.is_valid(arr)
    invalid = present & ~empty & ~valid
    if invalid.any():
        k = int(invalid.sum())
        reasons = [shapely.is_valid_reason(g) for g in arr[invalid][:3]]
        out.append(check("geometry.valid", "warn", f"{k} of {n} geometries are invalid",
                         detail={"invalid": k, "examples": reasons},
                         fix="Repair them with vector_op make_valid before overlay, clip or dissolve"))
    if null.any() or empty.any():
        out.append(check("geometry.empty", "info", f"{int(null.sum())} rows have no geometry and {int(empty.sum())} have an empty one",
                         detail={"null": int(null.sum()), "empty": int(empty.sum())}))
    families = {_family(t) for t in gs[present & ~empty].geom_type.unique()} - {None}
    if len(families) > 1:
        out.append(check("geometry.mixed_types", "warn", f"{what} mixes geometry types: {', '.join(sorted(families))}",
                         fix="Split it by geometry type before exporting to formats that allow one type per layer"))
    return out


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
        _fid_safe(df).to_file(path, layer=layer, driver="GPKG", engine="pyogrio")
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


def read_layer(path: str):
    """Open a workspace layer: a GeoPackage (checked by its header, not its
    name) or a CSV (read as text by pandas, never by GDAL). Anything else is
    refused, so a file whose content is really a VRT, a GDAL "CSV:" or
    "/vsicurl/" path, or another format that can point at other files or
    URLs is never handed to GDAL."""
    if not isinstance(path, str) or path.startswith(("/vsi", "\\vsi")) or ":" in os.path.basename(path):
        raise GISError("input.format", "input must be a file in the workspace")
    if path.lower().endswith(".gpkg"):
        if not _is_geopackage(path):
            raise GISError("input.format", f"{os.path.basename(path)} is not a GeoPackage")
        return gpd.read_file(path, engine="pyogrio")
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


# Texas, in lon/lat: EPSG:3081 and EPSG:3083 cover the whole state.
_TEXAS = (-106.65, 25.84, -93.51, 36.50)


def suggest_crs(df: gpd.GeoDataFrame) -> str:
    """A projected CRS for measuring in the layer's area: the statewide
    Texas systems inside Texas, otherwise the UTM zone of its centre (NAD83
    in North America, WGS 84 elsewhere)."""
    try:
        b = df.to_crs(4326).total_bounds if not df.crs.equals(CRS.from_epsg(4326)) else df.total_bounds
    except Exception:  # noqa: BLE001
        return "a UTM zone or State Plane zone for the area"
    if not all(math.isfinite(v) for v in b):
        return "a UTM zone or State Plane zone for the area"
    lon, lat = (b[0] + b[2]) / 2, (b[1] + b[3]) / 2
    if b[0] >= _TEXAS[0] and b[1] >= _TEXAS[1] and b[2] <= _TEXAS[2] and b[3] <= _TEXAS[3]:
        return ("for Texas, EPSG:3081 NAD83 / Texas State Mapping System for distances statewide, "
                "EPSG:3083 Texas Centric Albers for areas, or the local State Plane zone")
    zone = min(60, max(1, int((lon + 180) // 6) + 1))
    if lat >= 0 and -172 <= lon <= -48 and 1 <= zone <= 23:
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


def _join_checks(left: pd.DataFrame, right: pd.DataFrame, lk: str, rk: str, out_rows: int) -> list[dict]:
    out = []
    lkeys = left[lk]
    rkeys = right[rk]
    n = len(left)
    usable = _usable_keys(lkeys)
    matched = usable & lkeys.isin(set(rkeys[_usable_keys(rkeys)]))
    unmatched = int((~matched).sum())
    empty = int((~usable).sum())
    if n:
        pct = 100.0 * unmatched / n
        msg = f"{unmatched} of {n} rows ({pct:.1f}%) had no match on {lk}"
        sev = "info" if unmatched == 0 else ("fail" if pct >= 40 else "warn")
        if empty:
            msg += f"; {empty} of those have an empty {lk}, which never matches"
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
        if v is None or (isinstance(v, float) and math.isnan(v)) or v is pd.NA:
            return None
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
    out = df.to_crs(target.strip().upper())
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
    if _key_options(params):
        # The key fields come out as the normalized text they were joined on.
        ldf[lk] = _normalize_keys(ldf[lk], params)
        rdf[rk] = _normalize_keys(rdf[rk], params)
        checks.append(check("join.keys_normalized", "info",
                            f"Joined on {lk} and {rk} as text"
                            + (", trimmed" if params.get("trim") else "")
                            + (", ignoring case" if params.get("ignore_case") else "")
                            + (f", zero-padded to {params['pad']} characters" if params.get("pad") is not None else "")))
    else:
        lnum = pd.api.types.is_numeric_dtype(ldf[lk]) and not pd.api.types.is_bool_dtype(ldf[lk])
        rnum = pd.api.types.is_numeric_dtype(rdf[rk]) and not pd.api.types.is_bool_dtype(rdf[rk])
        if lnum != rnum:
            num, text = (lk, rk) if lnum else (rk, lk)
            raise GISError("join.key_types", f"{num} is a number and {text} is text, so no rows would match",
                           fix="Pass \"key_as_text\": true to join both as text (add \"pad\": N if codes lost their leading zeros)")
    rdf = rdf[_usable_keys(rdf[rk])]
    usable = _usable_keys(ldf[lk])
    # Rows out = for each left key, the number of right rows with it (at least
    # one for a left join). Refuse before building something huge.
    counts = rdf[rk].value_counts()
    per_left = ldf.loc[usable, lk].map(counts).fillna(0)
    estimate = int(per_left.clip(lower=1 if how == "left" else 0).sum()) + (int((~usable).sum()) if how == "left" else 0)
    _refuse_large(estimate, _out_limit(params), "join")
    merged = ldf[usable].merge(rdf, how=how, left_on=lk, right_on=rk, suffixes=("", "_right"))
    if how == "left" and not usable.all():
        merged = pd.concat([merged, ldf[~usable]], ignore_index=False).sort_index(kind="stable")
    merged = merged.reset_index(drop=True)
    checks.extend(_join_checks(ldf, rdf, lk, rk, len(merged)))
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
