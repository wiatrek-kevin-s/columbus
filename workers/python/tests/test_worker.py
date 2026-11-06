"""Tests for the Python engine worker. Standard library only.

Run from the repository root: ``python -m unittest discover -s workers/python/tests``
"""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))

import columbus_worker as w  # noqa: E402


def make_db(path: str, n: int = 10) -> None:
    db = sqlite3.connect(path)
    db.execute("CREATE TABLE roads (fid INTEGER PRIMARY KEY, route_id TEXT, geom BLOB)")
    db.executemany(
        "INSERT INTO roads (route_id, geom) VALUES (?, ?)",
        [(f"R{i}", b"GP\x00\x01" + b"\x00" * 8) for i in range(n)],
    )
    db.commit()
    db.close()


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.tmp.name, "roads.gpkg")
        make_db(self.path)
        self.conn = {"kind": "gpkg", "path": self.path}

    def tearDown(self):
        self.tmp.cleanup()

    def test_ping_reports_sqlite(self):
        r = w.ping({})
        self.assertEqual(r["protocol"], w.PROTOCOL_VERSION)
        self.assertTrue(r["engines"]["sqlite"])

    def test_select_returns_rows_and_marks_geometry(self):
        r = w.sql_select({"connection": self.conn, "sql": "SELECT route_id, geom FROM roads", "max_rows": 100})
        self.assertEqual(r["rows_read"], 10)
        self.assertEqual(r["rows"][0][1], {"geometry": "gpkg", "bytes": 12})
        self.assertFalse(r["hit_read_limit"])

    def test_read_limit_is_enforced_and_reported(self):
        r = w.sql_select({"connection": self.conn, "sql": "SELECT * FROM roads", "max_rows": 4})
        self.assertEqual(r["rows_read"], 4)
        self.assertTrue(r["hit_read_limit"])

    def test_large_results_go_to_workspace_file(self):
        out = os.path.join(self.tmp.name, "ws", "results", "p_1.csv")
        r = w.sql_select(
            {"connection": self.conn, "sql": "SELECT route_id FROM roads", "max_rows": 100, "return_rows": 3, "output_path": out}
        )
        self.assertEqual(len(r["rows"]), 3)
        self.assertEqual(r["output_path"], out)
        with open(out, encoding="utf-8") as f:
            self.assertEqual(len(f.read().strip().splitlines()), 11)

    def test_hidden_fields_never_reach_the_workspace_file(self):
        out = os.path.join(self.tmp.name, "ws", "results", "p_h.csv")
        r = w.sql_select(
            {"connection": self.conn, "sql": "SELECT fid, route_id FROM roads", "max_rows": 100,
             "return_rows": 2, "output_path": out, "hide_fields": ["ROUTE_*"]}
        )
        self.assertEqual(r["columns"], ["fid"])
        self.assertEqual(r["hidden_columns"], ["route_id"])
        with open(out, encoding="utf-8") as f:
            text = f.read()
        self.assertNotIn("route_id", text)
        self.assertNotIn("R1", text)

    def test_connection_is_read_only(self):
        for sql in ["DELETE FROM roads", "INSERT INTO roads (route_id) VALUES ('x')", "CREATE TABLE t (a)"]:
            with self.assertRaises(w.WorkerError, msg=sql):
                w.sql_select({"connection": self.conn, "sql": sql, "max_rows": 10})
        db = sqlite3.connect(self.path)
        try:
            self.assertEqual(db.execute("SELECT count(*) FROM roads").fetchone()[0], 10)
        finally:
            db.close()  # Windows cannot delete a file with an open handle

    def test_extensions_cannot_be_loaded(self):
        with self.assertRaises(w.WorkerError):
            w.sql_select({"connection": self.conn, "sql": "SELECT load_extension('x')", "max_rows": 1})

    def test_unsupported_connection_type(self):
        with self.assertRaises(w.WorkerError) as cm:
            w.sql_select({"connection": {"kind": "duckdb"}, "sql": "SELECT 1", "max_rows": 1})
        self.assertEqual(cm.exception.code, w.UNSUPPORTED)

    def test_list_datasets(self):
        r = w.list_datasets({"connection": self.conn})
        names = [d["name"] for d in r["datasets"]]
        self.assertEqual(names, ["roads"])
        self.assertEqual([f["name"] for f in r["datasets"][0]["fields"]], ["fid", "route_id", "geom"])

    def test_protocol_v2_loop(self):
        script = os.path.join(os.path.dirname(HERE), "columbus_worker.py")
        lines = [
            json.dumps({"jsonrpc": "2.0", "id": 1, "method": "describe"}),
            "not json",
            json.dumps({"jsonrpc": "2.0", "id": 2, "method": "nope"}),
            json.dumps({"jsonrpc": "2.0", "id": 3, "method": "run", "params": {
                "run": "r1", "tool": "sql_select",
                "params": {"connection": self.conn, "sql": "SELECT 1", "max_rows": 1}}}),
        ]
        p = subprocess.run([sys.executable, "-I", "-u", script], input="\n".join(lines) + "\n",
                           capture_output=True, text=True, timeout=30)
        replies = {r.get("id"): r for r in map(json.loads, p.stdout.splitlines())}
        self.assertEqual(replies[1]["result"]["protocol"], 2)
        self.assertEqual(replies[1]["result"]["plugin"]["id"], "columbus.core")
        self.assertIn("extract", [t["name"] for t in replies[1]["result"]["tools"]])
        self.assertEqual(replies[None]["error"]["code"], -32700)
        self.assertEqual(replies[2]["error"]["code"], -32601)
        self.assertEqual(replies[3]["result"]["summary"]["rows"], [[1]])

    def test_extract_keeps_geometry_and_drops_hidden_fields(self):
        sys.path.insert(0, os.path.dirname(HERE))
        from columbus_plugin import Extract

        out = os.path.join(self.tmp.name, "ws", "inputs", "r1-roads.sqlite")
        r = w.extract({"connection": self.conn, "sql": "SELECT fid, route_id, geom FROM roads", "max_rows": 100,
                       "output_path": out, "hide_fields": ["route_*"], "connection_id": "roads", "dataset": "roads",
                       "labels": ["connection:roads"]})
        self.assertEqual((r["rows"], r["hit_read_limit"], r["hidden_columns"]), (10, False, ["route_id"]))
        ex = Extract(out)
        self.assertEqual(ex.columns, ["fid", "geom"])
        self.assertEqual((ex.row_count, ex.geometry_column, ex.geometry_format), (10, "geom", "gpkg"))
        self.assertEqual(ex.labels, ["connection:roads"])
        first = next(ex.rows())
        self.assertEqual(bytes(first["geom"])[:2], b"GP", "raw geometry kept")
        ex.close()
        r = w.extract({"connection": self.conn, "sql": "SELECT fid FROM roads", "max_rows": 4, "output_path": out})
        self.assertTrue(r["hit_read_limit"])


