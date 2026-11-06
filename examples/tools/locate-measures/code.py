"""Locate points along routes: each point's route id and measure."""
import geopandas as gpd
from columbus_plugin import Severity, ToolError

# Metres in one of each unit.
METRES = {"mile": 1609.344, "foot": 0.3048, "kilometre": 1000.0, "metre": 1.0}


def run(ctx):
    routes = gpd.read_file(ctx.input_file("routes"))
    points = gpd.read_file(ctx.input_file("points"))
    route_field = ctx.param("route_field")
    unit = ctx.param("measure_unit")
    if route_field not in routes.columns:
        fields = ", ".join(c for c in routes.columns if c != routes.geometry.name)
        raise ToolError(f"routes has no field {route_field!r}; its fields are {fields}")
    if routes.crs is None or not routes.crs.is_projected:
        raise ToolError("routes must be in a projected CRS; reproject it with vector_op first")
    if not routes.geom_type.isin(["LineString", "MultiLineString"]).all():
        raise ToolError("routes must be lines")
    if points.crs is None:
        raise ToolError("points has no CRS")
    points = points.to_crs(routes.crs)
    crs_metres = routes.crs.axis_info[0].unit_conversion_factor
    per_unit = METRES[unit] / crs_metres
    max_distance = ctx.param("max_distance") * METRES["foot"] / crs_metres

    ids, measures, offsets = [], [], []
    for geom in points.geometry:
        route_id = measure = offset = None
        if geom is not None and not geom.is_empty:
            distance = routes.distance(geom)
            nearest = distance.idxmin()
            offset = round(float(distance[nearest]) * crs_metres / METRES["foot"], 2)
            if distance[nearest] <= max_distance:
                route_id = routes[route_field][nearest]
                measure = round(routes.geometry[nearest].project(geom) / per_unit, 4)
        ids.append(route_id)
        measures.append(measure)
        offsets.append(offset)
    out = points.copy()
    out["ROUTE_ID"] = ids
    out["MEASURE"] = measures
    out["OFFSET_FT"] = offsets
    out.to_file(ctx.output_path("located"), driver="GPKG", engine="pyogrio")
    off = sum(1 for i in ids if i is None)
    if off:
        ctx.warn("lrs.off_route", Severity.WARN,
                 f"{off} of {len(points)} points are farther than {ctx.param('max_distance')} ft from every route; "
                 "they have no route id or measure")
    return {"located": len(points) - off, "off_route": off, "measure_unit": unit}
