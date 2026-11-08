# Designing a personal tool

When the person says "add the script we just made as a tool" (or a job keeps coming back),
package it with `propose_tool`. A personal tool is a small, single-purpose Python function
that Columbus runs as an engine plugin: confined (no network; each run starts a fresh process
that reads only the layers it is given for that run and writes only its own folder, deleted
when the run ends), from the next call after the person installs it,
with no approval per run. Other assistants, in other chats, will find it with `search_tools`
and call it with `run_tool` knowing only what you write in its manifest. Write for them.

## How it goes

1. **Check it is allowed.** `whoami` → `policy.personal_tools_allowed` and
   `policy.scripts_allowed` (the test run runs your code as a script). If either is false,
   say so: the person can turn them on in the Columbus app's Settings if their organization
   lets them. Do not offer workarounds.
2. **Have a working script first**, usually one you ran with `run_python` on real layers.
3. **Turn it into `run(ctx)`** (below) and write the manifest.
4. **Call `propose_tool`** with the manifest, the code and a test on this session's layers
   (`test.inputs`: a `ws://` handle for every input). Columbus checks everything and refuses
   with the full list of problems; fix them all and call again.
5. **The person approves the test run**, reading the code, as for a script. If it fails, fix
   the code; nothing was filed.
6. **The result says `waiting_for_person`.** Tell the person, in plain words, that the tool
   is waiting in the Columbus app under **Settings, Tools**, that they read its code and
   install it there (Touch ID or Windows Hello), and what it does. You cannot install it.
   Report the test run's checks, especially any `fail`.
7. **After they install it**, it is `personal.<name>.run` from the next call, with no restart.

To change a tool, propose it again with the same `name` and a higher `version`; the person
installs the new one, which replaces the old. Any change to the code or manifest is a new
proposal and a new install: an edited file is switched off.

## The code: `run(ctx)`

```python
import geopandas as gpd
from columbus_plugin import Severity, ToolError   # optional helpers; nothing else from it

def run(ctx):
    routes = gpd.read_file(ctx.input_file("routes"))     # a path to a copy of the input
    tolerance = ctx.param("tolerance")                   # validated, default filled in
    ...
    out.to_file(ctx.output_path("located"), driver="GPKG", engine="pyogrio")
    ctx.warn("lrs.off_route", Severity.WARN, "3 of 120 points are off every route")
    return {"located": 117, "off_route": 3}              # a small summary, no rows
```

| From a `run_python` script | In a tool |
| --- | --- |
| `in/routes.gpkg`, `inputs.json` | `ctx.input_file("routes")` |
| constants at the top you edited per run | `ctx.param("name")`, declared in `params` |
| `out/result.gpkg` | `ctx.output_path("result")`: GeoPackage for a layer, CSV for a table, JSON for a report |
| `print(...)` | `ctx.warn(id, severity, message)` for findings, the returned dict for the summary |
| `sys.exit(1)`, assert | `raise ToolError("what is wrong and how to fix it")` |

Columbus adds the lines that connect `run` to it; leave out `if __name__ == "__main__"`,
`Plugin(...)` and `serve()`. Write every declared output, even an empty one.

Routes with measures (M values): read and write with `engine="pyogrio", use_arrow=True`
(`gpd.read_file(path, engine="pyogrio", use_arrow=True)`, and the same in `to_file`).
pyogrio's plain read drops M, and GeoPandas' plain write fails on it.

## Rules Columbus enforces (refused if broken)

- **No network**: no `socket`, `urllib`, `requests`, `http`, cloud SDKs, URLs or
  `/vsicurl/` paths. The sandbox has no network anyway; the check stops a tool that would
  only fail.
- **No credentials or environment**: no passwords, keys or tokens in the code, no
  `os.environ` or `os.getenv`, no `getpass` or `keyring`. A tool takes what it needs as
  parameters, and never a secret.
- **No paths of its own**: no absolute paths (`/data/...`, `C:\...`, `~/...`). It reads only
  `ctx.input_file(...)` and writes only `ctx.output_path(...)`; it cannot read other data.
- **No programs or dynamic code**: no `subprocess`, `os.system`, `multiprocessing`,
  `ctypes`, `eval`, `exec`, `importlib`, `pickle`.