PG_HOST = os.environ.get("COLUMBUS_TEST_PG_HOST")


def pg_conn(user="columbus_ro", password="columbus_ro_test", **extra):
    c = {
        "kind": "postgis",
        "host": PG_HOST,
        "port": int(os.environ.get("COLUMBUS_TEST_PG_PORT", "5432")),
        "database": os.environ.get("COLUMBUS_TEST_PG_DB", "gis"),
        "user": user,
        "password": password,
        "sslmode": "disable",
    }
    c.update(extra)
    return c


@unittest.skipUnless(PG_HOST, "set COLUMBUS_TEST_PG_HOST (and load crates/gateway/tests/fixtures/postgis.sql)")
class PostgresTests(unittest.TestCase):
    def test_select_with_geometry_summary(self):
        r = w.sql_select(
            {
                "connection": pg_conn(),
                "sql": "SELECT route_id, district_nbr, geom FROM roads.roadway_inventory WHERE district_nbr = 14 ORDER BY fid",
                "max_rows": 1000,
                "return_rows": 5,
            }
        )
        self.assertEqual(r["rows_read"], 600)
        self.assertEqual(len(r["rows"]), 5)
        self.assertEqual(r["columns"], ["route_id", "district_nbr", "geom"])
        self.assertEqual(r["rows"][0][2]["geometry"], "ewkb")
        self.assertFalse(r["hit_read_limit"])

    def test_operators_no_extension_installed_are_refused(self):
        with self.assertRaises(w.WorkerError) as cm:
            w.sql_select({"connection": pg_conn(search_path="lookalike,public"),
                          "sql": "SELECT 1 FROM roads.roadway_inventory", "max_rows": 1})
        self.assertIn("lookalike.=", cm.exception.message)

    def test_query_layer_streams_in_batches(self):
        from test_db_engines import streamed_layer

        streamed_layer(self, pg_conn(), "SELECT fid, route_id, owner_name, geom FROM roads.roadway_inventory WHERE district_nbr = 14", "geom", crs="EPSG:3081")

    def test_read_limit_on_server_cursor(self):
        r = w.sql_select({"connection": pg_conn(), "sql": "SELECT fid FROM roads.roadway_inventory", "max_rows": 2500})
        self.assertEqual(r["rows_read"], 1200)
        r = w.sql_select({"connection": pg_conn(), "sql": "SELECT fid FROM roads.roadway_inventory", "max_rows": 1000})
        self.assertEqual(r["rows_read"], 1000)
        self.assertTrue(r["hit_read_limit"])

    def test_database_refuses_writes_even_for_a_login_that_can_write(self):
        rw = pg_conn("columbus_rw", "columbus_rw_test")
        # Plain write statements cannot even be declared as a cursor.
        for sql in ["INSERT INTO roads.roadway_audit (who) VALUES ('agent')", "DELETE FROM roads.roadway_audit"]:
            with self.assertRaises(w.WorkerError, msg=sql):
                w.sql_select({"connection": rw, "sql": sql, "max_rows": 10})
        # A SELECT with side effects gets through the parser; the read-only
        # transaction is what stops it.
        for sql in ["SELECT roads.log_visit()", "SELECT nextval('roads.roadway_audit_id_seq')"]:
            with self.assertRaises(w.WorkerError, msg=sql) as cm:
                w.sql_select({"connection": rw, "sql": sql, "max_rows": 10})
            self.assertIn("read-only", cm.exception.message, sql)
        r = w.sql_select({"connection": rw, "sql": "SELECT count(*) FROM roads.roadway_audit", "max_rows": 1})
        self.assertEqual(r["rows"], [[1]])

    def test_statement_timeout(self):
        with self.assertRaises(w.WorkerError) as cm:
            w.sql_select({"connection": pg_conn(), "sql": "SELECT pg_sleep(5)", "max_rows": 1, "timeout_s": 0.5})
        self.assertEqual(cm.exception.code, w.TIMEOUT)

    def test_wrong_password_is_scrubbed_from_errors(self):
        secret = "not-the-password-xyz"
        with self.assertRaises(w.WorkerError) as cm:
            w.sql_select({"connection": pg_conn(password=secret), "sql": "SELECT 1", "max_rows": 1})
        self.assertNotIn(secret, cm.exception.message)
        self.assertIn("cannot connect", cm.exception.message)

    def test_errors_do_not_echo_data_values(self):
        for sql in (
            "SELECT fid FROM roads.roadway_inventory WHERE CAST(owner_name AS INTEGER) = 1",
            "SELECT fid FROM roads.roadway_inventory WHERE TO_DATE(owner_name, 'YYYY') IS NULL",
        ):
            with self.assertRaises(w.WorkerError) as cm:
                w.sql_select({"connection": pg_conn(), "sql": sql, "max_rows": 10})
            self.assertNotIn("Owner", cm.exception.message, sql)
            self.assertIn("SQLSTATE", cm.exception.message)
        # Errors about names in the query keep the name: they help and leak nothing.
        with self.assertRaises(w.WorkerError) as cm:
            w.sql_select({"connection": pg_conn(), "sql": "SELECT no_such_col FROM roads.roadway_inventory", "max_rows": 1})
        self.assertIn("no_such_col", cm.exception.message)

    def test_list_datasets_reflects_database_grants(self):
        r = w.list_datasets({"connection": pg_conn()})
        by_name = {d["name"]: d for d in r["datasets"]}
        self.assertIn("roads.roadway_inventory", by_name)
        self.assertNotIn("hr.employees", by_name, "columbus_ro has no grant on hr")
        inv = by_name["roads.roadway_inventory"]
        self.assertEqual(inv["srs_id"], 3081)
        self.assertEqual(inv["geometry_type"], "LINESTRING")
        self.assertEqual(inv["estimated_rows"], 1200)
        self.assertEqual(len(inv["extent"]), 4)
        self.assertIn("owner_name", [f["name"] for f in inv["fields"]])

    def test_extract_from_postgis(self):
        sys.path.insert(0, os.path.dirname(HERE))
        from columbus_plugin import Extract

        with tempfile.TemporaryDirectory() as d:
            out = os.path.join(d, "x.sqlite")
            r = w.extract({"connection": pg_conn(), "sql": "SELECT fid, route_id, owner_name, geom FROM roads.roadway_inventory WHERE district_nbr = 14",
                           "max_rows": 1000, "output_path": out, "hide_fields": ["owner_*"]})
            self.assertEqual((r["rows"], r["geometry_column"], r["srid"]), (600, "geom", "3081"))
            ex = Extract(out)
            g = next(ex.rows()).geometry()
            self.assertEqual((g.type, g.srid), ("LineString", 3081))
            self.assertEqual(len(g.parts), 2)
            ex.close()

    def test_bad_search_path_refused(self):
        with self.assertRaises(w.WorkerError):
            w.sql_select({"connection": pg_conn(search_path="public; drop"), "sql": "SELECT 1", "max_rows": 1})


if __name__ == "__main__":
    unittest.main()
