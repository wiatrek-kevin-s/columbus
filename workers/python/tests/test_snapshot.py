"""map_snapshot: layers drawn as a PNG, geometry only."""

import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

try:
    import geopandas as gpd
    import rasterio
    from shapely.geometry import LineString, Point, box

    import columbus_gis as g
except ImportError:  # pragma: no cover
    gpd = None


@unittest.skipIf(gpd is None, "needs geopandas and rasterio")
class SnapshotTest(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()

    def layer(self, name, geoms, crs="EPSG:4269"):
        p = os.path.join(self.d, f"{name}.gpkg")
        gpd.GeoDataFrame({"SECRET": ["x"] * len(geoms)}, geometry=geoms, crs=crs).to_file(p, layer=name, driver="GPKG", engine="pyogrio")
        return p

    def test_layers_are_drawn_in_one_piece_across_the_antimeridian(self):
        a = self.layer("a", [box(172, 51, 179.9, 53), box(-179.9, 51, -170, 53)])
        b = self.layer("b", [Point(175, 52)], crs="EPSG:4326")
        c = self.layer("c", [LineString([(-175, 51.5), (178, 52.5)])])
        out = os.path.join(self.d, "s.png")
        r = g.map_snapshot([a, b, c], out, 900, 600)
        self.assertTrue(r["wrapped_antimeridian"])
        # About 18 degrees wide, not the whole world.
        self.assertLess(r["extent"][2] - r["extent"][0], 40)
        self.assertEqual([x["features"] for x in r["layers"]], [2, 1, 1])
        self.assertEqual(len({x["color"] for x in r["layers"]}), 3)
        with rasterio.open(out) as src:
            self.assertEqual((src.width, src.height), (r["width"], r["height"]))
            self.assertEqual(src.count, 3)
        with open(out, "rb") as f:
            self.assertTrue(f.read(8).startswith(b"\x89PNG"))
        self.assertFalse(os.path.exists(out + ".aux.xml"), "no side file")

    def raster(self, name, data, crs, bounds):
        import numpy as np
        from rasterio.transform import from_bounds
        p = os.path.join(self.d, f"{name}.tif")
        data = np.asarray(data)
        count, h, w = data.shape
        with rasterio.open(p, "w", driver="GTiff", width=w, height=h, count=count, dtype=data.dtype,
                           crs=crs, transform=from_bounds(*bounds, w, h), nodata=0 if data.dtype.kind == "u" else None) as dst:
            dst.write(data)
        return p

    def test_imagery_is_drawn_underneath_in_the_layers_crs(self):
        import numpy as np
        # A layer in metres (Texas), imagery in degrees around it: the
        # imagery is warped onto the picture, the layer drawn over it.
        a = self.layer("a", [box(1_205_000, 946_000, 1_225_000, 966_000)], crs="EPSG:3081")
        red = np.zeros((3, 50, 50), dtype="uint8")
        red[0] = 200
        img = self.raster("rgb", red, "EPSG:4326", (-98.5, 30.0, -97.0, 31.5))
        out = os.path.join(self.d, "s.png")
        r = g.map_snapshot([a], out, 400, 400, imagery=img)
        self.assertEqual(r["imagery"]["bands"], "red, green, blue")
        with rasterio.open(out) as src:
            px = src.read()
        # The corner, outside the polygon but under the imagery: red.
        self.assertEqual(int(px[0, 5, 5]), 200)
        self.assertEqual(int(px[1, 5, 5]), 0)
        # One band: grey.
        dem = self.raster("dem", np.arange(2500, dtype="float32").reshape(1, 50, 50) + 1,
                          "EPSG:4326", (-98.5, 30.0, -97.0, 31.5))
        r = g.map_snapshot([a], out, 400, 400, imagery=dem)
        self.assertEqual(r["imagery"]["bands"], "one band, grey")
        with rasterio.open(out) as src:
            px = src.read()
        self.assertEqual(int(px[0, 5, 5]), int(px[1, 5, 5]))
        self.assertEqual(int(px[1, 5, 5]), int(px[2, 5, 5]))
        # Imagery somewhere else entirely: the paper shows through.
        far = self.raster("far", red, "EPSG:4326", (10, 10, 11, 11))
        r = g.map_snapshot([a], out, 400, 400, imagery=far)
        self.assertFalse(r["imagery"]["covers"])

    def test_large_imagery_is_read_smaller_not_refused(self):
        import numpy as np
        a = self.layer("a", [box(1_205_000, 946_000, 1_225_000, 966_000)], crs="EPSG:3081")
        big = np.full((1, 3000, 4000), 50.0, dtype="float32")
        img = self.raster("big", big, "EPSG:3081", (1_100_000, 850_000, 1_300_000, 1_050_000))
        out = os.path.join(self.d, "s.png")
        r = g.map_snapshot([a], out, 1600, 1200, imagery=img)
        self.assertTrue(r["imagery"]["covers"])

    def test_imagery_that_is_not_a_geotiff_is_refused(self):
        import numpy as np
        a = self.layer("a", [box(1_205_000, 946_000, 1_225_000, 966_000)], crs="EPSG:3081")
        red = np.full((3, 20, 20), 200, dtype="uint8")
        self.raster("secret", red, "EPSG:4326", (-98.5, 30.0, -97.0, 31.5))
        kml = os.path.join(self.d, "over.kml")
        with open(kml, "w") as f:
            f.write('<?xml version="1.0"?><kml xmlns="http://www.opengis.net/kml/2.2"><Document><GroundOverlay>'
                    '<Icon><href>secret.tif</href></Icon><LatLonBox><north>31.5</north><south>30</south>'
                    '<east>-97</east><west>-98.5</west></LatLonBox></GroundOverlay></Document></kml>')
        out = os.path.join(self.d, "s.png")
        with self.assertRaises(g.GISError):
            g.map_snapshot([a], out, 400, 400, imagery=kml)

    def test_what_it_refuses(self):
        a = self.layer("a", [box(0, 0, 1, 1)], crs="EPSG:3081")
        out = os.path.join(self.d, "s.png")
        with self.assertRaises(g.GISError):
            g.map_snapshot([], out)
        with self.assertRaises(g.GISError):
            g.map_snapshot([a] * 9, out)
        with self.assertRaises(g.GISError):
            g.map_snapshot([a], out, bbox=[1, 1, 0, 0])
        csv = os.path.join(self.d, "t.csv")
        with open(csv, "w") as f:
            f.write("a,b\n1,2\n")
        with self.assertRaises(g.GISError):
            g.map_snapshot([csv], out)
        # A picture keeps the area's shape within the size asked for.
        r = g.map_snapshot([a], out, 1600, 400)
        self.assertLessEqual(r["width"], 1600)
        self.assertEqual(r["height"], 400)


if __name__ == "__main__":
    unittest.main()
