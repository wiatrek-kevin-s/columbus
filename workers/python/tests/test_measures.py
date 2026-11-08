"""M (measure) and Z values survive end to end (AF-17, REQ-24).

A GeoPackage route layer of LINESTRING M routes is read with query_layer
(in memory and streamed, through the gpkg and files engines), put through
the vector operations that keep vertices, handed to a plugin as an extract
and to a personal tool as a workspace file, and exported to gpkg, gdb and
shp. Every output's first vertex carries the source's M value. buffer and
simplify drop M and say so (geometry.m_dropped). The map's GeoJSON is 2D.

The Oracle side of the same chain is in test_db_engines.py (CI's Oracle
job). Skipped when the GIS engine (geopandas, pyogrio, shapely 2.1+,
pyproj) is not installed.
"""

import json
import os
import struct
import subprocess
import sys
import tempfile
import textwrap
import unittest
import warnings

HERE = os.path.dirname(os.path.abspath(__file__))
SDK_DIR = os.path.dirname(HERE)
sys.path.insert(0, SDK_DIR)

try:
    import geopandas as gpd
    import pandas as pd
    import pyogrio
    import shapely

    import columbus_gis as g

    HAVE_GIS = hasattr(shapely, "has_m")
except ImportError:  # pragma: no cover
    HAVE_GIS = False

import columbus_worker as w  # noqa: E402

# pyogrio says it reads M types as 2D even where it reads them through
# Arrow (which keeps M); the tests check the values themselves.
warnings.filterwarnings("ignore", r"Measured \(M\) geometry types are not supported", UserWarning)

ROUTES = 300  # rows; the streamed reads use a threshold of 100


def route_m(i: int) -> float:
    """Route i's first measure: unique, so a wrong row shows."""
    return 1000.0 + 10 * i


def routes_frame(n: int = ROUTES):
    """n two-vertex LINESTRING M routes in Austin (EPSG:4326), district 14
    for even ids and 9 for odd, measures route_m(i) and route_m(i) + 5."""
    geoms = [shapely.from_wkt(
        f"LINESTRING M (-97.8 {30.2 + i * 0.0001} {route_m(i)}, -97.7999 {30.2001 + i * 0.0001} {route_m(i) + 5})")
        for i in range(n)]
    return gpd.GeoDataFrame({"route_id": [f"R{i:04d}" for i in range(n)], "district": [14 if i % 2 == 0 else 9 for i in range(n)]},
                            geometry=geoms, crs="EPSG:4326")


def first_m(geom) -> float:
    return float(shapely.get_coordinates(geom, include_m=True)[0][-1])


def read_m(path: str, layer: str | None = None):
    """A written layer read back with its M values (through Arrow), as a
    GeoDataFrame."""
    kw = {"layer": layer} if layer else {}
    meta, table = pyogrio.read_arrow(path, **kw)
    gname = meta.get("geometry_name") or "wkb_geometry"
    geoms = shapely.from_wkb(table.column(gname).to_pylist())
    df = table.drop([gname]).to_pandas()
    return gpd.GeoDataFrame(df, geometry=list(geoms), crs=meta.get("crs"))


def r_geom(conn, default):
    """The geometry column name the esri engine reports for a query."""
    cols, _rows, _hit, _cells, geoms = w._fetch(conn, "SELECT * FROM routes", 1, 30)
    return cols[next(iter(geoms))] if geoms else default


def source_vertices(df) -> set:
    return {tuple(round(v, 6) for v in c) for c in shapely.get_coordinates(df.geometry.values, include_m=True)}


TOOL = textwrap.dedent('''
    import geopandas as gpd
    import shapely
    from columbus_plugin import Plugin

    plugin = Plugin("personal.first-measure", "1.0.0")

    @plugin.tool("run", params=["routes"], outputs=["out"])
    def run(ctx):
        routes = gpd.read_file(ctx.input_file("routes"), engine="pyogrio", use_arrow=True)
        c = shapely.get_coordinates(routes.geometry.values[:1], include_m=True)
        routes.to_file(ctx.output_path("out"), driver="GPKG", engine="pyogrio", use_arrow=True)
        return {"first_m": float(c[0][-1]), "has_m": bool(shapely.has_m(routes.geometry.values).all())}

    if __name__ == "__main__":
        plugin.serve()
''')


