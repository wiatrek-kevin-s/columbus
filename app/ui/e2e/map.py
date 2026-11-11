"""End-to-end test of the companion window's map tools (REQ-28, T-22) in a
real browser, against a real session.

The map fills the page and sheets float on it (the page's panel on the
left, the table at the foot), so "in view" here means the part of the map
no sheet covers. A layer of three routes, far apart, is fitted in the
middle of that part, and again when the table opens over it. Clicking row 2
in the table fits the map to route 2, in the middle of what is left in
view; with **Zoom to selection** off, clicking row 3 leaves the view as it
was, and a second window of the same session remembers the choice.
**Measure** across a line 100 m long (geodesic, WGS 84) shows 328 ft,
within 1%, at the foot of the part in view; Escape clears it. Once the
person has moved the map, closing the table keeps the middle of their view
in the middle and does not zoom. Screenshots go to e2e/out/.

Then the assistant and the map (T-30, REQ-40), with the same session: after
the person pans and picks a feature on the map, `map_view` returns the view
the page shows (the part of the map no sheet covers), the layers that are
on, the active one and the picked feature with its attributes; asked to
hide a layer, then to show it, make another active and zoom to it, the page
follows within a few seconds and the tool returns the new view; a zoom to
an extent fits it; `map_snapshot` with `extent: "view"` draws that same
view; with two windows open the one used last is read; and with no window
open the tool says so and changes nothing.

First it runs `unit.mjs` with Node: the checks of the map's pure parts,
which need no browser (the street background's colours, the app's inset,
where the sheets go).

    python app/ui/e2e/map.py [path/to/columbus-mcp]

Needs the GIS engine (geopandas, pyogrio, shapely, pyproj), Playwright, and
Node 22.18 or newer (the one that built the UI). `COLUMBUS_E2E_CHROMIUM` may
name a Chromium or Chrome to use instead of Playwright's own (for machines
that cannot download it).
"""

import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import Mcp, wait_file  # noqa: E402
import a11y  # noqa: E402

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


def unmerc(y):
    return math.degrees(2 * math.atan(math.exp(y)) - math.pi / 2)


def to_px(page, lon, lat):
    w, s, e, n = view(page)
    r = page.locator(".map canvas").bounding_box()
    x = r["x"] + (lon - w) / (e - w) * r["width"]
    y = r["y"] + (merc(n) - merc(lat)) / (merc(n) - merc(s)) * r["height"]
    return x, y


def clear(page):
    """The part of the map no sheet covers, in page pixels (left, top,
    right, bottom): right of the page's panel, above the table sheet. Read
    from the sheets themselves, not from what the page says it pads."""
    r = page.locator(".map canvas").bounding_box()
    left, top, right, bottom = r["x"], r["y"], r["x"] + r["width"], r["y"] + r["height"]
    panel = page.locator(".panel").bounding_box()
    assert panel, "the page's own panel is there"
    left = max(left, panel["x"] + panel["width"])
    table = page.locator("section.table")
    if table.count():
        bottom = min(bottom, table.bounding_box()["y"])
    assert right - left > 300 and bottom - top > 200, f"too little of the map is in view: {(left, top, right, bottom)}"
    return left, top, right, bottom


def in_view(page):
    """The same part, as west, south, east, north."""
    w, s, e, n = view(page)
    r = page.locator(".map canvas").bounding_box()
    left, top, right, bottom = clear(page)

    def lon(x):
        return w + (x - r["x"]) / r["width"] * (e - w)

    def lat(y):
        return unmerc(merc(n) - (y - r["y"]) / r["height"] * (merc(n) - merc(s)))

    return lon(left), lat(bottom), lon(right), lat(top)


