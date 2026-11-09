"""Large results are written in batches as they arrive (sql_select's CSV,
query_layer's GeoPackage or CSV, plugin extracts), give the same rows and
checks as the in-memory path, and keep the read-limit semantics.

The GIS parts are skipped when geopandas, pyogrio, shapely or pyproj are
not installed.
"""

import csv
import os
import sqlite3
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from engine_required import skip_unless_engine  # noqa: E402

try:
    import geopandas as gpd
    import shapely
    from shapely.geometry import Point, Polygon

    import columbus_gis as g

    HAVE_GIS = True
except ImportError:  # pragma: no cover
    HAVE_GIS = False

import columbus_worker as w  # noqa: E402


def points_gpkg(path: str, n: int) -> None:
    """n points in EPSG:3081 with an integer, a real and a text column, and
    a column that is null in the first rows and integer after."""
    pts = shapely.points([(1_000_000 + i, 7_000_000 + (i % 1000)) for i in range(n)])
    gpd.GeoDataFrame(
        {"k": list(range(n)), "x": [i / 4 for i in range(n)], "code": [f"{i % 7:03d}" for i in range(n)],
         "late": [None] * 50 + list(range(n - 50))},
        geometry=pts, crs="EPSG:3081",
    ).to_file(path, layer="pts", driver="GPKG", engine="pyogrio")


class CsvStreamingTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.tmp.name, "t.sqlite")
        db = sqlite3.connect(self.path)
        db.execute("CREATE TABLE t (id INTEGER, secret TEXT, v REAL)")
        db.executemany("INSERT INTO t VALUES (?, ?, ?)", ((i, f"s{i}", i / 2) for i in range(25_001)))
        db.commit()
        db.close()
        self.conn = {"kind": "sqlite", "path": self.path}

    def tearDown(self):
        self.tmp.cleanup()

    def test_sql_select_streams_every_row_to_the_csv(self):
        out = os.path.join(self.tmp.name, "ws", "results", "p.csv")
        r = w.sql_select({"connection": self.conn, "sql": "SELECT * FROM t LIMIT 25001", "max_rows": 25_000,
                          "return_rows": 5, "output_path": out, "hide_fields": ["secret"]})
        self.assertEqual((r["rows_read"], r["rows_returned"], r["hit_read_limit"]), (25_000, 5, True))
        self.assertEqual(r["columns"], ["id", "v"])
        self.assertEqual(r["rows"][4], [4, 2.0])
        with open(out, encoding="utf-8", newline="") as f:
            rows = list(csv.reader(f))
        self.assertEqual(rows[0], ["id", "v"])
        self.assertEqual(len(rows), 25_001)
        self.assertEqual(rows[-1], ["24999", "12499.5"])
        with open(out, encoding="utf-8") as f:
            self.assertNotIn("s1", f.read())

    def test_drain_hands_rows_on_in_chunks(self):
        seen = []

        class Sink:
            def start(self, columns, cells, geoms):
                seen.append(("start", columns))

            def add(self, chunk):
                seen.append(len(chunk))

        cols, rows, hit, _, _ = w._fetch(self.conn, "SELECT id FROM t", 5000, 30, sink=Sink())
        self.assertEqual((cols, rows, hit), (["id"], [], True))
        self.assertEqual(seen[0], ("start", ["id"]))
        self.assertTrue(all(n <= w.FETCH_CHUNK for n in seen[1:]))
        self.assertEqual(sum(seen[1:]), 5000)

    def test_a_failed_read_leaves_no_partial_file(self):
        out = os.path.join(self.tmp.name, "ws", "results", "bad.csv")

        class Boom(Exception):
            pass

        real = w._CsvSink.add
        calls = {"n": 0}

        def add(self, chunk):
            calls["n"] += 1
            if calls["n"] == 3:
                raise Boom()
            real(self, chunk)

        w._CsvSink.add = add
        try:
            with self.assertRaises(Boom):
                w.sql_select({"connection": self.conn, "sql": "SELECT id FROM t", "max_rows": 25_000,
                              "return_rows": 5, "output_path": out})
        finally:
            w._CsvSink.add = real
        self.assertFalse(os.path.exists(out))


