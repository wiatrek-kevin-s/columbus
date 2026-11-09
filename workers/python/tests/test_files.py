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
import files_fixtures as fx  # noqa: E402


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

    def test_cache_has_an_rtree(self):
        # T-13: each geometry table in the cache gets its R-tree, so the
        # gateway can push a bbox into it; rebuilt with the table.
        self.write("pts.csv", "id,lon,lat\n1,-97.7,30.2\n2,,\n3,-97.6,30.4\n")
        self.write("lines.csv", "id,WKT\n1,\"LINESTRING (0 0, 2 1)\"\n2,\"POINT EMPTY\"\n")
        d = w.describe_dataset({"connection": self.conn, "dataset": "pts"})
        self.assertIs(d["spatial_index"], True)
        self.assertIs(w.describe_dataset({"connection": self.conn, "dataset": "lines"})["spatial_index"], True)
        import sqlite3

        db = sqlite3.connect(self.conn["cache"])
        boxes = db.execute(f'SELECT id, minx, maxx, miny, maxy FROM "rtree_pts_{d["geometry_column"]}" ORDER BY id').fetchall()
        lines = db.execute('SELECT id, minx, maxx, miny, maxy FROM "rtree_lines_geometry"').fetchall()
        ext = db.execute("SELECT extension_name FROM gpkg_extensions WHERE table_name = 'pts'").fetchall()
        db.close()
        # No row for the point without coordinates or the empty one.
        self.assertEqual([b[0] for b in boxes], [1, 3])
        self.assertAlmostEqual(boxes[0][1], -97.7, places=4)
        self.assertAlmostEqual(boxes[0][3], 30.2, places=4)
        self.assertEqual(len(lines), 1)
        self.assertAlmostEqual(lines[0][2], 2.0, places=5)
        self.assertEqual(ext, [("gpkg_rtree_index",)])
        # Rebuilt with the file; gone with it.
        time.sleep(0.01)
        self.write("pts.csv", "id,lon,lat\n1,-97.7,30.2\n")
        w.describe_dataset({"connection": self.conn, "dataset": "pts"})
        db = sqlite3.connect(self.conn["cache"])
        self.assertEqual(db.execute(f'SELECT COUNT(*) FROM "rtree_pts_{d["geometry_column"]}"').fetchone()[0], 1)
        db.close()
        os.remove(os.path.join(self.root, "pts.csv"))
        self.names()
        db = sqlite3.connect(self.conn["cache"])
        self.assertIsNone(db.execute("SELECT 1 FROM sqlite_master WHERE name LIKE 'rtree_pts%'").fetchone())
        self.assertEqual(db.execute("SELECT COUNT(*) FROM gpkg_extensions WHERE table_name = 'pts'").fetchone()[0], 0)
        db.close()

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
        # REQ-4: an unreadable file is left out, named, with the cause.
        self.assertIn("could not be read", reasons["x.geojson"])
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


try:
    # The engine's own import first: it switches GDAL's VRT and other
    # drivers off before GDAL loads (test_gis checks they stay off).
    import columbus_gis  # noqa: F401
    import geopandas  # noqa: F401
    import pyogrio  # noqa: F401
    import pyarrow  # noqa: F401

    HAVE_FORMATS = HAVE_GIS
except ImportError:
    HAVE_FORMATS = False


def _meta(conn, dataset):
    import sqlite3

    db = sqlite3.connect(conn["cache"])
    try:
        row = db.execute("SELECT builds, size, mtime_ns, sha256 FROM columbus_sources WHERE dataset = ?",
                         (dataset,)).fetchone()
    finally:
        db.close()
    return row


