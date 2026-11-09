"""ArcGIS query pages (Esri JSON, ``f=json``) become workspace layers with
Z and M kept (T-10, AF-1, AF-17): ``esrijson_layer`` reads pages the
gateway fetched through GDAL's ESRIJSON driver only, keeps only the fields
the gateway allows, and writes M through ``write_ogr``. Tables (no
geometry) become CSV. ``crs_describe`` gives CRS facts for a latestWkid.

Skipped when the GIS engine (geopandas, pyogrio, shapely 2.1+, pyproj) is
not installed.
"""

import json
import os
import sys
import tempfile
import unittest
import warnings

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

try:
    import geopandas as gpd
    import shapely

    import columbus_gis as g
    import columbus_worker as w

    HAVE_GIS = hasattr(shapely, "has_m")
except ImportError:  # pragma: no cover
    HAVE_GIS = False


def _page(path, features, geometry_type="esriGeometryPolyline", has_m=True, has_z=False, wkid=4326):
    doc = {
        "objectIdFieldName": "OBJECTID",
        "spatialReference": {"wkid": wkid, "latestWkid": wkid},
        "fields": [{"name": "OBJECTID", "type": "esriFieldTypeOID"},
                   {"name": "ROUTE", "type": "esriFieldTypeString", "length": 20},
                   {"name": "LANES", "type": "esriFieldTypeInteger"}],
        "features": features,
    }
    if geometry_type:
        doc.update({"geometryType": geometry_type, "hasM": has_m, "hasZ": has_z})
    with open(path, "w", encoding="utf-8") as f:
        json.dump(doc, f)
    return path


def _route(oid, m0):
    return {"attributes": {"OBJECTID": oid, "ROUTE": f"R{oid}", "LANES": 2 + oid % 3},
            "geometry": {"hasM": True, "paths": [[[-97.7, 30.2 + oid * 1e-4, m0], [-97.69, 30.21 + oid * 1e-4, m0 + 5]]]}}


@unittest.skipUnless(HAVE_GIS, "the GIS engine is not installed")
class EsriJsonLayers(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def test_pages_keep_m_and_only_allowed_fields(self):
        a = _page(os.path.join(self.dir, "a.json"), [_route(1, 1000.0), _route(2, 1010.5)])
        empty = _page(os.path.join(self.dir, "b.json"), [])
        c = _page(os.path.join(self.dir, "c.json"), [_route(3, 1020.25)])
        out = w.esrijson_layer({"pages": [a, empty, c], "out_stem": os.path.join(self.dir, "out", "routes"),
                                "layer_name": "routes", "fields": ["OBJECTID", "ROUTE", "LANES"],
                                "hide_fields": ["LANES"], "geometry": True, "return_rows": 2})
        self.assertEqual(out["hidden_columns"], ["LANES"])
        self.assertEqual(out["columns"], ["OBJECTID", "ROUTE", "geometry"])
        self.assertEqual(out["summary"]["rows"], 3)
        self.assertEqual(len(out["rows"]), 2)
        self.assertEqual(out["rows"][0][1], "R1")
        self.assertEqual(out["rows"][0][2]["geometry"], "LineString M")
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            df = gpd.read_file(out["output_path"], engine="pyogrio", use_arrow=True)
        self.assertNotIn("LANES", df.columns)
        self.assertTrue(bool(shapely.has_m(df.geometry.values).all()))
        first = shapely.get_coordinates(df.geometry.iloc[2], include_m=True)[0]
        self.assertAlmostEqual(first[-1], 1020.25)
        self.assertEqual(df.crs.to_epsg(), 4326)

    def test_z_and_m_survive_together(self):
        feat = {"attributes": {"OBJECTID": 1, "ROUTE": "R1", "LANES": 2},
                "geometry": {"hasZ": True, "hasM": True, "paths": [[[0, 0, 5, 10.5], [1, 1, 6, 12.25]]]}}
        p = _page(os.path.join(self.dir, "zm.json"), [feat], has_z=True, wkid=2277)
        out = w.esrijson_layer({"pages": [p], "out_stem": os.path.join(self.dir, "zm"), "fields": ["OBJECTID"],
                                "geometry": True, "return_rows": 0})
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            df = gpd.read_file(out["output_path"], engine="pyogrio", use_arrow=True)
        geom = df.geometry.iloc[0]
        self.assertTrue(shapely.has_z(geom) and shapely.has_m(geom))
        self.assertEqual(list(shapely.get_coordinates(geom, include_z=True, include_m=True)[1]), [1, 1, 6, 12.25])
        self.assertEqual(df.crs.to_epsg(), 2277)

    def test_a_table_becomes_csv_and_sampling_writes_nothing(self):
        p = _page(os.path.join(self.dir, "t.json"),
                  [{"attributes": {"OBJECTID": 1, "ROUTE": "R1", "LANES": 2}}], geometry_type=None)
        out = w.esrijson_layer({"pages": [p], "out_stem": os.path.join(self.dir, "t"), "fields": ["ROUTE"],
                                "geometry": False, "return_rows": 5})
        self.assertTrue(out["output_path"].endswith(".csv"))
        self.assertEqual(out["rows"], [["R1"]])
        sample = w.esrijson_layer({"pages": [p], "out_stem": None, "fields": ["ROUTE", "LANES"],
                                   "geometry": False, "return_rows": 5})
        self.assertIsNone(sample["output_path"])
        self.assertEqual(sample["rows"], [["R1", 2]])

    def test_not_esri_json_is_refused(self):
        p = os.path.join(self.dir, "x.json")
        with open(p, "w") as f:
            f.write('<html>Service unavailable</html>')
        with self.assertRaises(w.WorkerError):
            w.esrijson_layer({"pages": [p], "out_stem": None, "fields": [], "geometry": True, "return_rows": 1})

    def test_crs_describe(self):
        out = w.crs_describe({"code": "EPSG:3857", "extent": [-1.09e7, 3.5e6, -1.08e7, 3.6e6]})
        self.assertEqual(out["crs"]["code"], "EPSG:3857")
        self.assertIn("crs.info", [c["id"] for c in out["checks"]])
        self.assertIsNone(w.crs_describe({"code": None})["crs"])


if __name__ == "__main__":
    unittest.main()
