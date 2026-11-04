"""Raster engine for the Columbus core worker (``rasters`` connections and
``raster_op``).

Built on rasterio (GDAL) and numpy. ``columbus_gis`` is imported first: it
switches off the GDAL drivers whose files can point at other files or URLs
(VRT and the like) and PROJ's network access before any GDAL library loads.
On top of that, every raster is checked by its first bytes before rasterio
sees it and opened with the GTiff driver only, so a ".tif" holding VRT XML,
or any other format, never reaches GDAL's format detection.

Outputs are Cloud-Optimized GeoTIFFs (GDAL's COG driver, DEFLATE). Every
operation returns ``checks`` in the verification report format, like
``columbus_gis``.
"""

from __future__ import annotations

import math
import os
import re
from typing import Any

import columbus_gis as g  # noqa: I001 - sets GDAL_SKIP and PROJ_NETWORK before rasterio loads GDAL

import numpy as np  # noqa: E402
import rasterio  # noqa: E402
import shapely  # noqa: E402
from pyproj import CRS  # noqa: E402
from rasterio.crs import CRS as RioCRS  # noqa: E402
from rasterio.enums import Resampling  # noqa: E402
from rasterio.features import geometry_mask  # noqa: E402
from rasterio.shutil import copy as rio_copy  # noqa: E402
from rasterio.transform import Affine, from_origin  # noqa: E402
from rasterio.warp import reproject as warp_reproject, transform_bounds  # noqa: E402
from rasterio.windows import Window  # noqa: E402

GISError = g.GISError
check = g.check

# Classic TIFF and BigTIFF, little- and big-endian.
TIFF_MAGIC = (b"II*\x00", b"MM\x00*", b"II+\x00", b"MM\x00+")
RASTER_EXTENSIONS = (".tif", ".tiff")
# The most pixels any raster_op output may have (the gateway may pass less).
MAX_PIXELS = 400_000_000
# info: statistics on every cell up to this many; above it, on an overview
# or a sample of about SAMPLE_PIXELS.
FULL_STATS_PIXELS = 16_000_000
SAMPLE_PIXELS = 4_000_000
# Rows processed at a time by the terrain operations.
STRIP_ROWS = 1024

RESAMPLING = {
    "nearest": Resampling.nearest,
    "bilinear": Resampling.bilinear,
    "cubic": Resampling.cubic,
    "average": Resampling.average,
    "mode": Resampling.mode,
}
BLENDING = {"bilinear", "cubic", "average"}
Z_UNITS = {"m": "m", "metre": "m", "meter": "m", "metres": "m", "meters": "m",
           "ft": "ft", "feet": "ft", "us-ft": "us-ft"}
ZONAL_STATS = ("count", "mean", "min", "max", "sum", "std")
DEFAULT_STATS = ["count", "mean", "min", "max"]

__all__ = ["is_tiff", "open_raster", "describe_raster", "raster_op", "OPS"]


def versions() -> dict[str, str]:
    return {**g.versions(), "rasterio": rasterio.__version__, "rasterio_gdal": rasterio.__gdal_version__}


# ---------------------------------------------------------------------------
# Opening files
# ---------------------------------------------------------------------------

# "GTIFF_DIR:1:x.tif", "HDF5:x:y" and the like: a driver prefix with options.
_PREFIXED = re.compile(r"^[A-Za-z0-9_]{2,}:")


def is_tiff(path: str) -> bool:
    try:
        with open(path, "rb") as f:
            return f.read(4) in TIFF_MAGIC
    except OSError:
        return False


def _check_path(path: Any) -> str:
    """A local GeoTIFF file, refused before GDAL sees it otherwise."""
    if not isinstance(path, str) or not path or "\x00" in path:
        raise GISError("input.format", "a raster input must be a file")
    low = path.replace("\\", "/").lower()
    base = os.path.basename(path)
    if low.startswith("/vsi") or "://" in low or _PREFIXED.match(path) or ":" in base or not os.path.isabs(path):
        raise GISError("input.format", "a raster input must be a local file, without driver prefixes, /vsi paths or open options")
    if not base.lower().endswith(RASTER_EXTENSIONS):
        raise GISError("input.format", f"{base} is not a GeoTIFF (.tif or .tiff)")
    if not os.path.isfile(path):
        raise GISError("input.format", f"{base} does not exist")
    if not is_tiff(path):
        raise GISError("input.format", f"{base} is named like a GeoTIFF but its content is not TIFF, so it was not opened",
                       fix="Ask the data owner for the real GeoTIFF")
    return path


def open_raster(path: str):
    """Open a GeoTIFF read-only with the GTiff driver only."""
    _check_path(path)
    try:
        ds = rasterio.open(path, "r", driver="GTiff")
    except rasterio.errors.RasterioIOError as e:
        raise GISError("input.format", f"{os.path.basename(path)} could not be read as a GeoTIFF ({e})") from None
    # Nothing but the file itself: no external overviews, masks or sidecars.
    try:
        files = [os.path.realpath(f) for f in (ds.files or [])]
    except Exception:  # noqa: BLE001
        files = []
    if files and files != [os.path.realpath(path)]:
        ds.close()
        raise GISError("input.format", f"{os.path.basename(path)} brings in other files ({', '.join(os.path.basename(f) for f in files[1:])}), which Columbus does not read")
    return ds


