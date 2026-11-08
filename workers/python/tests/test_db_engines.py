"""Tests for the Oracle and SQL Server engines.

The helper tests run everywhere. The database tests run only when the
database is there and its fixture is loaded (CI does both):

- Oracle: COLUMBUS_TEST_ORA_HOST (and _PORT, _SERVICE), with
  crates/gateway/tests/fixtures/oracle.sql loaded; needs oracledb.
- SQL Server: COLUMBUS_TEST_MSSQL_HOST (and _PORT, _DB), with
  crates/gateway/tests/fixtures/mssql.sql loaded; needs pyodbc and ODBC
  Driver 18 for SQL Server.

Both connect with a login that *can* write, to show writes are refused (or
never kept) anyway.
"""

import os
import struct
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import columbus_worker as w  # noqa: E402


def streamed_layer(test, conn, sql, geom, crs="EPSG:4326", rows=600):
    """query_layer on a database engine with a small batch threshold: the
    rows go to the GeoPackage in batches, and read back whole."""
    try:
        import geopandas as gpd
    except ImportError:
        test.skipTest("GIS engine not installed")
    with tempfile.TemporaryDirectory() as d:
        r = w.query_layer({"connection": conn, "sql": sql, "max_rows": 100_000, "geometry_column": geom,
                           "crs_spec": {"code": crs}, "return_rows": 3, "stream_after": 100,
                           "output_stem": os.path.join(d, "results", "s"), "hide_fields": ["owner_*"]})
        test.assertTrue(r["streamed"])
        test.assertEqual((r["rows_read"], r["summary"]["rows"]), (rows, rows))
        test.assertEqual(len(r["rows"]), 3)
        out = gpd.read_file(r["output_path"], engine="pyogrio")
        test.assertEqual(len(out), rows)
        test.assertEqual(out.crs.to_epsg(), int(crs.split(":")[1]))
        test.assertFalse(any(c.lower().startswith("owner_") for c in out.columns))
        test.assertTrue(out.geometry.notna().all())


def _wkb_line():
    # LINESTRING(1 2, 3 4), little-endian WKB
    return struct.pack("<BII", 1, 2, 2) + struct.pack("<4d", 1, 2, 3, 4)


class HelperTests(unittest.TestCase):
    def test_wkb_with_srid_becomes_ewkb(self):
        from columbus_plugin import decode_geometry

        g = decode_geometry(w._ewkb(_wkb_line(), 3081))
        self.assertEqual((g.type, g.srid, g.parts), ("LineString", 3081, [(1, 2), (3, 4)]))
        self.assertIsNone(decode_geometry(w._ewkb(_wkb_line(), None)).srid)
        self.assertIsNone(w._ewkb(None, 4326))
        # ISO Z codes become EWKB flags
        iso_z = struct.pack("<BI", 1, 1001) + struct.pack("<3d", 1, 2, 3)
        out = w._ewkb(iso_z, 4326)
        self.assertEqual(struct.unpack_from("<I", out, 1)[0], 1 | 0x80000000 | 0x20000000)
        self.assertEqual(decode_geometry(out).srid, 4326)

    def test_error_texts_keep_only_the_primary_message_without_values(self):
        class E(Exception):
            pass

        ms = E("22018", "[22018] [Microsoft][ODBC Driver 18 for SQL Server][SQL Server]Conversion failed when "
                        "converting the varchar value 'SECRET-1' to data type int. (245) (SQLExecDirectW)")
        text = w._ms_error_text(ms)
        self.assertNotIn("SECRET", text)
        self.assertIn("Conversion failed", text)
        self.assertIn("245", text)
        name = E("42S22", "[42S22] [Microsoft][ODBC Driver 18 for SQL Server][SQL Server]Invalid column name 'nope'. "
                          "(207) (SQLExecDirectW); [42000] [Microsoft][ODBC Driver 18 for SQL Server][SQL Server]"
                          "Statement(s) could not be prepared. (8180)")
        text = w._ms_error_text(name)
        self.assertIn("'nope'", text)
        self.assertNotIn("prepared", text)

        class O:
            code, full_code = 1722, "ORA-01722"
            message = ("ORA-01722: unable to convert string value containing 'SECRET-1' to a number\n"
                       "Help: https://docs.oracle.com/error-help/db/ora-01722/")

        text = w._ora_error_text(E(O()))
        self.assertNotIn("SECRET", text)
        self.assertNotIn("Help", text)
        self.assertIn("ORA-01722", text)

    def test_connection_string_values_cannot_add_keywords(self):
        self.assertEqual(w._odbc_value("db;Trusted_Connection=yes"), "{db;Trusted_Connection=yes}")
        self.assertEqual(w._odbc_value("a}b"), "{a}}b}")
        self.assertEqual(w._ms_encrypt(None), {"Encrypt": "yes", "TrustServerCertificate": "no"})
        self.assertEqual(w._ms_encrypt("disable"), {"Encrypt": "no"})
        self.assertEqual(w._ms_encrypt("require")["TrustServerCertificate"], "yes")

    def test_schema_patterns(self):
        self.assertEqual(w._like_patterns(["ROADS", "gis_*"]), ["ROADS", "GIS\\_%"])
        self.assertIsNone(w._like_patterns(["ROADS", "*"]))
        self.assertIsNone(w._like_patterns(None))


