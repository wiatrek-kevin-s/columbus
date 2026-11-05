"""What notes are drafted from: keys, relationships, indexes, comments and
codes, read from each engine's catalog (columbus_catalog).

The GeoPackage tests run everywhere. The database tests run when the
database and its fixture are there (CI): COLUMBUS_TEST_PG_HOST,
COLUMBUS_TEST_ORA_HOST, COLUMBUS_TEST_MSSQL_HOST, as in test_worker.py and
test_db_engines.py.
"""

import os
import sqlite3
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

import columbus_catalog as c  # noqa: E402


class GeoPackageTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.dir.name, "t.gpkg")
        db = sqlite3.connect(self.path)
        db.execute("CREATE TABLE districts (id INTEGER PRIMARY KEY, name TEXT)")
        db.execute("CREATE TABLE segs (fid INTEGER PRIMARY KEY, district INTEGER REFERENCES districts(id), fc INTEGER)")
        db.execute("CREATE INDEX segs_d ON segs (district)")
        db.execute("CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT)")
        db.execute("INSERT INTO gpkg_contents VALUES ('segs', 'attributes', 'segs', 'Road segments')")
        db.execute("CREATE TABLE gpkg_data_columns (table_name TEXT, column_name TEXT, name TEXT, title TEXT, description TEXT, mime_type TEXT, constraint_name TEXT)")
        db.execute("CREATE TABLE gpkg_data_column_constraints (constraint_name TEXT, constraint_type TEXT, value TEXT, min NUMERIC, min_is_inclusive BOOLEAN, max NUMERIC, max_is_inclusive BOOLEAN, description TEXT)")
        db.execute("INSERT INTO gpkg_data_columns VALUES ('segs', 'fc', 'fc', 'Functional class', NULL, NULL, 'fc_codes')")
        db.executemany("INSERT INTO gpkg_data_column_constraints VALUES ('fc_codes', 'enum', ?, NULL, NULL, NULL, NULL, ?)",
                       [("1", "Interstate"), ("2", "Arterial")])
        db.executemany("INSERT INTO segs VALUES (?, ?, ?)", [(1, None, 1), (2, None, 2), (3, None, 2)])
        db.commit()
        db.close()

    def tearDown(self):
        self.dir.cleanup()

    def test_keys_relationships_comments_and_codes(self):
        sql = 'SELECT "fc" AS v, COUNT(*) AS n FROM "segs" GROUP BY "fc" ORDER BY COUNT(*) DESC'
        r = c.catalog_details({"connection": {"kind": "gpkg", "path": self.path}, "dataset": "segs",
                               "value_sqls": [{"field": "fc", "sql": sql}]})
        self.assertEqual(r["primary_key"], ["fid"])
        self.assertEqual(r["foreign_keys"][0]["columns"], ["district"])
        self.assertEqual(r["foreign_keys"][0]["references"], {"dataset": "districts", "columns": ["id"]})
        self.assertEqual(r["indexes"][0]["columns"], ["district"])
        self.assertEqual(r["comments"]["table"], "Road segments")
        self.assertEqual(r["comments"]["columns"]["fc"], "Functional class")
        self.assertEqual(r["domains"]["fc"]["values"], {"1": "Interstate", "2": "Arterial"})
        self.assertEqual(r["values"]["fc"], [[2, 2], [1, 1]])

    def test_nothing_is_written(self):
        before = os.path.getmtime(self.path), os.path.getsize(self.path)
        c.catalog_details({"connection": {"kind": "gpkg", "path": self.path}, "dataset": "segs"})
        self.assertEqual(before, (os.path.getmtime(self.path), os.path.getsize(self.path)))


def _offices_points_at_districts(test, r, dataset_suffix):
    fk = r["foreign_keys"][0]
    test.assertEqual([x.lower() for x in fk["columns"]], ["district_id"])
    test.assertTrue(fk["references"]["dataset"].lower().endswith(dataset_suffix), fk)
    test.assertEqual([x.lower() for x in fk["references"]["columns"]], ["id"])
    test.assertEqual([x.lower() for x in r["primary_key"]], ["id"])


PG_HOST = os.environ.get("COLUMBUS_TEST_PG_HOST")


@unittest.skipUnless(PG_HOST, "set COLUMBUS_TEST_PG_HOST (and load crates/gateway/tests/fixtures/postgis.sql)")
class PostgisTests(unittest.TestCase):
    def conn(self):
        from test_worker import pg_conn

        return pg_conn(user="columbus_rw", password="columbus_rw_test")

    def test_relationships_and_comments(self):
        r = c.catalog_details({"connection": self.conn(), "dataset": "roads.district_offices"})
        _offices_points_at_districts(self, r, "roads.districts")
        r = c.catalog_details({"connection": self.conn(), "dataset": "roads.roadway_inventory"})
        self.assertEqual(r["comments"]["table"], "Roadway segments on the county system")
        self.assertEqual(r["comments"]["columns"]["route_id"], "Route identifier")


ORA_HOST = os.environ.get("COLUMBUS_TEST_ORA_HOST")


@unittest.skipUnless(ORA_HOST, "set COLUMBUS_TEST_ORA_HOST (and load crates/gateway/tests/fixtures/oracle.sql)")
class OracleTests(unittest.TestCase):
    def conn(self):
        from test_db_engines import ora_conn

        return ora_conn()

    def test_relationships_and_comments(self):
        r = c.catalog_details({"connection": self.conn(), "dataset": "ROADS.DISTRICT_OFFICES"})
        _offices_points_at_districts(self, r, "roads.districts")
        r = c.catalog_details({"connection": self.conn(), "dataset": "ROADS.ROADS"})
        self.assertEqual(r["comments"]["table"], "Road segments on the county system")
        self.assertEqual(r["comments"]["columns"]["DISTRICT"], "Maintenance district")


MS_HOST = os.environ.get("COLUMBUS_TEST_MSSQL_HOST")


@unittest.skipUnless(MS_HOST, "set COLUMBUS_TEST_MSSQL_HOST (and load crates/gateway/tests/fixtures/mssql.sql)")
class SqlServerTests(unittest.TestCase):
    def conn(self):
        from test_db_engines import ms_conn

        return ms_conn()

    def test_relationships_and_comments(self):
        r = c.catalog_details({"connection": self.conn(), "dataset": "roads.district_offices"})
        _offices_points_at_districts(self, r, "roads.districts")
        r = c.catalog_details({"connection": self.conn(), "dataset": "roads.roads"})
        self.assertEqual(r["comments"]["table"], "Road segments on the county system")
        self.assertEqual(r["comments"]["columns"]["district"], "Maintenance district")


if __name__ == "__main__":
    unittest.main()
