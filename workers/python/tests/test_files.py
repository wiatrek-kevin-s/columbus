"""files connections (columbus_files): dataset names (F-1), skipped files
(R-16), CSV typing (R-4) and geometry (R-1 to R-3), and the cache
GeoPackage: built once, rebuilt when the file changes, dropped when the
file goes."""

import os
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

try:
    import shapely  # noqa: F401
    import pyproj  # noqa: F401

    HAVE_GIS = True
except ImportError:  # the engine's GIS packages
    HAVE_GIS = False

import columbus_worker as w  # noqa: E402
import columbus_files as f  # noqa: E402


class TypingAndNames(unittest.TestCase):
    def test_r4_typing(self):
        self.assertEqual(f.column_type(["000123456", "1"]), "TEXT")
        self.assertEqual(f.column_type(["12", "", "-3"]), "INTEGER")
        self.assertEqual(f.column_type(["12.500", "1.50", "3"]), "REAL")
        self.assertEqual(f.column_type(["0.5", "0"]), "REAL")
        self.assertEqual(f.column_type(["1,234", "5"]), "TEXT")
        self.assertEqual(f.column_type(["2026-11-08", "2025-01-31"]), "DATE")
        self.assertEqual(f.column_type(["2026-11-08T10:00:00Z"]), "DATETIME")
        self.assertEqual(f.column_type(["11/08/2026"]), "TEXT")
        self.assertEqual(f.column_type(["nan", "inf"]), "TEXT")
        self.assertEqual(f.column_type(["", " "]), "TEXT")

    def test_f1_names(self):
        self.assertEqual(f.dataset_name(["roads.v2.csv"], ".csv"), "roads_v2")
        self.assertEqual(f.dataset_name(["crashes_2026.01.csv"], ".csv"), "crashes_2026_01")
        self.assertEqual(f.dataset_name(["2026", "crashes.CSV"], ".csv"), "2026/crashes")

    def test_r6c_headers(self):
        cols, checks = f.unique_headers(["ID", "Name", "name", ""])
        self.assertEqual(cols, ["ID", "Name", "name_2", "field_4"])
        self.assertEqual(checks[0]["id"], "csv.headers")


