"""End-to-end test of the companion window's map tools (REQ-28, T-22) in a
real browser, against a real session.

A layer of three routes, far apart: clicking row 2 in the table fits the
map to route 2; with **Zoom to selection** off, clicking row 3 leaves the
view as it was, and a second window of the same session remembers the
choice. **Measure** across a line 100 m long (geodesic, WGS 84) shows
328 ft, within 1%; Escape clears it. Screenshots go to e2e/out/.

    python app/ui/e2e/map.py [path/to/columbus-mcp]

Needs the GIS engine (geopandas, pyogrio, shapely, pyproj) and Playwright.
`COLUMBUS_E2E_CHROMIUM` may name a Chromium or Chrome to use instead of
Playwright's own (for machines that cannot download it).
"""

import json
import math
import os
import re
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import Mcp, wait_file  # noqa: E402

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
OUT = HERE / "out"

# Three routes about 300 m long, 3 km apart (Austin, EPSG:4326).
ROUTES = [[(-97.80 + i * 0.03, 30.25), (-97.7985 + i * 0.03, 30.2515), (-97.797 + i * 0.03, 30.2525)] for i in range(3)]
FT_PER_M = 1 / 0.3048


def bbox(coords):
    xs = [c[0] for c in coords]
    ys = [c[1] for c in coords]
    return min(xs), min(ys), max(xs), max(ys)


def view(page):
    v = page.locator(".map").get_attribute("data-view")
    return tuple(float(x) for x in v.split(",")) if v else None


def settled(page, before=None, t=15):
    """The view once the map has stopped moving (and, given `before`, has
    moved away from it)."""
    end = time.time() + t
    last, since = None, time.time()
    while time.time() < end:
        v = view(page)
        if v != last:
            last, since = v, time.time()
        elif v is not None and v != before and time.time() - since > 0.8:
            return v
        page.wait_for_timeout(100)
    raise AssertionError(f"the map did not settle (view {last}, before {before})")


def merc(lat):
    return math.log(math.tan(math.pi / 4 + math.radians(lat) / 2))


def to_px(page, lon, lat):
    w, s, e, n = view(page)
    r = page.locator(".map canvas").bounding_box()
    x = r["x"] + (lon - w) / (e - w) * r["width"]
    y = r["y"] + (merc(n) - merc(lat)) / (merc(n) - merc(s)) * r["height"]
    return x, y


def open_link(port, root):
    launch = root / "launch.txt"
    if launch.exists():
        launch.unlink()
    req = urllib.request.Request(f"http://127.0.0.1:{port}/api/open", method="POST", headers={"X-Columbus-Open": "1"})
    assert urllib.request.urlopen(req).status == 200
    return wait_file(launch)


