"""Place route events as points: the point at each event's measure."""
import geopandas as gpd
import pandas as pd
from columbus_plugin import Severity, ToolError

METRES = {"mile": 1609.344, "foot": 0.3048, "kilometre": 1000.0, "metre": 1.0}


def run(ctx):
    routes = gpd.read_file(ctx.input_file("routes"))
    path = ctx.input_file("events")
    events = pd.read_csv(path, dtype=str) if path.lower().endswith(".csv") else gpd.read_file(path)
    route_field = ctx.param("route_field")
    measure_field = ctx.param("measure_field")
    for table, field in (("routes", route_field), ("events", measure_field), ("events", route_field)):
        columns = routes.columns if table == "routes" else events.columns
        if field not in columns:
            raise ToolError(f"{table} has no field {field!r}")
    if routes.crs is None or not routes.crs.is_projected:
        raise ToolError("routes must be in a projected CRS; reproject it with vector_op first")
    per_unit = METRES[ctx.param("measure_unit")] / routes.crs.axis_info[0].unit_conversion_factor
    lines = dict(zip(routes[route_field].astype(str), routes.geometry))

    points, unknown, beyond = [], 0, 0
    for route_id, measure in zip(events[route_field].astype(str), pd.to_numeric(events[measure_field], errors="coerce")):
        line = lines.get(route_id)
        if line is None or pd.isna(measure):
            unknown += 1
            points.append(None)
            continue
        distance = float(measure) * per_unit
        if distance < 0 or distance > line.length:
            beyond += 1
        points.append(line.interpolate(min(max(distance, 0.0), line.length)))
    out = gpd.GeoDataFrame(events.drop(columns=[c for c in ("geometry",) if c in events.columns]),
                           geometry=points, crs=routes.crs)
    out.to_file(ctx.output_path("placed"), driver="GPKG", engine="pyogrio")
    if unknown:
        ctx.warn("lrs.unknown_route", Severity.WARN, f"{unknown} events name no route in routes or have no measure; they have no point")
    if beyond:
        ctx.warn("lrs.out_of_range", Severity.WARN, f"{beyond} measures are beyond their route's ends; they were placed at the nearest end")
    return {"placed": len(points) - unknown, "unknown_route": unknown, "out_of_range": beyond}