def _crs(ds) -> CRS | None:
    if ds.crs is None:
        return None
    try:
        return CRS.from_wkt(ds.crs.to_wkt())
    except Exception:  # noqa: BLE001 - an unreadable CRS is no CRS
        return None


def _num(v: Any) -> Any:
    """A number JSON can carry (NaN and infinities as text)."""
    if v is None:
        return None
    v = float(v)
    if math.isnan(v):
        return "nan"
    if math.isinf(v):
        return "inf" if v > 0 else "-inf"
    return int(v) if v.is_integer() and abs(v) < 2**53 else v


def _rotated(ds) -> bool:
    t = ds.transform
    return t.b != 0 or t.d != 0


def raster_summary(ds) -> dict[str, Any]:
    t = ds.transform
    crs = _crs(ds)
    block = ds.block_shapes[0] if ds.count else (0, 0)
    tiled = bool(ds.count) and block[1] < ds.width and block[0] > 1
    overviews = ds.overviews(1) if ds.count else []
    layout = ds.tags(ns="IMAGE_STRUCTURE").get("LAYOUT")
    return {
        "type": "raster",
        "width": ds.width,
        "height": ds.height,
        "pixels": ds.width * ds.height,
        "band_count": ds.count,
        "bands": [{"band": i + 1, "dtype": ds.dtypes[i], "nodata": _num(ds.nodatavals[i]),
                   "description": ds.descriptions[i]} for i in range(ds.count)],
        "crs": g.crs_facts(crs),
        "resolution": [abs(t.a), abs(t.e)],
        "rotated": _rotated(ds),
        "extent": [float(v) for v in ds.bounds],
        "tiled": tiled,
        "block": list(block),
        "overviews": overviews,
        "compression": ds.compression.value if ds.compression else None,
        "cog": layout == "COG" or (tiled and bool(overviews)),
    }


def raster_checks(ds, what: str = "The raster") -> list[dict]:
    """crs.present, crs.info and crs.area_of_use (from columbus_gis), a
    geographic CRS (info: terrain needs a projected one), nodata not set,
    and a rotated grid."""
    crs = _crs(ds)
    out = g.crs_checks(crs, [float(v) for v in ds.bounds], what=what)
    if crs is not None and crs.is_geographic:
        out.append(check("crs.geographic", "info",
                         f"{what} is in a geographic CRS ({g._label(crs)}); slope, aspect, hillshade and cell areas need a projected one",
                         fix="raster_op reproject to a projected CRS for the area first, with an explicit resampling"))
    missing = [i + 1 for i, v in enumerate(ds.nodatavals) if v is None]
    if missing:
        out.append(check("raster.nodata", "warn",
                         f"{what} has no nodata value on band(s) {', '.join(map(str, missing))}, so fill values (0, -9999, -3.4e38) are read as data",
                         fix="Ask the data owner which value marks missing cells and set it (gdal_edit -a_nodata) before analysis"))
    if _rotated(ds):
        out.append(check("raster.rotated", "warn", f"{what} has a rotated grid; raster_op does not process rotated rasters",
                         fix="Warp it to a north-up grid first (gdalwarp)"))
    return out


def describe_raster(path: str) -> dict[str, Any]:
    with open_raster(path) as ds:
        out = raster_summary(ds)
        checks = raster_checks(ds, "The raster")
        if not out["cog"] and ds.width * ds.height > 4_000_000:
            checks.append(check("raster.cog", "info", "The raster is not tiled with overviews (not a Cloud-Optimized GeoTIFF), so reading part of it or zooming out reads more than needed",
                                fix="gdal_translate -of COG -co COMPRESS=DEFLATE in.tif out.tif"))
        out["checks"] = checks
        return out


# ---------------------------------------------------------------------------
# Writing
# ---------------------------------------------------------------------------


def _fill_value(dtype: str, data: np.ndarray | None = None):
    """A nodata value for a raster that has none: one the data does not use."""
    dt = np.dtype(dtype)
    if dt.kind == "f":
        candidates = [-9999.0, float(np.finfo(dt).min)]
    elif dt.kind == "i":
        candidates = [int(np.iinfo(dt).min), int(np.iinfo(dt).max)]
    elif dt.kind == "u":
        candidates = [int(np.iinfo(dt).max), 0]
    else:
        raise GISError("raster.dtype", f"raster_op does not process {dtype} rasters")
    for c in candidates:
        if data is None or not np.any(data == c):
            return c
    raise GISError("raster.nodata", "the raster has no nodata value, and every value that could mark missing cells occurs in its data",
                   fix="Ask the data owner which value marks missing cells and set it before processing")


def write_cog(stem: str, data: np.ndarray, crs, transform, nodata, overview_resampling: str) -> str:
    """Write (bands, rows, cols) as a Cloud-Optimized GeoTIFF at stem + ".tif"."""
    os.makedirs(os.path.dirname(stem) or ".", exist_ok=True)
    path = stem + ".tif"
    tmp = stem + ".part.tif"
    for p in (path, tmp):
        if os.path.exists(p):
            os.remove(p)
    bands, rows, cols = data.shape
    profile = {"driver": "GTiff", "height": rows, "width": cols, "count": bands, "dtype": data.dtype.name,
               "crs": crs, "transform": transform, "nodata": nodata, "tiled": True, "blockxsize": 512,
               "blockysize": 512, "compress": "deflate", "BIGTIFF": "IF_SAFER"}
    try:
        with rasterio.open(tmp, "w", **profile) as dst:
            dst.write(data)
        with rasterio.open(tmp, "r", driver="GTiff") as src:
            rio_copy(src, path, driver="COG", COMPRESS="DEFLATE", PREDICTOR="YES", BLOCKSIZE=512,
                     OVERVIEW_RESAMPLING=overview_resampling, BIGTIFF="IF_SAFER")
    finally:
        if os.path.exists(tmp):
            os.remove(tmp)
    return path


