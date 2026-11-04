"""Tests for the Python plugin SDK. Standard library only."""

import json
import math
import os
import sqlite3
import struct
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SDK_DIR = os.path.dirname(HERE)
sys.path.insert(0, SDK_DIR)

from columbus_plugin import Extract, GeometryError, decode_geometry, readonly_uri, write_csv, write_geojson  # noqa: E402


def wkb(code, body, little=True, srid=None):
    e = "<" if little else ">"
    if srid is not None:
        return struct.pack(e + "BII", 1 if little else 0, code | 0x20000000, srid) + body
    return struct.pack(e + "BI", 1 if little else 0, code) + body


def pts(e, coords):
    return b"".join(struct.pack(e + "d" * len(c), *c) for c in coords)


def gpkg(wkb_bytes, srid=3081, envelope=1):
    env = {0: b"", 1: struct.pack("<4d", 0, 1, 0, 1)}[envelope]
    flags = 0x01 | (envelope << 1)
    return b"GP" + bytes([0, flags]) + struct.pack("<i", srid) + env + wkb_bytes


class GeometryTests(unittest.TestCase):
    def test_types_and_byte_orders(self):
        for little in (True, False):
            e = "<" if little else ">"
            p = decode_geometry(wkb(1, pts(e, [(1, 2)]), little))
            self.assertEqual((p.type, p.parts), ("Point", [(1.0, 2.0)]))
            line = decode_geometry(wkb(2, struct.pack(e + "I", 2) + pts(e, [(0, 0), (3, 4)]), little))
            self.assertEqual(line.bbox(), (0, 0, 3, 4))
            ring = [(0, 0), (1, 0), (1, 1), (0, 0)]
            poly = decode_geometry(wkb(3, struct.pack(e + "II", 1, 4) + pts(e, ring), little))
            self.assertTrue(poly.is_valid_basic())

    def test_multi_z_ewkb_and_gpkg(self):
        e = "<"
        pz = wkb(1001, pts(e, [(1, 2, 3)]))
        mp = decode_geometry(wkb(4, struct.pack(e + "I", 2) + pz + wkb(1, pts(e, [(5, 6)]))))
        self.assertEqual((mp.type, mp.bbox()), ("MultiPoint", (1, 2, 5, 6)))
        ew = decode_geometry(wkb(2 | 0x80000000, struct.pack(e + "I", 2) + pts(e, [(0, 0, 9), (1, 1, 9)]), srid=4326))
        self.assertEqual((ew.type, ew.srid, ew.bbox()), ("LineString", 4326, (0, 0, 1, 1)))
        for env in (0, 1):
            g = decode_geometry(gpkg(wkb(1, pts(e, [(7, 8)])), envelope=env))
            self.assertEqual((g.srid, g.parts), (3081, [(7.0, 8.0)]))

    def test_empty_invalid_and_truncated(self):
        empty = decode_geometry(wkb(1, pts("<", [(math.nan, math.nan)])))
        self.assertTrue(empty.is_empty)
        self.assertIsNone(empty.bbox())
        open_ring = wkb(3, struct.pack("<II", 1, 4) + pts("<", [(0, 0), (1, 0), (1, 1), (0, 1)]))
        self.assertFalse(decode_geometry(open_ring).is_valid_basic())
        with self.assertRaises(GeometryError):
            decode_geometry(wkb(2, struct.pack("<I", 5) + pts("<", [(0, 0)])))
        with self.assertRaises(GeometryError):
            decode_geometry(b"\x01\x02")
        self.assertIsNone(decode_geometry(None))


class UriTests(unittest.TestCase):
    def test_windows_paths(self):
        self.assertEqual(readonly_uri(r"\\?\D:\a\ws\inputs\r1-x.sqlite", windows=True), "file:///D:/a/ws/inputs/r1-x.sqlite?mode=ro")
        self.assertEqual(readonly_uri(r"C:\GIS\My Data\roads#1.gpkg", windows=True), "file:///C:/GIS/My%20Data/roads%231.gpkg?mode=ro")
        self.assertEqual(readonly_uri(r"\\server\gis\roads.gpkg", windows=True), "file://server/gis/roads.gpkg?mode=ro")
        self.assertEqual(readonly_uri(r"\\?\UNC\server\gis\roads.gpkg", windows=True), "file://server/gis/roads.gpkg?mode=ro")

    def test_posix_paths(self):
        self.assertEqual(readonly_uri("/tmp/a b?.sqlite", windows=False), "file:///tmp/a%20b%3F.sqlite?mode=ro")


class WriterTests(unittest.TestCase):
    def test_csv_and_geojson(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "a", "t.csv")
            self.assertEqual(write_csv(p, ["k", "v"], [["a,b", 'say "hi"'], ["x\ny", 1]]), 2)
            with open(p, encoding="utf-8", newline="") as f:
                self.assertEqual(f.read(), 'k,v\r\n"a,b","say ""hi"""\r\n"x\ny",1\r\n')
            g = os.path.join(d, "f.geojson")
            n = write_geojson(g, ({"type": "Feature", "geometry": None, "properties": {"i": i}} for i in range(3)))
            self.assertEqual(n, 3)
            with open(g, encoding="utf-8") as f:
                self.assertEqual(len(json.load(f)["features"]), 3)