def run_personal_tool(test, layer: str, folder: str) -> tuple[dict, str]:
    """Run TOOL through the plugin host on a copy of a workspace layer, as
    Columbus hands a personal tool its inputs. (The reply's summary, the
    output layer's path.)"""
    d = os.path.join(folder, "tool")
    run_dir = os.path.join(folder, "run")
    os.makedirs(d, exist_ok=True)
    os.makedirs(run_dir, exist_ok=True)
    with open(os.path.join(d, "tool.py"), "w", encoding="utf-8") as f:
        f.write(TOOL)
    copy = os.path.join(run_dir, "routes.gpkg")
    with open(layer, "rb") as a, open(copy, "wb") as b:
        b.write(a.read())
    out = os.path.join(run_dir, "out.gpkg")
    host = os.path.join(SDK_DIR, "columbus_plugin_host.py")
    msg = {"jsonrpc": "2.0", "id": 1, "method": "run", "params": {
        "run": "r1", "tool": "run", "inputs": {"routes": {"kind": "file", "path": copy}},
        "outputs": {"out": {"type": "layer", "path": out}}}}
    p = subprocess.run([sys.executable, "-I", "-u", host, d, "tool.py"], input=json.dumps(msg) + "\n",
                       capture_output=True, text=True, timeout=120)
    replies = [json.loads(line) for line in p.stdout.splitlines() if line.strip().startswith("{")]
    reply = next((r for r in replies if r.get("id") == 1), None)
    test.assertIsNotNone(reply, p.stderr)
    test.assertIn("result", reply, p.stderr)
    return reply["result"]["summary"], out


