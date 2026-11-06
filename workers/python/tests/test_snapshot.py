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