- **Names match the manifest**: every `ctx.input_file`, `ctx.param`, `ctx.output_path` and
  `ctx.warn` names a declared input, parameter, output or check, and every declared input
  and output is used. Write the names out as text so they can be checked.
- **Plain text**: at most 100 KB, lines of 200 characters or fewer, no invisible or
  right-to-left characters.
- **Allowed modules only**: a tool imports only from math, cmath, statistics, decimal,
  fractions, numbers, random, re, json, csv, datetime, time, calendar, zoneinfo, itertools,
  functools, operator, collections, heapq, bisect, array, dataclasses, enum, typing, abc,
  contextlib, copy, string, textwrap, unicodedata, uuid, hashlib, base64, struct, pathlib,
  logging, warnings, zipfile, geopandas, shapely, pyproj, pandas, numpy, pyogrio, fiona, rasterio,
  scipy and networkx (plus `from columbus_plugin import ...` helpers). No `os`: build paths
  with `pathlib.Path(ctx.output_path(...))`. Use only public names (nothing starting with
  `_` on another object), never reach a module through another object (`shutil.os`,
  `logging.os`), and never filter with an expression given as text: `gdf.query(...)` and
  `.eval(...)` take only a fixed expression written in the code, with no `@names`. Filter
  with a mask instead: `gdf[gdf[field] > value]`. No pickles (`read_pickle`,
  `allow_pickle=True`), no clipboard (`read_clipboard`, `to_clipboard`), no library settings
  (`pd.set_option`, `rasterio.Env(...)` with settings, anything named `config`, `backend`,
  `setenv`, `data_dir` or `network`, and no `backend=` keyword), and not `numpy.ctypeslib`,
  `numpy.f2py`, `numpy.testing` or `logging.handlers`. `zipfile` is fine for a zipped output. These rules make the code you read the code that runs; the sandbox
  (no network, writing only its outputs) is what contains it either way.
- **Nothing at import time**: the top level holds only a docstring, imports, functions,
  classes and names given plain values (numbers, text, `True`/`False`/`None`, and lists,
  tuples, sets and dicts of them: `METRES = {"mile": 1609.344}`). No calls, loops or `if`
  there; the work goes in `run`. That includes the usual one-liners: write
  `log = logging.getLogger("my-tool")` and `crs = pyproj.CRS.from_epsg(3081)` inside
  `run(ctx)` (or a helper it calls), not at the top. Class bodies hold only a docstring, methods and plain
  values; no metaclass or class keywords, base classes by name only. Default values are
  plain values (use `None` and set the real default inside), annotations only name types.
- **Reads as it runs**: names in plain ASCII (no look-alike letters); each top-level name
  bound once (`run` exactly once, never rebound, no `global`); `run` has no decorator, and
  helpers only `staticmethod`, `classmethod`, `property`, `dataclasses.dataclass`,
  `functools.cache` or `functools.lru_cache`; no Python special attributes or names
  (`__code__`, `__globals__`, `__dict__`, `__builtins__`... `__init__` and `__name__` are
  fine), no `getattr`/`setattr`/`eval`/`exec`/`globals()` (called or passed along), no
  `from x import *` (name each import); never use the names of those built-ins for
  anything of your own (`input = ...` is refused: call it `points`); import each name once,
  as one thing, and use modules like `os` only as `os.something`; no `timeit`, `pydoc`,
  `pkgutil`, `logging.config`, `operator.attrgetter`/`methodcaller` or `string.Formatter`,
  and no Python special names inside text (`"{0.__globals__}"`), no `sys.modules`, frames, `inspect`,
  `builtins`, `__main__`, `types` or `gc`; no coding declaration (`# coding: ...`) or
  byte-order mark; at most 2,000 lines, no more than 5 blank lines or 40 comment lines in
  a row.
- **Nothing kept between runs**: each run is a new process with an empty folder; do not
  rely on files left by an earlier run.

## The manifest: written for another assistant

**`name`**: lower_snake_case, verb first, saying the job: `locate_measures`,
`check_event_gaps`, `summarize_pavement_by_district`. Not `tool1`, `my_script`, `lrs`.

**`title`**: for people, a few words: "Locate points along routes".