@unittest.skipUnless(HAVE_FORMATS, "needs the GIS engine (geopandas, pyogrio, pyarrow)")
class Formats(unittest.TestCase):
    """One fixture per format: each lists, describes and queries."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.root = os.path.join(cls.tmp.name, "data")
        fx.write_all(cls.root)
        cls.conn = {"kind": "files", "path": cls.root, "cache": os.path.join(cls.tmp.name, "c.gpkg")}
        cls.listing = w.list_datasets({"connection": cls.conn})

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def describe(self, name):
        return w.describe_dataset({"connection": self.conn, "dataset": name})

    def rows(self, sql):
        _, rows, *_ = w._fetch(self.conn, sql, 100, 30)
        return rows

    def fields(self, d):
        return {x["name"]: x["type"] for x in d["fields"]}

    def test_every_format_is_listed(self):
        names = sorted(d["name"] for d in self.listing["datasets"])
        self.assertEqual(names, ["crashes", "dupes", "empty", "inventory/2025", "inventory/2026", "legacy",
                                 "parcels", "places/Parks", "places/Schools", "roads"])
        reasons = {s["name"]: s["reason"] for s in self.listing["skipped"]}
        self.assertEqual(reasons["inventory/Notes"], f.SKIP_EMPTY_SHEET)
        self.assertEqual(reasons["settings"], f.SKIP_NOT_GEOJSON)

    def test_xlsx_sheets(self):
        d = self.describe("inventory/2025")
        t = self.fields(d)
        # R-4 on the cells' text; R-6 the formula's saved value; R-6b dates
        # from date-formatted cells, a General serial stays a number.
        self.assertEqual((t["CSJ"], t["Milepost"], t["Total"], t["Date"], t["Serial"], t["ID"]),
                         ("TEXT", "REAL", "REAL", "DATE", "INTEGER", "INTEGER"))
        self.assertIn("Name_2", t)
        self.assertEqual(d["crs"]["code"], "EPSG:4326")
        self.assertEqual(d["geometry_type"], "POINT")
        rows = self.rows('SELECT CSJ, "Date", Serial, Total, Milepost FROM "inventory/2025" ORDER BY ID')
        self.assertEqual(rows[0], ("000123456", "2026-11-08", 46334, 12.5, 12.5))
        self.assertEqual(self.rows('SELECT COUNT(*) FROM "inventory/2026"'), [(0,)])

    def test_geojson_fgb_parquet_kml(self):
        for name, gtype, code, n in [("roads", "LINESTRING", "EPSG:4326", 2), ("crashes", "POINT", "EPSG:4326", 3),
                                     ("parcels", "POLYGON", "EPSG:2277", 1), ("places/Parks", "POINT", "EPSG:4326", 2),
                                     ("places/Schools", "POINT", "EPSG:4326", 1)]:
            d = self.describe(name)
            self.assertEqual((d["geometry_type"], d["crs"]["code"]), (gtype, code), name)
            self.assertEqual(self.rows(f'SELECT COUNT(*) FROM "{name}"'), [(n,)], name)
        self.assertEqual(self.fields(self.describe("roads"))["LANES"], "INTEGER")
        self.assertEqual(self.rows('SELECT Name FROM "places/Schools"'), [("Austin High",)])
        self.assertEqual(self.rows("SELECT ACRES FROM parcels"), [(1.25,)])

    def test_csv_oddities(self):
        self.assertIn("file.encoding", [c["id"] for c in self.describe("legacy")["checks"]])
        self.assertEqual(self.rows("SELECT Name FROM legacy"), [("Café",)])
        self.assertEqual(list(self.fields(self.describe("dupes"))), ["fid", "ID", "Name", "Name_2", "field_4"])
        self.assertEqual(self.rows("SELECT COUNT(*) FROM empty"), [(0,)])


@unittest.skipUnless(HAVE_FORMATS, "needs the GIS engine (geopandas, pyogrio, pyarrow)")
class FormatEdges(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = os.path.join(self.tmp.name, "data")
        os.makedirs(self.root)
        self.conn = {"kind": "files", "path": self.root, "cache": os.path.join(self.tmp.name, "c.gpkg")}

    def tearDown(self):
        self.tmp.cleanup()

    def test_a5_parquet_crs(self):
        """A-5: geopandas.read_parquet keeps the CRS from the geo metadata;
        a file without one takes the connection's crs, with a warning."""
        import geopandas as gpd
        from shapely.geometry import Point

        df = gpd.GeoDataFrame({"v": [1]}, geometry=[Point(3110000, 10070000)], crs="EPSG:2277")
        df.to_parquet(os.path.join(self.root, "kept.geoparquet"))
        df.set_crs(None, allow_override=True).to_parquet(os.path.join(self.root, "bare.parquet"))
        d = w.describe_dataset({"connection": self.conn, "dataset": "kept"})
        self.assertEqual(d["crs"]["code"], "EPSG:2277")
        d = w.describe_dataset({"connection": self.conn, "dataset": "bare"})
        self.assertIsNone(d["crs"])
        present = [c for c in d["checks"] if c["id"] == "crs.present"]
        self.assertEqual([c["severity"] for c in present], ["warn"])
        conn = dict(self.conn, crs="EPSG:2277", cache=os.path.join(self.tmp.name, "c2.gpkg"))
        d = w.describe_dataset({"connection": conn, "dataset": "bare"})
        self.assertEqual(d["crs"]["code"], "EPSG:2277")
        # The connection's crs never overrides a file's own.
        d = w.describe_dataset({"connection": dict(conn, crs="EPSG:3857"), "dataset": "kept"})
        self.assertEqual(d["crs"]["code"], "EPSG:2277")

    def test_no_crs_is_a_warning_for_files(self):
        with open(os.path.join(self.root, "far.csv"), "w") as fh:
            fh.write("id,x,y\n1,3110000,10070000\n")
        d = w.describe_dataset({"connection": self.conn, "dataset": "far"})
        self.assertEqual([c["severity"] for c in d["checks"] if c["id"] == "crs.present"], ["warn"])
        out = w.query_layer({"connection": self.conn, "sql": 'SELECT * FROM "far"', "max_rows": 10,
                             "geometry_column": "geometry", "output_stem": None})
        self.assertNotIn("fail", [c["severity"] for c in out["checks"] if c["id"] == "crs.present"])

    def test_unreadable_and_disguised_files(self):
        """REQ-4: a file that can't be read is named with the cause; a file
        whose content is another format is never handed to GDAL as it."""
        with open(os.path.join(self.root, "broken.geojson"), "w") as fh:
            fh.write('{"type": "FeatureCollection", "features": [')
        with open(os.path.join(self.root, "vrt.kml"), "w") as fh:
            fh.write('<OGRVRTDataSource><OGRVRTLayer name="x"><SrcDataSource>/etc/passwd</SrcDataSource>'
                     '</OGRVRTLayer></OGRVRTDataSource>')
        with open(os.path.join(self.root, "fake.xlsx"), "wb") as fh:
            fh.write(b"\xd0\xcf\x11\xe0 an old or protected workbook")
        with open(os.path.join(self.root, "ok.csv"), "w") as fh:
            fh.write("v\n1\n")
        out = w.list_datasets({"connection": self.conn})
        self.assertEqual([d["name"] for d in out["datasets"]], ["ok"])
        reasons = {s["file"]: s["reason"] for s in out["skipped"]}
        self.assertIn("could not be read", reasons["broken.geojson"])
        self.assertIn("isn't .kml data", reasons["vrt.kml"])
        self.assertIn("password-protected", reasons["fake.xlsx"])
        with self.assertRaises(w.WorkerError) as e:
            w.describe_dataset({"connection": self.conn, "dataset": "broken"})
        self.assertIn("broken.geojson", str(e.exception))
        with self.assertRaises(w.WorkerError) as e:
            w.describe_dataset({"connection": self.conn, "dataset": "nothing"})
        self.assertIn("no dataset nothing", str(e.exception))

    def test_excel_dates_with_times_and_formulas_without_values(self):
        fx.write_xlsx(os.path.join(self.root, "log.xlsx"), [("Sheet1", [
            ["When", "Calc", "Flag"],
            [{"v": 46334.5, "date": True}, {"f": "1+1"}, "x"],
            [{"v": 46335, "date": True}, 3, None],
        ])])
        d = w.describe_dataset({"connection": self.conn, "dataset": "log/Sheet1"})
        self.assertEqual(self.fields_of(d)["When"], "DATETIME")
        self.assertIn("xlsx.formulas", [c["id"] for c in d["checks"]])
        _, rows, *_ = w._fetch(self.conn, 'SELECT "When", Calc FROM "log/Sheet1" ORDER BY fid', 10, 10)
        self.assertEqual(rows, [("2026-11-08T12:00:00", None), ("2026-11-09T00:00:00", 3)])

    def fields_of(self, d):
        return {x["name"]: x["type"] for x in d["fields"]}

    def test_layer_names_collide_with_files(self):
        fx.write_xlsx(os.path.join(self.root, "book.xlsx"), [("a.b", [["v"], [1]])])
        os.makedirs(os.path.join(self.root, "book"))
        with open(os.path.join(self.root, "book", "a_b.csv"), "w") as fh:
            fh.write("v\n1\n")
        out = w.list_datasets({"connection": dict(self.conn, recursive=True)})
        self.assertEqual(out["datasets"], [])
        reasons = {s["file"]: s["reason"] for s in out["skipped"]}
        self.assertIn("also the name of book/a_b.csv", reasons["book.xlsx"])
        self.assertIn("'a.b' in book.xlsx", reasons["book/a_b.csv"])


