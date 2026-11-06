"""Project databases (columbus_projectdb): loads stream layers into DuckDB,
statements are limited to DDL and DML on the file itself, and nothing
reaches another file, URL, extension or setting.

Skipped when DuckDB or the GIS engine is not installed.
"""

import csv
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

try:
    import duckdb  # noqa: F401
    import geopandas as gpd
    import shapely
    from shapely.geometry import Point

    import columbus_gis as g
    import columbus_projectdb as pdb

    HAVE = True
except ImportError:  # pragma: no cover
    HAVE = False

import columbus_worker as w  # noqa: E402


@unittest.skipUnless(HAVE, "DuckDB or the GIS engine not installed")
class ProjectDbTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = self.tmp.name
        self.db = os.path.join(root, "projectdb", "p", "p.duckdb")
        self.ws = os.path.join(root, "ws")
        os.makedirs(self.ws)
        # Parcels with geometry, and an attribute table of owners by parcel.
        self.parcels = os.path.join(self.ws, "parcels.gpkg")
        gpd.GeoDataFrame(
            {"parcel_id": [f"{i:05d}" for i in range(500)], "acres": [i / 10 for i in range(500)], "ssn": ["x"] * 500},
            geometry=[Point(3_100_000 + i, 10_070_000) for i in range(500)], crs="EPSG:2277",
        ).to_file(self.parcels, layer="parcels", driver="GPKG", engine="pyogrio")
        self.owners = os.path.join(self.ws, "owners.csv")
        with open(self.owners, "w", newline="", encoding="utf-8") as f:
            wr = csv.writer(f)
            wr.writerow(["parcel_id", "owner", "value"])
            for i in range(0, 500, 2):
                wr.writerow([f"{i:05d}", f"Owner {i}", i * 1000])
        # Files outside the database the SQL must never reach.
        self.outside = os.path.join(root, "outside.csv")
        with open(self.outside, "w", encoding="utf-8") as f:
            f.write("secret\ns3cr3t\n")
        pdb.create({"path": self.db})

    def tearDown(self):
        self.tmp.cleanup()

    def load(self, **kw):
        p = {"path": self.db, "source": self.parcels, "table": "parcels"}
        p.update(kw)
        return pdb.load(p)

    def test_load_join_and_query_as_a_layer(self):
        r = self.load(primary_key=["parcel_id"], hide_fields=["ssn"])
        self.assertEqual((r["rows"], r["crs"], r["geometry_column"]), (500, "EPSG:2277", "geom"))
        self.assertEqual(r["hidden_columns"], ["ssn"])
        self.assertNotIn("ssn", [c["name"] for c in r["columns"]])
        o = pdb.load({"path": self.db, "source": self.owners, "table": "owners", "primary_key": ["parcel_id"]})
        types = {c["name"]: c["type"] for c in o["columns"]}
        # Codes keep their padding; numbers become numbers.
        self.assertEqual(types, {"parcel_id": "VARCHAR", "owner": "VARCHAR", "value": "BIGINT"})
        e = pdb.execute({"path": self.db, "sql": """
            CREATE INDEX owners_parcel ON owners (parcel_id);
            ALTER TABLE owners ADD COLUMN band VARCHAR;
            UPDATE owners SET band = CASE WHEN value > 100000 THEN 'high' ELSE 'low' END;
            CREATE VIEW parcel_owners AS SELECT p.parcel_id, p.acres, o.owner, o.band, p.geom FROM parcels p JOIN owners o USING (parcel_id);
            COMMENT ON TABLE owners IS 'Owners by parcel, from the assessor';
        """})
        self.assertEqual([s["type"] for s in e["statements"]], ["CREATE", "ALTER", "UPDATE", "CREATE", "ALTER"])
        self.assertEqual(e["statements"][2]["rows"], 250)
        q = pdb.query({"path": self.db, "sql": "SELECT * FROM parcel_owners WHERE band = 'high' ORDER BY parcel_id",
                       "max_rows": 1000, "return_rows": 2, "crs": "EPSG:2277",
                       "output_stem": os.path.join(self.ws, "results", "q1")})
        self.assertEqual(q["rows_read"], 199)
        self.assertFalse(q["hit_read_limit"])
        self.assertEqual(q["rows"][0][0], "00102")
        out = gpd.read_file(q["output_path"], engine="pyogrio")
        self.assertEqual(len(out), 199)
        self.assertEqual(out.crs.to_epsg(), 2277)
        d = pdb.describe({"path": self.db})
        names = {t["name"]: t for t in d["tables"]}
        self.assertEqual(names["owners"]["comment"], "Owners by parcel, from the assessor")
        self.assertEqual(names["parcels"]["rows"], 500)
        self.assertEqual(names["parcel_owners"]["kind"], "view")
        self.assertIn("PRIMARY KEY", [c["type"] for c in d["constraints"]])
        self.assertEqual([i["name"] for i in d["indexes"]], ["owners_parcel"])

    def test_query_limit_is_reported(self):
        self.load()
        q = pdb.query({"path": self.db, "sql": "SELECT parcel_id, acres FROM parcels", "max_rows": 100,
                       "output_stem": os.path.join(self.ws, "results", "q2")})
        self.assertEqual((q["rows_read"], q["hit_read_limit"]), (100, True))
        self.assertTrue(q["output_path"].endswith(".csv"))

    def test_geometry_needs_a_crs(self):
        self.load()
        with self.assertRaises(pdb.ProjectDbError):
            pdb.query({"path": self.db, "sql": "SELECT * FROM parcels", "max_rows": 10,
                       "output_stem": os.path.join(self.ws, "results", "q3")})

    def test_a_failed_batch_keeps_nothing(self):
        self.load(primary_key=["parcel_id"])
        with self.assertRaises(pdb.ProjectDbError) as cm:
            pdb.execute({"path": self.db, "sql": "CREATE TABLE a (x INTEGER); INSERT INTO parcels (parcel_id) VALUES ('00001')"})
        self.assertIn("statement 2", str(cm.exception))
        tables = [t["name"] for t in pdb.describe({"path": self.db})["tables"]]
        self.assertNotIn("a", tables)
        # A duplicate key refuses the whole load.
        with self.assertRaises(pdb.ProjectDbError):
            self.load(table="dup", primary_key=["acres"], source=self._dups())
        self.assertNotIn("dup", [t["name"] for t in pdb.describe({"path": self.db})["tables"]])
        with self.assertRaises(pdb.ProjectDbError):
            self.load()  # mode create: the table exists

    def _dups(self):
        p = os.path.join(self.ws, "dups.gpkg")
        gpd.GeoDataFrame({"acres": [1.0, 1.0]}, geometry=[Point(0, 0), Point(1, 1)], crs="EPSG:2277").to_file(p, driver="GPKG", engine="pyogrio")
        return p

    def test_nothing_outside_the_file_is_reachable(self):
        self.load()
        o = self.outside.replace("'", "''")
        other = os.path.join(self.tmp.name, "other.duckdb").replace("'", "''")
        refused_execute = [
            f"ATTACH '{other}' AS x",
            "DETACH p",
            f"COPY parcels TO '{o}.out'",
            f"COPY parcels FROM '{o}'",
            f"EXPORT DATABASE '{self.tmp.name}/exp'",
            f"IMPORT DATABASE '{self.tmp.name}'",
            "INSTALL httpfs",
            "LOAD httpfs",
            "SET enable_external_access = true",
            "SET lock_configuration = false",
            "RESET memory_limit",
            "PRAGMA version",
            "CALL pragma_version()",
            "CHECKPOINT",
            "USE p",
            "BEGIN TRANSACTION",
            f"CREATE TABLE leak AS SELECT * FROM read_csv('{o}')",
            f"CREATE TABLE leak AS SELECT * FROM '{o}'",
            f"CREATE TABLE leak AS SELECT * FROM \"read_text\"('{o}')",
            f"INSERT INTO parcels (parcel_id) SELECT content FROM read_text('{o}')",
            f"CREATE TABLE leak AS SELECT * FROM ST_Read('{o}')",
            f"CREATE TABLE leak AS SELECT * FROM glob('{self.tmp.name}/*')",
            "CREATE TABLE leak AS SELECT * FROM query('SELECT 1')",
            "CREATE SECRET s (TYPE s3, KEY_ID 'k', SECRET 's')",
            "CREATE PERSISTENT SECRET s (TYPE s3)",
            "SELECT 1",
            "PREPARE x AS SELECT 1",
        ]
        for sql in refused_execute:
            with self.assertRaises(pdb.ProjectDbError, msg=sql):
                pdb.execute({"path": self.db, "sql": sql})
        for sql in [f"SELECT * FROM read_csv('{o}')", f"SELECT * FROM '{o}'", "SELECT 1; SELECT 2",
                    "DROP TABLE parcels", f"SELECT * FROM ST_Read('{o}')", "SELECT current_setting('temp_directory')"]:
            with self.assertRaises(pdb.ProjectDbError, msg=sql):
                pdb.query({"path": self.db, "sql": sql, "max_rows": 10, "output_stem": os.path.join(self.ws, "results", "x")})
        # Even past the name checks, the engine refuses files: a connection
        # as the module opens it cannot read one.
        con, _ = pdb._open(self.db, 256)
        try:
            for sql in [f"SELECT * FROM read_csv('{o}')", f"ATTACH '{other}' AS y", "SET enable_external_access = true"]:
                with self.assertRaises(duckdb.Error, msg=sql):
                    con.execute(sql).fetchall()
        finally:
            con.close()
        self.assertFalse(os.path.exists(os.path.join(self.tmp.name, "other.duckdb")))
        self.assertFalse(os.path.exists(self.outside + ".out"))
        self.assertEqual(sorted(t["name"] for t in pdb.describe({"path": self.db})["tables"]), ["parcels"])

    def test_names_in_literals_are_not_refused(self):
        self.load()
        pdb.execute({"path": self.db, "sql": "CREATE TABLE notes (t VARCHAR); INSERT INTO notes VALUES ('use read_csv() elsewhere')"})
        q = pdb.query({"path": self.db, "sql": "SELECT t FROM notes", "max_rows": 10, "output_stem": os.path.join(self.ws, "results", "n")})
        self.assertEqual(q["rows"], [])  # return_rows defaults to 0
        self.assertEqual(q["rows_read"], 1)

    def test_spatial_functions_when_installed(self):
        self.load()
        if pdb.spatial_extension() is None:
            self.skipTest("the spatial extension is not installed")
        q = pdb.query({"path": self.db, "sql": "SELECT count(*) AS n FROM parcels WHERE ST_Intersects(geom, ST_MakeEnvelope(3100000, 10069999, 3100009, 10070001))",
                       "max_rows": 10, "return_rows": 5, "output_stem": os.path.join(self.ws, "results", "s")})
        self.assertEqual(q["rows"], [[10]])
        self.assertTrue(q["spatial"])

    def test_append_and_replace(self):
        self.load()
        self.load(mode="append")
        self.assertEqual({t["name"]: t["rows"] for t in pdb.describe({"path": self.db})["tables"]}["parcels"], 1000)
        self.load(mode="replace")
        self.assertEqual({t["name"]: t["rows"] for t in pdb.describe({"path": self.db})["tables"]}["parcels"], 500)
        with self.assertRaises(pdb.ProjectDbError):
            pdb.load({"path": self.db, "source": self.owners, "table": "parcels", "mode": "append"})
        for bad in ["1abc", "a b", "duckdb_x", 'x"y', ""]:
            with self.assertRaises(pdb.ProjectDbError, msg=bad):
                self.load(table=bad)

    def q(self, sql, **kw):
        p = {"path": self.db, "sql": sql, "max_rows": 100, "return_rows": 10,
             "output_stem": os.path.join(self.ws, "results", "q")}
        p.update(kw)
        return pdb.query(p)

    def tables(self):
        return sorted(t["name"] for t in pdb.describe({"path": self.db})["tables"])

    def test_query_cannot_change_the_database(self):
        self.load()
        for sql in ["EXPLAIN ANALYZE DELETE FROM parcels", "EXPLAIN ANALYZE DROP TABLE parcels",
                    "EXPLAIN ANALYZE CREATE TABLE x AS SELECT 1", "EXPLAIN ANALYZE INSERT INTO parcels (parcel_id) VALUES ('z')",
                    "EXPLAIN ANALYZE UPDATE parcels SET acres = 0", "EXPLAIN DELETE FROM parcels", "ANALYZE"]:
            with self.assertRaises(pdb.ProjectDbError, msg=sql):
                self.q(sql)
        self.assertEqual(self.tables(), ["parcels"])
        self.assertEqual(self.q("SELECT count(*) FROM parcels WHERE acres > 0")["rows"], [[499]])
        self.assertEqual(self.q("EXPLAIN ANALYZE SELECT count(*) FROM parcels")["rows_read"], 1)
        # Past every name check, the connection query and describe use is
        # read-only in DuckDB itself.
        con, _ = pdb._open(self.db, 256, read_only=True)
        try:
            for sql in ["DROP TABLE parcels", "EXPLAIN ANALYZE DELETE FROM parcels", "CREATE TABLE y (a INT)"]:
                with self.assertRaises(duckdb.Error, msg=sql):
                    con.execute(sql)
            self.assertEqual(con.execute("SELECT count(*) FROM parcels").fetchone()[0], 500)
        finally:
            con.close()

    def test_errors_do_not_carry_stored_values(self):
        self.load(primary_key=["parcel_id"])
        for sql, value in [
            ("CREATE TABLE x AS SELECT error(string_agg(parcel_id, ',')) FROM parcels", "00001"),
            ("CREATE TABLE x AS SELECT CAST(parcel_id || 'x' AS INTEGER) FROM parcels", "00000x"),
            ("INSERT INTO parcels (parcel_id, acres) SELECT parcel_id, acres FROM parcels LIMIT 1", "00000"),
            ("CREATE TABLE x AS SELECT CAST(parcel_id AS DATE) FROM parcels", "00000"),
        ]:
            with self.assertRaises(pdb.ProjectDbError, msg=sql) as cm:
                pdb.execute({"path": self.db, "sql": sql})
            self.assertNotIn(value, str(cm.exception), sql)
        # Name errors keep the name that is wrong.
        with self.assertRaises(pdb.ProjectDbError) as cm:
            pdb.execute({"path": self.db, "sql": "CREATE TABLE x AS SELECT nosuchcolumn FROM parcels"})
        self.assertIn("nosuchcolumn", str(cm.exception))
        with self.assertRaises(pdb.ProjectDbError) as cm:
            self.q("SELECT CAST(parcel_id || 'q' AS INTEGER) FROM parcels")
        self.assertNotIn("00000q", str(cm.exception))

    def test_size_cap(self):
        r = self.load()
        self.assertGreater(r["bytes"], 0)
        with self.assertRaises(pdb.ProjectDbError) as cm:
            self.load(table="again", max_mb=0.0001)
        self.assertIn("project_db.max_mb", str(cm.exception))
        with self.assertRaises(pdb.ProjectDbError):
            pdb.execute({"path": self.db, "sql": "CREATE TABLE more AS SELECT * FROM parcels", "max_mb": 0.0001})
        # Freeing space still runs, and reports the size.
        e = pdb.execute({"path": self.db, "sql": "DELETE FROM parcels WHERE acres > 10; DROP TABLE parcels", "max_mb": 0.0001})
        self.assertEqual([s["type"] for s in e["statements"]], ["DELETE", "DROP"])
        self.assertGreater(e["bytes"], 0)

    def test_name_checks_cannot_be_hidden(self):
        self.load()
        for sql in ["SELECT 1 AS \"x'\", current_setting('temp_directory') AS \"'\"",
                    "SELECT enable_logging()", "SELECT * FROM duckdb_logs()", "SELECT truncate_duckdb_logs()",
                    "SELECT \"read_text\"('x')", "SELECT 1 AS \"a\"\"'\", \"read_text\"('x')"]:
            with self.assertRaises(pdb.ProjectDbError, msg=sql):
                self.q(sql)
        for sql in ["CREATE /*x*/ SECRET s (TYPE s3)", "CREATE OR REPLACE /* */ PERSISTENT SECRET s (TYPE s3)",
                    "CALL enable_logging()"]:
            with self.assertRaises(pdb.ProjectDbError, msg=sql):
                pdb.execute({"path": self.db, "sql": sql})

    def test_query_usability(self):
        self.load()
        # A statement ending in a comment.
        self.assertEqual(self.q("SELECT count(*) AS n FROM parcels -- every parcel")["rows"], [[500]])
        pdb.execute({"path": self.db, "sql": "CREATE TABLE sales AS SELECT * FROM (VALUES ('a', 'x', 1), ('b', 'y', 2), ('a', 'y', 3)) v(k, c, n)"})
        r = self.q("PIVOT sales ON c USING sum(n) ORDER BY k")
        self.assertEqual(r["rows"], [["a", 1, 3], ["b", None, 2]])
        r = self.q("PIVOT sales ON c IN ('x', 'y') USING sum(n) ORDER BY k")
        self.assertEqual(r["rows_read"], 2)
        with self.assertRaises(pdb.ProjectDbError):
            pdb.execute({"path": self.db, "sql": "PIVOT sales ON c USING sum(n)"})
        e = pdb.execute({"path": self.db, "sql": "ANALYZE sales"})
        self.assertEqual(e["statements"][0]["type"], "ANALYZE")

    def test_through_the_worker(self):
        r = w._projectdb("describe")({"path": self.db})
        self.assertEqual(r["tables"], [])
        with self.assertRaises(w.WorkerError):
            w._projectdb("execute")({"path": self.db, "sql": "ATTACH 'x.duckdb'"})


if __name__ == "__main__":
    unittest.main()
