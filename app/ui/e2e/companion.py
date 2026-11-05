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

import base64
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

sys.path.insert(0, str(Path(__file__).resolve().parent))

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
OUT = HERE / "out"


from mcp_client import Mcp, wait_file  # noqa: E402

PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")


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
            ctx = browser.new_context(viewport={"width": 1360, "height": 860})
            # OpenFreeMap, played by the test: the browser applies the page's
            # policy before a request is made, so a refused tile never
            # reaches this route.
            tiles = []
            ofm = "https://tiles.openfreemap.org"
            # Vector tiles, as OpenFreeMap's: the map's worker fetches them.
            style = {"version": 8, "sprite": ofm + "/sprites/test", "sources": {"t": {"type": "vector",
                     "tiles": [ofm + "/test/{z}/{x}/{y}.pbf"]}},
                     "layers": [{"id": "t", "type": "fill", "source": "t", "source-layer": "water"}]}

            def fake_ofm(route):
                url = route.request.url
                if "/sprites/" in url:
                    # Icons that take a moment, as a real basemap's do.
                    time.sleep(0.8)
                    if url.endswith(".json"):
                        route.fulfill(status=200, content_type="application/json", body="{}",
                                      headers={"access-control-allow-origin": "*"})
                    else:
                        route.fulfill(status=200, content_type="image/png", body=PNG,
                                      headers={"access-control-allow-origin": "*"})
                    return
                if "/styles/" in url:
                    route.fulfill(status=200, content_type="application/json", body=json.dumps(style),
                                  headers={"access-control-allow-origin": "*"})
                else:
                    tiles.append(url)
                    # An empty tile: valid, with nothing in it.
                    route.fulfill(status=200, content_type="application/x-protobuf", body=b"",
                                  headers={"access-control-allow-origin": "*"})
            ctx.route(ofm + "/**", fake_ofm)
            page = ctx.new_page()
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

            # The basemap: its tiles load (through the map's worker).
            page.get_by_role("checkbox", name="Basemap").check()
            deadline = time.time() + 20
            while not tiles and time.time() < deadline:
                page.wait_for_timeout(250)
            assert tiles, "no basemap tile was requested"
            expect(page.get_by_text("could not be loaded")).to_have_count(0)
            expect(page.get_by_role("checkbox", name="Basemap")).to_be_checked()
            # The session's layer is still on the map over the basemap, and
            # after switching it off again.
            expect(page.locator(".map[data-layers='1']")).to_have_count(1, timeout=10000)
            page.wait_for_timeout(1500)
            page.screenshot(path=str(OUT / "1-basemap.png"))
            page.get_by_role("checkbox", name="Basemap").uncheck()
            expect(page.locator(".map[data-layers='1']")).to_have_count(1, timeout=10000)
            page.get_by_role("checkbox", name="Basemap").check()
            expect(page.locator(".map[data-layers='1']")).to_have_count(1, timeout=10000)
            # Off again for the rest: the slow icons above hold up the
            # browser's other requests while they wait.
            page.get_by_role("checkbox", name="Basemap").uncheck()
            expect(page.locator(".map[data-layers='1']")).to_have_count(1, timeout=10000)
            page.wait_for_timeout(1000)

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
            page2 = ctx.new_page()
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