def assistant_and_the_map(mcp, ctx, page, port, root, routes, problems):
    """The assistant reads and steers the person's map (T-30, REQ-40)."""

    def read():
        return mcp.tool("map_view", {})["result"]

    def until(what, ok, t=8):
        """`map_view`'s answer once it is as expected (the page reports
        within a second of a change)."""
        end = time.time() + t
        while True:
            r = read()
            if ok(r):
                return r
            assert time.time() < end, f"{what}: {json.dumps(r)[:1500]}"
            # (Not the page's clock: this is also used once the page is closed.)
            time.sleep(0.25)

    def shown(where):
        """What the page shows, worked out here from the sheets: the part
        of the map no sheet covers, less the 16 px the page keeps clear
        beside its own panel (west, south, east, north)."""
        w, s, e, n = view(where)
        r = where.locator(".map canvas").bounding_box()
        left, top, right, bottom = clear(where)
        left += 16

        def lon(x):
            return w + (x - r["x"]) / r["width"] * (e - w)

        def lat(y):
            return unmerc(merc(n) - (y - r["y"]) / r["height"] * (merc(n) - merc(s)))

        return lon(left), lat(bottom), lon(right), lat(top)

    def same_view(got, where, what):
        """The extent the tool returned is the one the page shows, to
        within three pixels."""
        want = shown(where)
        r = where.locator(".map canvas").bounding_box()
        full = view(where)
        px = max((full[2] - full[0]) / r["width"], (full[3] - full[1]) / r["height"])
        for a, b in zip(got, want):
            assert abs(a - b) < 3 * px, f"{what}: the tool says {got}, the page shows {want} (a pixel is {px})"

    def name_of(handle):
        return handle.rsplit("/", 1)[1]

    # ---- Reading. The view, as the earlier steps left it: the person's
    # own zoom on route 2, row 3 selected in the table (now closed).
    r = read()
    assert r["status"] == "open", r
    v = r["view"]
    same_view(v["extent"], page, "the view read")
    assert v["crs"] == "EPSG:4326" and v["layers_on"] == [routes] and v["active"] == routes, v
    assert v["table_open"] is False and v["street_background"] is False, v
    assert v["selected"]["layer"] == routes and v["selected"]["feature"] == 2, v
    assert v["selected"]["attributes"] == {"ROUTE": "Route 3"}, v
    print("map: the assistant reads the view", v["extent"], "zoom", v["zoom"])

    # The person pans the map...
    left, top, right, bottom = clear(page)
    cx, cy = (left + right) / 2, (top + bottom) / 2
    before = view(page)
    page.mouse.move(cx, cy)
    page.mouse.down()
    page.mouse.move(cx - 90, cy + 50, steps=8)
    page.mouse.up()
    panned = settled(page, before=before)
    r = until("the view after a pan", lambda r: abs(r["view"]["extent"][0] - shown(page)[0]) < (panned[2] - panned[0]) / 400)
    same_view(r["view"]["extent"], page, "the view after the person panned")
    assert r["view"]["extent"] != v["extent"], "the view read follows the pan"
    # ... and picks route 2 on the map (a click on the line; a pixel either
    # side is tried, since the line is two pixels wide).
    lon, lat = ROUTES[1][1]
    x, y = to_px(page, lon, lat)
    assert left + 20 < x < right - 20 and top + 20 < y < bottom - 20, f"route 2 ({x:.0f}, {y:.0f}) is not in view {(left, top, right, bottom)}"
    picked = None
    for dx, dy in [(0, 0), (0, 1), (0, -1), (1, 0), (-1, 0)]:
        page.mouse.click(x + dx, y + dy)
        try:
            picked = until("the picked feature", lambda r: (r["view"]["selected"] or {}).get("feature") == 1, t=2.5)
            break
        except AssertionError:
            continue
    assert picked, "clicking route 2 on the map did not select it"
    sel = picked["view"]["selected"]
    assert sel["layer"] == routes and sel["attributes"] == {"ROUTE": "Route 2"}, sel
    assert sel["geometry"]["type"] == "LineString", sel
    same_view(picked["view"]["extent"], page, "the view after the pick")
    assert view(page) == panned, "picking a feature does not move the map"
    print("map: after a pan and a pick, the assistant reads", picked["view"]["extent"], sel)

    # The table open is said too.
    page.get_by_role("button", name="Table", exact=True).click()
    expect(page.locator("section.table")).to_be_visible()
    settled(page)
    r = until("the table open", lambda r: r["view"]["table_open"] is True)
    same_view(r["view"]["extent"], page, "the view above the open table")
    page.get_by_role("button", name="Close the table").click()
    expect(page.locator("section.table")).to_have_count(0)
    settled(page)
    until("the table closed", lambda r: r["view"]["table_open"] is False)

    # A turned map says the box around what it shows (Shift and an arrow
    # key turn the map by 15 degrees).
    flat = read()["view"]
    page.locator(".map canvas").focus()
    page.keyboard.press("Shift+ArrowRight")
    settled(page, before=view(page))
    turned = until("a turned map", lambda r: r["view"]["extent"] != flat["extent"])["view"]

    def spans(ext):
        return math.radians(ext[2] - ext[0]), merc(ext[3]) - merc(ext[1])

    (w0, h0), (w1, h1) = spans(flat["extent"]), spans(turned["extent"])
    a = math.radians(15)
    assert abs(w1 - (w0 * math.cos(a) + h0 * math.sin(a))) < 0.02 * w1, (flat, turned)
    assert abs(h1 - (w0 * math.sin(a) + h0 * math.cos(a))) < 0.02 * h1, (flat, turned)
    page.keyboard.press("Shift+ArrowLeft")
    settled(page, before=view(page))
    until("the map turned back", lambda r: all(abs(x - y) < 1e-5 for x, y in zip(r["view"]["extent"], flat["extent"])))
    print("map: a turned map reports the box around what it shows")

    # A page that is not in front of the person (its tab in the background)
    # is not their map: it says so at once, and again when it is back.
    page.evaluate("() => { Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });"
                  " document.dispatchEvent(new Event('visibilitychange')); }")
    until("a page in the background", lambda r: r["status"] == "not_open", t=4)
    page.evaluate("() => { delete document.visibilityState; document.dispatchEvent(new Event('visibilitychange')); }")
    until("the page in front again", lambda r: r["status"] == "open" and r["view"]["extent"] == flat["extent"])
    print("map: a page in the background is not the person's map")

    # ---- Steering. A second result: the page lists it, turns it on and
    # makes it the active one, as it does for every new layer.
    q = mcp.tool("query_features", {"connection": "roads", "dataset": "routes", "where": "ROUTE = 'Route 1'"})
    one = q["result"]["layer"]["handle"]
    expect(page.locator(".map[data-layers='2']")).to_have_count(1, timeout=15000)
    settled(page)
    r = until("the new layer on", lambda r: r["view"]["layers_on"] == [one, routes] and r["view"]["active"] == one)

    # Hide it: the page follows, and the answer is the new view.
    t0 = time.time()
    r = mcp.tool("map_view", {"hide": [one]})["result"]
    took = time.time() - t0
    assert r["status"] == "changed" and r["changed"] is True, r
    assert took < 6, f"the page took {took:.1f} s to follow"
    assert r["view"]["layers_on"] == [routes], r
    expect(page.locator(".map[data-layers='1']")).to_have_count(1)
    expect(page.get_by_role("checkbox", name=f"Show {name_of(one)} on the map")).not_to_be_checked()
    expect(page.get_by_role("status")).to_contain_text("Your assistant changed what the map shows.")
    print(f"map: hide followed in {took:.1f} s")

    # Layers on, another active, and a zoom to it: the page follows within
    # a few seconds, and the view returned is the one it shows.
    before = view(page)
    t0 = time.time()
    r = mcp.tool("map_view", {"show": [one], "active": routes, "zoom_to": routes})["result"]
    took = time.time() - t0
    assert r["status"] == "changed", r
    assert took < 6, f"the page took {took:.1f} s to follow"
    assert r["view"]["layers_on"] == [one, routes] and r["view"]["active"] == routes, r
    expect(page.locator(".map[data-layers='2']")).to_have_count(1)
    expect(page.get_by_role("checkbox", name=f"Show {name_of(one)} on the map")).to_be_checked()
    expect(page.locator("li.out.active .out-name")).to_have_text(name_of(routes))
    assert view(page) != before, "the map moved"
    assert not page.evaluate("document.querySelector('.map canvas') === null")
    # The zoom has ended by the time the tool answers: the map is still.
    page.wait_for_timeout(700)
    w, s, e, n = in_view(page)
    same_view(r["view"]["extent"], page, "the view after show, active and zoom")
    for route in ROUTES:
        rw, rs, re_, rn = bbox(route)
        assert w <= rw and s <= rs and e >= re_ and n >= rn, f"route {bbox(route)} is not all in view {(w, s, e, n)}"
    aw, as_, ae, an = bbox([c for route in ROUTES for c in route])
    assert abs((w + e) / 2 - (aw + ae) / 2) < (e - w) * 0.05, f"the layer {(aw, as_, ae, an)} is not centred in view {(w, s, e, n)}"
    assert (ae - aw) / (e - w) > 0.6, f"the view {(w, s, e, n)} is not fitted to the layer"
    assert not a11y.unnamed(page), a11y.unnamed(page)  # every control has a name, and Tab reaches it
    page.screenshot(path=str(OUT / "map-3-assistant.png"))
    print(f"map: show, active and zoom followed in {took:.1f} s", r["view"]["extent"])

    # A zoom to an extent: all of it in view, fitted.
    target = [-97.775, 30.2495, -97.765, 30.2535]
    t0 = time.time()
    r = mcp.tool("map_view", {"zoom_to": target})["result"]
    took = time.time() - t0
    assert r["status"] == "changed" and took < 6, (took, r)
    page.wait_for_timeout(700)
    w, s, e, n = r["view"]["extent"]
    assert w <= target[0] and s <= target[1] and e >= target[2] and n >= target[3], f"{target} is not all in view {(w, s, e, n)}"
    assert max((target[2] - target[0]) / (e - w), (target[3] - target[1]) / (n - s)) > 0.8, f"the view {(w, s, e, n)} is not fitted to {target}"
    same_view(r["view"]["extent"], page, "the view after a zoom to an extent")
    # Across the 180th meridian: east passes 180, as the view is read.
    across = [170.0, 50.0, 190.0, 54.0]
    r = mcp.tool("map_view", {"zoom_to": across})["result"]
    assert r["status"] == "changed", r
    w, s, e, n = r["view"]["extent"]
    assert w <= 170 and e >= 190 and e > 180 and s <= 50 and n >= 54, f"{across} is not in view {(w, s, e, n)}"
    assert e - w < 40, f"not fitted: {(w, s, e, n)}"
    print(f"map: zoom to an extent followed in {took:.1f} s; across the 180th meridian the view is", (w, s, e, n))

    # ---- A picture of the same view.
    r = mcp.tool("map_view", {"zoom_to": target})["result"]
    page.wait_for_timeout(700)
    reply = mcp.request("tools/call", {"name": "map_snapshot", "arguments": {"extent": "view"}})["result"]
    assert not reply.get("isError"), reply
    assert any(c.get("type") == "image" and c.get("mimeType") == "image/png" for c in reply["content"]), "a picture for the assistant"
    snap = reply["structuredContent"]["result"]
    now = read()["view"]
    assert snap["view"]["extent"] == now["extent"], f"the snapshot's extent {snap['view']['extent']} is the view's {now['extent']}"
    same_view(snap["view"]["extent"], page, "the snapshot's extent")
    radius = 6378137.0
    want = [radius * math.radians(now["extent"][0]), radius * merc(now["extent"][1]),
            radius * math.radians(now["extent"][2]), radius * merc(now["extent"][3])]
    assert snap["crs"] == "EPSG:3857" and all(abs(a - b) < 0.01 for a, b in zip(snap["extent"], want)), (snap["extent"], want)
    # The layers that are on, the lower one first; route 2 is the only
    # route in this view.
    assert [x["handle"] for x in snap["layers"]] == [routes, one], snap["layers"]
    assert [x["in_view"] for x in snap["layers"]] == [1, 0], snap["layers"]
    # The picture has the shape of the part of the map in view.
    left, top, right, bottom = clear(page)
    assert abs(snap["width"] / snap["height"] - (right - left) / (bottom - top)) < 0.03, (snap["width"], snap["height"], right - left, bottom - top)
    print("map: the snapshot of the view", snap["view"], snap["width"], "x", snap["height"])

    # ---- Two windows of the session: the one the person used last is
    # their map. A new window comes on show with only the newest layer on.
    page2 = ctx.new_page()
    page2.on("pageerror", lambda e: problems.append(str(e)))
    page2.goto(open_link(port, root))
    expect(page2.locator(".map[data-layers='1']")).to_have_count(1, timeout=20000)
    until("the new window", lambda r: r["status"] == "open" and r["view"]["layers_on"] == [one])
    # A change goes to that window only.
    r = mcp.tool("map_view", {"show": [routes]})["result"]
    assert r["status"] == "changed" and r["view"]["layers_on"] == [one, routes], r
    expect(page2.locator(".map[data-layers='2']")).to_have_count(1)
    # The person goes back to the first window and uses it.
    left, top, right, bottom = clear(page)
    page.mouse.click(left + 40, bottom - 40)
    r = until("the first window again", lambda r: r["view"]["extent"] == now["extent"])
    same_view(r["view"]["extent"], page, "the view of the window used last")
    page2.close()
    print("map: with two windows, the one used last is read and steered")

    # ---- No map open: it says so, and changes nothing.
    before = view(page)
    page.close()
    r = until("no map open", lambda r: r["status"] == "not_open", t=10)
    assert "No map is open" in r["message"], r
    t0 = time.time()
    r = mcp.tool("map_view", {"show": [routes], "zoom_to": routes})["result"]
    assert r["status"] == "not_open" and r["changed"] is False and r["message"].startswith("Nothing was changed."), r
    assert time.time() - t0 < 12
    reply = mcp.request("tools/call", {"name": "map_snapshot", "arguments": {"extent": "view"}})["result"]
    assert reply.get("isError") and "map is not open" in json.dumps(reply), reply
    # A window opened afterwards is as the person left the first one: the
    # change that found no map was dropped, not kept for later.
    page3 = ctx.new_page()
    page3.on("pageerror", lambda e: problems.append(str(e)))
    page3.goto(open_link(port, root))
    expect(page3.locator(".map[data-layers='1']")).to_have_count(1, timeout=20000)
    r = until("a window opened later", lambda r: r["status"] == "open")
    page3.wait_for_timeout(2500)
    assert read()["view"]["layers_on"] == [one], "the dropped change was not applied later"
    page3.close()
    print("map: with no map open the assistant is told so, and nothing changes")

    # ---- A layer the map cannot draw (its file changed since Columbus
    # wrote it, so the page is refused its data): asked to show it and zoom
    # to it, the tool says which parts were not done, and does not count
    # the layer as on.
    until("no map open again", lambda r: r["status"] == "not_open", t=10)
    session, name = one[len("ws://"):].split("/", 1)
    changed = root / "ws" / session / name
    assert changed.is_file(), changed
    changed.write_bytes(changed.read_bytes() + b"changed")
    page4 = ctx.new_page()
    page4.on("pageerror", lambda e: problems.append(str(e)))
    page4.goto(open_link(port, root))
    expect(page4.get_by_text("Not on the map:")).to_be_visible(timeout=20000)
    r = until("a window with a layer it cannot draw", lambda r: r["status"] == "open")
    assert r["view"]["layers_on"] == [], r
    t0 = time.time()
    r = mcp.tool("map_view", {"show": [one, routes], "zoom_to": one})["result"]
    took = time.time() - t0
    assert r["status"] == "partly_changed" and r["changed"] is True, r
    assert sorted(x["part"] for x in r["not_done"]) == ["show", "zoom_to"], r
    assert r["view"]["layers_on"] == [routes], r
    assert took < 7, f"it took {took:.1f} s to say so"
    expect(page4.locator(".map[data-layers='1']")).to_have_count(1)
    page4.close()
    print(f"map: a layer the map cannot draw is said as not done, in {took:.1f} s")


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
    # The map's pure parts first: no browser needed.
    node = shutil.which("node")
    assert node, "Node is needed for app/ui/e2e/unit.mjs (the one that built the UI)"
    unit = subprocess.run([node, str(HERE / "unit.mjs")], cwd=REPO / "app/ui", capture_output=True, text=True, timeout=300)
    assert unit.returncode == 0, f"unit.mjs failed:\n{unit.stdout[-6000:]}\n{unit.stderr[-2000:]}"
    passed = re.search(r"^# pass (\d+)$", unit.stdout, re.M)
    assert passed and int(passed.group(1)) > 0 and re.search(r"^# fail 0$", unit.stdout, re.M), unit.stdout[-2000:]
    print(f"map: unit.mjs passed ({passed.group(1)} checks)")
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
        routes = mcp.tool("query_features", {"connection": "roads", "dataset": "routes"})["result"]["layer"]["handle"]
        link = open_link(port, root)

        with sync_playwright() as pw:
            browser = pw.chromium.launch(executable_path=os.environ.get("COLUMBUS_E2E_CHROMIUM") or None)
            ctx = browser.new_context(viewport={"width": 1360, "height": 860})
            page = ctx.new_page()
            problems = []
            page.on("pageerror", lambda e: problems.append(str(e)))
            page.goto(link)
            expect(page.locator(".map[data-layers='1']")).to_have_count(1, timeout=20000)
            # The layer's own fit first: all three routes, in the middle of
            # the part of the map the page's panel leaves in view.
            layer_view = settled(page)
            w, s, e, n = in_view(page)
            for route in ROUTES:
                rw, rs, re_, rn = bbox(route)
                assert w <= rw and s <= rs and e >= re_ and n >= rn, f"route {bbox(route)} is not all in view {(w, s, e, n)}"
            aw, as_, ae, an = bbox([c for route in ROUTES for c in route])
            assert abs((w + e) / 2 - (aw + ae) / 2) < (e - w) * 0.05, f"the layer {(aw, as_, ae, an)} is not centred in view {(w, s, e, n)}"
            assert abs((s + n) / 2 - (as_ + an) / 2) < (n - s) * 0.05, f"the layer {(aw, as_, ae, an)} is not centred in view {(w, s, e, n)}"

            # The table: a sheet over the map, opened from the active
            # output. The map makes room: the layer, still as it was fitted,
            # is fitted again into the part above the sheet.
            page.get_by_role("button", name="Table", exact=True).click()
            expect(page.locator("section.table")).to_be_visible()
            expect(page.locator("table tbody tr")).to_have_count(3, timeout=20000)
            zoom = page.get_by_role("checkbox", name="Zoom to selection")
            expect(zoom).to_be_checked()
            opened = settled(page, before=layer_view)
            w, s, e, n = in_view(page)
            for route in ROUTES:
                rw, rs, re_, rn = bbox(route)
                assert w <= rw and s <= rs and e >= re_ and n >= rn, f"route {bbox(route)} is not all in view above the table {(w, s, e, n)}"
            assert abs((w + e) / 2 - (aw + ae) / 2) < (e - w) * 0.05, f"the layer {(aw, as_, ae, an)} is not centred above the table {(w, s, e, n)}"
            assert abs((s + n) / 2 - (as_ + an) / 2) < (n - s) * 0.05, f"the layer {(aw, as_, ae, an)} is not centred above the table {(w, s, e, n)}"

            # Row 2: the map fits route 2, in the part still in view (right
            # of the panel, above the table).
            page.locator("table tbody tr").nth(1).click()
            v = settled(page, before=opened)
            w, s, e, n = in_view(page)
            rw, rs, re_, rn = bbox(ROUTES[1])
            assert w <= rw and s <= rs and e >= re_ and n >= rn, f"route 2 {bbox(ROUTES[1])} is not all in view {(w, s, e, n)}"
            cx, cy = (w + e) / 2, (s + n) / 2
            assert abs(cx - (rw + re_) / 2) < (e - w) * 0.05, f"view {(w, s, e, n)} is not centred on route 2"
            assert abs(cy - (rs + rn) / 2) < (n - s) * 0.05, f"view {(w, s, e, n)} is not centred on route 2"
            # Fitted: route 2 fills the part in view across or down, less the padding.
            fill = max((re_ - rw) / (e - w), (rn - rs) / (n - s))
            assert fill > 0.4, f"view {(w, s, e, n)} is not fitted to route 2 (it fills {fill:.0%})"
            # Nowhere on the map, under a sheet or not, is another route.
            for other in (ROUTES[0], ROUTES[2]):
                ow, os_, oe, on = bbox(other)
                assert oe < v[0] or ow > v[2], f"another route {bbox(other)} is in view {v}"
            expect(page.locator("table tbody tr.selected")).to_have_count(1)
            assert not a11y.unnamed(page), a11y.unnamed(page)  # every control has a name, and Tab reaches it
            page.screenshot(path=str(OUT / "map-1-row2.png"))
            print("map: row 2 fits route 2", (w, s, e, n))

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
            expect(page2.locator(".map[data-layers='1']")).to_have_count(1, timeout=20000)
            page2.get_by_role("button", name="Table", exact=True).click()
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
            # The line lies on the part of the map no sheet covers, clear of
            # the zoom buttons at its right edge and the result at its foot.
            left, top, right, bottom = clear(page)
            assert left < x1 < x2 < right - 60, f"the line ({x1:.0f}..{x2:.0f} px) is not all on the map in view {(left, right)}"
            assert top + 20 < y1 < bottom - 100 and top + 20 < y2 < bottom - 100, f"the line (y {y1:.0f}, {y2:.0f}) is not on the map in view {(top, bottom)}"
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
            # The result shows at the foot of the part of the map in view,
            # in the middle, clear of the sheets.
            pill = result.bounding_box()
            assert left < pill["x"] and pill["x"] + pill["width"] < right, f"the result {pill} is not over the map in view {(left, right)}"
            assert (top + bottom) / 2 < pill["y"] and pill["y"] + pill["height"] < bottom, f"the result {pill} is not at the foot of the map in view {(top, bottom)}"
            assert abs(pill["x"] + pill["width"] / 2 - (left + right) / 2) < 20, f"the result {pill} is not in the middle of {(left, right)}"
            # Measuring never identifies: row 3 is still the selection.
            expect(page.locator("table tbody tr").nth(2)).to_have_class(re.compile("selected"))
            # The double-click finished the line without zooming.
            assert view(page) == at, f"measuring moved the map: {at} -> {view(page)}"
            assert not a11y.unnamed(page), a11y.unnamed(page)  # every control has a name, and Tab reaches it
            page.screenshot(path=str(OUT / "map-2-measure.png"))
            print(f"map: measured {text!r}")

            # Escape clears it.
            page.keyboard.press("Escape")
            expect(result).to_have_count(0)
            expect(measure).to_have_attribute("aria-pressed", "false")

            # The view is the person's own now (they zoomed in). Closing the
            # table keeps its middle in the middle of the part in view, at
            # the same scale: the map makes room, it does not zoom.
            before = view(page)
            mw, ms, me, mn = in_view(page)
            page.get_by_role("button", name="Close the table").click()
            expect(page.locator("section.table")).to_have_count(0)
            settled(page, before=before)
            w, s, e, n = in_view(page)
            assert abs((w + e) / 2 - (mw + me) / 2) < (me - mw) * 0.01, f"closing the table moved the middle: {(mw, ms, me, mn)} -> {(w, s, e, n)}"
            assert abs((s + n) / 2 - (ms + mn) / 2) < (mn - ms) * 0.01, f"closing the table moved the middle: {(mw, ms, me, mn)} -> {(w, s, e, n)}"
            assert abs((e - w) - (me - mw)) < (me - mw) * 0.001, f"closing the table changed the scale: {(mw, me)} -> {(w, e)}"
            assert n - s > (mn - ms) * 1.3, "more of the map is in view with the table closed"
            print("map: closing the table keeps the middle of the view")

            assistant_and_the_map(mcp, ctx, page, port, root, routes, problems)

            assert not problems, problems
            browser.close()
        print("map e2e: ok")
    finally:
        mcp.close()


if __name__ == "__main__":
    main()