@unittest.skipUnless(HAVE_GIS, "needs shapely and pyproj")
class Cache(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = os.path.join(self.tmp.name, "data")
        os.makedirs(os.path.join(self.root, "sub"))
        self.conn = {"kind": "files", "path": self.root,
                     "cache": os.path.join(self.tmp.name, "cache", "files", "c.gpkg")}

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, rel, text):
        with open(os.path.join(self.root, rel), "w", encoding="utf-8", newline="") as fh:
            fh.write(text)

    def names(self, conn=None):
        return sorted(d["name"] for d in w.list_datasets({"connection": conn or self.conn})["datasets"])

    def test_points_wkt_and_tables(self):
        self.write("pts.csv", "id,lon,lat\n1,-97.7,30.2\n2,,\n")
        self.write("far.csv", "id,x,y\n1,3110000,10070000\n")
        self.write("lines.csv", "id,WKT,lon,lat\n1,\"LINESTRING (0 0, 1 1)\",1,1\n")
        self.write("plain.csv", "id,name\n1,a\n")
        self.write("empty.csv", "id,name\n")
        self.assertEqual(self.names(), ["empty", "far", "lines", "plain", "pts"])
        d = w.describe_dataset({"connection": self.conn, "dataset": "pts"})
        self.assertEqual(d["crs"]["code"], "EPSG:4326")
        self.assertIn("csv.no_coordinates", [c["id"] for c in d["checks"]])
        self.assertEqual(d["rows"], None)
        d = w.describe_dataset({"connection": self.conn, "dataset": "far"})
        self.assertIsNone(d["crs"])
        d = w.describe_dataset({"connection": self.conn, "dataset": "lines"})
        self.assertEqual(d["geometry_type"], "LINESTRING")
        self.assertIn("csv.geometry_source", [c["id"] for c in d["checks"]])
        # A check naming a hidden column stays in the engine.
        d = w.describe_dataset({"connection": self.conn, "dataset": "lines", "hide_fields": ["lon"]})
        self.assertNotIn("csv.geometry_source", [c["id"] for c in d["checks"]])
        d = w.describe_dataset({"connection": self.conn, "dataset": "plain"})
        self.assertIsNone(d.get("geometry_column"))
        self.assertIn("crs.present", [c["id"] for c in d["checks"]])
        _, rows, *_ = w._fetch(self.conn, 'SELECT COUNT(*) FROM "empty"', 2, 10)
        self.assertEqual(rows[0][0], 0)

    def test_connection_crs_and_xy(self):
        self.write("a.csv", "E,N\n3110000,10070000\n")
        conn = dict(self.conn, crs="EPSG:2277", x="E", y="N")
        d = w.describe_dataset({"connection": conn, "dataset": "a"})
        self.assertEqual(d["crs"]["code"], "EPSG:2277")

    def test_rebuild_and_drop(self):
        self.write("a.csv", "v\n1\n")
        _, rows, *_ = w._fetch(self.conn, "SELECT v FROM a", 10, 10)
        self.assertEqual(rows, [(1,)])
        time.sleep(0.01)
        self.write("a.csv", "v\n1\n2\n")
        _, rows, *_ = w._fetch(self.conn, "SELECT v FROM a", 10, 10)
        self.assertEqual(rows, [(1,), (2,)])
        os.remove(os.path.join(self.root, "a.csv"))
        self.assertEqual(self.names(), [])
        import sqlite3

        db = sqlite3.connect(self.conn["cache"])
        self.assertIsNone(db.execute("SELECT 1 FROM sqlite_master WHERE name='a'").fetchone())
        db.close()

    def test_skipped_and_collisions(self):
        self.write("a.b.csv", "v\n1\n")
        self.write("a_b.csv", "v\n1\n")
        self.write(".hidden.csv", "v\n1\n")
        self.write("gpkg_contents.csv", "v\n1\n")
        self.write("notes.txt", "hello")
        self.write("x.geojson", "{}")
        self.write("sub/deep.csv", "v\n1\n")
        out = w.list_datasets({"connection": self.conn})
        self.assertEqual(out["datasets"], [])
        reasons = {s["file"]: s["reason"] for s in out["skipped"]}
        self.assertIn("also the name of a_b.csv", reasons["a.b.csv"])
        self.assertIn("also the name of a.b.csv", reasons["a_b.csv"])
        self.assertEqual(reasons[".hidden.csv"], f.SKIP_HIDDEN)
        self.assertEqual(reasons["gpkg_contents.csv"], f.SKIP_RESERVED)
        self.assertIn("doesn't read .geojson", reasons["x.geojson"])
        self.assertNotIn("notes.txt", reasons)
        self.assertNotIn("sub/deep.csv", reasons)
        conn = dict(self.conn, recursive=True)
        self.assertIn("sub/deep", self.names(conn))

    @unittest.skipUnless(hasattr(os, "symlink"), "needs symbolic links")
    def test_links_out_of_the_folder(self):
        outside = os.path.join(self.tmp.name, "secret.csv")
        with open(outside, "w") as fh:
            fh.write("s\nhush\n")
        try:
            os.symlink(outside, os.path.join(self.root, "link.csv"))
            os.symlink(self.tmp.name, os.path.join(self.root, "up"), target_is_directory=True)
        except OSError:
            self.skipTest("cannot make symbolic links here")
        out = w.list_datasets({"connection": dict(self.conn, recursive=True)})
        reasons = {s["file"]: s["reason"] for s in out["skipped"]}
        self.assertEqual(reasons["link.csv"], f.SKIP_LINK)
        self.assertEqual(reasons["up"], f.SKIP_LINK)
        self.assertEqual(out["datasets"], [])

    def test_single_file_and_encoding(self):
        with open(os.path.join(self.root, "one.csv"), "wb") as fh:
            fh.write("name\nCafé\n".encode("cp1252"))
        conn = dict(self.conn, path=os.path.join(self.root, "one.csv"))
        self.assertEqual(self.names(conn), ["one"])
        d = w.describe_dataset({"connection": conn, "dataset": "one"})
        self.assertIn("file.encoding", [c["id"] for c in d["checks"]])
        _, rows, *_ = w._fetch(conn, "SELECT name FROM one", 10, 10)
        self.assertEqual(rows, [("Café",)])

    def test_relative_path_refused(self):
        with self.assertRaises(w.WorkerError):
            w.list_datasets({"connection": dict(self.conn, path="data")})


if __name__ == "__main__":
    unittest.main()