def _overview_method(dtype: str) -> str:
    # Classes and codes stay classes: no averaging of integer rasters.
    return "AVERAGE" if np.dtype(dtype).kind == "f" else "NEAREST"


def _guard(width: int, height: int, bands: int, limit: int, what: str) -> None:
    pixels = int(width) * int(height) * max(1, int(bands))
    if pixels > limit:
        raise GISError("output.too_large",
                       f"{what} would be {width:,} x {height:,} cells" + (f" x {bands} bands" if bands > 1 else "")
                       + f" = {pixels:,} pixels, more than the limit of {limit:,}",
                       fix="Clip to the area you need first (raster_op clip with a mask or a bbox), or choose a coarser resolution")


# ---------------------------------------------------------------------------
# Operations
# ---------------------------------------------------------------------------


class _Ctx:
    def __init__(self, stem: str, max_pixels: int):
        self.stem = stem
        self.max_pixels = max_pixels
        self.checks: list[dict] = []


def _need_crs(ds, name: str) -> CRS:
    crs = _crs(ds)
    if crs is None:
        raise GISError("crs.present", f"input {name!r} has no coordinate reference system",
                       fix="Find the source's CRS (metadata, the data owner) and assign it (gdal_edit -a_srs) before processing")
    if _rotated(ds):
        raise GISError("raster.rotated", f"input {name!r} has a rotated grid, which raster_op does not process",
                       fix="Warp it to a north-up grid first (gdalwarp)")
    return crs


def _epsg(value: Any, key: str) -> CRS:
    if not isinstance(value, str) or not g.EPSG_CODE.fullmatch(value.strip()):
        raise GISError("params", f"{key} must be an EPSG code such as \"EPSG:2277\"")
    try:
        return CRS.from_user_input(value.strip().upper())
    except Exception as e:  # noqa: BLE001
        raise GISError("params", f"{key} {value!r} is not a known EPSG code") from e


def _rio_crs(crs: CRS):
    return RioCRS.from_wkt(crs.to_wkt())


def _flag(params: dict, key: str, default: bool = False) -> bool:
    v = params.get(key, default)
    if not isinstance(v, bool):
        raise GISError("params", f"{key} must be true or false")
    return v


def _number(params: dict, key: str, default: float, lo: float, hi: float) -> float:
    v = params.get(key, default)
    if isinstance(v, bool) or not isinstance(v, (int, float)) or not lo <= float(v) <= hi:
        raise GISError("params", f"{key} must be a number from {lo:g} to {hi:g}")
    return float(v)


def _band(params: dict, ds) -> int:
    b = params.get("band", 1)
    if isinstance(b, bool) or not isinstance(b, int) or not 1 <= b <= ds.count:
        raise GISError("params", f"band must be a band number from 1 to {ds.count}")
    return b


def _pixel_window(transform: Affine, width: int, height: int, bounds) -> Window | None:
    """The cells (whole cells, clamped to the grid) covering bounds."""
    inv = ~transform
    corners = [inv @ (x, y) for x in (bounds[0], bounds[2]) for y in (bounds[1], bounds[3])]
    cols = [c for c, _ in corners]
    rows = [r for _, r in corners]
    c0 = max(0, math.floor(min(cols) + 1e-9))
    c1 = min(width, math.ceil(max(cols) - 1e-9))
    r0 = max(0, math.floor(min(rows) + 1e-9))
    r1 = min(height, math.ceil(max(rows) - 1e-9))
    if c1 <= c0 or r1 <= r0:
        return None
    return Window(c0, r0, c1 - c0, r1 - r0)


def _polygons(df, name: str, crs: CRS, ctx: _Ctx) -> list:
    """A layer's geometries in the raster's CRS, saying so when moved."""
    df = g._need_geo(df, name)
    if not df.crs.equals(crs):
        ctx.checks.append(check("crs.agreement", "info",
                                f"Reprojected {name} from {g._label(df.crs)} to the raster's {g._label(crs)}"))
        df = df.to_crs(crs)
    return [geom for geom in df.geometry.to_numpy() if geom is not None and not geom.is_empty]