ORA_HOST = os.environ.get("COLUMBUS_TEST_ORA_HOST")


def ora_conn(user="columbus_rw", password="columbus_rw_test", **extra):
    c = {
        "kind": "oracle",
        "host": ORA_HOST,
        "port": int(os.environ.get("COLUMBUS_TEST_ORA_PORT", "1521")),
        "database": os.environ.get("COLUMBUS_TEST_ORA_SERVICE", "FREEPDB1"),
        "user": user,
        "password": password,
    }
    c.update(extra)
    return c


def sel(conn, sql, max_rows=2000, **kw):
    return w.sql_select({"connection": conn, "sql": sql, "max_rows": max_rows, **kw})


@unittest.skipUnless(ORA_HOST, "set COLUMBUS_TEST_ORA_HOST (and load crates/gateway/tests/fixtures/oracle.sql)")
class OracleTests(unittest.TestCase):
    def audit_rows(self):
        return sel(ora_conn(), "SELECT COUNT(*) FROM roads.roads_audit")["rows"][0][0]

    def test_select_with_geometry_as_wkb(self):
        r = sel(ora_conn(), "SELECT id, district, geom FROM roads.roads WHERE district = 14 ORDER BY id", return_rows=5)
        self.assertEqual(r["rows_read"], 600)
        self.assertEqual(r["columns"], ["ID", "DISTRICT", "GEOM"])
        self.assertEqual(r["rows"][0][2]["geometry"], "ewkb")
        self.assertFalse(r["hit_read_limit"])

    def test_query_layer_streams_in_batches(self):
        streamed_layer(self, ora_conn(), "SELECT id, route_code, owner_name, geom FROM roads.roads WHERE district = 14", "GEOM")

    def test_lrs_routes_keep_their_measures_end_to_end(self):
        """REQ-24 on Oracle: SDO_GTYPE 3302 routes (x, y, m) through
        query_layer (in memory and streamed), reproject, join,
        spatial_join, dissolve, a plugin extract, a personal tool and
        exports to gpkg, gdb and shp; buffer drops M with a check."""
        try:
            import test_measures as tm
        except ImportError:
            sys.path.insert(0, HERE)
            import test_measures as tm
        if not tm.HAVE_GIS:
            self.skipTest("GIS engine (with shapely 2.1+) not installed")
        with tempfile.TemporaryDirectory() as d:
            tm.measure_chain(self, ora_conn(), "SELECT id, route_id, district, geom FROM roads.routes ORDER BY id",
                             "GEOM", d, 400, key="ROUTE_ID", district="DISTRICT")

    def test_read_limit_on_server_cursor(self):
        r = sel(ora_conn(), "SELECT id FROM roads.roads", max_rows=1000)
        self.assertEqual(r["rows_read"], 1000)
        self.assertTrue(r["hit_read_limit"])

    def test_database_refuses_writes_even_for_a_login_that_can_write(self):
        before = self.audit_rows()
        for sql in ["INSERT INTO roads.roads_audit (who) VALUES ('agent')", "DELETE FROM roads.roads_audit"]:
            with self.assertRaises(w.WorkerError, msg=sql):
                sel(ora_conn(), sql)
        # A SELECT that calls a function writing in an autonomous
        # transaction: SET TRANSACTION READ ONLY alone would not stop it.
        with self.assertRaises(w.WorkerError) as cm:
            sel(ora_conn(), "SELECT roads.log_visit() FROM dual")
        print("oracle autonomous write refused with:", cm.exception.message)
        self.assertEqual(self.audit_rows(), before, "nothing was written")

    def test_timeout(self):
        with self.assertRaises(w.WorkerError) as cm:
            sel(ora_conn(), "SELECT COUNT(*) FROM all_objects a, all_objects b, all_objects c", timeout_s=1)
        self.assertEqual(cm.exception.code, w.TIMEOUT, cm.exception.message)

    def test_wrong_password_is_scrubbed_from_errors(self):
        secret = "not-the-password-xyz"
        with self.assertRaises(w.WorkerError) as cm:
            sel(ora_conn(password=secret), "SELECT 1 FROM dual")
        self.assertNotIn(secret, cm.exception.message)
        self.assertIn("cannot connect", cm.exception.message)

    def test_errors_do_not_echo_data_values(self):
        with self.assertRaises(w.WorkerError) as cm:
            sel(ora_conn(), "SELECT id FROM roads.roads WHERE TO_NUMBER(route_code) = 1")
        self.assertNotIn("SECRET", cm.exception.message)
        self.assertIn("ORA-", cm.exception.message)
        with self.assertRaises(w.WorkerError) as cm:
            sel(ora_conn(), "SELECT no_such_col FROM roads.roads")
        self.assertIn("NO_SUCH_COL", cm.exception.message)

    def test_the_barrier_keeps_filtered_rows_away_from_the_callers_conditions(self):
        # What the gateway sends for a filtered table (district = 14) and a
        # caller's condition that fails on any filtered-out row.
        barrier = ('SELECT * FROM (SELECT * FROM (SELECT "ID", "ROUTE_CODE", "DISTRICT" FROM "ROADS"."ROADS" '
                   "WHERE (district = 14) AND ROWNUM > 0) ROADS WHERE TO_NUMBER(route_code) = 1002) columbus_q "
                   "FETCH FIRST 11 ROWS ONLY")
        r = sel(ora_conn(), barrier)
        self.assertEqual(r["rows"], [[2, "1002", 14]])
        # The same without the barrier may run the condition on the other
        # rows first: shown for the record, not relied on.
        merged = ('SELECT * FROM (SELECT "ID", "ROUTE_CODE", "DISTRICT" FROM "ROADS"."ROADS" WHERE (district = 14)) ROADS '
                  "WHERE TO_NUMBER(route_code) = 1002")
        try:
            print("oracle without the barrier:", sel(ora_conn(), merged)["rows"])
        except w.WorkerError as e:
            print("oracle without the barrier failed (the barrier is needed):", e.message)
        # The plans show why. Without the barrier Oracle merges the view and
        # tests both conditions in one filter, in an order of its choosing;
        # with it, the caller's condition sits on the VIEW, above the
        # COUNT (ROWNUM) step, and the row filter on the table below.
        import oracledb

        db = oracledb.connect(user="columbus_rw", password="columbus_rw_test", host=ORA_HOST,
                              port=int(os.environ.get("COLUMBUS_TEST_ORA_PORT", "1521")),
                              service_name=os.environ.get("COLUMBUS_TEST_ORA_SERVICE", "FREEPDB1"))
        try:
            plans = {}
            for name, sql in (("barrier", barrier), ("merged", merged)):
                cur = db.cursor()
                cur.execute(f"EXPLAIN PLAN SET STATEMENT_ID = '{name}' FOR {sql}")
                cur.execute("SELECT id, parent_id, operation, options, filter_predicates FROM plan_table "
                            "WHERE statement_id = :s ORDER BY id", s=name)
                plans[name] = cur.fetchall()
                print(f"oracle plan ({name}):")
                for r in plans[name]:
                    print("  ", r)
            db.rollback()
        finally:
            db.close()

        def nodes(plan, word):
            return [r for r in plan if r[4] and word in r[4].upper()]

        merged_both = [r for r in plans["merged"] if r[4] and "TO_NUMBER" in r[4].upper() and "DISTRICT" in r[4].upper()]
        print("oracle merged plan tests both conditions in one step:", bool(merged_both))
        plan = plans["barrier"]
        by_id = {r[0]: r for r in plan}
        self.assertFalse([r for r in plan if r[4] and "TO_NUMBER" in r[4].upper() and "DISTRICT" in r[4].upper()],
                         "with the barrier no step tests both conditions")
        conv, = nodes(plan, "TO_NUMBER")
        dist, = nodes(plan, "DISTRICT")
        # The row filter's step is below the caller's condition's step.
        up, ancestors = dist[1], set()
        while up is not None:
            ancestors.add(up)
            up = by_id[up][1]
        self.assertIn(conv[0], ancestors, "the caller's condition runs only on what the filtered view returns")
        self.assertTrue(any(r[2] == "COUNT" for r in plan), "the ROWNUM step is in the plan")

    def test_list_datasets(self):
        r = w.list_datasets({"connection": ora_conn("columbus_ro", "columbus_ro_test"), "schemas": ["ROADS"]})
        by_name = {d["name"]: d for d in r["datasets"]}
        self.assertIn("ROADS.ROADS", by_name)
        self.assertFalse([n for n in by_name if "MDRT" in n], by_name.keys())
        self.assertFalse([n for n in by_name if not n.startswith("ROADS.")], by_name.keys())
        roads = by_name["ROADS.ROADS"]
        self.assertEqual((roads["geometry_column"], roads["srs_id"]), ("GEOM", 4326))
        self.assertEqual(roads["crs_spec"], {"org": "EPSG", "org_id": 4326})
        # From the spatial index's metadata when the login can see it, else
        # unknown in a listing; describing the one dataset reads it from a row.
        print("oracle listing geometry_type:", roads["geometry_type"])
        self.assertIn(roads["geometry_type"], ("LINESTRING", "GEOMETRY"))
        self.assertEqual([f["name"] for f in roads["fields"]], ["ID", "ROUTE_CODE", "DISTRICT", "OWNER_NAME", "GEOM"])
        self.assertEqual(by_name["ROADS.PARCELS"]["unsupported_geometry"][0]["column"], "SHAPE")
        one = w.list_datasets({"connection": ora_conn(), "only": "roads.roads"})["datasets"]
        self.assertEqual([d["name"] for d in one], ["ROADS.ROADS"])
        self.assertEqual(one[0]["geometry_type"], "LINESTRING")

    def test_unsupported_geometry_comes_back_as_a_placeholder(self):
        r = sel(ora_conn(), "SELECT id, shape FROM roads.parcels ORDER BY id")
        self.assertEqual(r["rows"][0], [1, "unsupported: ROADS.ST_GEOMETRY"])

    def test_extract_from_oracle(self):
        from columbus_plugin import Extract

        with tempfile.TemporaryDirectory() as d:
            out = os.path.join(d, "x.sqlite")
            r = w.extract({"connection": ora_conn(), "sql": "SELECT id, route_code, owner_name, geom FROM roads.roads WHERE district = 14",
                           "max_rows": 1000, "output_path": out, "hide_fields": ["owner_*"]})
            self.assertEqual((r["rows"], r["geometry_column"], r["srid"]), (600, "GEOM", "4326"))
            self.assertEqual(r["hidden_columns"], ["OWNER_NAME"])
            ex = Extract(out)
            g = next(ex.rows()).geometry()
            self.assertEqual((g.type, g.srid, len(g.parts)), ("LineString", 4326, 2))
            ex.close()


