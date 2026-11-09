"""export_layer: the file geodatabase (R-8) and GeoTIFF (R-9) formats."""

from __future__ import annotations

import math
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from engine_required import skip_unless_engine  # noqa: E402

try:
    # First: it sets GDAL_SKIP and the other GDAL settings before GDAL loads.
    import columbus_gis as g  # noqa: I001

    import geopandas as gpd
    import numpy as np
    import pyogrio
    import rasterio
    from rasterio.transform import from_origin
    from shapely.geometry import Point

    import columbus_raster as r

    HAVE_ENGINE = True
except ImportError:  # pragma: no cover
    HAVE_ENGINE = False

import columbus_worker as w  # noqa: E402

BIG = 3_000_000_000_123


def ids(result):
    return [c["id"] for c in result["checks"]]


@skip_unless_engine(HAVE_ENGINE, "GIS engine not installed")
class GdbExportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def p(self, name):
        return os.path.join(self.dir, name)

    def points(self, big=False, **extra):
        cols = {"site": ["a", "b", "c"], "count": np.array([1, 2, 3], dtype="int64"), **extra}
        if big:
            cols["parcel_id"] = np.array([1, BIG, -5], dtype="int64")
        gdf = gpd.GeoDataFrame(cols, geometry=[Point(620000 + i, 3350000) for i in range(3)], crs="EPSG:32614")
        path = self.p("in.gpkg")
        gdf.to_file(path, layer="layer", driver="GPKG", engine="pyogrio")
        return path

    def test_points_to_gdb(self):
        out = g.export_layer(self.points(), self.p("out/crash_hotspots"), "gdb")
        self.assertEqual(out["files"], [self.p("out/crash_hotspots.gdb")])
        self.assertTrue(os.path.isdir(out["files"][0]))
        self.assertEqual(pyogrio.list_layers(out["files"][0])[:, 0].tolist(), ["crash_hotspots"])
        info = pyogrio.read_info(out["files"][0])
        self.assertEqual(info["features"], 3)
        self.assertEqual(info["crs"], "EPSG:32614")
        # 32-bit values stay integers, readable in every ArcGIS: no Pro 3.2 option.
        self.assertEqual(dict(zip(info["fields"], info["ogr_types"]))["count"], "OFTInteger")
        self.assertNotIn("export.needs_pro_3_2", ids(out))
        # Delivery spec aliases resolve to the same driver.
        out = g.export_layer(self.points(), self.p("out2/x"), "File Geodatabase")
        self.assertTrue(out["files"][0].endswith("x.gdb"))

    def test_int64_beyond_32_bits_is_exact(self):
        out = g.export_layer(self.points(big=True), self.p("out/big"), "gdb")
        c = next(c for c in out["checks"] if c["id"] == "export.needs_pro_3_2")
        self.assertEqual(c["severity"], "info")
        self.assertEqual(c["detail"]["fields"], ["parcel_id"])
        back = gpd.read_file(out["files"][0], engine="pyogrio")
        self.assertEqual(back["parcel_id"].tolist(), [1, BIG, -5])
        self.assertEqual(str(back["parcel_id"].dtype), "int64")

    def test_long_field_names_are_refused_with_the_list(self):
        long_a, long_b = "a" * 65, "b" * 70
        src = self.points(**{long_a: [1, 2, 3], long_b: ["x", "y", "z"], "c" * 64: [0, 0, 0]})
        with self.assertRaises(g.GISError) as e:
            g.export_layer(src, self.p("out/long"), "gdb")
        self.assertEqual(e.exception.check_id, "export.format")
        self.assertIn(long_a, e.exception.message)
        self.assertIn(long_b, e.exception.message)
        self.assertNotIn("c" * 64 + ",", e.exception.message)
        self.assertFalse(os.path.exists(self.p("out/long.gdb")))

    def test_layer_name_must_be_a_feature_class_name(self):
        with self.assertRaises(g.GISError) as e:
            g.export_layer(self.points(), self.p("out/district 14"), "gdb")
        self.assertIn("crash_hotspots", e.exception.fix)

    def test_an_existing_gdb_is_never_written_into(self):
        first = g.export_layer(self.points(), self.p("out/same"), "gdb")["files"][0]
        with self.assertRaises(g.GISError) as e:
            g.export_layer(self.points(), self.p("out/same"), "gdb")
        self.assertEqual(e.exception.check_id, "export.exists")
        self.assertEqual(pyogrio.list_layers(first)[:, 0].tolist(), ["same"])

    def test_laundered_field_names_are_reported(self):
        out = g.export_layer(self.points(**{"lane count": [1, 2, 3]}), self.p("out/laundered"), "gdb")
        c = next(c for c in out["checks"] if c["id"] == "export.field_names")
        self.assertEqual(c["severity"], "warn")
        self.assertIn("lane count to lane_count", c["message"])

    def test_raster_as_gdb_is_refused(self):
        tif = self.p("dem.tif")
        with rasterio.open(tif, "w", driver="GTiff", height=2, width=2, count=1, dtype="float32",
                           crs="EPSG:32614", transform=from_origin(0, 2, 1, 1)) as d:
            d.write(np.zeros((1, 2, 2), dtype="float32"))
        with self.assertRaises(g.GISError) as e:
            g.export_layer(tif, self.p("out/r"), "gdb")
        self.assertIn("tif only", e.exception.message)