def _op_clip(ins, params, ctx):
    ds = ins["raster"]
    crs = _need_crs(ds, "raster")
    bbox = params.get("bbox")
    if ("mask" in ins) == (bbox is not None):
        raise GISError("params", "clip takes either a mask input or params.bbox with bbox_crs, not both and not neither")
    if "mask" in ins:
        geoms = _polygons(ins["mask"], "the mask", crs, ctx)
        if any(geom.geom_type not in ("Polygon", "MultiPolygon") for geom in geoms):
            raise GISError("params", "the mask must be polygons", fix="Buffer lines or points into polygons first (vector_op buffer)")
    else:
        if (not isinstance(bbox, list) or len(bbox) != 4 or any(isinstance(v, bool) or not isinstance(v, (int, float)) for v in bbox)
                or not all(math.isfinite(v) for v in bbox) or bbox[0] >= bbox[2] or bbox[1] >= bbox[3]):
            raise GISError("bbox.invalid", "bbox must be [min x, min y, max x, max y]")
        if "bbox_crs" not in params:
            raise GISError("params", "bbox_crs is required with bbox", fix="Give the CRS of the bbox, e.g. \"EPSG:4326\" for longitude and latitude")
        bcrs = _epsg(params.get("bbox_crs"), "bbox_crs")
        box = shapely.box(*bbox)
        box = shapely.segmentize(box, max(bbox[2] - bbox[0], bbox[3] - bbox[1]) / 32)
        if not bcrs.equals(crs):
            ctx.checks.append(check("crs.agreement", "info", f"Placed the bbox from {g._label(bcrs)} on the raster's {g._label(crs)}"))
            box = g.gpd.GeoSeries([box], crs=bcrs).to_crs(crs).iloc[0]
        geoms = [box]
    all_touched = _flag(params, "all_touched")
    if not geoms:
        raise GISError("clip.empty", "the mask has no geometry")
    bounds = shapely.total_bounds(np.array(geoms, dtype=object))
    win = _pixel_window(ds.transform, ds.width, ds.height, bounds)
    if win is None:
        raise GISError("clip.empty", "the mask does not overlap the raster",
                       fix="Check the mask's (or bbox's) CRS and coordinates against the raster's extent (describe_dataset)")
    _guard(win.width, win.height, ds.count, ctx.max_pixels, "The clipped raster")
    data = ds.read(window=win)
    wt = ds.window_transform(win)
    inside = geometry_mask(geoms, out_shape=(win.height, win.width), transform=wt, invert=True, all_touched=all_touched)
    n_in = int(inside.sum())
    if n_in == 0:
        raise GISError("clip.empty", "no cell of the raster falls inside the mask",
                       fix="For small or thin masks pass all_touched: true")
    nodata = ds.nodata
    if nodata is None:
        nodata = _fill_value(ds.dtypes[0], data)
        ctx.checks.append(check("raster.nodata", "info", f"The input has no nodata value; cells outside the mask are set to {_num(nodata)}, recorded as the output's nodata"))
    data[:, ~inside] = nodata
    ctx.checks.append(check("clip.cells", "info", f"{n_in:,} of {inside.size:,} cells in the clipped window fall inside the mask"
                            + (" (all touched)" if all_touched else " (cell centres)")))
    return "raster", write_cog(ctx.stem, data, ds.crs, wt, nodata, _overview_method(ds.dtypes[0]))


def _target_grid(ds, src: CRS, dst: CRS, resolution, max_pixels: int, what: str):
    b = transform_bounds(_rio_crs(src), _rio_crs(dst), *ds.bounds, densify_pts=21)
    if not all(math.isfinite(v) for v in b) or b[0] >= b[2] or b[1] >= b[3]:
        raise GISError("crs.area_of_use", f"the raster's extent cannot be placed in {g._label(dst)}",
                       fix="Check the target CRS covers the raster's area")
    if resolution is None:
        # Keep about as many cells along the diagonal as the source has.
        res_x = res_y = math.hypot(b[2] - b[0], b[3] - b[1]) / math.hypot(ds.width, ds.height)
    else:
        if isinstance(resolution, (int, float)) and not isinstance(resolution, bool):
            resolution = [resolution, resolution]
        if (not isinstance(resolution, list) or len(resolution) != 2
                or any(isinstance(v, bool) or not isinstance(v, (int, float)) or not v > 0 or not math.isfinite(v) for v in resolution)):
            raise GISError("params", "resolution must be a positive number (or [x, y]) in the target CRS's units")
        res_x, res_y = float(resolution[0]), float(resolution[1])
    wf, hf = (b[2] - b[0]) / res_x, (b[3] - b[1]) / res_y
    if not (math.isfinite(wf) and math.isfinite(hf)) or wf > 1e12 or hf > 1e12:
        raise GISError("output.too_large", f"{what} at that resolution would be far too large",
                       fix="Choose a coarser resolution, or clip first")
    width = max(1, math.ceil(wf))
    height = max(1, math.ceil(hf))
    _guard(width, height, ds.count, max_pixels, what)
    # Much coarser than the source along one axis (poles, odd projections):
    # say so rather than hand back a smeared grid as if it were fine.
    if resolution is None and (width < ds.width / 4 or height < ds.height / 4):
        raise GISError("raster.distorted",
                       f"{what} would squash {ds.width} x {ds.height} cells into {width} x {height} in {g._label(dst)}; "
                       "the target CRS distorts this area badly",
                       fix="Pass an explicit resolution, or choose a CRS suited to the area (polar areas need a polar projection)")
    return from_origin(b[0], b[3], res_x, res_y), width, height