PLUGIN = textwrap.dedent('''
    import time
    from columbus_plugin import Plugin, Severity

    plugin = Plugin("test.plugin", "0.1.0")

    @plugin.tool("count", params=["layer"], outputs=["report"])
    def count(ctx):
        ex = ctx.dataset("layer")
        n = 0
        for row in ex.rows():
            n += 1
            ctx.progress(None, f"{n} rows")
        ctx.set_output("report", rows=ctx_write(ctx, n))
        ctx.warn("test.note", Severity.INFO, f"{n} rows counted")
        return {"rows": n}

    def ctx_write(ctx, n):
        from columbus_plugin import write_csv
        return write_csv(ctx.output_path("report"), ["rows"], [[n]])

    @plugin.tool("wait", params=["seconds"])
    def wait(ctx):
        end = time.time() + float(ctx.param("seconds", 1))
        while time.time() < end:
            ctx.progress(None, "waiting")
            time.sleep(0.05)
        return {}

    if __name__ == "__main__":
        plugin.serve()
''')


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = os.path.join(self.tmp.name, "plugin")
        os.makedirs(self.dir)
        with open(os.path.join(self.dir, "worker.py"), "w", encoding="utf-8") as f:
            f.write(PLUGIN)
        host = os.path.join(SDK_DIR, "columbus_plugin_host.py")
        self.proc = subprocess.Popen([sys.executable, "-I", "-u", host, self.dir, "worker.py"], stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, text=True, bufsize=1)

    def tearDown(self):
        if self.proc.poll() is None:
            self.proc.stdin.close()
            self.proc.wait(timeout=15)
        self.proc.stdout.close()
        self.tmp.cleanup()

    def send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def until(self, rid, notes=None, timeout=15):
        deadline = time.time() + timeout
        while time.time() < deadline:
            msg = json.loads(self.proc.stdout.readline())
            if "id" in msg and msg["id"] == rid:
                return msg
            if notes is not None:
                notes.append(msg)
        self.fail("no reply")

    def make_extract(self, n):
        path = os.path.join(self.tmp.name, "ex.sqlite")
        db = sqlite3.connect(path)
        db.execute('CREATE TABLE "data" (a)')
        db.executemany('INSERT INTO "data" VALUES (?)', [(i,) for i in range(n)])
        db.execute("CREATE TABLE columbus_extract (key TEXT PRIMARY KEY, value TEXT)")
        db.execute("INSERT INTO columbus_extract VALUES ('rows', ?)", (str(n),))
        db.commit()
        db.close()
        return path

    def test_describe_run_and_outputs(self):
        self.send({"jsonrpc": "2.0", "id": 1, "method": "describe"})
        d = self.until(1)["result"]
        self.assertEqual(d["plugin"], {"id": "test.plugin", "version": "0.1.0"})
        self.assertEqual(d["tools"][0], {"name": "count", "params": ["layer"], "outputs": ["report"]})
        out = os.path.join(self.tmp.name, "results", "r1-report.csv")
        notes = []
        self.send({"jsonrpc": "2.0", "id": 2, "method": "run", "params": {
            "run": "r1", "tool": "count", "inputs": {"layer": {"kind": "dataset", "path": self.make_extract(5)}},
            "outputs": {"report": {"type": "table", "path": out}}}})
        r = self.until(2, notes)["result"]
        self.assertEqual(r["summary"], {"rows": 5})
        self.assertEqual(r["outputs"], {"report": {"rows": 1}})
        self.assertEqual(r["warnings"][0]["id"], "test.note")
        self.assertTrue(any(n.get("method") == "progress" and n["params"]["run"] == "r1" for n in notes))
        self.assertTrue(os.path.exists(out))

    def test_cancel_busy_and_unknown_tool(self):
        self.send({"jsonrpc": "2.0", "id": 1, "method": "run", "params": {"run": "r9", "tool": "wait", "params": {"seconds": 30}}})
        notes = []
        # Wait for the run to start reporting, then try a second run and cancel.
        while not notes or notes[-1].get("method") != "progress":
            notes.append(json.loads(self.proc.stdout.readline()))
        self.send({"jsonrpc": "2.0", "id": 2, "method": "run", "params": {"run": "r10", "tool": "wait"}})
        self.assertIn("busy", self.until(2)["error"]["message"])
        started = time.time()
        self.send({"jsonrpc": "2.0", "id": 3, "method": "cancel", "params": {"run": "r9"}})
        replies = {}
        while 1 not in replies or 3 not in replies:
            msg = json.loads(self.proc.stdout.readline())
            if "id" in msg:
                replies[msg["id"]] = msg
        self.assertEqual(replies[3]["result"], {})
        self.assertEqual(replies[1]["error"]["code"], -32004)
        self.assertLess(time.time() - started, 10)
        self.send({"jsonrpc": "2.0", "id": 4, "method": "run", "params": {"run": "r11", "tool": "nope"}})
        self.assertEqual(self.until(4)["error"]["code"], -32602)

    def test_next_run_right_after_a_reply_is_never_busy(self):
        path = self.make_extract(3)
        out = os.path.join(self.tmp.name, "results", "r.csv")
        for i in range(30):
            self.send({"jsonrpc": "2.0", "id": 100 + i, "method": "run", "params": {
                "run": f"q{i}", "tool": "count", "inputs": {"layer": {"kind": "dataset", "path": path}},
                "outputs": {"report": {"type": "table", "path": out}}}})
            reply = self.until(100 + i)
            self.assertIn("result", reply, reply)

    def test_entry_outside_plugin_folder_refused(self):
        host = os.path.join(SDK_DIR, "columbus_plugin_host.py")
        p = subprocess.run([sys.executable, "-I", host, self.dir, "../../etc/passwd"], capture_output=True, text=True, timeout=30)
        self.assertEqual(p.returncode, 2)


if __name__ == "__main__":
    unittest.main()