def measure_chain(test, conn: dict, sql: str, geom: str, folder: str, rows: int,
                  key: str = "route_id", district: str = "district") -> None:
    """The whole REQ-24 chain on one source of LINESTRING M routes (route i
    has id R<i>, first measure route_m(i)): query_layer in memory and
    streamed, reproject, join, spatial_join, dissolve, a plugin extract, a
    personal tool, exports to gpkg, gdb and shp; and buffer drops M with
    geometry.m_dropped. Every output's first vertex has the source's M."""
    ws = os.path.join(folder, "ws", "results")

    def check_rows(df):
        test.assertTrue(len(df))
        for rid, shape in zip(df[key], df.geometry):
            test.assertTrue(shapely.has_m(shape), f"{rid}: {shape.wkt[:60]}")
            test.assertAlmostEqual(first_m(shape), route_m(int(str(rid)[1:])), places=6, msg=rid)

    def query(stem, **kw):
        p = {"connection": conn, "sql": sql, "max_rows": 10_000, "geometry_column": geom,
             "crs_spec": {"code": "EPSG:4326"}, "return_rows": 3, "output_stem": os.path.join(ws, stem),
             "layer_name": "routes"}
        p.update(kw)
        return w.query_layer(p)

    def op(name, inputs, stem, **params):
        return w.vector_op({"op": name, "inputs": inputs, "params": params,
                            "output_stem": os.path.join(ws, stem), "max_rows": 100_000})

    mem = query("mem")
    streamed = query("streamed", stream_after=100)
    test.assertEqual((mem["streamed"], streamed["streamed"]), (False, True))
    for r in (mem, streamed):
        test.assertEqual(r["summary"]["rows"], rows)
        check_rows(read_m(r["output_path"]))
    layer = mem["output_path"]

    rep = op("reproject", {"layer": layer}, "rep", crs="EPSG:3081")
    check_rows(read_m(rep["output_path"]))
    test.assertGreater(shapely.get_coordinates(read_m(rep["output_path"]).geometry[0])[0][0], 1_000_000)

    table = os.path.join(ws, "districts.csv")
    pd.DataFrame({district: ["14", "9"], "district_name": ["Austin", "Waco"]}).to_csv(table, index=False)
    joined = read_m(op("join", {"left": layer, "right": table}, "join", left_key=district, key_as_text=True)["output_path"])
    test.assertEqual(set(joined["district_name"]), {"Austin", "Waco"})
    check_rows(joined)

    zones = os.path.join(ws, "zones.gpkg")
    gpd.GeoDataFrame({"zone": ["north", "south"]},
                     geometry=[shapely.box(-97.9, 30.21, -97.6, 30.4), shapely.box(-97.9, 30.0, -97.6, 30.21)],
                     crs="EPSG:4326").to_file(zones, driver="GPKG", engine="pyogrio")
    sj = read_m(op("spatial_join", {"left": layer, "right": zones}, "sjoin")["output_path"])
    test.assertIn("zone", sj.columns)
    check_rows(sj)

    dis = op("dissolve", {"layer": layer}, "dissolve", by=district)
    out = read_m(dis["output_path"])
    test.assertEqual(len(out), 2)
    seen = source_vertices(read_m(layer))
    for shape in out.geometry:
        test.assertTrue(shapely.has_m(shape))
        test.assertIn(tuple(round(v, 6) for v in shapely.get_coordinates(shape, include_m=True)[0]), seen)
    test.assertNotIn("geometry.m_dropped", [c["id"] for c in dis["checks"]])

    from columbus_plugin import Extract

    ex_path = os.path.join(folder, "extracts", "routes.sqlite")
    w.extract({"connection": conn, "sql": sql, "max_rows": 10_000, "output_path": ex_path})
    ex = Extract(ex_path)
    try:
        got = list(ex.rows())
        test.assertEqual(len(got), rows)
        for row in got[:20]:
            shape = row.geometry()
            test.assertTrue(shape.has_m)
            test.assertEqual(shape.measures()[0], route_m(int(str(row[key])[1:])))
    finally:
        ex.close()

    summary, tool_out = run_personal_tool(test, streamed["output_path"], folder)
    first = read_m(streamed["output_path"])
    test.assertEqual(summary, {"first_m": first_m(first.geometry[0]), "has_m": True})
    check_rows(g.read_layer(tool_out))

    share = os.path.join(folder, "share")
    for src, tag in ((layer, "mem"), (streamed["output_path"], "streamed")):
        for fmt, ext in (("gpkg", ".gpkg"), ("gdb", ".gdb"), ("shp", ".shp")):
            stem = os.path.join(share, f"routes_{tag}_{fmt}")
            g.export_layer(src, stem, fmt)
            check_rows(read_m(stem + ext))

    buf = op("buffer", {"layer": rep["output_path"]}, "buf", distance=10, unit="m")
    test.assertFalse(shapely.has_m(read_m(buf["output_path"]).geometry.values).any())
    test.assertIn(("geometry.m_dropped", "info"), [(c["id"], c["severity"]) for c in buf["checks"]])