**`description`**: what it does, its result, and when to use it, for an assistant that has
never seen this conversation. The first sentence is the one-line summary `search_tools`
shows, so make it complete: "Locate each point on the nearest route and give its route id
and measure." Say what the measure is measured from and in what unit. Describe; do not
instruct: wording aimed at an assistant ("you must", "always call", "do not tell the user")
is refused, as are URLs and mentions of credentials.

**`not_for`**: when not to use it and what to use instead: "Lines or polygons (locate their
start points first); calibrated LRS measures (this measures length along the line)."

**`inputs`**: workspace layers or tables, by name.

- `kind`: `layer` (geometry) or `table` (rows only).
- `description`: what it should hold, one row per what.
- `geometry` (layers): `point`, `line`, `polygon` or `any`.
- `crs` (layers): what it must be in: `"EPSG:3081"`, `"any projected CRS"`, or
  `"any (reprojected to the routes' CRS)"` when the tool reprojects. If it needs a projected
  CRS, check it in code and refuse with a fix ("reproject it with vector_op first").
- `fields`: the fields it reads, with `type` (`text`, `integer`, `number`, `boolean`,
  `date`, `any`). When a field name varies between datasets, take it as a string parameter
  with a default (`route_field: RTE_ID`) and check it exists.

**`params`**: scalars with `type` (`number`, `integer`, `string`, `boolean`), a
`description`, and:

- `unit` for every number (required): `foot`, `mile`, `metre`, `degree`, `count`, `ratio`.
  Name the unit in the parameter when it helps (`max_distance` in `foot`).
- `default` for anything with a sensible one; `min` and `max`; `optional: true` for one that
  may be left out with no default.
- **Every text parameter** has either `enum` (a fixed set: `measure_unit: [mile, foot,
  kilometre, metre]`) or a `format`: `field_name` (a column name: letters, digits and `_`)
  or `label` (a short label or code, such as a route id: letters, digits, spaces and
  `- _ . , : / # & +`, up to 80). Nothing else is accepted, so what another assistant passes
  at run time is data, never code, an expression or a template. Use field names as
  `gdf[field]`, never inside a `.query()` text. A parameter never chooses which function
  runs (`gdf.agg(ctx.param("stat"))` is refused): give it an `enum` and look the function
  up in a fixed dict, `{"sum": "sum", "mean": "mean"}[ctx.param("stat")]`. Spatial-index
  lookups (`gdf.sindex.query(geom, predicate="intersects")`, `STRtree(...).query(g)`) are
  fine. Use only ctx's documented methods (`param`, `input_file`, `output_path`,
  `set_output`, `warn`, `progress`, `cancelled`, `check_cancelled`), taking each parameter
  with `ctx.param(name)`. No parameter may be named for a library setting (`allow_pickle`,
  `backend`, `engine`, `options`, `config`, `env`, `driver_options`).

**`outputs`**: `kind` `layer` (GeoPackage), `table` (CSV) or `report` (JSON), with a
`description`, and for layers `geometry` and `crs` (`"same as routes"`). List the `fields`
it adds, with units in their descriptions.

**`checks`**: every id the code reports with `ctx.warn`, and what it means:
`{"lrs.off_route": "Points farther than max_distance from every route"}`. Use dotted
lower-case ids, the domain first (`lrs.`, `pavement.`, `parcel.`). Use `fail` for "this
result should not be used as it is", `warn` for "probably wrong somewhere", `info` for
context. A fail blocks export of anything made from the output, as Columbus's own fails do.
Columbus also checks each GeoPackage or CSV output as a layer (CRS, validity, empties,
duplicates); you can name those ids (`geometry.valid`) too.

**`examples`**: one to five calls as `run_tool` would get them, with placeholders for
handles: `{"routes": "ws://<session>/results/routes.gpkg", "points": "...", "measure_unit": "mile"}`.

## Good tools

- **Small and single-purpose.** One job, a few inputs. Two jobs are two tools.
- **Deterministic.** The same inputs and parameters give the same outputs: no clocks, random
  numbers or state kept between runs. Sort when order matters.
- **Validate inputs first** and fail with a message that says what is wrong and how to fix
  it: the missing field and the fields there are, the CRS it needs, the geometry type it
  got. A `ToolError` message reaches the assistant as the refusal.