def _reproject(ds, params: dict, stem: str, max_pixels: int, checks: list, what: str) -> str:
    src = _need_crs(ds, "raster")
    dst = _epsg(params.get("crs"), "crs")
    name = params.get("resampling")
    if name not in RESAMPLING:
        raise GISError("params", "resampling is required: nearest, bilinear, cubic, average or mode",
                       fix="nearest (or mode when making cells coarser) for categories such as land cover or classes; "
                           "bilinear, cubic or average for continuous values such as elevation")
    dtype = ds.dtypes[0]
    if np.dtype(dtype).kind in "iu" and name in BLENDING:
        checks.append(check("raster.resampling", "warn",
                            f"{name} resampling of an integer ({dtype}) raster blends neighbouring values; if the cells are categories (land cover, classes, ids) the output has codes that mean nothing",
                            fix="Use nearest, or mode when making cells coarser, for categorical rasters"))
    transform, width, height = _target_grid(ds, src, dst, params.get("resolution"), max_pixels, what)
    nodata = ds.nodata
    if nodata is None:
        nodata = _fill_value(dtype)
        checks.append(check("raster.nodata", "info", f"The input has no nodata value; cells outside its footprint are set to {_num(nodata)}, recorded as the output's nodata"))
    out = np.full((ds.count, height, width), nodata, dtype=dtype)
    for b in range(1, ds.count + 1):
        warp_reproject(rasterio.band(ds, b), out[b - 1], src_nodata=ds.nodata, dst_transform=transform,
                       dst_crs=_rio_crs(dst), dst_nodata=nodata, resampling=RESAMPLING[name])
    checks.append(check("crs.reprojected", "info",
                        f"Reprojected from {g._label(src)} to {g._label(dst)} with {name} resampling; cells {transform.a:g} x {abs(transform.e):g} {g.crs_facts(dst)['units']}"))
    return write_cog(stem, out, _rio_crs(dst), transform, nodata, _overview_method(dtype))


def _op_reproject(ins, params, ctx):
    return "raster", _reproject(ins["raster"], params, ctx.stem, ctx.max_pixels, ctx.checks, "The reprojected raster")


def _raster_frame(ds, crs: CRS):
    """The raster's footprint as a one-row GeoDataFrame, for suggest_crs."""
    return g.gpd.GeoDataFrame(geometry=[shapely.box(*ds.bounds)], crs=crs)


def _terrain_input(ins, params, ctx, what: str):
    """The DEM band as float32 with NaN for nodata, and the z-factor that
    puts elevations in the grid's horizontal units."""
    ds = ins["raster"]
    crs = _need_crs(ds, "raster")
    if crs.is_geographic or g._pseudo_mercator(crs):
        why = ("its horizontal units are degrees and its elevations are not" if crs.is_geographic
               else "Web Mercator stretches horizontal distances away from the equator (about 15% at Austin), so slopes come out too gentle")
        raise GISError("crs.measure", f"cannot compute {what} on a raster in {g._label(crs)}: {why}",
                       fix=f"Reproject the DEM first with raster_op reproject (resampling bilinear) to a projected CRS suited to the area "
                           f"({g.suggest_crs(_raster_frame(ds, crs))}), then run it again")
    z_unit = Z_UNITS.get(str(params.get("z_unit") or "").lower())
    if z_unit is None:
        raise GISError("params", "z_unit is required: m or ft (us-ft for US survey feet), the unit the elevations are in",
                       fix="Check the DEM's metadata or describe_dataset; USGS 3DEP DEMs are in metres, many county lidar DEMs in feet")
    band = _band(params, ds)
    _guard(ds.width, ds.height, 1, ctx.max_pixels, f"The {what} raster")
    z = ds.read(band, masked=True).astype("float32").filled(np.nan)
    z[~np.isfinite(z)] = np.nan
    horizontal = crs.axis_info[0].unit_conversion_factor
    zf = g.UNITS[z_unit] / horizontal
    units = g.crs_facts(crs)["units"]
    ctx.checks.append(check("terrain.z_factor", "info",
                            f"Elevations in {z_unit} on a grid in {units}: z-factor {zf:.6g} (elevations multiplied into horizontal units)",
                            detail={"z_unit": z_unit, "horizontal_units": units, "z_factor": zf}))
    t = ds.transform
    return ds, z, zf, abs(t.a), abs(t.e)


def _gradient_strips(z: np.ndarray, xres: float, yres: float, zf: float):
    """Horn's method, a strip of rows at a time. Yields (row0, dzdx, dzds,
    edge) where dzdx is the rise toward east and dzds toward south, both in
    horizontal units per horizontal unit, and edge marks valid cells next to
    nodata or the raster's edge. A missing neighbour is extrapolated from
    the opposite one (or takes the centre's value when that is missing
    too), so a fill value never becomes a cliff, the edge rows are still
    computed, and a plane gives its true slope right to the edge."""
    rows = z.shape[0]
    p = np.pad(z, 1, constant_values=np.nan)
    for r0 in range(0, rows, STRIP_ROWS):
        r1 = min(rows, r0 + STRIP_ROWS)
        h = r1 - r0
        centre = p[r0 + 1 : r1 + 1, 1:-1]
        missing = np.zeros(centre.shape, dtype=bool)

        def raw(dr: int, dc: int) -> np.ndarray:
            return p[r0 + 1 + dr : r0 + 1 + dr + h, 1 + dc : p.shape[1] - 1 + dc]

        def fill(v: np.ndarray, candidates) -> np.ndarray:
            gap = np.isnan(v)
            if not gap.any():
                return v
            missing.__ior__(gap)
            out = v.copy()
            for cand in candidates:
                todo = np.isnan(out)
                if not todo.any():
                    break
                c = cand()
                out[todo] = c[todo]
            todo = np.isnan(out)
            out[todo] = centre[todo]
            return out

        # A missing neighbour continues the slope: from the opposite
        # neighbour, or (for a corner) from the two filled side neighbours
        # next to it; failing both, the centre's own value.
        side = {}
        for dr, dc in ((-1, 0), (1, 0), (0, -1), (0, 1)):
            side[(dr, dc)] = fill(raw(dr, dc), [lambda dr=dr, dc=dc: 2 * centre - raw(-dr, -dc)])
        corner = {}
        for dr, dc in ((-1, -1), (-1, 1), (1, -1), (1, 1)):
            corner[(dr, dc)] = fill(raw(dr, dc), [lambda dr=dr, dc=dc: 2 * centre - raw(-dr, -dc),
                                                  lambda dr=dr, dc=dc: side[(dr, 0)] + side[(0, dc)] - centre])
        a, b, c = corner[(-1, -1)], side[(-1, 0)], corner[(-1, 1)]
        d, f = side[(0, -1)], side[(0, 1)]
        gg, hh, i = corner[(1, -1)], side[(1, 0)], corner[(1, 1)]
        dzdx = ((c + 2 * f + i) - (a + 2 * d + gg)) * np.float32(zf / (8 * xres))
        dzds = ((gg + 2 * hh + i) - (a + 2 * b + c)) * np.float32(zf / (8 * yres))
        valid = ~np.isnan(centre)
        yield r0, r1, dzdx, dzds, valid, missing & valid


