"""Tests for the raster engine (columbus_raster) and the worker's raster tools.

Small synthetic DEMs are written with rasterio in a temporary folder.
Skipped when rasterio or the GIS engine is not installed.
"""

import math
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

try:
    import geopandas as gpd
    import numpy as np
    import rasterio
    from rasterio.transform import from_origin
    from shapely.geometry import Polygon, box

    import columbus_raster as r

    HAVE_RASTER = True
except ImportError:  # pragma: no cover
    HAVE_RASTER = False

import columbus_worker as w  # noqa: E402

# A UTM zone 14N origin near Austin.
X0, Y0 = 620000.0, 3350000.0


def write(path, data, crs="EPSG:32614", res=1.0, nodata=-9999.0, x0=X0, y0=Y0):
    data = np.asarray(data)
    if data.ndim == 2:
        data = data[np.newaxis]
    with rasterio.open(path, "w", driver="GTiff", height=data.shape[1], width=data.shape[2], count=data.shape[0],
                       dtype=data.dtype.name, crs=crs, transform=from_origin(x0, y0, res, res), nodata=nodata) as d:
        d.write(data)
    return path


def plane(rows=40, cols=40, rise=0.1, res=1.0):
    """Elevations rising `rise` per horizontal unit toward the east."""
    cols_idx = np.arange(cols, dtype="float32")[np.newaxis, :].repeat(rows, axis=0)
    return (100.0 + rise * res * cols_idx).astype("float32")


def ids(result):
    return [c["id"] for c in result["checks"]]


def find(result, cid):
    return next(c for c in result["checks"] if c["id"] == cid)


def read1(path):
    with rasterio.open(path) as d:
        return d.read(1), d.nodata, d.profile, d.tags(ns="IMAGE_STRUCTURE")


