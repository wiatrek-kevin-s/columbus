"""Tests for the GIS engine (columbus_gis) and the worker's GIS tools.

Skipped when geopandas, pyogrio, shapely or pyproj are not installed.
"""

import math
import os
import sqlite3
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

try:
    import geopandas as gpd
    import pandas as pd
    import shapely
    from shapely.geometry import LineString, Point, Polygon

    import columbus_gis as g

    HAVE_GIS = True
except ImportError:  # pragma: no cover
    HAVE_GIS = False

import columbus_worker as w  # noqa: E402


def make_fixture(folder: str) -> str:
    """Roads around Austin in EPSG:3081, a county polygon, and attributes
    whose route codes are zero-padded on one side only."""
    path = os.path.join(folder, "austin.gpkg")
    lines = [
        LineString([(-97.80 + i * 0.01, 30.20), (-97.80 + i * 0.01, 30.30)]) for i in range(10)
    ]
    roads = gpd.GeoDataFrame(
        {"route_id": [f"{i:04d}" for i in range(10)], "district": [14] * 8 + [12] * 2, "lanes": [2, 4] * 5},
        geometry=lines,
        crs="EPSG:4326",
    ).to_crs("EPSG:3081")
    roads.to_file(path, layer="roads", driver="GPKG", engine="pyogrio")
    county = gpd.GeoDataFrame(
        {"name": ["Travis"]}, geometry=[Polygon([(-97.85, 30.15), (-97.74, 30.15), (-97.74, 30.35), (-97.85, 30.35)])], crs="EPSG:4326"
    ).to_crs("EPSG:3081")
    county.to_file(path, layer="county", driver="GPKG", engine="pyogrio")
    return path


@unittest.skipUnless(HAVE_GIS, "GIS engine not installed")
class CrsTests(unittest.TestCase):
    def test_facts(self):
        f = g.crs_facts(g.crs_from_spec({"org": "EPSG", "org_id": 3081}))
        self.assertEqual(f["code"], "EPSG:3081")
        self.assertFalse(f["geographic"])
        self.assertEqual(f["units"], "metre")
        self.assertIsNone(g.crs_from_spec({"wkt": "undefined"}))
        # Only authority codes: PROJ strings can point at init files.
        self.assertIsNone(g.crs_from_spec({"code": "+init=/etc/passwd:x"}))
        self.assertIsNone(g.crs_from_spec({"code": "EPSG:4326 +init=x"}))

    def test_area_of_use(self):
        wgs = g.crs_from_spec({"code": "EPSG:4326"})
        self.assertEqual([c["id"] for c in g.crs_checks(wgs, [-97.8, 30.2, -97.7, 30.3])], ["crs.info"])
        swapped = g.crs_checks(g.crs_from_spec({"code": "EPSG:4269"}), [30.2, -97.8, 30.3, -97.7])
        self.assertEqual(swapped[-1]["id"], "crs.area_of_use")
        self.assertIn("swapped", swapped[-1]["fix"])
        feet = g.crs_checks(wgs, [3000000, 10000000, 3100000, 10100000])
        self.assertEqual((feet[-1]["id"], feet[-1]["severity"]), ("crs.area_of_use", "fail"))
        tx = g.crs_from_spec({"code": "EPSG:3081"})
        far = g.crs_checks(tx, [-9e6, -9e6, -8.9e6, -8.9e6])
        self.assertEqual(far[-1]["id"], "crs.area_of_use")
        self.assertEqual(g.crs_checks(None, [0, 0, 1, 1])[0]["id"], "crs.present")

    def test_area_of_use_across_the_180_line_and_far_beyond(self):
        """NAD83's area runs from 167.65°E across the 180° line to 40.73°W;
        lower-48 data is inside it. A projection made for Texas used for the
        whole country is warned about; one used where it is meant is not."""
        from pyproj import Transformer

        def area(code, ll):
            crs = g.crs_from_spec({"code": code})
            ext = list(Transformer.from_crs(4326, crs, always_xy=True).transform_bounds(*ll)) if crs.is_projected else ll
            return [c for c in g.crs_checks(crs, ext) if c["id"] == "crs.area_of_use"]

        lower48 = [-124.09, 25.75, -67.03, 48.63]
        self.assertEqual(area("EPSG:4269", lower48), [])
        self.assertEqual(area("EPSG:4269", [-179.23, 51.17, 179.86, 71.44]), [], "Aleutians, across the line")
        self.assertEqual(area("EPSG:4269", [-160.3, 18.9, -154.8, 22.3]), [], "Hawaii")
        stretched = area("EPSG:3081", lower48)
        self.assertEqual(len(stretched), 1)
        self.assertIn("well beyond", stretched[0]["message"])
        self.assertIn("EPSG:5070", stretched[0]["fix"])
        self.assertEqual(area("EPSG:3081", [-97.9, 30.1, -97.6, 30.5]), [], "Austin in the Texas system")
        self.assertEqual(area("EPSG:2277", [-97.9, 30.1, -97.6, 30.5]), [], "Austin in Texas Central")
        self.assertIn("fall outside", area("EPSG:2277", [-96.9, 32.7, -96.7, 32.9])[0]["message"], "Dallas is in North Central")

    def test_geometry_checks(self):
        bowtie = Polygon([(0, 0), (1, 1), (1, 0), (0, 1), (0, 0)])
        gs = gpd.GeoSeries([bowtie, Point(0, 0), None, LineString()], crs="EPSG:3081")
        ids = {c["id"]: c for c in g.geometry_checks(gs)}
        self.assertEqual(ids["geometry.valid"]["detail"]["invalid"], 1)
        self.assertEqual(ids["geometry.empty"]["detail"], {"null": 1, "empty": 1})
        self.assertIn("geometry.mixed_types", ids)