def _terrain(ins, params, ctx, what: str, cell):
    ds, z, zf, xres, yres = _terrain_input(ins, params, ctx, what)
    out_dtype, nodata = cell["dtype"], cell["nodata"]
    out = np.full(z.shape, nodata, dtype=out_dtype)
    edges = 0
    for r0, r1, dzdx, dzds, valid, edge in _gradient_strips(z, xres, yres, zf):
        values = cell["fn"](dzdx, dzds)
        strip = out[r0:r1]
        strip[valid] = values[valid]
        edges += int(edge.sum())
    nan_cells = int(np.isnan(z).sum())
    if nan_cells:
        ctx.checks.append(check("raster.nodata", "info", f"{nan_cells:,} nodata cells in the DEM stay nodata ({_num(nodata)}) in the output"))
    if edges:
        ctx.checks.append(check("terrain.edges", "info",
                                f"{edges:,} cells next to nodata or the raster's edge were computed with missing neighbours extrapolated from the cells opposite them"))
    if ds.nodata is None:
        ctx.checks.append(check("raster.nodata", "warn", "The DEM has no nodata value, so any fill value in it was used as an elevation",
                                fix="Set the DEM's nodata value (gdal_edit -a_nodata) and run it again"))
    return "raster", write_cog(ctx.stem, out[np.newaxis], ds.crs, ds.transform, nodata, cell.get("overviews", "AVERAGE"))


def _op_slope(ins, params, ctx):
    units = params.get("units", "degrees")
    if units not in ("degrees", "percent"):
        raise GISError("params", "units must be degrees or percent")

    def fn(dzdx, dzds):
        rise = np.hypot(dzdx, dzds)
        return (100.0 * rise) if units == "percent" else np.degrees(np.arctan(rise))

    ctx.checks.append(check("terrain.units", "info", f"Slope in {units}" + (" (100% is 45 degrees)" if units == "percent" else " (0 to 90)")))
    return _terrain(ins, params, ctx, "slope", {"dtype": "float32", "nodata": -9999.0, "fn": fn})


def _op_aspect(ins, params, ctx):
    def fn(dzdx, dzds):
        # Downslope direction, clockwise from north: east part -dz/dx,
        # north part +dz/d(south).
        az = (np.degrees(np.arctan2(-dzdx, dzds)) + 360.0) % 360.0
        return np.where((dzdx == 0) & (dzds == 0), -1.0, az)

    ctx.checks.append(check("terrain.units", "info", "Aspect in degrees clockwise from north, the direction the slope faces; flat cells are -1. Average aspect with its sine and cosine, never the raw degrees"))
    return _terrain(ins, params, ctx, "aspect", {"dtype": "float32", "nodata": -9999.0, "fn": fn})


def _op_hillshade(ins, params, ctx):
    azimuth = _number(params, "azimuth", 315, 0, 360)
    altitude = _number(params, "altitude", 45, 0.1, 90)
    zenith = math.radians(90 - altitude)
    az_math = math.radians((360.0 - azimuth + 90.0) % 360.0)

    def fn(dzdx, dzds):
        slope = np.arctan(np.hypot(dzdx, dzds))
        aspect = np.arctan2(dzds, -dzdx)
        aspect = np.where(aspect < 0, aspect + 2 * math.pi, aspect)
        shade = 255.0 * (math.cos(zenith) * np.cos(slope) + math.sin(zenith) * np.sin(slope) * np.cos(az_math - aspect))
        # 0 is nodata; lit values are 1 to 255.
        return np.clip(np.rint(shade), 1, 255)

    ctx.checks.append(check("terrain.units", "info", f"Hillshade 1 to 255 (0 is nodata), sun at azimuth {azimuth:g}, altitude {altitude:g}; for display, not analysis"))
    return _terrain(ins, params, ctx, "hillshade", {"dtype": "uint8", "nodata": 0, "fn": fn})