@skip_unless_engine(HAVE_ENGINE, "GIS engine not installed")
class TifExportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def p(self, name):
        return os.path.join(self.dir, name)

    def slope(self):
        z = (100.0 + 0.1 * np.arange(40, dtype="float32"))[np.newaxis, :].repeat(40, axis=0).astype("float32")
        z[18:21, 18:21] = -9999
        dem = self.p("dem.tif")
        with rasterio.open(dem, "w", driver="GTiff", height=40, width=40, count=1, dtype="float32",
                           crs="EPSG:32614", transform=from_origin(620000, 3350000, 1, 1), nodata=-9999) as d:
            d.write(z, 1)
        return r.raster_op("slope", {"raster": dem}, {"z_unit": "m"}, self.p("ws/slope"))["output_path"]

    def test_slope_to_tif(self):
        src = self.slope()
        out = g.export_layer(src, self.p("out/slope"), "tif")
        self.assertEqual(out["files"], [self.p("out/slope.tif")])
        with open(src, "rb") as a, open(out["files"][0], "rb") as b:
            self.assertEqual(a.read(), b.read())
        with rasterio.open(src) as s, rasterio.open(out["files"][0]) as d:
            self.assertEqual(d.tags(ns="IMAGE_STRUCTURE")["LAYOUT"], "COG")
            self.assertEqual(d.crs, s.crs)
            self.assertEqual(d.nodata, s.nodata)
        self.assertNotIn("export.cog", ids(out))
        # A matching delivery CRS is fine.
        out = g.export_layer(src, self.p("out/match"), "GeoTIFF", crs="EPSG:32614")
        self.assertTrue(os.path.isfile(out["files"][0]))

    def test_another_crs_needs_reprojecting_first(self):
        with self.assertRaises(g.GISError) as e:
            g.export_layer(self.slope(), self.p("out/x"), "tif", crs="EPSG:3081")
        self.assertEqual(e.exception.check_id, "export.crs")
        self.assertIn("reproject first (raster_op reproject)", e.exception.message)
        self.assertFalse(os.path.exists(self.p("out/x.tif")))

    def test_a_plain_geotiff_becomes_a_cog(self):
        plain = self.p("plain.tif")
        with rasterio.open(plain, "w", driver="GTiff", height=8, width=8, count=1, dtype="float32",
                           crs="EPSG:32614", transform=from_origin(0, 8, 1, 1), nodata=float("nan")) as d:
            d.write(np.ones((1, 8, 8), dtype="float32"))
        out = g.export_layer(plain, self.p("out/plain"), "tif")
        self.assertIn("export.cog", ids(out))
        with rasterio.open(out["files"][0]) as d:
            self.assertEqual(d.tags(ns="IMAGE_STRUCTURE")["LAYOUT"], "COG")
            self.assertTrue(math.isnan(d.nodata))

    def test_vector_as_tif_is_refused(self):
        gpkg = self.p("pts.gpkg")
        gpd.GeoDataFrame({"a": [1]}, geometry=[Point(0, 0)], crs="EPSG:4326").to_file(gpkg, engine="pyogrio")
        for fmt in ("tif", "cog"):
            with self.assertRaises(g.GISError) as e:
                g.export_layer(gpkg, self.p("out/v"), fmt)
            self.assertEqual(e.exception.check_id, "export.format")
            self.assertIn("raster results", e.exception.message)

    def test_through_the_worker(self):
        out = w.export_layer({"path": self.slope(), "out_stem": self.p("out/w"), "format": "tif", "crs": None, "rename": {}})
        self.assertTrue(out["files"][0].endswith("w.tif"))


if __name__ == "__main__":
    unittest.main()