MS_HOST = os.environ.get("COLUMBUS_TEST_MSSQL_HOST")


def ms_conn(user="columbus_rw", password="Columbus_rw_test1", **extra):
    c = {
        "kind": "sqlserver",
        "host": MS_HOST,
        "port": int(os.environ.get("COLUMBUS_TEST_MSSQL_PORT", "1433")),
        "database": os.environ.get("COLUMBUS_TEST_MSSQL_DB", "gis"),
        "user": user,
        "password": password,
        "sslmode": "require",  # the container's certificate is self-signed
    }
    c.update(extra)
    return c


@unittest.skipUnless(MS_HOST, "set COLUMBUS_TEST_MSSQL_HOST (and load crates/gateway/tests/fixtures/mssql.sql)")
class SqlServerTests(unittest.TestCase):
    def audit_rows(self):
        return sel(ms_conn(), "SELECT COUNT(*) AS n FROM roads.roads_audit")["rows"][0][0]

    def test_select_with_geometry_as_wkb(self):
        r = sel(ms_conn(), "SELECT TOP (2000) id, district, geom FROM roads.roads WHERE district = 14 ORDER BY id", return_rows=5)
        self.assertEqual(r["rows_read"], 600)
        self.assertEqual(r["columns"], ["id", "district", "geom"])
        self.assertEqual(r["rows"][0][2]["geometry"], "ewkb")

    def test_geography_too(self):
        from columbus_plugin import Extract

        with tempfile.TemporaryDirectory() as d:
            out = os.path.join(d, "x.sqlite")
            r = w.extract({"connection": ms_conn(), "sql": "SELECT id, name, loc FROM roads.places", "max_rows": 10, "output_path": out})
            self.assertEqual((r["rows"], r["srid"]), (2, "4326"))
            ex = Extract(out)
            g = next(ex.rows()).geometry()
            self.assertEqual(g.type, "Point")
            self.assertAlmostEqual(g.parts[0][0], -97.7404)  # longitude first, as WKB has it
            ex.close()

    def test_query_layer_streams_in_batches(self):
        streamed_layer(self, ms_conn(), "SELECT id, route_code, owner_name, geom FROM roads.roads WHERE district = 14", "geom", crs="EPSG:3081")

    def test_read_limit(self):
        r = sel(ms_conn(), "SELECT id FROM roads.roads", max_rows=1000)
        self.assertEqual(r["rows_read"], 1000)
        self.assertTrue(r["hit_read_limit"])

    def test_nothing_a_statement_does_is_kept(self):
        before = self.audit_rows()
        for sql in ["INSERT INTO roads.roads_audit (who) VALUES ('agent')",
                    "DELETE FROM roads.roads_audit",
                    "INSERT INTO roads.roads_audit (who) VALUES ('agent'); SELECT 1 AS x"]:
            try:
                sel(ms_conn(), sql)
            except w.WorkerError as e:
                print("sql server write:", sql, "->", e.message)
            self.assertEqual(self.audit_rows(), before, sql)

    def test_timeout(self):
        with self.assertRaises(w.WorkerError) as cm:
            sel(ms_conn(), "SELECT COUNT_BIG(*) AS n FROM sys.all_objects a CROSS JOIN sys.all_objects b "
                           "CROSS JOIN sys.all_objects c CROSS JOIN sys.all_objects d", timeout_s=1)
        self.assertEqual(cm.exception.code, w.TIMEOUT, cm.exception.message)

    def test_wrong_password_is_scrubbed_from_errors(self):
        secret = "not-the-password-xyz"
        with self.assertRaises(w.WorkerError) as cm:
            sel(ms_conn(password=secret), "SELECT 1 AS x")
        self.assertNotIn(secret, cm.exception.message)
        self.assertIn("cannot connect", cm.exception.message)

    def test_errors_do_not_echo_data_values(self):
        with self.assertRaises(w.WorkerError) as cm:
            sel(ms_conn(), "SELECT id FROM roads.roads WHERE CAST(route_code AS INT) = 1")
        self.assertNotIn("SECRET", cm.exception.message)
        self.assertIn("245", cm.exception.message)
        with self.assertRaises(w.WorkerError) as cm:
            sel(ms_conn(), "SELECT no_such_col FROM roads.roads")
        self.assertIn("no_such_col", cm.exception.message)

    def test_the_barrier_keeps_filtered_rows_away_from_the_callers_conditions(self):
        barrier = ("SELECT TOP (11) * FROM (SELECT TOP (9223372036854775807) [id], [route_code], [district] "
                   "FROM [roads].[roads] WHERE (district = 14)) AS roads WHERE CAST(route_code AS INT) = 1002")
        r = sel(ms_conn(), barrier)
        self.assertEqual(r["rows"], [[2, "1002", 14]])
        merged = ("SELECT * FROM (SELECT [id], [route_code], [district] FROM [roads].[roads] WHERE (district = 14)) AS roads "
                  "WHERE CAST(route_code AS INT) = 1002")
        try:
            print("sql server without the barrier:", sel(ms_conn(), merged)["rows"])
        except w.WorkerError as e:
            print("sql server without the barrier failed (the barrier is needed):", e.message)
        # The plans show why: with the barrier the conversion is tested in a
        # Filter above the Top, and the row filter below it.
        import pyodbc

        c = ms_conn()
        db = pyodbc.connect(
            "DRIVER={ODBC Driver 18 for SQL Server};SERVER=tcp:%s,%s;DATABASE=%s;UID=%s;PWD=%s;Encrypt=yes;TrustServerCertificate=yes"
            % (c["host"], c["port"], c["database"], c["user"], c["password"]), autocommit=True)
        try:
            cur = db.cursor()
            plans = {}
            for name, sql in (("barrier", barrier), ("merged", merged)):
                cur.execute("SET SHOWPLAN_ALL ON")
                cur.execute(sql)
                cols = [d[0] for d in cur.description]
                rows = [dict(zip(cols, r)) for r in cur.fetchall()]
                cur.execute("SET SHOWPLAN_ALL OFF")
                plans[name] = rows
                print(f"sql server plan ({name}):")
                for r in rows:
                    print("  ", r["NodeId"], r["Parent"], r["PhysicalOp"], (r["Argument"] or "")[:200])
        finally:
            db.close()
        plan = [r for r in plans["barrier"] if r["PhysicalOp"]]
        by_id = {r["NodeId"]: r for r in plan}

        def ancestors(node):
            out, up = set(), node["Parent"]
            while up in by_id:
                out.add(up)
                up = by_id[up]["Parent"]
            return out

        args = {r["NodeId"]: r["Argument"] or "" for r in plan}
        # The barrier's TOP (the outer one is the server's row limit).
        top, = [r for r in plan if r["PhysicalOp"] == "Top" and "9223372036854775807" in args[r["NodeId"]]]
        # The row filter is tested where the rows are read, below the TOP.
        dist = [r for r in plan if "[district]" in args[r["NodeId"]] and "WHERE" in args[r["NodeId"]]]
        self.assertTrue(dist, plan)
        for r in dist:
            self.assertIn(top["NodeId"], ancestors(r), "the row filter runs below the TOP")
        # No step tests both (the merged plan does, in one scan predicate).
        self.assertFalse([r for r in plan if "[district]" in args[r["NodeId"]] and "route_code" in args[r["NodeId"]]])
        # The caller's comparison is a Filter above the TOP; the conversion it
        # compares is computed above the row filter's scan (SQL Server may
        # define it below the TOP, but only rows the scan let through reach it).
        compare = [r for r in plan if r["PhysicalOp"] == "Filter"]
        self.assertTrue(compare, plan)
        for r in compare:
            self.assertIn(r["NodeId"], ancestors(top), "the caller's condition runs above the TOP")
        conv = [r for r in plan if "CONVERT" in args[r["NodeId"]].upper() and "route_code" in args[r["NodeId"]]]
        for r in conv:
            for d in dist:
                self.assertIn(r["NodeId"], ancestors(d), "the conversion only sees rows the row filter kept")

    def test_list_datasets(self):
        r = w.list_datasets({"connection": ms_conn("columbus_ro", "Columbus_ro_test1"), "schemas": ["roads"]})
        by_name = {d["name"]: d for d in r["datasets"]}
        self.assertIn("roads.roads", by_name)
        roads = by_name["roads.roads"]
        self.assertEqual((roads["geometry_column"], roads["srs_id"], roads["geometry_type"]), ("geom", 3081, "GEOMETRY"))
        self.assertEqual([f["name"] for f in roads["fields"]], ["id", "route_code", "district", "owner_name", "geom"])
        self.assertEqual(by_name["roads.places"]["srs_id"], 4326)
        one = w.list_datasets({"connection": ms_conn(), "only": "roads.places"})["datasets"]
        self.assertEqual([d["name"] for d in one], ["roads.places"])


if __name__ == "__main__":
    unittest.main()