@unittest.skipUnless(HAVE_GIS, "GIS engine (with shapely 2.1+) not installed")
class MeasuresTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.root = os.path.join(cls.tmp.name, "data")
        os.makedirs(cls.root)
        cls.src = os.path.join(cls.root, "routes.gpkg")
        cls.routes = routes_frame()
        g.write_ogr(cls.routes, cls.src, "GPKG", layer="routes")
        cls.ws = os.path.join(cls.tmp.name, "ws", "results")
        # The streamed and in-memory query results, read once.
        cls.mem = cls.query("mem")
        cls.streamed = cls.query("streamed", stream_after=100)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    @classmethod
    def query(cls, stem, conn=None, sql="SELECT * FROM routes", geom="geom", **kw):
        p = {"connection": conn or {"kind": "gpkg", "path": cls.src}, "sql": sql, "max_rows": 10_000,
             "geometry_column": geom, "crs_spec": {"code": "EPSG:4326"}, "return_rows": 3,
             "output_stem": os.path.join(cls.ws, stem), "layer_name": "routes"}
        p.update(kw)
        return w.query_layer(p)

    def assert_first_m(self, df, key="route_id"):
        """Every row's first vertex has its source route's first M."""
        self.assertTrue(len(df))
        for rid, geom in zip(df[key], df.geometry):
            self.assertTrue(shapely.has_m(geom), f"{rid}: {geom.wkt[:60]}")
            self.assertAlmostEqual(first_m(geom), route_m(int(str(rid)[1:])), places=6, msg=rid)

    def op(self, op, inputs, stem, **params):
        return w.vector_op({"op": op, "inputs": inputs, "params": params,
                            "output_stem": os.path.join(self.ws, stem), "max_rows": 100_000})

    # -- the write that failed (AF-17) ---------------------------------------

    def test_ewkb_with_the_m_flag_is_written(self):
        """Oracle's geometry arrives as EWKB with the M flag; GDAL refused
        it ("WKB type 1073741826"). It is now written as ISO WKB."""
        iso = shapely.to_wkb(shapely.from_wkt("LINESTRING M (0 0 5, 1 1 6)"), flavor="iso", output_dimension=4)
        ewkb = w._ewkb(iso, 4326)
        self.assertEqual(struct.unpack_from("<I", ewkb, 1)[0], 2 | 0x40000000 | 0x20000000)
        geom = g.decode(ewkb, "ewkb")
        layer = g.build_layer(["id"], [[1]], "geom", [geom], g.crs_from_spec({"code": "EPSG:4326"}))
        path = g.write_layer(layer, os.path.join(self.ws, "ewkb"))
        self.assertEqual(read_m(path).geometry[0].wkt, "LINESTRING M (0 0 5, 1 1 6)")

    # -- query_features ------------------------------------------------------

    def test_query_in_memory_and_streamed_keep_m(self):
        self.assertFalse(self.mem["streamed"])
        self.assertTrue(self.streamed["streamed"])
        for r in (self.mem, self.streamed):
            self.assertEqual(r["summary"]["rows"], ROUTES)
            out = read_m(r["output_path"])
            self.assert_first_m(out)
            self.assertTrue(g._gpkg_has_m(r["output_path"]))
            # read_layer (every vector tool's input) keeps M too.
            self.assert_first_m(g.read_layer(r["output_path"]))

    def test_files_engine_keeps_m(self):
        """A files connection's FlatGeobuf, through its GeoPackage cache."""
        folder = os.path.join(self.tmp.name, "files")
        os.makedirs(folder, exist_ok=True)
        g.write_ogr(self.routes, os.path.join(folder, "routes.fgb"), "FlatGeobuf")
        conn = {"kind": "files", "path": folder, "cache": os.path.join(self.tmp.name, "cache", "c.gpkg")}
        names = [d["name"] for d in w._files().files_list(conn, 30)["datasets"]]
        self.assertEqual(len(names), 1, names)
        sql = f'SELECT * FROM "{names[0]}"'
        r = self.query("files_mem", conn=conn, sql=sql, geom="geometry")
        self.assert_first_m(read_m(r["output_path"]))
        r = self.query("files_streamed", conn=conn, sql=sql, geom="geometry", stream_after=100)
        self.assertTrue(r["streamed"])
        self.assert_first_m(read_m(r["output_path"]))

    def test_shapefile_and_filegdb_engines_keep_m(self):
        shp_dir = os.path.join(self.tmp.name, "shp")
        os.makedirs(shp_dir, exist_ok=True)
        g.write_ogr(self.routes, os.path.join(shp_dir, "routes.shp"), "ESRI Shapefile")
        gdb = os.path.join(self.tmp.name, "src.gdb")
        g.write_ogr(self.routes, gdb, "OpenFileGDB", layer="routes")
        for conn, geom in (({"kind": "shapefile", "path": shp_dir}, "GEOMETRY"), ({"kind": "filegdb", "path": gdb}, "SHAPE")):
            for stream_after in (None, 100):
                r = self.query(f"{conn['kind']}_{stream_after}", conn=conn, sql="SELECT * FROM routes",
                               geom=r_geom(conn, geom), stream_after=stream_after)
                self.assertEqual(r["streamed"], stream_after is not None, conn["kind"])
                self.assert_first_m(read_m(r["output_path"]))

    def test_bbox_filter_reads_m(self):
        r = self.query("box", bbox=[-97.81, 30.19, -97.79, 30.21], stream_after=50)
        self.assertGreater(r["summary"]["rows"], 50)
        self.assert_first_m(read_m(r["output_path"]))

    # -- vector operations that keep vertices -------------------------------

    def test_reproject_keeps_m(self):
        r = self.op("reproject", {"layer": self.mem["output_path"]}, "rep", crs="EPSG:3081")
        out = read_m(r["output_path"])
        self.assertEqual(out.crs.to_epsg(), 3081)
        self.assert_first_m(out)
        x, y = shapely.get_coordinates(out.geometry[0])[0]
        self.assertGreater(x, 1_000_000)  # really reprojected (Texas Centric Albers metres)
        self.assertNotIn("geometry.m_dropped", [c["id"] for c in r["checks"]])

    def test_reproject_keeps_z_beside_m(self):
        df = gpd.GeoDataFrame({"route_id": ["R0001"]}, geometry=[shapely.from_wkt(
            f"LINESTRING ZM (-97.8 30.2 150 {route_m(1)}, -97.79 30.21 151 {route_m(1) + 5})")], crs="EPSG:4326")
        out = g.to_crs(df, "EPSG:3081")
        c = shapely.get_coordinates(out.geometry[0], include_z=True, include_m=True)
        self.assertEqual([round(v, 6) for v in c[0][2:]], [150.0, route_m(1)])
        plain = df.to_crs("EPSG:3081")
        self.assertTrue(abs(c[0][0] - shapely.get_coordinates(plain.geometry[0])[0][0]) < 1e-6)

    def test_clip_keeps_m(self):
        mask = os.path.join(self.ws, "mask.gpkg")
        gpd.GeoDataFrame(geometry=[shapely.box(-97.81, 30.19, -97.79, 30.21)], crs="EPSG:4326").to_file(
            mask, driver="GPKG", engine="pyogrio")
        r = self.op("clip", {"layer": self.mem["output_path"], "mask": mask}, "clip")
        self.assert_first_m(read_m(r["output_path"]))

    def test_join_keeps_m(self):
        table = os.path.join(self.ws, "districts.csv")
        pd.DataFrame({"district": ["14", "9"], "name": ["Austin", "Waco"]}).to_csv(table, index=False)
        r = self.op("join", {"left": self.mem["output_path"], "right": table}, "join", left_key="district", key_as_text=True)
        out = read_m(r["output_path"])
        self.assertEqual(set(out["name"]), {"Austin", "Waco"})
        self.assert_first_m(out)

    def test_spatial_join_keeps_m(self):
        zones = os.path.join(self.ws, "zones.gpkg")
        gpd.GeoDataFrame({"zone": ["north", "south"]},
                         geometry=[shapely.box(-97.9, 30.21, -97.6, 30.4), shapely.box(-97.9, 30.0, -97.6, 30.21)],
                         crs="EPSG:4326").to_file(zones, driver="GPKG", engine="pyogrio")
        r = self.op("spatial_join", {"left": self.mem["output_path"], "right": zones}, "sjoin")
        out = read_m(r["output_path"])
        self.assertIn("zone", out.columns)
        self.assert_first_m(out)

    def test_dissolve_keeps_m(self):
        r = self.op("dissolve", {"layer": self.mem["output_path"]}, "dissolve", by="district")
        out = read_m(r["output_path"])
        self.assertEqual(sorted(out["district"]), [9, 14])
        seen = source_vertices(self.routes)
        for geom in out.geometry:
            self.assertTrue(shapely.has_m(geom))
            first = tuple(round(v, 6) for v in shapely.get_coordinates(geom, include_m=True)[0])
            self.assertIn(first, seen)  # x, y and the M the source has there
        self.assertNotIn("geometry.m_dropped", [c["id"] for c in r["checks"]])

    # -- buffer and simplify drop M, and say so -----------------------------

    def test_buffer_drops_m_with_a_check(self):
        rep = self.op("reproject", {"layer": self.mem["output_path"]}, "rep_buf", crs="EPSG:3081")["output_path"]
        r = self.op("buffer", {"layer": rep}, "buf", distance=10, unit="m")
        out = read_m(r["output_path"])
        self.assertFalse(shapely.has_m(out.geometry.values).any())
        self.assertFalse(g._gpkg_has_m(r["output_path"]))
        c = next(c for c in r["checks"] if c["id"] == "geometry.m_dropped")
        self.assertEqual(c["severity"], "info")

    def test_simplify_drops_m_with_a_check(self):
        rep = self.op("reproject", {"layer": self.mem["output_path"]}, "rep_simp", crs="EPSG:3081")["output_path"]
        r = self.op("simplify", {"layer": rep}, "simp", tolerance=1, unit="m")
        self.assertIn("geometry.m_dropped", [c["id"] for c in r["checks"]])

    def test_layer_without_m_gets_no_check(self):
        flat = os.path.join(self.ws, "flat.gpkg")
        gpd.GeoDataFrame(geometry=[shapely.LineString([(1_200_000, 900_000), (1_200_100, 900_100)])], crs="EPSG:3081").to_file(
            flat, driver="GPKG", engine="pyogrio")
        r = self.op("buffer", {"layer": flat}, "flatbuf", distance=10, unit="m")
        self.assertNotIn("geometry.m_dropped", [c["id"] for c in r["checks"]])

    # -- exports ---------------------------------------------------------------

    def test_exports_to_gpkg_gdb_and_shp_keep_m(self):
        share = os.path.join(self.tmp.name, "share")
        for src, tag in ((self.mem["output_path"], "mem"), (self.streamed["output_path"], "streamed")):
            for fmt, ext in (("gpkg", ".gpkg"), ("gdb", ".gdb"), ("shp", ".shp")):
                stem = os.path.join(share, f"routes_{tag}_{fmt}")
                r = g.export_layer(src, stem, fmt)
                self.assertTrue(os.path.exists(stem + ext), r)
                self.assert_first_m(read_m(stem + ext))
        # Reprojected for delivery, too.
        stem = os.path.join(share, "routes_state")
        g.export_layer(self.mem["output_path"], stem, "gdb", crs="EPSG:2277")
        out = read_m(stem + ".gdb")
        self.assertEqual(out.crs.to_epsg(), 2277)
        self.assert_first_m(out)

    def test_other_exports_do_not_fail_on_m(self):
        share = os.path.join(self.tmp.name, "share_other")
        for fmt in ("geojson", "fgb", "csv"):
            r = g.export_layer(self.mem["output_path"], os.path.join(share, f"r_{fmt}"), fmt)
            self.assertTrue(r["files"])
        with open(os.path.join(share, "r_csv.csv"), encoding="utf-8") as f:
            self.assertIn("LINESTRING M", f.read())

    # -- extracts -------------------------------------------------------------

    def test_plugin_extract_keeps_m(self):
        from columbus_plugin import Extract

        out = os.path.join(self.tmp.name, "extracts", "routes.sqlite")
        w.extract({"connection": {"kind": "gpkg", "path": self.src}, "sql": "SELECT route_id, geom FROM routes",
                   "max_rows": 1000, "output_path": out})
        ex = Extract(out)
        try:
            rows = list(ex.rows())
            self.assertEqual(len(rows), ROUTES)
            for row in rows[:20]:
                geom = row.geometry()
                self.assertTrue(geom.has_m)
                self.assertEqual(geom.measures()[0], route_m(int(row["route_id"][1:])))
                self.assertEqual(len(geom.vertices[0]), 3)
        finally:
            ex.close()

    def test_personal_tool_reads_and_writes_m(self):
        """A personal tool gets the workspace layer as a file (a copy, as
        Columbus hands it over) and reads its M through Arrow."""
        summary, out = run_personal_tool(self, self.streamed["output_path"], os.path.join(self.tmp.name, "pt"))
        self.assertEqual(summary, {"first_m": route_m(0), "has_m": True})
        # The output, checked as Columbus checks a tool's outputs, keeps M.
        self.assert_first_m(g.read_layer(out))
        g.verify_layer(out)

    def test_the_whole_chain_on_the_geopackage(self):
        measure_chain(self, {"kind": "gpkg", "path": self.src}, "SELECT * FROM routes", "geom",
                      os.path.join(self.tmp.name, "chain"), ROUTES)

    # -- the map: 2D, no failure ----------------------------------------------

    def test_map_geojson_is_2d(self):
        fc = g.display_geojson(self.mem["output_path"])
        self.assertEqual(fc["shown"], ROUTES)
        coords = fc["features"][0]["geometry"]["coordinates"]
        self.assertEqual(len(coords[0]), 2)
        rows = g.read_rows(self.mem["output_path"], limit=2)
        self.assertEqual(rows["rows"][0][-1]["type"], "LineString")

    def test_map_snapshot_draws_m_layers(self):
        out = os.path.join(self.tmp.name, "snap.png")
        r = g.map_snapshot([self.mem["output_path"]], out)
        self.assertTrue(os.path.exists(out), r)