@unittest.skipUnless(HAVE_GIS, "needs shapely and pyproj")
class Freshness(unittest.TestCase):
    """R-7 to R-7c, with the rebuild count ``builds`` in the cache."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = os.path.join(self.tmp.name, "data")
        os.makedirs(self.root)
        self.file = os.path.join(self.root, "crashes.csv")
        self.conn = {"kind": "files", "path": self.root, "cache": os.path.join(self.tmp.name, "c.gpkg")}

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, text, mtime=None):
        with open(self.file, "w", encoding="utf-8", newline="") as fh:
            fh.write(text)
        if mtime is not None:
            os.utime(self.file, ns=(mtime, mtime))

    def count(self):
        _, rows, *_ = w._fetch(self.conn, "SELECT COUNT(*) FROM crashes", 2, 30)
        return rows[0][0]

    def test_append_touch_change_delete(self):
        self.write("id\n1\n2\n", mtime=1_700_000_000_000_000_000)
        self.assertEqual(self.count(), 2)
        self.assertEqual(_meta(self.conn, "crashes")[0], 1)
        # Appended: rebuilt, the new row seen.
        with open(self.file, "a", encoding="utf-8", newline="") as fh:
            fh.write("3\n")
        os.utime(self.file, ns=(1_700_000_001_000_000_000,) * 2)
        self.assertEqual(self.count(), 3)
        self.assertEqual(_meta(self.conn, "crashes")[0], 2)
        # Touched, same bytes: no rebuild; the new time is recorded.
        os.utime(self.file, ns=(1_700_000_002_000_000_000,) * 2)
        self.assertEqual(self.count(), 3)
        builds, _size, mtime, _sha = _meta(self.conn, "crashes")
        self.assertEqual((builds, mtime), (2, 1_700_000_002_000_000_000))
        # Same size, other bytes: the hash differs, so it is rebuilt.
        self.write("id\n1\n2\n4\n", mtime=1_700_000_003_000_000_000)
        _, rows, *_ = w._fetch(self.conn, "SELECT id FROM crashes ORDER BY id", 10, 30)
        self.assertEqual(rows, [(1,), (2,), (4,)])
        self.assertEqual(_meta(self.conn, "crashes")[0], 3)
        # Deleted: gone from the list, its table dropped, queries say so.
        os.remove(self.file)
        self.assertEqual(w.list_datasets({"connection": self.conn})["datasets"], [])
        self.assertIsNone(_meta(self.conn, "crashes"))
        import sqlite3

        db = sqlite3.connect(self.conn["cache"])
        self.assertIsNone(db.execute("SELECT 1 FROM sqlite_master WHERE name = 'crashes'").fetchone())
        db.close()
        with self.assertRaises(w.WorkerError) as e:
            w.describe_dataset({"connection": self.conn, "dataset": "crashes"})
        self.assertIn("no dataset crashes", str(e.exception))

    def test_an_older_cache_is_rebuilt(self):
        import sqlite3

        self.write("id\n1\n")
        os.makedirs(os.path.dirname(self.conn["cache"]), exist_ok=True)
        db = sqlite3.connect(self.conn["cache"])
        db.execute("PRAGMA application_id = 1196444487")
        db.execute("CREATE TABLE columbus_sources (dataset TEXT PRIMARY KEY, source TEXT NOT NULL, size INTEGER NOT NULL, "
                   "mtime_ns INTEGER NOT NULL, build INTEGER NOT NULL, checks TEXT NOT NULL, geometry_column TEXT)")
        db.execute("CREATE TABLE crashes (fid INTEGER PRIMARY KEY, id TEXT)")
        db.execute("INSERT INTO columbus_sources VALUES ('crashes', 'x', 1, 1, 1, '[]', NULL)")
        db.commit()
        db.close()
        self.assertEqual(self.count(), 1)
        self.assertEqual(_meta(self.conn, "crashes")[0], 1)

    def test_two_processes_on_a_stale_dataset_rebuild_once(self):
        """R-7b: two engines (as two sessions run) query a stale dataset at
        once; one rebuilds, the other waits and reads the new cache."""
        import json
        import subprocess

        self.write("id\n" + "".join(f"{i}\n" for i in range(1000)))
        self.assertEqual(self.count(), 1000)
        with open(self.file, "a", encoding="utf-8", newline="") as fh:
            fh.write("1000\n")
        go = os.path.join(self.tmp.name, "go")
        script = (
            "import json, os, sys, time\n"
            f"sys.path.insert(0, {os.path.dirname(HERE)!r})\n"
            "import columbus_files as f, columbus_worker as w\n"
            "read = f.READERS['.csv']\n"
            "def slow(*a):\n"
            "    time.sleep(1.0)\n"
            "    return read(*a)\n"
            "f.READERS['.csv'] = slow\n"
            f"conn = {self.conn!r}\n"
            f"while not os.path.exists({go!r}):\n"
            "    time.sleep(0.005)\n"
            "t0 = time.time()\n"
            "_, rows, *_ = w._fetch(conn, 'SELECT COUNT(*) FROM crashes', 2, 60)\n"
            "print(json.dumps({'t0': t0, 't1': time.time(), 'rows': rows[0][0]}))\n"
        )
        procs = [subprocess.Popen([sys.executable, "-c", script], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                 for _ in range(2)]
        time.sleep(1.0)  # both started and waiting
        open(go, "w").close()
        outs = []
        for p in procs:
            out, err = p.communicate(timeout=120)
            self.assertEqual(p.returncode, 0, err)
            outs.append(json.loads(out.strip().splitlines()[-1]))
        self.assertEqual([o["rows"] for o in outs], [1001, 1001])
        a, b = outs
        self.assertTrue(a["t0"] < b["t1"] and b["t0"] < a["t1"], f"the two queries did not overlap: {outs}")
        self.assertEqual(_meta(self.conn, "crashes")[0], 2, "exactly one rebuild for the two queries")

if __name__ == "__main__":
    unittest.main()
