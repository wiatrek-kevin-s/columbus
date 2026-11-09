"""Find the measure ranges along each route that have no ROW width event.

A stand-in for the owner's own find-gaps tool (T-23): it proves the
plumbing (M values in, a layer out), not the owner's algorithm.
"""
import geopandas as gpd
import pandas as pd
import pyogrio
import shapely
from shapely.ops import substring
from columbus_plugin import ToolError

# Gaps shorter than this many measure units are joins, not gaps.
SLACK = 0.001


def read_table(path):
    """A table input: CSV (a query without geometry) or a GeoPackage."""
    if str(path).lower().endswith(".csv"):
        return pd.read_csv(path)
    return pyogrio.read_dataframe(path, read_geometry=False)


def measure_range(geom):
    """The first and last M value of a measured line."""
    if geom is None or geom.is_empty or geom.geom_type != "LineString":
        raise ToolError("routes must be single-part lines")
    if not shapely.has_m(geom):
        raise ToolError("routes have no M values: give the route network with measures")
    c = shapely.get_coordinates(geom, include_m=True)
    return float(c[0][-1]), float(c[-1][-1])


def run(ctx):
    routes = gpd.read_file(ctx.input_file("routes"), engine="pyogrio", use_arrow=True)
    events = read_table(ctx.input_file("events"))
    for f in ("ROUTE_ID", "ROUTE_NAME"):
        if f not in routes.columns:
            raise ToolError(f"routes has no field {f}")
    for f in ("ROUTE_ID", "FROM_M", "TO_M"):
        if f not in events.columns:
            raise ToolError(f"events has no field {f}")
    rows, geoms = [], []
    for _, r in routes.iterrows():
        m0, m1 = measure_range(r.geometry)
        flat = shapely.linestrings(shapely.get_coordinates(r.geometry))
        mine = events[events["ROUTE_ID"] == r["ROUTE_ID"]].sort_values("FROM_M")
        found, at = [], m0
        for _, e in mine.iterrows():
            lo, hi = float(e["FROM_M"]), float(e["TO_M"])
            if lo > at + SLACK:
                found.append((at, lo))
            at = max(at, hi)
        if at < m1 - SLACK:
            found.append((at, m1))
        for lo, hi in found:
            rows.append({"ROUTE_ID": r["ROUTE_ID"], "ROUTE_NAME": r["ROUTE_NAME"],
                         "FROM_M": round(lo, 3), "TO_M": round(hi, 3)})
            geoms.append(substring(flat, (lo - m0) / (m1 - m0), (hi - m0) / (m1 - m0), normalized=True))
    out = gpd.GeoDataFrame(rows, geometry=geoms, crs=routes.crs) if rows else gpd.GeoDataFrame(
        {"ROUTE_ID": [], "ROUTE_NAME": [], "FROM_M": [], "TO_M": []}, geometry=[], crs=routes.crs)
    out.to_file(ctx.output_path("gaps"), driver="GPKG", engine="pyogrio")
    return {"gaps": len(rows), "routes": len(routes), "gap_length": round(sum(r["TO_M"] - r["FROM_M"] for r in rows), 3)}