class SdkMeasureTests(unittest.TestCase):
    """The plugin SDK keeps Z and M values (no GIS engine needed)."""

    def test_vertices_and_measures(self):
        from columbus_plugin import decode_geometry

        iso_zm = struct.pack("<BII", 1, 3002, 2) + struct.pack("<8d", 1, 2, 3, 4, 5, 6, 7, 8)
        geom = decode_geometry(iso_zm)
        self.assertEqual((geom.type, geom.has_z, geom.has_m), ("LineString", True, True))
        self.assertEqual(geom.parts, [(1, 2), (5, 6)])
        self.assertEqual(geom.vertices, [(1, 2, 3, 4), (5, 6, 7, 8)])
        self.assertEqual(geom.measures(), [4, 8])
        # EWKB M flag with SRID, multi with ISO members
        member = struct.pack("<BII", 1, 2002, 2) + struct.pack("<6d", 0, 0, 10, 1, 1, 11)
        multi = struct.pack("<BIII", 1, 5 | 0x40000000 | 0x20000000, 4326, 2) + member + member
        geom = decode_geometry(multi)
        self.assertEqual((geom.type, geom.srid, geom.has_m), ("MultiLineString", 4326, True))
        self.assertEqual(geom.measures(), [10, 11, 10, 11])
        flat = decode_geometry(struct.pack("<BII", 1, 2, 2) + struct.pack("<4d", 1, 2, 3, 4))
        self.assertEqual((flat.has_m, flat.measures()), (False, []))


