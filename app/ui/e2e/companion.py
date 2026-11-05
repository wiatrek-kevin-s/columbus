"""End-to-end test of the companion window in a real browser.

Starts columbus-mcp (a debug build: launch links are handed off to a file
instead of opening a browser) with the built UI, speaks MCP to it over stdio
as an AI client would, and drives the window with Playwright: the layer the
agent queried appears in the list, on the map and in the table; an export
that needs approval shows a card. The window the app opens may only look:
its card offers "Review in your browser", which opens the page that can
decide (a second browser page here); allowing it there lets the export
finish. Screenshots go to e2e/out/.

    python app/ui/e2e/companion.py [path/to/columbus-mcp]

Needs the GIS engine (geopandas, pyogrio, shapely, pyproj) and Playwright.
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
OUT = HERE / "out"


class Mcp:
    """A minimal MCP client over stdio: requests in, responses by id."""

    def __init__(self, cmd, env):
        self.p = subprocess.Popen(cmd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        self.next = 0
        self.lock = threading.Lock()
        self.replies = {}
        self.cv = threading.Condition()
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                    "clientInfo": {"name": "companion-e2e", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, msg):
        with self.lock:
            self.p.stdin.write(json.dumps(msg) + "\n")
            self.p.stdin.flush()

    def _read(self):
        for line in self.p.stdout:
            m = json.loads(line)
            if "id" in m and ("result" in m or "error" in m):
                with self.cv:
                    self.replies[m["id"]] = m
                    self.cv.notify_all()

    def request(self, method, params, timeout=120):
        with self.lock:
            self.next += 1
            i = self.next
        self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        end = time.time() + timeout
        with self.cv:
            while i not in self.replies:
                left = end - time.time()
                assert left > 0, f"no reply to {method}"
                self.cv.wait(left)
            return self.replies.pop(i)

    def tool(self, name, args, timeout=120):
        r = self.request("tools/call", {"name": name, "arguments": args}, timeout)
        res = r["result"]
        assert not res.get("isError"), res
        return res["structuredContent"]

    def close(self):
        self.p.stdin.close()
        self.p.wait(10)


def wait_file(p: Path, t=20):
    end = time.time() + t
    while time.time() < end:
        if p.exists() and p.read_text().strip():
            return p.read_text().strip()
        time.sleep(0.1)
    raise AssertionError(f"{p} never appeared")


def main():
    exe = Path(sys.argv[1] if len(sys.argv) > 1 else REPO / "target/debug/columbus-mcp")
    ui = REPO / "app/ui/dist"
    assert (ui / "index.html").exists(), "build the UI first: npm run build"
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box

    gpd.GeoDataFrame(
        {"NAME": [f"Tract {i}" for i in range(12)], "Land Area": [float(i * 10) for i in range(12)]},
        geometry=[box(-97.9 + (i % 4) * 0.06, 30.2 + (i // 4) * 0.05, -97.86 + (i % 4) * 0.06, 30.24 + (i // 4) * 0.05) for i in range(12)],
        crs="EPSG:4269",
    ).to_crs("EPSG:3081").to_file(root / "t.gpkg", layer="Tribal Census Tract", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "outbox").mkdir()
    (root / "policy.yaml").write_text(
        "connection_types: [gpkg]\napprovals: { page: true, wait_seconds: 60 }\n"
        f"exports:\n  - {{ id: outbox, path: '{root / 'outbox'}' }}\n"
    )
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndescription: Census TIGER test layer\n"
        "datasets:\n  allow: [\"*\"]\n"
    )
    env = dict(os.environ,
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_UI_DIR=str(ui),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_LAUNCH_FILE=str(root / "launch.txt"), COLUMBUS_LOG="warn")
    mcp = Mcp([str(exe), "serve"], env)
    try:
        session = mcp.tool("whoami", {})["result"]["session"]
        live = json.loads(wait_file(root / "live" / f"{session}.json"))
        port = live["port"]
        q = mcp.tool("query_features", {"connection": "tiger", "dataset": "Tribal Census Tract"})
        handle = q["result"]["layer"]["handle"]

        # What the Columbus app does: ask the session for a window.
        req = urllib.request.Request(f"http://127.0.0.1:{port}/api/open", method="POST",
                                     headers={"X-Columbus-Open": "1"})
        assert urllib.request.urlopen(req).status == 200
        link = wait_file(root / "launch.txt")

        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1360, "height": 860})
            problems = []
            page.on("console", lambda m: m.type in ("error",) and problems.append(m.text))
            page.on("pageerror", lambda e: problems.append(str(e)))
            page.goto(link)
            expect(page.get_by_role("button", name="tribal_census_tract", exact=False)).to_be_visible(timeout=20000)
            assert "#t=" not in page.url, "the launch token leaves the address bar"
            expect(page.get_by_text("12 rows").first).to_be_visible()
            expect(page.get_by_role("checkbox", name="Show")).to_be_checked()
            expect(page.locator("table tbody tr")).to_have_count(12, timeout=15000)
            expect(page.locator("table")).to_contain_text("Tract 3")
            page.wait_for_timeout(1500)
            page.screenshot(path=str(OUT / "1-layer.png"))

            # Click a row: it is selected.
            page.locator("table tbody tr").nth(2).click()
            expect(page.locator("table tbody tr.selected")).to_have_count(1)

            # The agent exports; the window shows the request, but this
            # window (opened for the app) cannot decide it.
            (root / "launch.txt").unlink()
            result = {}
            t = threading.Thread(target=lambda: result.update(
                r=mcp.tool("export_output", {"handle": handle, "folder": "outbox", "name": "tracts"}, timeout=90)))
            t.start()
            card = page.locator("article.approval")
            expect(card).to_be_visible(timeout=20000)
            expect(card).to_contain_text("tracts")
            expect(card.get_by_role("button", name="Allow once")).to_have_count(0)
            page.screenshot(path=str(OUT / "2-approval-view.png"))
            # The approval page opened for the waiting request (or on review).
            if not (root / "launch.txt").exists():
                page.get_by_role("button", name="Review in your browser").click()
            approve_link = wait_file(root / "launch.txt")
            assert approve_link.startswith("http://127.0.0.1:"), approve_link
            page2 = browser.new_page(viewport={"width": 1360, "height": 860})
            page2.on("pageerror", lambda e: problems.append(str(e)))
            page2.goto(approve_link)
            card2 = page2.locator("article.approval")
            expect(card2).to_be_visible(timeout=20000)
            allow = card2.get_by_role("button", name="Allow once")
            expect(allow).to_be_disabled()
            expect(allow).to_be_enabled(timeout=3000)
            page2.screenshot(path=str(OUT / "2-approval-browser.png"))
            allow.click()
            t.join(60)
            assert result.get("r", {}).get("result", {}).get("status") == "exported", result
            assert (root / "outbox" / "tracts.gpkg").exists()
            expect(card).to_have_count(0, timeout=10000)
            expect(page2.get_by_role("status")).to_contain_text("Allowed")
            page.screenshot(path=str(OUT / "3-done.png"))

            # The rendered page held to its policy: no CSP violations.
            csp = [p for p in problems if "Content Security Policy" in p or "Refused to" in p]
            assert not csp, csp
            browser.close()
        print("companion e2e: ok")
    finally:
        mcp.close()


if __name__ == "__main__":
    main()