def _op_zonal_stats(ins, params, ctx):
    ds = ins["raster"]
    crs = _need_crs(ds, "raster")
    stats = params.get("stats", DEFAULT_STATS)
    if (not isinstance(stats, list) or not stats or not all(isinstance(s, str) and s in ZONAL_STATS for s in stats)
            or len(set(stats)) != len(stats)):
        raise GISError("params", f"stats must be a list of {', '.join(ZONAL_STATS)} (default {', '.join(DEFAULT_STATS)})")
    band = _band(params, ds)
    all_touched = _flag(params, "all_touched")
    zones = g._need_geo(ins["zones"], "zones")
    placed = zones
    if not zones.crs.equals(crs):
        ctx.checks.append(check("crs.agreement", "info", f"Reprojected the zones from {g._label(zones.crs)} to the raster's {g._label(crs)} to compute; the output keeps the zones' own CRS"))
        placed = zones.to_crs(crs)
    geoms = placed.geometry.to_numpy()
    footprint = shapely.box(*ds.bounds)
    present = [i for i, geom in enumerate(geoms) if geom is not None and not geom.is_empty]
    results: dict[str, list] = {s: [None] * len(geoms) for s in stats}
    if "count" in results:
        results["count"] = [0] * len(geoms)
    fully_out = partly_out = no_pixels = 0
    cells_per_zone: list[int] = []
    in_cells = nodata_cells = 0
    meeting = [i for i in present if geoms[i].intersects(footprint)]
    fully_out = len(present) - len(meeting)
    no_pixels += len(geoms) - len(meeting)
    win = None
    if meeting:
        win = _pixel_window(ds.transform, ds.width, ds.height, shapely.total_bounds(geoms[meeting]))
    if win is not None:
        _guard(win.width, win.height, 1, ctx.max_pixels, "The cells zonal_stats would read")
        arr = ds.read(band, window=win, masked=True)
        values = arr.data
        invalid = np.ma.getmaskarray(arr) | ~np.isfinite(values)
        wt = ds.window_transform(win)
        for i in meeting:
            zone = geoms[i]
            if not footprint.contains(zone):
                partly_out += 1
            zw = _pixel_window(wt, win.width, win.height, zone.bounds)
            if zw is None:
                no_pixels += 1
                cells_per_zone.append(0)
                continue
            r0, c0 = int(zw.row_off), int(zw.col_off)
            sub_t = wt @ Affine.translation(c0, r0)
            inside = geometry_mask([zone], out_shape=(zw.height, zw.width), transform=sub_t, invert=True, all_touched=all_touched)
            block_invalid = invalid[r0 : r0 + zw.height, c0 : c0 + zw.width]
            good = inside & ~block_invalid
            cells = int(inside.sum())
            cells_per_zone.append(cells)
            in_cells += cells
            nodata_cells += int((inside & block_invalid).sum())
            v = values[r0 : r0 + zw.height, c0 : c0 + zw.width][good].astype("float64")
            if v.size == 0:
                no_pixels += 1
                continue
            for s in stats:
                results[s][i] = {"count": lambda: int(v.size), "mean": lambda: float(v.mean()), "min": lambda: float(v.min()),
                                 "max": lambda: float(v.max()), "sum": lambda: float(v.sum()), "std": lambda: float(v.std())}[s]()
    else:
        no_pixels = len(geoms) if not meeting else no_pixels + len(meeting)
        fully_out = len(present)
    out = zones.copy()
    for s in stats:
        col = s if s not in out.columns else f"raster_{s}"
        out[col] = results[s]
    n = len(geoms)
    how = "all touched cells" if all_touched else "cells whose centre is inside"
    ctx.checks.append(check("zonal.stats", "info",
                            f"{', '.join(stats)} of band {band} for {n} zones ({how}); each zone is measured on its own, so overlapping zones each get all their cells"))
    if fully_out or partly_out:
        ctx.checks.append(check("zonal.outside", "warn",
                                f"{fully_out} of {n} zones fall entirely outside the raster and {partly_out} partly; their statistics cover only cells on the raster",
                                detail={"fully_outside": fully_out, "partly_outside": partly_out},
                                fix="Check the zones and the raster cover the same area (CRS, extent); report partial coverage with the results"))
    if no_pixels:
        ctx.checks.append(check("zonal.no_pixels", "warn",
                                f"{no_pixels} of {n} zones have no valid cells (no geometry, outside the raster, all nodata, or too small for any cell centre); their statistics are empty and count is 0",
                                detail={"zones": no_pixels},
                                fix="For small or thin zones pass all_touched: true, or use a finer raster"))
    if cells_per_zone:
        median = float(np.median(cells_per_zone))
        if median < 10:
            t = ds.transform
            ctx.checks.append(check("zonal.cell_size", "warn",
                                    f"The median zone covers {median:g} cells: at {abs(t.a):g} x {abs(t.e):g} {g.crs_facts(crs)['units']} cells the statistics rest on very few values",
                                    detail={"median_cells": median},
                                    fix="Use a finer raster, or fractional coverage (exactextract) outside Columbus; always report count with each mean"))
    if in_cells and nodata_cells:
        ctx.checks.append(check("zonal.nodata", "info", f"{100.0 * nodata_cells / in_cells:.1f}% of the cells inside the zones are nodata and were left out",
                                detail={"cells": in_cells, "nodata": nodata_cells}))
    if np.dtype(ds.dtypes[band - 1]).kind in "iu" and any(s in stats for s in ("mean", "sum", "std")):
        ctx.checks.append(check("zonal.categorical", "info", f"Band {band} holds integers; if they are categories (land cover, classes), a mean or sum means nothing: report counts per class instead"))
    return "vector", out