class OracleMeasureHelperTests(unittest.TestCase):
    """How an Oracle LRS geometry's measures become M (no database)."""

    def test_lrs_z_becomes_m(self):
        iso_z = struct.pack("<BII", 1, 1002, 2) + struct.pack("<6d", 1, 2, 5, 3, 4, 6)
        out = w._ora_measured(w._ewkb(iso_z, 4326), 3302)
        self.assertEqual(struct.unpack_from("<I", out, 1)[0], 2 | 0x40000000 | 0x20000000)
        self.assertEqual(out[9:], iso_z[5:])
        # A 3D geometry that is not LRS keeps Z.
        same = w._ewkb(iso_z, 4326)
        self.assertEqual(w._ora_measured(same, 3002), same)
        # Already M: unchanged.
        iso_m = struct.pack("<BII", 1, 2002, 2) + struct.pack("<6d", 1, 2, 5, 3, 4, 6)
        m = w._ewkb(iso_m, 4326)
        self.assertEqual(w._ora_measured(m, 3302), m)
        self.assertIsNone(w._ora_measured(None, 3302))

    def test_multi_lrs_members_become_m(self):
        member = struct.pack("<BII", 1, 1002, 2) + struct.pack("<6d", 0, 0, 10, 1, 1, 11)
        raw = struct.pack("<BII", 1, 1005, 2) + member + member
        out = w._ora_measured(w._ewkb(raw, 4326), 3306)
        from columbus_plugin import decode_geometry

        geom = decode_geometry(out)
        self.assertEqual((geom.type, geom.has_z, geom.has_m), ("MultiLineString", False, True))
        self.assertEqual(geom.measures(), [10, 11, 10, 11])


if __name__ == "__main__":
    unittest.main()