def main():
    exe = Path(sys.argv[1] if len(sys.argv) > 1 else REPO / "target/debug/columbus-mcp")
    ui = REPO / "app/ui/dist"
    assert (ui / "index.html").exists(), "build the UI first: npm run build"
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from pyproj import Geod
    from shapely.geometry import LineString

    gpd.GeoDataFrame(
        {"ROUTE": ["Route 1", "Route 2", "Route 3"]},
        geometry=[LineString(r) for r in ROUTES],
        crs="EPSG:4326",
    ).to_file(root / "r.gpkg", layer="routes", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "policy.yaml").write_text("connection_types: [gpkg]\n")
    (root / "connections/roads.yaml").write_text(
        f"id: roads\ntype: gpkg\npath: {json.dumps(str(root / 'r.gpkg'))}\ndescription: Three test routes\n"
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
        port = json.loads(wait_file(root / "live" / f"{session}.json"))["port"]
        mcp.tool("query_features", {"connection": "roads", "dataset": "routes"})
        link = open_link(port, root)

        with sync_playwright() as pw:
            browser = pw.chromium.launch(executable_path=os.environ.get("COLUMBUS_E2E_CHROMIUM") or None)
            ctx = browser.new_context(viewport={"width": 1360, "height": 860})
            page = ctx.new_page()
            problems = []
            page.on("pageerror", lambda e: problems.append(str(e)))
            page.goto(link)
            expect(page.locator("table tbody tr")).to_have_count(3, timeout=20000)
            expect(page.locator(".map[data-layers='1']")).to_have_count(1, timeout=15000)
            zoom = page.get_by_role("checkbox", name="Zoom to selection")
            expect(zoom).to_be_checked()
            # The layer's own fit (all three routes) first.
            layer_view = settled(page)

            # Row 2: the map fits route 2.
            page.locator("table tbody tr").nth(1).click()
            v = settled(page, before=layer_view)
            w, s, e, n = v
            rw, rs, re_, rn = bbox(ROUTES[1])
            assert w <= rw and s <= rs and e >= re_ and n >= rn, f"route 2 {bbox(ROUTES[1])} is not all in view {v}"
            cx, cy = (w + e) / 2, (s + n) / 2
            assert abs(cx - (rw + re_) / 2) < (e - w) * 0.05, f"view {v} is not centred on route 2"
            assert abs(cy - (rs + rn) / 2) < (n - s) * 0.05, f"view {v} is not centred on route 2"
            # Fitted: route 2 fills the view across or down, less the padding.
            fill = max((re_ - rw) / (e - w), (rn - rs) / (n - s))
            assert fill > 0.4, f"view {v} is not fitted to route 2 (it fills {fill:.0%})"
            for other in (ROUTES[0], ROUTES[2]):
                ow, os_, oe, on = bbox(other)
                assert oe < w or ow > e, f"another route {bbox(other)} is in view {v}"
            expect(page.locator("table tbody tr.selected")).to_have_count(1)
            page.screenshot(path=str(OUT / "map-1-row2.png"))
            print("map: row 2 fits route 2", v)

            # Zoom to selection off: row 3 is selected, the view stays.
            zoom.uncheck()
            before = view(page)
            page.locator("table tbody tr").nth(2).click()
            expect(page.locator("table tbody tr").nth(2)).to_have_class(re.compile("selected"))
            page.wait_for_timeout(1500)
            assert view(page) == before, f"the view moved with zoom off: {before} -> {view(page)}"
            print("map: zoom off keeps the view")

            # Remembered for this chat: another window of the session.
            page2 = ctx.new_page()
            page2.goto(open_link(port, root))
            expect(page2.locator("table tbody tr")).to_have_count(3, timeout=20000)
            expect(page2.get_by_role("checkbox", name="Zoom to selection")).not_to_be_checked()
            page2.close()

            # Measure 100 m, zoomed in so a pixel is a small part of it.
            b = view(page)
            page.get_by_role("button", name="Zoom in").click()
            settled(page, before=b)
            g = Geod(ellps="WGS84")
            lon1, lat1 = (rw + re_) / 2, (rs + rn) / 2
            lon2, lat2, _ = g.fwd(lon1, lat1, 90, 100)
            assert abs(g.inv(lon1, lat1, lon2, lat2)[2] - 100) < 1e-6
            measure = page.get_by_role("button", name="Measure")
            measure.click()
            expect(measure).to_have_attribute("aria-pressed", "true")
            x1, y1 = to_px(page, lon1, lat1)
            x2, y2 = to_px(page, lon2, lat2)
            assert x2 - x1 > 200, f"100 m is only {x2 - x1:.0f} px; zoom in further"
            r = page.locator(".map canvas").bounding_box()
            assert r["x"] < x1 < x2 < r["x"] + r["width"] - 60, f"the line ({x1:.0f}..{x2:.0f} px) is not all on the map {r}"
            at = view(page)
            page.mouse.click(x1, y1)
            page.mouse.click(x2, y2)
            page.mouse.dblclick(x2, y2)
            result = page.locator(".measure-result")
            expect(result).to_contain_text(" ft", timeout=5000)
            text = result.inner_text()
            ft = float(re.search(r"([\d,.]+) ft", text).group(1).replace(",", ""))
            m = float(re.search(r"\(([\d,.]+) m\)", text).group(1).replace(",", ""))
            want = 100 * FT_PER_M
            assert abs(ft - want) / want < 0.01, f"measured {text!r}, want {want:.0f} ft within 1%"
            assert abs(m - 100) < 1, f"measured {text!r}, want 100 m"
            # Measuring never identifies: row 3 is still the selection.
            expect(page.locator("table tbody tr").nth(2)).to_have_class(re.compile("selected"))
            # The double-click finished the line without zooming.
            assert view(page) == at, f"measuring moved the map: {at} -> {view(page)}"
            page.screenshot(path=str(OUT / "map-2-measure.png"))
            print(f"map: measured {text!r}")

            # Escape clears it.
            page.keyboard.press("Escape")
            expect(result).to_have_count(0)
            expect(measure).to_have_attribute("aria-pressed", "false")

            assert not problems, problems
            browser.close()
        print("map e2e: ok")
    finally:
        mcp.close()


if __name__ == "__main__":
    main()