def _op_info(ins, params, ctx):
    ds = ins["raster"]
    total = ds.width * ds.height
    factor = 1
    if total > FULL_STATS_PIXELS:
        factor = math.ceil(math.sqrt(total / SAMPLE_PIXELS))
    shape = (max(1, math.ceil(ds.height / factor)), max(1, math.ceil(ds.width / factor)))
    has_overviews = bool(ds.overviews(1)) and max(ds.overviews(1)) >= 2
    if factor == 1:
        computed_on = "all cells"
    elif has_overviews:
        computed_on = f"an overview or decimated read at about 1/{factor} of full resolution ({shape[1]} x {shape[0]} cells)"
    else:
        computed_on = f"a sample of every {factor}th cell in each direction ({shape[1]} x {shape[0]} cells)"
    bands = []
    for b in range(1, ds.count + 1):
        arr = ds.read(b, masked=True, out_shape=shape, resampling=Resampling.nearest) if factor > 1 else ds.read(b, masked=True)
        values = arr.data
        invalid = np.ma.getmaskarray(arr) | ~np.isfinite(values.astype("float64"))
        v = values[~invalid].astype("float64")
        entry = {"band": b, "dtype": ds.dtypes[b - 1], "nodata": _num(ds.nodatavals[b - 1]),
                 "cells": int(values.size), "valid": int(v.size), "nodata_share": round(float(invalid.mean()), 6)}
        if v.size:
            entry.update({"min": _num(v.min()), "max": _num(v.max()), "mean": _num(v.mean()), "std": _num(v.std())})
        bands.append(entry)
    ctx.checks.extend(raster_checks(ds, "The raster"))
    if factor > 1:
        ctx.checks.append(check("raster.stats_sampled", "info",
                                f"Statistics computed on {computed_on}, not every cell; minimum and maximum may miss extremes",
                                detail={"factor": factor}))
    for e in bands:
        if e["valid"] == 0:
            ctx.checks.append(check("raster.empty", "warn", f"Band {e['band']} has no valid cells (all nodata)"))
    return "info", {"bands": bands, "computed_on": computed_on, "raster": raster_summary(ds)}


# name: (handler, required inputs, optional inputs, summary)
OPS: dict[str, tuple[Any, list[str], list[str], str]] = {
    "clip": (_op_clip, ["raster"], ["mask"], "mask input or params bbox + bbox_crs; all_touched?"),
    "reproject": (_op_reproject, ["raster"], [], "params: crs (\"EPSG:…\"), resampling (nearest|bilinear|cubic|average|mode), resolution?"),
    "slope": (_op_slope, ["raster"], [], "params: z_unit (m|ft|us-ft), units (degrees|percent)?, band?"),
    "aspect": (_op_aspect, ["raster"], [], "params: z_unit, band?"),
    "hillshade": (_op_hillshade, ["raster"], [], "params: z_unit, azimuth? (315), altitude? (45), band?"),
    "zonal_stats": (_op_zonal_stats, ["raster", "zones"], [], "params: stats? (count, mean, min, max, sum, std), band?, all_touched?"),
    "info": (_op_info, ["raster"], [], "no params"),
}
RASTER_INPUTS = {"raster"}


def raster_op(op: str, inputs: dict[str, str], params: dict[str, Any], stem: str, max_pixels: int | None = None) -> dict[str, Any]:
    if op not in OPS:
        raise GISError("params", f"unknown op {op!r}; one of {', '.join(OPS)}")
    fn, required, optional, _ = OPS[op]
    names = set(inputs)
    if not set(required) <= names or not names <= set(required) | set(optional):
        listed = ", ".join(required) + (f" (and optionally {', '.join(optional)})" if optional else "")
        raise GISError("params", f"{op} takes inputs {listed}")
    limit = MAX_PIXELS if not isinstance(max_pixels, int) or max_pixels < 1 else min(MAX_PIXELS, max_pixels)
    ctx = _Ctx(stem, limit)
    opened = []
    loaded: dict[str, Any] = {}
    try:
        for n, p in inputs.items():
            if n in RASTER_INPUTS:
                loaded[n] = open_raster(p)
                opened.append(loaded[n])
            else:
                loaded[n] = g.read_layer(p)
                if isinstance(loaded[n], g.gpd.GeoDataFrame):
                    ctx.checks.extend(c for c in g.geometry_checks(loaded[n].geometry, what=f"Input {n}") if c["id"] == "geometry.valid")
        kind, value = fn(loaded, params or {}, ctx)
    finally:
        for ds in opened:
            ds.close()
    if kind == "info":
        return {"info": value, "summary": value["raster"], "checks": ctx.checks, "engine": versions()}
    if kind == "vector":
        path = g.write_layer(value, stem, layer=op)
        ctx.checks.extend(g._layer_checks(value, "The output"))
        return {"output_path": path, "summary": g.layer_summary(value), "checks": ctx.checks, "engine": versions()}
    with open_raster(value) as o:
        summary = raster_summary(o)
        ctx.checks.extend(raster_checks(o, "The output"))
    ctx.checks.append(check("raster.cog", "info", f"Written as a Cloud-Optimized GeoTIFF (DEFLATE, 512-cell tiles, {len(summary['overviews'])} overview levels)"))
    return {"output_path": value, "summary": summary, "checks": ctx.checks, "engine": versions()}