@skip_unless_engine(HAVE_GIS, "GIS engine not installed")
class LayerStreamingTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.tmp.name, "pts.gpkg")
        points_gpkg(self.path, 3000)
        self.conn = {"kind": "gpkg", "path": self.path}

    def tearDown(self):
        self.tmp.cleanup()

    def query(self, stem, **kw):
        p = {"connection": self.conn, "sql": "SELECT * FROM pts", "max_rows": 10_000,
             "geometry_column": "geom", "crs_spec": {"code": "EPSG:3081"}, "return_rows": 3,
             "output_stem": os.path.join(self.tmp.name, "ws", "results", stem), "layer_name": "pts"}
        p.update(kw)
        return w.query_layer(p)

    def test_streamed_layer_matches_the_in_memory_one(self):
        whole = self.query("whole")
        streamed = self.query("streamed", stream_after=500)
        self.assertFalse(whole["streamed"])
        self.assertTrue(streamed["streamed"])
        self.assertEqual(streamed["rows_read"], 3000)
        self.assertEqual(streamed["rows"], whole["rows"])
        self.assertEqual(streamed["summary"]["rows"], 3000)
        self.assertEqual(streamed["summary"]["extent"], whole["summary"]["extent"])
        self.assertEqual(streamed["summary"]["geometry_types"], ["Point"])
        self.assertEqual([c["id"] for c in streamed["checks"]], [c["id"] for c in whole["checks"]])
        a = gpd.read_file(whole["output_path"], engine="pyogrio")
        b = gpd.read_file(streamed["output_path"], engine="pyogrio")
        self.assertEqual(len(b), 3000)
        self.assertEqual(b.crs.to_epsg(), 3081)
        # The GeoPackage's own fid numbers the features; the source fid is kept.
        self.assertEqual(list(b["source_fid"][:3]), [1, 2, 3])
        self.assertTrue(b.geometry.geom_equals(a.geometry).all())
        self.assertEqual(list(b["k"]), list(a["k"]))
        self.assertEqual(str(b["k"].dtype), "int64")
        self.assertEqual(str(b["x"].dtype), "float64")
        # Codes keep their padding.
        self.assertEqual(list(b["code"][:3]), ["000", "001", "002"])
        # A column null in the first batch and integer later becomes integer.
        self.assertTrue(b["late"].iloc[:50].isna().all())
        self.assertEqual(int(b["late"].iloc[-1]), 2949)
        self.assertTrue(g._is_geopackage(streamed["output_path"]))
        db = sqlite3.connect(streamed["output_path"])
        try:
            self.assertEqual(db.execute("PRAGMA integrity_check").fetchone()[0], "ok")
            gtype, srs = db.execute("SELECT geometry_type_name, srs_id FROM gpkg_geometry_columns").fetchone()
            self.assertEqual((gtype, srs), ("POINT", 3081))
        finally:
            db.close()

    def test_read_limit_still_reports_truncation(self):
        r = self.query("cut", sql="SELECT * FROM pts LIMIT 2001", max_rows=2000, stream_after=500)
        self.assertTrue(r["streamed"])
        self.assertEqual((r["rows_read"], r["hit_read_limit"]), (2000, True))
        self.assertEqual(len(gpd.read_file(r["output_path"], engine="pyogrio")), 2000)

    def test_bbox_filter_and_hidden_fields_apply_when_streaming(self):
        r = self.query("box", stream_after=100, hide_fields=["code"],
                       bbox=[1_000_000, 7_000_000, 1_000_999, 7_000_999])
        self.assertTrue(r["streamed"])
        self.assertEqual(r["rows_read"], 1000)
        out = gpd.read_file(r["output_path"], engine="pyogrio")
        self.assertNotIn("code", out.columns)
        self.assertEqual(r["hidden_columns"], ["code"])

    def test_types_widen_across_batches_without_losing_values(self):
        stem = os.path.join(self.tmp.name, "ws", "results", "wide")
        s = g.LayerStream(stem, "t", ["a", "b", "c"], "geom", g.crs_from_spec({"code": "EPSG:3081"}))
        s.write([[1, True, 5], [2, False, None]], [Point(0, 0), Point(1, 1)])
        s.write([[2.5, 3, "x"]], [Polygon([(0, 0), (1, 0), (1, 1)])])
        path = s.close()
        out = gpd.read_file(path, engine="pyogrio")
        self.assertEqual(list(out["a"]), [1.0, 2.0, 2.5])
        self.assertEqual(list(out["b"]), [1, 0, 3])
        self.assertEqual([None if not isinstance(v, str) else v for v in out["c"]], ["5", None, "x"])
        self.assertEqual(sorted(out.geometry.geom_type), ["Point", "Point", "Polygon"])
        self.assertIn("geometry.mixed_types", [c["id"] for c in s.checks()])

    def test_csv_layer_without_geometry(self):
        r = self.query("attrs", sql="SELECT k, code FROM pts", geometry_column=None, stream_after=100)
        self.assertTrue(r["streamed"])
        self.assertTrue(r["output_path"].endswith(".csv"))
        with open(r["output_path"], encoding="utf-8", newline="") as f:
            rows = list(csv.reader(f))
        self.assertEqual(rows[0], ["k", "code"])
        self.assertEqual(rows[1], ["0", "000"])
        self.assertEqual(len(rows), 3001)

    def test_read_rows_pages_a_large_layer(self):
        path = self.query("paged", stream_after=500)["output_path"]
        whole = g.read_rows(path, 2990, 5)
        old = g.LARGE_LAYER
        g.LARGE_LAYER = 1000
        try:
            paged = g.read_rows(path, 2990, 5)
            past = g.read_rows(path, 5000, 5)
        finally:
            g.LARGE_LAYER = old
        self.assertEqual(paged["rows"], whole["rows"])
        self.assertEqual(paged["columns"], whole["columns"])
        self.assertEqual((paged["total"], paged["summary"]["rows"]), (3000, 3000))
        self.assertEqual(paged["summary"]["crs"]["code"], "EPSG:3081")
        self.assertEqual(past["rows"], [])

    def test_extract_streams(self):
        out = os.path.join(self.tmp.name, "ws", "inputs", "x.sqlite")
        r = w.extract({"connection": self.conn, "sql": "SELECT fid, k, geom FROM pts", "max_rows": 2500,
                       "output_path": out, "hide_fields": ["k"]})
        self.assertEqual((r["rows"], r["hit_read_limit"], r["columns"]), (2500, True, ["fid", "geom"]))
        db = sqlite3.connect(out)
        try:
            self.assertEqual(db.execute('SELECT count(*) FROM "data"').fetchone()[0], 2500)
            self.assertEqual(dict(db.execute("SELECT key, value FROM columbus_extract"))["srid"], "3081")
        finally:
            db.close()


if __name__ == "__main__":
    unittest.main()