- **Keep every row.** Do not drop rows silently: keep them with empty results and report a
  check with the count.
- **Units everywhere.** Convert using the CRS's own units
  (`crs.axis_info[0].unit_conversion_factor`, metres per unit), never by assuming feet or
  metres. Measures and distances say their unit in the field description.
- **Small summaries.** Return counts and settings, never rows or values from the data: the
  data goes in the outputs, which carry the inputs' data labels.

## Worked example: locating measures along routes (LRS)

A DOT analyst often needs each crash or sign point's route and milepoint. The script that
did it once becomes this tool. Code ([examples/tools/locate-measures/code.py](../../../examples/tools/locate-measures/code.py)):

```python
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
```

`line.project(point)` is the distance along the line to the point nearest it, in the CRS's
units, measured from the line's first vertex; dividing by the CRS units in one mile gives
miles. The manifest
([examples/tools/locate-measures/propose.json](../../../examples/tools/locate-measures/propose.json)),
shortened:

```json
{
  "name": "locate_measures",
  "title": "Locate points along routes",
  "description": "Locate each point on the nearest route and give its route id and its measure (distance along the route from its start) in the measure unit asked for. Use it for crash, sign, bridge or asset points that need a route and milepoint ...",
  "not_for": "Lines or polygons (locate their start or centroid points first), ... or calibrated LRS measures (this measures distance along the line, not the official milepoints).",
  "inputs": {
    "routes": {"kind": "layer", "description": "Route centerlines, one line per route, digitized in the direction of increasing measure.",
               "geometry": "line", "crs": "any projected CRS (EPSG:3081 for Texas statewide work)",
               "fields": [{"name": "RTE_ID", "type": "text", "description": "the route id (the field is chosen with route_field)"}]},
    "points": {"kind": "layer", "description": "The points to locate, such as crashes or signs.",
               "geometry": "point", "crs": "any (reprojected to the routes' CRS)"}
  },
  "params": {
    "route_field": {"type": "string", "format": "field_name", "description": "The routes field holding the route id", "default": "RTE_ID"},
    "measure_unit": {"type": "string", "description": "Unit of the measures written", "enum": ["mile", "foot", "kilometre", "metre"], "default": "mile"},
    "max_distance": {"type": "number", "description": "Farthest a point may be from a route and still be located", "unit": "foot", "default": 100, "min": 0, "max": 5280}
  },
  "outputs": {
    "located": {"kind": "layer", "description": "Every input point with ROUTE_ID, MEASURE (in measure_unit) and OFFSET_FT ...",
                "geometry": "point", "crs": "same as routes", "fields": [{"name": "MEASURE", "type": "number", "description": "in measure_unit, from the route's first vertex"}]}
  },
  "checks": {"lrs.off_route": "Points farther than max_distance from every route: they have no route id or measure"},
  "examples": [{"description": "Locate crashes on state routes, measures in miles",
                "arguments": {"routes": "ws://<session>/results/routes.gpkg", "points": "ws://<session>/results/crashes.gpkg", "measure_unit": "mile"}}],
  "code": "<the code above>",
  "test": {"inputs": {"routes": "ws://<session>/results/routes-1a2b.gpkg", "points": "ws://<session>/results/crashes-3c4d.gpkg"}}
}
```

The inverse, placing events at their measures (an events table with route id and measure
in, points out), uses `line.interpolate(distance)` the same way:
[examples/tools/place-measures/code.py](../../../examples/tools/place-measures/code.py).

Both measure from each line's first vertex along its digitized length. Where the
organization's routes carry calibrated measures (M values, calibration points, equations),
say so in `not_for`, or read the M values instead; check the context pack's `lrs` entries
(`context_search {"kind": "lrs"}`) for how the organization measures its routes.

## What the person sees

The test-run approval shows the whole `tool.py` and the inputs. In Settings, Tools, they
see the title, description, `not_for`, the test run's outputs and check ids, your warnings,
and the full manifest and code, and install with Touch ID or Windows Hello, or decline.
Installed tools are listed with when they were installed, and can be removed at once.
Proposals record only counts and check ids from the test run, never the data.
