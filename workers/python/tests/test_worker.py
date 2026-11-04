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

    def test_connection_is_read_only(self):
        for sql in ["DELETE FROM roads", "INSERT INTO roads (route_id) VALUES ('x')", "CREATE TABLE t (a)"]:
            with self.assertRaises(w.WorkerError, msg=sql):
                w.sql_select({"connection": self.conn, "sql": sql, "max_rows": 10})
        db = sqlite3.connect(self.path)
        self.assertEqual(db.execute("SELECT count(*) FROM roads").fetchone()[0], 10)

    def test_extensions_cannot_be_loaded(self):
        with self.assertRaises(w.WorkerError):
            w.sql_select({"connection": self.conn, "sql": "SELECT load_extension('x')", "max_rows": 1})

    def test_unsupported_connection_type(self):
        with self.assertRaises(w.WorkerError) as cm:
            w.sql_select({"connection": {"kind": "oracle"}, "sql": "SELECT 1 FROM dual", "max_rows": 1})
        self.assertEqual(cm.exception.code, w.UNSUPPORTED)

    def test_list_datasets(self):
        r = w.list_datasets({"connection": self.conn})
        names = [d["name"] for d in r["datasets"]]
        self.assertEqual(names, ["roads"])
        self.assertEqual([f["name"] for f in r["datasets"][0]["fields"]], ["fid", "route_id", "geom"])

    def test_json_rpc_loop(self):
        script = os.path.join(os.path.dirname(HERE), "columbus_worker.py")
        lines = [
            json.dumps({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
            "not json",
            json.dumps({"jsonrpc": "2.0", "id": 2, "method": "nope"}),
            json.dumps({"jsonrpc": "2.0", "id": 3, "method": "sql_select", "params": {"connection": self.conn, "sql": "SELECT 1", "max_rows": 1}}),
        ]
        p = subprocess.run([sys.executable, "-I", "-u", script], input="\n".join(lines) + "\n", capture_output=True, text=True, timeout=30)
        replies = [json.loads(l) for l in p.stdout.splitlines()]
        self.assertEqual(replies[0]["id"], 1)
        self.assertEqual(replies[1]["error"]["code"], -32700)
        self.assertEqual(replies[2]["error"]["code"], -32601)
        self.assertEqual(replies[3]["result"]["rows"], [[1]])


if __name__ == "__main__":
    unittest.main()
