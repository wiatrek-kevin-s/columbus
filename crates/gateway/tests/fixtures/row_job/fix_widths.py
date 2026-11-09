"""Fill gaps in ROW width events from the owned-ROW polygons.

A stand-in for the owner's own fix-widths tool (T-23): along each gap it
casts a transect across the route every `interval` feet, measures in feet
where the transect overlaps the ROW polygons, and groups runs of widths
within +/- `tolerance` feet of their running mean into one segment of the
mean width (20, 23, 22, 20, 21 become one segment of 21). It proves the
plumbing, not the owner's algorithm.
"""
import geopandas as gpd
import shapely
from shapely.ops import substring
from columbus_plugin import Severity, ToolError

FOOT = 0.3048


def measure_range(geom):
    if geom is None or geom.is_empty or geom.geom_type != "LineString":
        raise ToolError("routes must be single-part lines")
    if not shapely.has_m(geom):
        raise ToolError("routes have no M values: give the route network with measures")
    c = shapely.get_coordinates(geom, include_m=True)
    return float(c[0][-1]), float(c[-1][-1])


def stations(lo, hi, step):
    out, m = [], lo
    while m < hi - 1e-9:
        out.append(m)
        m += step
    out.append(hi)
    return out


def transect(line, f, half):
    """A straight line `half` metres either side of `line` at fraction f,
    square to it."""
    p = line.interpolate(f, normalized=True)
    a = line.interpolate(max(f - 0.001, 0.0), normalized=True)
    b = line.interpolate(min(f + 0.001, 1.0), normalized=True)
    dx, dy = b.x - a.x, b.y - a.y
    n = (dx * dx + dy * dy) ** 0.5
    ux, uy = -dy / n, dx / n
    return shapely.LineString([(p.x - ux * half, p.y - uy * half), (p.x + ux * half, p.y + uy * half)])


def group(values, tolerance):
    """Runs of (measure, width) whose widths stay within tolerance of the
    run's mean."""
    runs = []
    for m, w in values:
        if runs:
            cur = runs[-1]
            mean = sum(x for _, x in cur) / len(cur)
            if abs(w - mean) <= tolerance:
                cur.append((m, w))
                continue
        runs.append([(m, w)])
    return runs


def run(ctx):
    routes = gpd.read_file(ctx.input_file("routes"), engine="pyogrio", use_arrow=True)
    gaps = gpd.read_file(ctx.input_file("gaps"), engine="pyogrio")
    row = gpd.read_file(ctx.input_file("row"), engine="pyogrio")
    interval = float(ctx.param("interval"))
    tolerance = float(ctx.param("tolerance"))
    half = float(ctx.param("transect_length")) * FOOT / 2
    if routes.crs is None or row.crs is None:
        raise ToolError("routes and row need a CRS")
    metric = routes.estimate_utm_crs()
    owned = row.to_crs(metric).union_all()
    by_id = {r["ROUTE_ID"]: r for _, r in routes.iterrows()}
    rows, geoms, transects, missed = [], [], 0, 0
    for _, g in gaps.iterrows():
        r = by_id.get(g["ROUTE_ID"])
        if r is None:
            raise ToolError("a gap names a route that is not in routes")
        m0, m1 = measure_range(r.geometry)
        flat = shapely.linestrings(shapely.get_coordinates(r.geometry))
        line = gpd.GeoSeries([flat], crs=routes.crs).to_crs(metric).iloc[0]
        lo, hi = float(g["FROM_M"]), float(g["TO_M"])
        widths = []
        for m in stations(lo, hi, interval):
            transects += 1
            cut = transect(line, (m - m0) / (m1 - m0), half).intersection(owned)
            if cut.is_empty:
                missed += 1
                continue
            widths.append((m, cut.length / FOOT))
        runs = group(widths, tolerance)
        for i, part in enumerate(runs):
            start = lo if i == 0 else (runs[i - 1][-1][0] + part[0][0]) / 2
            end = hi if i == len(runs) - 1 else (part[-1][0] + runs[i + 1][0][0]) / 2
            rows.append({"ROUTE_ID": g["ROUTE_ID"], "ROUTE_NAME": g["ROUTE_NAME"],
                         "FROM_M": round(start, 3), "TO_M": round(end, 3),
                         "ROW_WIDTH_FT": round(sum(w for _, w in part) / len(part), 1),
                         "TRANSECTS": len(part)})
            geoms.append(substring(flat, (start - m0) / (m1 - m0), (end - m0) / (m1 - m0), normalized=True))
    if missed:
        ctx.warn("row.no_polygon", Severity.WARN,
                 f"{missed} of {transects} transects crossed no ROW polygon; those stations have no width")
    out = gpd.GeoDataFrame(rows, geometry=geoms, crs=routes.crs) if rows else gpd.GeoDataFrame(
        {"ROUTE_ID": [], "ROUTE_NAME": [], "FROM_M": [], "TO_M": [], "ROW_WIDTH_FT": [], "TRANSECTS": []},
        geometry=[], crs=routes.crs)
    out.to_file(ctx.output_path("segments"), driver="GPKG", engine="pyogrio")
    return {"segments": len(rows), "transects": transects, "missed": missed}