@unittest.skipUnless(HAVE_RASTER, "raster engine not installed")
class RasterTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def p(self, name):
        return os.path.join(self.dir, name)

    def op(self, op, inputs, params=None, max_pixels=None):
        return r.raster_op(op, inputs, params or {}, self.p(f"out-{op}"), max_pixels)

    # -- opening and describing ------------------------------------------

    def test_describe(self):
        dem = write(self.p("dem.tif"), plane())
        d = r.describe_raster(dem)
        self.assertEqual((d["width"], d["height"], d["band_count"]), (40, 40, 1))
        self.assertEqual(d["bands"][0], {"band": 1, "dtype": "float32", "nodata": -9999, "description": None})
        self.assertEqual(d["crs"]["code"], "EPSG:32614")
        self.assertEqual(d["resolution"], [1.0, 1.0])
        self.assertEqual(d["extent"], [X0, Y0 - 40, X0 + 40, Y0])
        self.assertIn("crs.info", ids(d))
        self.assertNotIn("raster.nodata", ids(d))
        # No nodata: a warning. Geographic: info. No CRS: a fail.
        d = r.describe_raster(write(self.p("nonull.tif"), plane(), nodata=None))
        self.assertEqual(find(d, "raster.nodata")["severity"], "warn")
        d = r.describe_raster(write(self.p("geo.tif"), plane(), crs="EPSG:4326", res=0.001, x0=-97.8, y0=30.3))
        self.assertEqual(find(d, "crs.geographic")["severity"], "info")
        d = r.describe_raster(write(self.p("nocrs.tif"), plane(), crs=None))
        self.assertEqual(find(d, "crs.present")["severity"], "fail")

    def test_only_real_tiffs_are_opened(self):
        # A ".tif" holding VRT XML that would read another file.
        fake = self.p("dem.tif")
        with open(fake, "w") as f:
            f.write('<VRTDataset rasterXSize="1" rasterYSize="1"><VRTRasterBand dataType="Byte" band="1">'
                    "<SimpleSource><SourceFilename>/etc/passwd</SourceFilename></SimpleSource></VRTRasterBand></VRTDataset>")
        with self.assertRaises(r.GISError) as e:
            r.describe_raster(fake)
        self.assertEqual(e.exception.check_id, "input.format")
        self.assertIn("not TIFF", e.exception.message)
        self.assertFalse(r.is_tiff(fake))
        # The VRT driver is off in the engine as well.
        self.assertIn("VRT", os.environ["GDAL_SKIP"].split())
        with rasterio.Env() as env:
            self.assertNotIn("VRT", env.drivers())
        real = write(self.p("real.tif"), plane())
        self.assertTrue(r.is_tiff(real))
        for bad in ["/vsicurl/http://example.org/x.tif", "GTIFF_DIR:1:" + real, real + ":stream", "relative.tif",
                    self.p("x.png"), "http://example.org/x.tif"]:
            with self.assertRaises(r.GISError, msg=bad):
                r.describe_raster(bad)

    # -- terrain ---------------------------------------------------------

    def test_slope_of_a_known_plane(self):
        dem = write(self.p("dem.tif"), plane(rise=0.1))
        out = self.op("slope", {"raster": dem}, {"z_unit": "m"})
        slope, nodata, profile, tags = read1(out["output_path"])
        self.assertEqual(nodata, -9999)
        # Every cell, edges included: no nodata border.
        self.assertTrue(np.allclose(slope, math.degrees(math.atan(0.1)), atol=1e-3), slope)
        self.assertEqual(tags.get("LAYOUT"), "COG")
        self.assertEqual(profile["compress"], "deflate")
        self.assertIn("raster.cog", ids(out))
        self.assertIn("terrain.edges", ids(out))
        self.assertAlmostEqual(find(out, "terrain.z_factor")["detail"]["z_factor"], 1.0)
        pct = self.op("slope", {"raster": dem}, {"z_unit": "m", "units": "percent"})
        self.assertTrue(np.allclose(read1(pct["output_path"])[0], 10.0, atol=1e-3))

    def test_z_factor_in_feet(self):
        # The same numbers read as feet on a metre grid: a gentler slope.
        dem = write(self.p("dem.tif"), plane(rise=0.1))
        out = self.op("slope", {"raster": dem}, {"z_unit": "ft"})
        self.assertAlmostEqual(find(out, "terrain.z_factor")["detail"]["z_factor"], 0.3048)
        self.assertTrue(np.allclose(read1(out["output_path"])[0], math.degrees(math.atan(0.1 * 0.3048)), atol=1e-3))
        # Feet on a US-feet grid (EPSG:2277): a factor of about 1.
        ft = write(self.p("dem_ft.tif"), plane(rise=0.1, res=3.0), crs="EPSG:2277", res=3.0, x0=3100000.0, y0=10070000.0)
        out = self.op("slope", {"raster": ft}, {"z_unit": "ft"})
        self.assertAlmostEqual(find(out, "terrain.z_factor")["detail"]["z_factor"], 1.0, places=5)
        self.assertTrue(np.allclose(read1(out["output_path"])[0], math.degrees(math.atan(0.1)), atol=1e-3))

    def test_aspect_and_hillshade(self):
        dem = write(self.p("dem.tif"), plane(rise=0.1))
        aspect = read1(self.op("aspect", {"raster": dem}, {"z_unit": "m"})["output_path"])[0]
        self.assertTrue(np.allclose(aspect, 270.0, atol=0.01), aspect)  # rising east, so facing west
        flat = write(self.p("flat.tif"), np.full((20, 20), 50.0, "float32"))
        self.assertTrue(np.all(read1(self.op("aspect", {"raster": flat}, {"z_unit": "m"})["output_path"])[0] == -1))
        hs, nodata, profile, _ = read1(self.op("hillshade", {"raster": flat}, {"z_unit": "m"})["output_path"])
        self.assertEqual((profile["dtype"], nodata), ("uint8", 0))
        self.assertTrue(np.all(hs == round(255 * math.cos(math.radians(45)))))
        # Lit from the north-west, a west-facing slope is brighter than an east-facing one.
        west = read1(self.op("hillshade", {"raster": dem}, {"z_unit": "m"})["output_path"])[0]
        east_dem = write(self.p("east.tif"), plane(rise=-0.1))
        east = read1(r.raster_op("hillshade", {"raster": east_dem}, {"z_unit": "m"}, self.p("hs_east"))["output_path"])[0]
        self.assertGreater(int(west[20, 20]), int(east[20, 20]))

    def test_nodata_holes_and_edges(self):
        z = plane(rise=0.1)
        z[18:21, 18:21] = -9999.0
        dem = write(self.p("dem.tif"), z)
        out = self.op("slope", {"raster": dem}, {"z_unit": "m"})
        slope, nodata, _, _ = read1(out["output_path"])
        self.assertTrue(np.all(slope[18:21, 18:21] == nodata))
        valid = slope != nodata
        self.assertEqual(int(valid.sum()), 40 * 40 - 9)
        # Next to the hole the fill value never becomes a cliff.
        self.assertLess(float(slope[valid].max()), 10.0)
        self.assertTrue(np.allclose(slope[0:10, 0:10], math.degrees(math.atan(0.1)), atol=1e-3))
        self.assertIn("raster.nodata", ids(out))
        # A DEM without nodata: its fill value is used as an elevation, said so.
        nn = write(self.p("nn.tif"), plane(), nodata=None)
        self.assertEqual(find(self.op("slope", {"raster": nn}, {"z_unit": "m"}), "raster.nodata")["severity"], "warn")

    def test_terrain_refusals(self):
        geo = write(self.p("geo.tif"), plane(), crs="EPSG:4326", res=0.0001, x0=-97.8, y0=30.3)
        with self.assertRaises(r.GISError) as e:
            self.op("slope", {"raster": geo}, {"z_unit": "m"})
        self.assertEqual(e.exception.check_id, "crs.measure")
        self.assertIn("reproject", e.exception.fix)
        self.assertIn("EPSG:3081", e.exception.fix)  # suggested for Texas
        merc = write(self.p("merc.tif"), plane(), crs="EPSG:3857", x0=-10890000.0, y0=3530000.0)
        with self.assertRaises(r.GISError) as e:
            self.op("hillshade", {"raster": merc}, {"z_unit": "m"})
        self.assertEqual(e.exception.check_id, "crs.measure")
        dem = write(self.p("dem.tif"), plane())
        with self.assertRaises(r.GISError) as e:
            self.op("slope", {"raster": dem}, {})
        self.assertIn("z_unit is required", e.exception.message)
        with self.assertRaises(r.GISError):
            self.op("slope", {"raster": dem}, {"z_unit": "m", "units": "radians"})

    # -- clip, reproject, size guard -------------------------------------

    def test_clip_by_bbox_and_mask(self):
        dem = write(self.p("dem.tif"), plane())
        out = self.op("clip", {"raster": dem}, {"bbox": [X0 + 10, Y0 - 20, X0 + 20, Y0 - 10], "bbox_crs": "EPSG:32614"})
        a, nodata, profile, _ = read1(out["output_path"])
        self.assertEqual(a.shape, (10, 10))
        self.assertEqual(out["summary"]["extent"], [X0 + 10, Y0 - 20, X0 + 20, Y0 - 10])
        self.assertTrue(np.allclose(a[0], 100.0 + 0.1 * np.arange(10, 20)))
        # A mask in another CRS: moved to the raster's, and said so.
        mask_path = self.p("mask.gpkg")
        tri = gpd.GeoDataFrame(geometry=[Polygon([(X0 + 10, Y0 - 30), (X0 + 30, Y0 - 30), (X0 + 10, Y0 - 10)])],
                               crs="EPSG:32614").to_crs("EPSG:4326")
        tri.to_file(mask_path, engine="pyogrio")
        out = self.op("clip", {"raster": dem, "mask": mask_path})
        self.assertIn("crs.agreement", ids(out))
        a, nodata, _, _ = read1(out["output_path"])
        self.assertIn(a.shape, [(20, 20), (21, 21), (20, 21), (21, 20)])
        # Cells outside the triangle are nodata; about half the window.
        self.assertTrue(0.3 < float((a == nodata).mean()) < 0.7, a)
        self.assertIn("clip.cells", ids(out))
        with self.assertRaises(r.GISError) as e:
            self.op("clip", {"raster": dem}, {"bbox": [0, 0, 1, 1], "bbox_crs": "EPSG:32614"})
        self.assertEqual(e.exception.check_id, "clip.empty")
        with self.assertRaises(r.GISError):
            self.op("clip", {"raster": dem}, {"bbox": [X0, Y0 - 5, X0 + 5, Y0]})  # no bbox_crs
        with self.assertRaises(r.GISError):
            self.op("clip", {"raster": dem})  # neither mask nor bbox

    def test_reproject_and_the_resampling_warning(self):
        classes = write(self.p("landcover.tif"), (np.arange(1600).reshape(40, 40) % 7 + 41).astype("uint8"), nodata=0)
        out = self.op("reproject", {"raster": classes}, {"crs": "EPSG:2277", "resampling": "bilinear"})
        self.assertEqual(find(out, "raster.resampling")["severity"], "warn")
        self.assertEqual(out["summary"]["crs"]["code"], "EPSG:2277")
        out = self.op("reproject", {"raster": classes}, {"crs": "EPSG:2277", "resampling": "nearest", "resolution": 10})
        self.assertNotIn("raster.resampling", ids(out))
        self.assertEqual(out["summary"]["resolution"], [10.0, 10.0])
        self.assertIn("crs.reprojected", ids(out))
        # Continuous data with bilinear: no warning.
        dem = write(self.p("dem.tif"), plane())
        self.assertNotIn("raster.resampling", ids(self.op("reproject", {"raster": dem}, {"crs": "EPSG:2277", "resampling": "bilinear"})))
        with self.assertRaises(r.GISError) as e:
            self.op("reproject", {"raster": dem}, {"crs": "EPSG:2277"})
        self.assertIn("resampling is required", e.exception.message)
        with self.assertRaises(r.GISError):
            self.op("reproject", {"raster": dem}, {"crs": "+proj=longlat +init=x", "resampling": "nearest"})

    def test_oversize_output_refused(self):
        dem = write(self.p("dem.tif"), plane())
        for op, params in (("slope", {"z_unit": "m"}), ("reproject", {"crs": "EPSG:2277", "resampling": "bilinear", "resolution": 0.001}),
                           ("clip", {"bbox": [X0, Y0 - 40, X0 + 40, Y0], "bbox_crs": "EPSG:32614"})):
            with self.assertRaises(r.GISError, msg=op) as e:
                self.op(op, {"raster": dem}, params, max_pixels=1000 if op != "reproject" else None)
            self.assertEqual(e.exception.check_id, "output.too_large", op)
            self.assertIn("Clip", e.exception.fix)

    # -- zonal statistics ------------------------------------------------

    def test_zonal_stats(self):
        # Cell values are the column number (0 to 39); one nodata cell.
        values = np.arange(40, dtype="float32")[np.newaxis, :].repeat(40, axis=0)
        values[5, 5] = -9999.0
        ras = write(self.p("values.tif"), values)
        zones = gpd.GeoDataFrame(
            {"name": ["a", "b", "outside", "partly", "tiny"]},
            geometry=[box(X0, Y0 - 10, X0 + 10, Y0),                 # columns 0-9, with the nodata cell
                      box(X0 + 20, Y0 - 30, X0 + 30, Y0 - 20),       # columns 20-29
                      box(X0 + 100, Y0 - 10, X0 + 110, Y0),          # off the raster
                      box(X0 + 35, Y0 - 10, X0 + 45, Y0),            # half on
                      box(X0 + 12.1, Y0 - 12.4, X0 + 12.3, Y0 - 12.2)],  # no cell centre
            crs="EPSG:32614")
        zpath = self.p("zones.gpkg")
        zones.to_file(zpath, engine="pyogrio")
        out = self.op("zonal_stats", {"raster": ras, "zones": zpath}, {"stats": ["count", "mean", "min", "max", "sum", "std"]})
        self.assertTrue(out["output_path"].endswith(".gpkg"))
        df = gpd.read_file(out["output_path"]).set_index("name")
        self.assertEqual(int(df.loc["a", "count"]), 99)
        self.assertAlmostEqual(df.loc["a", "mean"], (4.5 * 100 - 5) / 99, places=5)
        self.assertEqual((df.loc["b", "min"], df.loc["b", "max"], int(df.loc["b", "count"])), (20.0, 29.0, 100))
        self.assertAlmostEqual(df.loc["b", "sum"], 2450.0)
        self.assertAlmostEqual(df.loc["b", "std"], float(np.std(np.arange(20, 30))), places=5)
        self.assertEqual(int(df.loc["outside", "count"]), 0)
        self.assertTrue(math.isnan(df.loc["outside", "mean"]))
        self.assertEqual(int(df.loc["partly", "count"]), 50)
        self.assertEqual(df.loc["partly", "mean"], 37.0)
        self.assertEqual(int(df.loc["tiny", "count"]), 0)
        c = find(out, "zonal.outside")
        self.assertEqual((c["severity"], c["detail"]), ("warn", {"fully_outside": 1, "partly_outside": 1}))
        self.assertEqual(find(out, "zonal.no_pixels")["detail"], {"zones": 2})
        self.assertIn("zonal.nodata", ids(out))
        self.assertNotIn("zonal.cell_size", ids(out))
        # all_touched gives the tiny zone its cell.
        out = self.op("zonal_stats", {"raster": ras, "zones": zpath}, {"all_touched": True})
        df = gpd.read_file(out["output_path"]).set_index("name")
        self.assertEqual(int(df.loc["tiny", "count"]), 1)
        self.assertEqual(list(df.columns[:1]), ["count"])

    def test_zonal_cell_size_warning(self):
        ras = write(self.p("coarse.tif"), np.ones((10, 10), "float32"), res=30.0)
        small = gpd.GeoDataFrame(geometry=[box(X0 + 30 * i, Y0 - 60, X0 + 30 * i + 60, Y0) for i in range(4)], crs="EPSG:32614")
        zpath = self.p("small.gpkg")
        small.to_file(zpath, engine="pyogrio")
        out = self.op("zonal_stats", {"raster": ras, "zones": zpath})
        self.assertEqual(find(out, "zonal.cell_size")["detail"]["median_cells"], 4.0)

    # -- info and the worker ---------------------------------------------

    def test_info(self):
        z = plane()
        z[0, :10] = -9999.0
        dem = write(self.p("dem.tif"), z)
        out = self.op("info", {"raster": dem})
        self.assertNotIn("output_path", out)
        b = out["info"]["bands"][0]
        self.assertEqual(out["info"]["computed_on"], "all cells")
        self.assertAlmostEqual(b["nodata_share"], 10 / 1600)
        self.assertEqual((b["min"], b["max"]), (100, 103.9000015258789))
        old = r.FULL_STATS_PIXELS, r.SAMPLE_PIXELS
        r.FULL_STATS_PIXELS, r.SAMPLE_PIXELS = 100, 100
        try:
            out = self.op("info", {"raster": dem})
        finally:
            r.FULL_STATS_PIXELS, r.SAMPLE_PIXELS = old
        self.assertIn("raster.stats_sampled", ids(out))
        self.assertIn("sample", out["info"]["computed_on"])

    def test_worker_methods(self):
        dem = write(self.p("dem.tif"), plane())
        d = w.describe_raster({"path": dem})
        self.assertEqual(d["band_count"], 1)
        out = w.raster_op({"op": "slope", "inputs": {"raster": dem}, "params": {"z_unit": "m", "_max_pixels": 1},
                           "output_stem": self.p("results/slope"), "max_pixels": 400_000_000})
        self.assertTrue(out["output_path"].endswith("slope.tif"))
        with self.assertRaises(w.WorkerError) as e:
            w.raster_op({"op": "slope", "inputs": {"raster": dem}, "params": {"z_unit": "m"},
                         "output_stem": self.p("results/slope2"), "max_pixels": 100})
        self.assertIn("output.too_large", str(e.exception))
        with self.assertRaises(w.WorkerError):
            w.raster_op({"op": "slope", "inputs": {"dem": dem}, "params": {"z_unit": "m"}, "output_stem": self.p("x")})
        self.assertIn("rasterio", w.ping({})["engines"])


if __name__ == "__main__":
    unittest.main()
