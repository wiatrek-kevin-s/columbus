"""map_snapshot: layers drawn as a PNG, geometry only."""

import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from engine_required import skip_unless_engine  # noqa: E402

try:
    import geopandas as gpd
    import rasterio
    from shapely.geometry import LineString, Point, box

    import columbus_gis as g
except ImportError:  # pragma: no cover
    gpd = None


@skip_unless_engine(gpd is not None, "needs geopandas and rasterio")
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

    @staticmethod
    def mercator(lon, lat):
        import math
        r = 6378137.0
        return r * math.radians(lon), r * math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))

    def pixel(self, r, lon, lat):
        """Where a place falls in a picture of a view (column, row)."""
        x, y = self.mercator(lon, lat)
        minx, miny, maxx, maxy = r["extent"]
        return (int((x - minx) / (maxx - minx) * r["width"]), int((maxy - y) / (maxy - miny) * r["height"]))

    def test_a_view_is_drawn_as_a_web_map_shows_it(self):
        # The person's view (degrees) over Austin; a layer in a Texas CRS in
        # metres, one square inside the view and one far outside it.
        from pyproj import Transformer
        to_tx = Transformer.from_crs(4326, 3081, always_xy=True)
        inside = box(*to_tx.transform(-97.76, 30.26), *to_tx.transform(-97.74, 30.28))
        outside = box(*to_tx.transform(-95.0, 29.0), *to_tx.transform(-94.9, 29.1))
        a = self.layer("a", [inside, outside], crs="EPSG:3081")
        out = os.path.join(self.d, "s.png")
        view = [-97.80, 30.24, -97.70, 30.30]
        r = g.map_snapshot([a], out, 1000, 700, view=view)
        # Exactly the view: its extent in degrees, and the same box in Web
        # Mercator, the CRS drawn.
        self.assertEqual(r["view"], view)
        self.assertEqual(r["crs"], "EPSG:3857")
        want = [*self.mercator(view[0], view[1]), *self.mercator(view[2], view[3])]
        for got, w in zip(r["extent"], want):
            self.assertAlmostEqual(got, w, places=3)
        self.assertFalse(r["wrapped_antimeridian"])
        # The picture has the view's shape (as the map has it).
        shape = (want[2] - want[0]) / (want[3] - want[1])
        self.assertAlmostEqual(r["width"] / r["height"], shape, delta=0.01)
        # The whole layer is counted; one of its two features is in view.
        self.assertEqual(r["layers"][0]["features"], 2)
        self.assertEqual(r["layers"][0]["in_view"], 1)
        with rasterio.open(out) as src:
            px = src.read()
        paper = (246, 243, 236)
        col, row = self.pixel(r, -97.75, 30.27)
        self.assertNotEqual(tuple(int(v) for v in px[:, row, col]), paper, "the square is drawn where it is")
        col, row = self.pixel(r, -97.79, 30.25)
        self.assertEqual(tuple(int(v) for v in px[:, row, col]), paper, "nothing is drawn beside it")
        # A view with none of the layer in it is still that view, empty.
        r = g.map_snapshot([a], out, 1000, 700, view=[10.0, 10.0, 11.0, 11.0])
        self.assertEqual(r["layers"][0]["in_view"], 0)
        self.assertEqual(r["layers"][0]["features"], 2)

    def test_a_view_across_the_180th_meridian_shows_both_sides(self):
        a = self.layer("a", [box(172, 51, 179.9, 53), box(-179.9, 51, -170, 53)])
        out = os.path.join(self.d, "s.png")
        # As a web map reports it: east past 180.
        r = g.map_snapshot([a], out, 900, 600, view=[170.0, 50.0, 192.0, 54.0])
        self.assertEqual(r["view"], [170.0, 50.0, 192.0, 54.0])
        self.assertEqual(r["layers"][0]["in_view"], 2)
        with rasterio.open(out) as src:
            px = src.read()
        paper = (246, 243, 236)
        for lon in (176.0, 185.0):  # 185 is -175, one world on
            col, row = self.pixel(r, lon, 52.0)
            self.assertNotEqual(tuple(int(v) for v in px[:, row, col]), paper, f"drawn at {lon}")
        col, row = self.pixel(r, 191.0, 52.0)
        self.assertEqual(tuple(int(v) for v in px[:, row, col]), paper)
        # The same layer seen from the other side of the line.
        r = g.map_snapshot([a], out, 900, 600, view=[-179.0, 50.0, -168.0, 54.0])
        self.assertEqual(r["layers"][0]["in_view"], 1)

    def test_a_view_reaches_the_edge_of_the_projection_not_past_it(self):
        # A layer that goes to the pole: drawn, with its latitudes held at
        # the edge of a web map's world.
        a = self.layer("a", [box(-180, -90, 180, 90)], crs="EPSG:4326")
        out = os.path.join(self.d, "s.png")
        r = g.map_snapshot([a], out, 800, 800, view=[-180.0, -90.0, 180.0, 90.0])
        self.assertAlmostEqual(r["view"][1], -85.0511287798066)
        self.assertAlmostEqual(r["view"][3], 85.0511287798066)
        self.assertAlmostEqual(r["extent"][0], -r["extent"][2], places=3)
        self.assertAlmostEqual(r["extent"][1], -r["extent"][3], places=3)
        self.assertEqual(r["layers"][0]["in_view"], 1)

    def test_what_a_view_refuses(self):
        a = self.layer("a", [box(-97.76, 30.26, -97.74, 30.28)], crs="EPSG:4326")
        out = os.path.join(self.d, "s.png")
        for bad in ([-97.7, 30.2, -97.8, 30.3], [-97.8, 30.3, -97.7, 30.2], [-200, 30, -97, 31],
                    [-97.8, 30.2, 400, 30.3], [-97.8, 30.2, -97.7], ["a", 1, 2, 3], [float("nan"), 1, 2, 3]):
            with self.assertRaises(g.GISError, msg=str(bad)):
                g.map_snapshot([a], out, view=bad)
        with self.assertRaises(g.GISError):
            g.map_snapshot([a], out, bbox=[-97.8, 30.2, -97.7, 30.3], view=[-97.8, 30.2, -97.7, 30.3])
        # A layer with no CRS cannot be placed on a view.
        nocrs = os.path.join(self.d, "n.gpkg")
        gpd.GeoDataFrame({"A": [1]}, geometry=[box(0, 0, 1, 1)]).to_file(nocrs, layer="n", driver="GPKG", engine="pyogrio")
        with self.assertRaises(g.GISError):
            g.map_snapshot([nocrs], out, view=[-97.8, 30.2, -97.7, 30.3])
        # Labels follow the layer onto the view.
        r = g.map_snapshot([a], out, 800, 600, labels=["SECRET"], view=[-97.80, 30.24, -97.70, 30.30])
        self.assertEqual(r["layers"][0]["labels"], 1)


if __name__ == "__main__":
    unittest.main()