@unittest.skipUnless(HAVE_GIS, "GIS engine not installed")
class WorkerGisTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = make_fixture(self.tmp.name)
        self.conn = {"kind": "gpkg", "path": self.path}

    def tearDown(self):
        self.tmp.cleanup()

    def test_describe_dataset(self):
        d = w.describe_dataset({"connection": self.conn, "dataset": "roads", "count_sql": 'SELECT count(*) FROM "roads" WHERE district = 14'})
        self.assertEqual(d["rows"], 8)
        self.assertEqual(d["crs"]["code"], "EPSG:3081")
        self.assertEqual(d["geometry_column"], "geom")
        self.assertEqual([c["id"] for c in d["checks"]], ["crs.info"])
        with self.assertRaises(w.WorkerError):
            w.describe_dataset({"connection": self.conn, "dataset": "nope"})

    def query(self, **kw):
        p = {"connection": self.conn, "sql": "SELECT * FROM roads WHERE district = 14", "max_rows": 1000,
             "geometry_column": "geom", "crs_spec": {"code": "EPSG:3081"},
             "output_stem": os.path.join(self.tmp.name, "ws", "results", "q1")}
        p.update(kw)
        return w.query_layer(p)

    def test_query_layer_writes_a_layer(self):
        r = self.query(hide_fields=["lanes"])
        self.assertEqual(r["rows_read"], 8)
        self.assertTrue(r["output_path"].endswith(".gpkg"))
        self.assertEqual(r["hidden_columns"], ["lanes"])
        out = gpd.read_file(r["output_path"])
        self.assertEqual(len(out), 8)
        self.assertEqual(out.crs.to_epsg(), 3081)
        self.assertNotIn("lanes", out.columns)
        self.assertEqual(r["summary"]["crs"]["code"], "EPSG:3081")

    def test_bbox_in_another_crs_is_applied_before_the_limit(self):
        # Box around the first three roads, given in lon/lat.
        r = self.query(bbox=[-97.805, 30.19, -97.775, 30.31], bbox_crs="EPSG:4326", max_rows=5)
        self.assertEqual(r["rows_read"], 3)
        self.assertFalse(r["hit_read_limit"])
        i = r["columns"].index("route_id")
        self.assertEqual(sorted(row[i] for row in r["rows"]), ["0000", "0001", "0002"])
        with self.assertRaises(w.WorkerError):
            self.query(bbox=[1, 1, 0, 0])
        with self.assertRaises(w.WorkerError):
            self.query(bbox=[0, 0, 1, 1], bbox_crs="+proj=longlat")

    def test_bbox_to_dataset_moves_the_box_as_query_layer_does(self):
        # T-13: the gateway pushes this box into the R-tree; query_layer
        # keeps the rows meeting the same box.
        box = [-97.805, 30.19, -97.775, 30.31]
        r = w.bbox_to_dataset({"bbox": box, "bbox_crs": "EPSG:4326", "crs_spec": {"code": "EPSG:3081"}})
        self.assertEqual(r["bbox"], g.bbox_in_layer_crs(box, g.crs_from_spec({"code": "EPSG:4326"}),
                                                         g.crs_from_spec({"code": "EPSG:3081"})))
        self.assertFalse(r["geographic"])
        self.assertGreater(r["bbox"][0], 1_000_000)
        # Same CRS, or none given: the box as it is.
        same = w.bbox_to_dataset({"bbox": [1, 2, 3, 4], "crs_spec": {"code": "EPSG:4326"}})
        self.assertEqual(same, {"bbox": [1.0, 2.0, 3.0, 4.0], "geographic": True})
        for bad in ({"bbox": [1, 1, 0, 0]}, {"bbox": [0, 0, 1]}, {"bbox": [0, 0, 1, 1], "bbox_crs": "+proj=longlat"},
                    {"bbox": [0, 0, 1, 1], "bbox_crs": "EPSG:4326"}):
            with self.assertRaises(w.WorkerError):
                w.bbox_to_dataset(bad)

    def test_rtree_is_reported_and_serves_the_box(self):
        # GDAL writes the R-tree; the listing says the gateway may use it.
        d = w.describe_dataset({"connection": self.conn, "dataset": "roads"})
        self.assertIs(d["spatial_index"], True)
        box = w.bbox_to_dataset({"bbox": [-97.805, 30.19, -97.775, 30.31], "bbox_crs": "EPSG:4326",
                                 "crs_spec": {"code": "EPSG:3081"}})["bbox"]
        sql = ("SELECT * FROM (SELECT * FROM roads columbus_filtered WHERE (district = 14) AND "
               "(columbus_filtered.rowid IN (SELECT \"id\" FROM \"rtree_roads_geom\" WHERE \"minx\" <= {2} AND "
               "\"maxx\" >= {0} AND \"miny\" <= {3} AND \"maxy\" >= {1})) LIMIT -1 OFFSET 0) AS roads").format(*box)
        pushed = self.query(sql=sql, bbox=[-97.805, 30.19, -97.775, 30.31], bbox_crs="EPSG:4326")
        plain = self.query(bbox=[-97.805, 30.19, -97.775, 30.31], bbox_crs="EPSG:4326")
        i = pushed["columns"].index("route_id")
        self.assertEqual(sorted(r[i] for r in pushed["rows"]), sorted(r[i] for r in plain["rows"]))
        self.assertEqual(pushed["rows_read"], 3)
        db = sqlite3.connect(self.path)
        plan = " ".join(r[-1] for r in db.execute("EXPLAIN QUERY PLAN " + sql))
        db.close()
        self.assertIn("rtree_roads_geom VIRTUAL TABLE INDEX", plan)
        # Without the R-tree: not reported.
        db = sqlite3.connect(self.path)
        for (n,) in db.execute("SELECT name FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'rtree_roads_geom%'").fetchall():
            db.execute(f'DROP TRIGGER "{n}"')
        db.execute("DROP TABLE rtree_roads_geom")
        db.commit()
        db.close()
        self.assertIs(w.describe_dataset({"connection": self.conn, "dataset": "roads"})["spatial_index"], False)

    def layer(self, sql, stem):
        return self.query(sql=sql, output_stem=os.path.join(self.tmp.name, "ws", "results", stem))["output_path"]

    def op(self, op, inputs, params=None, stem="out"):
        return w.vector_op({"op": op, "inputs": inputs, "params": params or {}, "output_stem": os.path.join(self.tmp.name, "ws", "results", stem)})

    def test_buffer_refuses_degrees_and_works_in_feet(self):
        roads = self.layer("SELECT * FROM roads", "roads")
        wgs = self.op("reproject", {"layer": roads}, {"crs": "EPSG:4326"}, "wgs")
        self.assertEqual(wgs["summary"]["crs"]["code"], "EPSG:4326")
        with self.assertRaises(w.WorkerError) as cm:
            self.op("buffer", {"layer": wgs["output_path"]}, {"distance": 100, "unit": "ft"})
        self.assertIn("crs.measure", cm.exception.message)
        self.assertIn("EPSG:3081", cm.exception.message)
        with self.assertRaises(w.WorkerError):
            self.op("buffer", {"layer": roads}, {"distance": 100})  # no unit
        b = self.op("buffer", {"layer": roads}, {"distance": 100, "unit": "ft"}, "buf")
        out = gpd.read_file(b["output_path"])
        road = gpd.read_file(roads).geometry.iloc[0]
        # The buffer's edge is 100 ft (30.48 m) from the road. Round ends are
        # drawn with 8 segments per quarter circle, so the nearest edge point
        # can sit up to r * (1 - cos(pi / 32)) inside the true circle.
        chord = 30.48 * (1 - math.cos(math.pi / 32))
        self.assertAlmostEqual(out.geometry.iloc[0].exterior.distance(road), 30.48, delta=chord + 0.01)
        self.assertAlmostEqual(out.geometry.iloc[0].exterior.hausdorff_distance(road), 30.48, delta=0.5)
        self.assertIn("crs.measure", [c["id"] for c in b["checks"]])

    def test_join_flags_padded_keys(self):
        roads = self.layer("SELECT * FROM roads", "roads")
        csv = os.path.join(self.tmp.name, "ws", "results", "attrs.csv")
        pd.DataFrame({"route_id": [str(i) for i in range(10)], "surface": ["asphalt"] * 10}).to_csv(csv, index=False)
        j = self.op("join", {"left": roads, "right": csv}, {"left_key": "route_id"}, "joined")
        c = next(c for c in j["checks"] if c["id"] == "join.unmatched")
        self.assertEqual(c["severity"], "fail")
        self.assertIn("10 of 10", c["message"])
        self.assertIn('"pad": 4', c["fix"])
        # The fix it names works, and says what it did.
        j = self.op("join", {"left": roads, "right": csv},
                    {"left_key": "route_id", "key_as_text": True, "pad": 4}, "joined_padded")
        c = next(c for c in j["checks"] if c["id"] == "join.unmatched")
        self.assertIn("0 of 10", c["message"])
        self.assertIn("join.keys_normalized", [c["id"] for c in j["checks"]])
        out = gpd.read_file(j["output_path"])
        self.assertEqual(sorted(out["route_id"])[:2], ["0000", "0001"])  # the source's own values
        self.assertNotIn("__columbus_left_key", out.columns)
        self.assertEqual(list(out["surface"].unique()), ["asphalt"])
        with self.assertRaises(w.WorkerError):
            self.op("join", {"left": roads, "right": csv}, {"left_key": "route_id", "trim": "false"}, "bad_flag")
        # Spacing and letter case.
        pd.DataFrame({"route_id": [f" {i:04d} " for i in range(10)], "surface": ["asphalt"] * 10}).to_csv(csv, index=False)
        j = self.op("join", {"left": roads, "right": csv}, {"left_key": "route_id"}, "joined_spaces")
        c = next(c for c in j["checks"] if c["id"] == "join.unmatched")
        self.assertIn('"trim": true', c.get("fix", ""))
        # A number key against a text key is refused with the fix, not a crash.
        pd.DataFrame({"district": ["14", "12"], "name": ["Austin", "Yoakum"]}).to_csv(csv, index=False)
        with self.assertRaises(w.WorkerError) as cm:
            self.op("join", {"left": roads, "right": csv}, {"left_key": "district"}, "joined_types")
        self.assertIn("join.key_types", cm.exception.message)
        j = self.op("join", {"left": roads, "right": csv}, {"left_key": "district", "key_as_text": True}, "joined_text")
        c = next(c for c in j["checks"] if c["id"] == "join.unmatched")
        self.assertIn("0 of 10", c["message"])
        # Duplicate keys on the right are flagged as many-to-many.
        pd.DataFrame({"route_id": ["0001", "0001", "0002"], "x": [1, 2, 3]}).to_csv(csv, index=False)
        j = self.op("join", {"left": roads, "right": csv}, {"left_key": "route_id"}, "joined2")
        ids = [c["id"] for c in j["checks"]]
        self.assertIn("join.many_to_many", ids)

    def test_overlays_align_crs_and_report(self):
        roads = self.layer("SELECT * FROM roads", "roads")
        county = self.layer("SELECT * FROM county", "county")
        county_wgs = self.op("reproject", {"layer": county}, {"crs": "EPSG:4326"}, "cw")["output_path"]
        clip = self.op("clip", {"layer": roads, "mask": county_wgs}, stem="clip")
        ids = [c["id"] for c in clip["checks"]]
        self.assertIn("crs.agreement", ids)
        self.assertEqual(clip["summary"]["crs"]["code"], "EPSG:3081")
        sj = self.op("spatial_join", {"left": roads, "right": county}, {"predicate": "intersects"}, "sj")
        self.assertEqual(sj["summary"]["rows"], 10)
        self.assertIn("name", sj["summary"]["fields"])
        d = self.op("dissolve", {"layer": roads}, {"by": "district"}, "diss")
        self.assertEqual(d["summary"]["rows"], 2)
        inter = self.op("intersect", {"a": county, "b": self.op("buffer", {"layer": roads}, {"distance": 10, "unit": "m"}, "b10")["output_path"]}, stem="inter")
        self.assertGreater(inter["summary"]["rows"], 0)

    def test_make_valid_and_verify(self):
        bowtie = Polygon([(0, 0), (10, 10), (10, 0), (0, 10), (0, 0)])
        path = os.path.join(self.tmp.name, "bad.gpkg")
        gpd.GeoDataFrame({"a": [1, 2]}, geometry=[bowtie, bowtie], crs="EPSG:3081").to_file(path, engine="pyogrio")
        v = w.verify_layer({"path": path})
        ids = [c["id"] for c in v["checks"]]
        self.assertIn("geometry.valid", ids)
        self.assertIn("geometry.duplicates", ids)
        fixed = self.op("make_valid", {"layer": path}, stem="fixed")
        self.assertNotIn("geometry.valid", [c["id"] for c in w.verify_layer({"path": fixed["output_path"]})["checks"]])

    def test_fid_columns_that_geopackage_would_refuse(self):
        empty = self.query(sql="SELECT * FROM roads WHERE route_id = 'none'")
        self.assertEqual(empty["rows_read"], 0)
        self.assertTrue(empty["output_path"].endswith(".gpkg"))
        dup = gpd.GeoDataFrame({"fid": [1, 1]}, geometry=[Point(0, 0), Point(1, 1)], crs="EPSG:3081")
        path = g.write_layer(dup, os.path.join(self.tmp.name, "ws", "dup"))
        self.assertEqual(list(gpd.read_file(path)["source_fid"]), [1, 1])

    def test_inputs_are_checked_by_content(self):
        # A "GeoPackage" whose content is a VRT pointing at the source file,
        # as a connected server could save into the workspace.
        vrt = os.path.join(self.tmp.name, "ws", "external", "evil.gpkg")
        os.makedirs(os.path.dirname(vrt), exist_ok=True)
        with open(vrt, "w") as f:
            f.write(f"<OGRVRTDataSource><OGRVRTLayer name='x'><SrcDataSource>{self.path}</SrcDataSource>"
                    "<SrcSQL>SELECT * FROM roads</SrcSQL></OGRVRTLayer></OGRVRTDataSource>")
        txt = vrt[:-5] + ".txt"
        os.rename(vrt, txt)
        for path in (txt, "CSV:/etc/passwd", "/vsicurl/http://127.0.0.1:1/x.gpkg"):
            with self.assertRaises(w.WorkerError, msg=path):
                w.verify_layer({"path": path})
        with open(vrt, "w") as f:
            f.write("<OGRVRTDataSource/>")
        with self.assertRaises(w.WorkerError) as cm:
            w.verify_layer({"path": vrt})
        self.assertIn("not a GeoPackage", cm.exception.message)
        # Plain SQLite is not a GeoPackage either.
        plain = os.path.join(self.tmp.name, "ws", "plain.gpkg")
        sqlite3.connect(plain).execute("CREATE TABLE t (a)").connection.close()
        with self.assertRaises(w.WorkerError):
            w.verify_layer({"path": plain})
        # GDAL itself no longer opens VRTs.
        import pyogrio
        self.assertNotIn("OGR_VRT", pyogrio.list_drivers())

    def test_empty_keys_never_match(self):
        left = gpd.GeoDataFrame({"k": ["a", None, None, "zz", ""]}, geometry=[Point(i, i) for i in range(5)], crs="EPSG:3081")
        lp = g.write_layer(left, os.path.join(self.tmp.name, "ws", "left"))
        rp = os.path.join(self.tmp.name, "ws", "right.csv")
        pd.DataFrame({"k": ["a", "", None], "v": ["1", "99", "98"]}).to_csv(rp, index=False)
        j = self.op("join", {"left": lp, "right": rp}, {"left_key": "k"}, "nk")
        out = gpd.read_file(j["output_path"])
        self.assertEqual(len(out), 5)
        self.assertEqual(list(out["v"].fillna("-")), ["1", "-", "-", "-", "-"])
        c = next(c for c in j["checks"] if c["id"] == "join.unmatched")
        self.assertIn("4 of 5", c["message"])
        self.assertIn("empty", c["message"])
        # Only empty keys unmatched: used to crash on pandas 3.
        left2 = gpd.GeoDataFrame({"k": ["a", None]}, geometry=[Point(0, 0), Point(1, 1)], crs="EPSG:3081")
        self.op("join", {"left": g.write_layer(left2, os.path.join(self.tmp.name, "ws", "l2")), "right": rp}, {"left_key": "k"}, "nk2")

    def test_join_and_overlay_size_limits(self):
        left = gpd.GeoDataFrame({"k": ["a"] * 50}, geometry=[Point(i, 0) for i in range(50)], crs="EPSG:3081")
        lp = g.write_layer(left, os.path.join(self.tmp.name, "ws", "big"))
        rp = os.path.join(self.tmp.name, "ws", "dups.csv")
        pd.DataFrame({"k": ["a"] * 50, "v": range(50)}).to_csv(rp, index=False)
        with self.assertRaises(w.WorkerError) as cm:
            w.vector_op({"op": "join", "inputs": {"left": lp, "right": rp}, "params": {"left_key": "k", "_max_rows": 10**9},
                         "max_rows": 1000, "output_stem": os.path.join(self.tmp.name, "ws", "results", "x")})
        self.assertIn("output.too_large", cm.exception.message)
        with self.assertRaises(w.WorkerError):
            self.op("buffer", {"layer": lp}, {"distance": 1, "unit": "m", "segments": 10**7})

    def test_dissolve_keeps_empty_groups_and_buffer_refuses_web_mercator(self):
        polys = gpd.GeoDataFrame({"g": ["x", None, None]}, geometry=[Point(i * 10, 0).buffer(1) for i in range(3)], crs="EPSG:3081")
        p = g.write_layer(polys, os.path.join(self.tmp.name, "ws", "polys"))
        d = self.op("dissolve", {"layer": p}, {"by": "g"}, "dn")
        self.assertEqual(d["summary"]["rows"], 2)
        self.assertIn("dissolve.null_groups", [c["id"] for c in d["checks"]])
        merc = self.op("reproject", {"layer": p}, {"crs": "EPSG:3857"}, "merc")["output_path"]
        with self.assertRaises(w.WorkerError) as cm:
            self.op("buffer", {"layer": merc}, {"distance": 100, "unit": "m"})
        self.assertIn("Web Mercator", cm.exception.message)

    def test_bbox_meets_the_geometry_not_its_envelope(self):
        diag = gpd.GeoDataFrame({"a": [1]}, geometry=[LineString([(0, 0), (10, 10)])], crs="EPSG:3081")
        path = os.path.join(self.tmp.name, "diag.gpkg")
        diag.to_file(path, layer="d", engine="pyogrio")
        conn = {"kind": "gpkg", "path": path}
        base = {"connection": conn, "sql": "SELECT * FROM d", "max_rows": 10, "geometry_column": "geom", "crs_spec": {"code": "EPSG:3081"}}
        self.assertEqual(w.query_layer({**base, "bbox": [8, 0, 10, 2]})["rows_read"], 0)
        self.assertEqual(w.query_layer({**base, "bbox": [4, 4, 6, 6]})["rows_read"], 1)
        with self.assertRaises(w.WorkerError):
            w.query_layer({**base, "crs_spec": None, "bbox": [0, 0, 1, 1], "bbox_crs": "EPSG:4326"})

    def test_unknown_op_and_wrong_inputs(self):
        roads = self.layer("SELECT * FROM roads", "roads")
        with self.assertRaises(w.WorkerError):
            self.op("reproject", {"layer": roads}, {"crs": "EPSG:4326 +init=x"})
        for op, inputs in [("explode_everything", {"layer": roads}), ("clip", {"layer": roads})]:
            with self.assertRaises(w.WorkerError):
                self.op(op, inputs)


if __name__ == "__main__":
    unittest.main()
