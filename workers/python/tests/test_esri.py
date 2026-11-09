"""Shapefiles and file geodatabases (columbus_esri): listing, reading with
and without pyarrow (streamed, or a page at a time), EWKB with the layer's
EPSG code, and the checks on the connection's path."""

import os
import sys
import tempfile
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from engine_required import skip_unless_engine  # noqa: E402

try:
    import geopandas as gpd
    from shapely.geometry import Point, box

    HAVE_GIS = True
except ImportError:  # the engine's GIS packages
    HAVE_GIS = False

import columbus_worker as w  # noqa: E402


@skip_unless_engine(HAVE_GIS, "needs geopandas, pyogrio and shapely")
class EsriTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import columbus_esri

        cls.e = columbus_esri
        cls.dir = tempfile.TemporaryDirectory()
        root = cls.dir.name
        parcels = gpd.GeoDataFrame(
            {"PARCEL_ID": ["P1", "P2", "P3"], "STATUS": ["A", "R", "A"]},
            geometry=[box(0, 0, 1, 1), box(1, 1, 2, 2), box(5, 5, 6, 6)], crs="EPSG:2277")
        os.makedirs(os.path.join(root, "shp"))
        cls.shp = os.path.join(root, "shp")
        parcels.to_file(os.path.join(cls.shp, "parcels.shp"), engine="pyogrio")
        cls.gdb = os.path.join(root, "county.gdb")
        parcels.to_file(cls.gdb, layer="parcels", driver="OpenFileGDB", engine="pyogrio")
        gpd.GeoDataFrame({"N": [1]}, geometry=[Point(0, 0)], crs="EPSG:2277").to_file(
            cls.gdb, layer="pts", driver="OpenFileGDB", engine="pyogrio")

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def test_lists_layers_with_fields_geometry_and_crs(self):
        gdb = {e["name"]: e for e in self.e.esri_list({"kind": "filegdb", "path": self.gdb}, 30)}
        self.assertEqual(sorted(gdb), ["parcels", "pts"])
        p = gdb["parcels"]
        self.assertEqual(p["geometry_column"], "SHAPE")
        self.assertEqual(p["srs_id"], 2277)
        self.assertEqual(p["estimated_rows"], 3)
        names = [f["name"] for f in p["fields"]]
        self.assertEqual(names[0], "OBJECTID")
        self.assertIn("SHAPE", names)
        shp = self.e.esri_list({"kind": "shapefile", "path": self.shp}, 30)
        self.assertEqual(shp[0]["geometry_column"], "GEOMETRY")
        # One layer only, when asked.
        only = self.e.esri_list({"kind": "filegdb", "path": self.gdb}, 30, "PTS")
        self.assertEqual([e["name"] for e in only], ["pts"])

    def _read(self, conn, sql, max_rows=10, keep=None):
        return self.e.esri_fetch(conn, sql, max_rows, 30, keep)

    def test_reads_streamed_and_paged_alike(self):
        sql = 'SELECT "PARCEL_ID", "SHAPE" FROM (SELECT * FROM "parcels" columbus_filtered WHERE (columbus_filtered.STATUS = \'A\')) parcels'
        conn = {"kind": "filegdb", "path": self.gdb}
        results = []
        variants = [False]
        try:
            import pyarrow  # noqa: F401

            variants.insert(0, True)
        except ImportError:
            pass
        for streamed in variants:
            with mock.patch.dict(sys.modules, {} if streamed else {"pyarrow": None}):
                cols, rows, hit, cells, geoms = self._read(conn, sql)
            self.assertEqual(cols, ["PARCEL_ID", "SHAPE"])
            self.assertEqual(geoms, {1: "ewkb"})
            self.assertFalse(hit)
            results.append([r[0] for r in rows])
            g = w.decode_geometry(rows[0][1], "ewkb")
            self.assertEqual(g.srid, 2277)
        for r in results:
            self.assertEqual(r, ["P1", "P3"])

    def test_row_limit_and_pages(self):
        conn = {"kind": "shapefile", "path": self.shp}
        with mock.patch.object(self.e, "PAGE", 1), mock.patch.dict(sys.modules, {"pyarrow": None}):
            cols, rows, hit, _, _ = self._read(conn, 'SELECT * FROM "parcels"', max_rows=2)
            self.assertEqual(len(rows), 2)
            self.assertTrue(hit)
            cols, rows, hit, _, _ = self._read(conn, 'SELECT * FROM "parcels"', max_rows=5)
            self.assertEqual([r[0] for r in rows], ["P1", "P2", "P3"])
            self.assertFalse(hit)

    def test_paths_are_checked(self):
        with self.assertRaises(w.WorkerError):
            self.e.esri_list({"kind": "filegdb", "path": self.shp}, 30)
        with self.assertRaises(w.WorkerError):
            self.e.esri_list({"kind": "shapefile", "path": os.path.join(self.shp, "parcels.dbf")}, 30)
        # One .shp file is a connection too.
        one = self.e.esri_list({"kind": "shapefile", "path": os.path.join(self.shp, "parcels.shp")}, 30)
        self.assertEqual([e["name"] for e in one], ["parcels"])

    def test_what_isnt_offered(self):
        root = tempfile.mkdtemp(dir=self.dir.name)
        g = gpd.GeoDataFrame({"N": [1]}, geometry=[Point(0, 0)], crs="EPSG:2277")
        g.to_file(os.path.join(root, "good.shp"), engine="pyogrio")
        # A dot in the name: SQL would read roads.v2 as file roads, layer v2.
        g.to_file(os.path.join(root, "roads.v2.shp"), engine="pyogrio")
        names = [e["name"] for e in self.e.esri_list({"kind": "shapefile", "path": root}, 30)]
        self.assertEqual(names, ["good"])
        # A .shp that is really VRT XML (GDAL picks a driver by content).
        vrt = os.path.join(root, "looks.shp")
        with open(vrt, "w") as f:
            f.write('<OGRVRTDataSource><OGRVRTLayer name="x"><SrcDataSource>/etc/passwd</SrcDataSource></OGRVRTLayer></OGRVRTDataSource>')
        with self.assertRaises(w.WorkerError):
            self.e.esri_list({"kind": "shapefile", "path": vrt}, 30)
        # A folder of CSVs opens with another driver: nothing offered.
        csvs = tempfile.mkdtemp(dir=self.dir.name)
        with open(os.path.join(csvs, "t.csv"), "w") as f:
            f.write("a,b\n1,2\n")
        self.assertEqual(self.e.esri_list({"kind": "shapefile", "path": csvs}, 30), [])
        with self.assertRaises(w.WorkerError):
            self.e.esri_fetch({"kind": "shapefile", "path": csvs}, 'SELECT * FROM "t"', 10, 30)
        # A shapefile whose .dbf is a link out of the folder.
        linked = tempfile.mkdtemp(dir=self.dir.name)
        outside = tempfile.mkdtemp(dir=self.dir.name)
        g.to_file(os.path.join(outside, "secret.shp"), engine="pyogrio")
        for ext in (".shp", ".shx"):
            os.link(os.path.join(outside, "secret" + ext), os.path.join(linked, "secret" + ext))
        os.symlink(os.path.join(outside, "secret.dbf"), os.path.join(linked, "secret.dbf"))
        self.assertEqual(self.e.esri_list({"kind": "shapefile", "path": linked}, 30), [])
        # Dates: as ISO text, a null one as null, and listed as text dates.
        dated = tempfile.mkdtemp(dir=self.dir.name)
        import numpy as np
        import shapely
        from pyogrio.raw import write

        write(os.path.join(dated, "visits.shp"),
              geometry=np.array([shapely.to_wkb(Point(0, 0)), shapely.to_wkb(Point(1, 1))], dtype=object),
              field_data=[np.array(["2024-01-02", "NaT"], dtype="datetime64[D]")], fields=["D"],
              geometry_type="Point", crs="EPSG:2277", driver="ESRI Shapefile")
        entry = self.e.esri_list({"kind": "shapefile", "path": dated}, 30)[0]
        self.assertEqual(entry["text_dates"], ["D"])
        with mock.patch.dict(sys.modules, {"pyarrow": None}):
            _, rows, _, _, _ = self.e.esri_fetch({"kind": "shapefile", "path": dated}, 'SELECT "D" FROM "visits"', 10, 30)
        self.assertEqual(rows[0][0], "2024-01-02")
        self.assertIsNone(rows[1][0])

    def test_dispatch_through_the_worker(self):
        out = w.list_datasets({"connection": {"kind": "filegdb", "path": self.gdb}, "timeout_s": 30})
        self.assertEqual(sorted(e["name"] for e in out["datasets"]), ["parcels", "pts"])


if __name__ == "__main__":
    unittest.main()
