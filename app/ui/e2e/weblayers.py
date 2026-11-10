"""End-to-end test of web map layers on the companion window's map
(T-25, REQ-33, R-21, D-14) in a real browser, against a real session and
the fake web map services in crates/gateway/examples/fake_oauth_server.rs.

One fake service of each kind (XYZ, WMTS with its tiles on another host,
WMS, a cached ArcGIS MapServer, a dynamic one, an ImageServer) and an
ArcGIS feature layer of 12,000 points are added as the Columbus app adds
them (`map-source inspect`, then `save`, with no `map.hosts`), plus two
services that do not answer. In the window, each starts off; turning the
first on shows the privacy notice once; each draws through this server
only; opacity, order, zoom to extent and a web layer as the basemap work;
5,000 features draw with "Zoom in to see all"; the dead services show the
R-14 message and the map keeps working; a direct request to a service
host is blocked by the page's content policy; another window of the same
chat keeps every setting and shows no notice, and a second chat starts
with everything off. Screenshots go to e2e/out/.

    cargo build -p columbus-mcp --example fake_oauth_server
    python app/ui/e2e/weblayers.py [path/to/columbus-mcp]

Needs Playwright. `COLUMBUS_E2E_CHROMIUM` may name a Chromium or Chrome to
use instead of Playwright's own (for machines that cannot download it).
"""

import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path
from urllib.parse import urlparse

from playwright.sync_api import expect, sync_playwright

sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import Mcp, wait_file  # noqa: E402

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
OUT = HERE / "out"
NOTICE = "Web layers' services see which area you're viewing"
TEST_HOSTS = "imagery.fake.test,cdn.fake.test"


def fake_server(exe):
    env = dict(os.environ, FAKE_WEB_FEATURES="12000")
    proc = subprocess.Popen([str(exe)], env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    first = queue.Queue()
    threading.Thread(target=lambda: first.put(proc.stdout.readline()), daemon=True).start()
    try:
        line = first.get(timeout=30).strip()
    except queue.Empty:
        raise SystemExit("the fake web services did not say where they listen within 30 s")
    assert line.startswith("listening "), line
    return proc, line.split(" ", 1)[1]


def fake_log(base):
    return json.loads(urllib.request.urlopen(f"{base}/test/log", timeout=10).read())


def open_link(port, root):
    launch = root / "launch.txt"
    if launch.exists():
        launch.unlink()
    req = urllib.request.Request(f"http://127.0.0.1:{port}/api/open", method="POST", headers={"X-Columbus-Open": "1"})
    assert urllib.request.urlopen(req).status == 200
    return wait_file(launch)


def cli(exe, env, *args, stdin=None):
    r = subprocess.run([str(exe), *args], env=env, input=stdin, capture_output=True, text=True, timeout=120)
    try:
        out = json.loads(r.stdout)
    except ValueError:
        raise AssertionError(f"{args}: not JSON: {r.stdout!r} {r.stderr!r}")
    return r.returncode == 0, out


def web_state(page):
    v = page.locator(".map").get_attribute("data-web")
    return json.loads(v) if v else []


def view(page):
    v = page.locator(".map").get_attribute("data-view")
    return tuple(float(x) for x in v.split(",")) if v else None


def settled(page, before=None, t=15):
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


def in_view(page):
    """The part of the map the page's panel does not cover, as west, south,
    east, north (the panel floats over the map's left side)."""
    w, s, e, n = view(page)
    r = page.locator(".map canvas").bounding_box()
    panel = page.locator(".panel").bounding_box()
    assert panel, "the page's own panel is there"
    left = panel["x"] + panel["width"]
    assert r["x"] + r["width"] - left > 300, f"too little of the map is in view: {panel} over {r}"
    return w + (left - r["x"]) / r["width"] * (e - w), s, e, n


def row(page, sid):
    return page.locator(f".web-row[data-id='{sid}']")


def main():
    exe = Path(sys.argv[1] if len(sys.argv) > 1 else REPO / "target/debug/columbus-mcp").resolve()
    fake_exe = exe.parent / "examples" / "fake_oauth_server"
    ui = REPO / "app/ui/dist"
    assert (ui / "index.html").exists(), "build the UI first: npm run build"
    assert fake_exe.exists(), "build the fake services first: cargo build -p columbus-mcp --example fake_oauth_server"
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    (root / "connections").mkdir()
    # No `map` section: no map.hosts, so saving a layer is the person's
    # approval of its host (the app confirms it at the keyboard first).
    (root / "policy.yaml").write_text("connection_types: [gpkg]\n")
    fake, base = fake_server(fake_exe)
    port = urlparse(base).port
    env = dict(os.environ,
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_MAP_SOURCES=str(root / "map-sources"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_UI_DIR=str(ui),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_LAUNCH_FILE=str(root / "launch.txt"), COLUMBUS_LOG="warn",
               COLUMBUS_DEV_SECRETS="1", COLUMBUS_DEV_SECRETS_DIR=str(root / "secrets"),
               COLUMBUS_FETCH_TEST_LOOPBACK="1", COLUMBUS_WEB_TEST_HOSTS=TEST_HOSTS)
    svc = lambda p: f"http://127.0.0.1:{port}/web/arcgis/rest/services/{p}"  # noqa: E731
    # (id, title, pasted address, layer chosen from those offered)
    kinds = [
        ("tiles", "Fake tiles", f"http://127.0.0.1:{port}/web/xyz/{{z}}/{{x}}/{{y}}.png", None),
        ("ortho", "Fake orthophoto", f"http://imagery.fake.test:{port}/web/wmts/cdn/WMTSCapabilities.xml", "ortho"),
        ("parcels-wms", "Fake parcels WMS", f"http://127.0.0.1:{port}/web/wms?service=WMS&request=GetCapabilities", "parcels"),
        # All layers of a cached map service: its tiles.
        ("imagery", "Fake imagery", svc("Imagery/MapServer"), None),
        ("roads", "Fake roads", svc("Dynamic/MapServer"), "0"),
        ("elevation", "Fake elevation", svc("Elevation/ImageServer"), None),
        ("parcels", "Fake parcels", svc("Parcels/FeatureServer"), "0"),
    ]
    mcp = None
    mcp2 = None
    try:
        print("weblayers: adding one fake service of each kind")
        for sid, title, url, layer in kinds:
            ok, v = cli(exe, env, "map-source", "inspect", url)
            assert ok and v["ok"], v
            assert v["confirm"] is True, f"with no map.hosts the app must confirm: {v}"
            offered = [x["layer"] for x in v["layers"]]
            if layer is not None:
                assert layer in offered, f"{sid}: {layer} not offered in {offered}"
            entry = {"id": sid, "title": title, "kind": v["kind"], "url": url}
            if layer is not None:
                entry["layer"] = layer
            ok, v = cli(exe, env, "map-source", "save", "-", stdin=json.dumps(entry))
            assert ok and v["ok"], v
            if sid == "ortho":
                assert v["hosts"] == ["cdn.fake.test"], v
        # Two services that do not answer (nothing listens on port 9).
        (root / "map-sources/dead-tiles.yaml").write_text(
            "id: dead-tiles\ntitle: Dead tiles\nkind: xyz\nurl: \"http://127.0.0.1:9/web/xyz/{z}/{x}/{y}.png\"\n")
        (root / "map-sources/dead-map.yaml").write_text(
            "id: dead-map\ntitle: Dead map service\nkind: arcgis-map\n"
            "url: \"http://127.0.0.1:9/web/arcgis/rest/services/Imagery/MapServer\"\n")

        mcp = Mcp([str(exe), "serve"], env)
        session = mcp.tool("whoami", {})["result"]["session"]
        gw_port = json.loads(wait_file(root / "live" / f"{session}.json"))["port"]
        gw = f"http://127.0.0.1:{gw_port}"
        link = open_link(gw_port, root)

        with sync_playwright() as pw:
            browser = pw.chromium.launch(executable_path=os.environ.get("COLUMBUS_E2E_CHROMIUM") or None)
            ctx = browser.new_context(viewport={"width": 1360, "height": 860})
            # Every content-policy refusal the page meets.
            ctx.add_init_script(
                "window.__csp = []; document.addEventListener('securitypolicyviolation', e => "
                "window.__csp.push({uri: e.blockedURI, directive: e.violatedDirective}));")
            requests = []
            ctx.on("request", lambda r: requests.append(r.url))
            page = ctx.new_page()
            problems = []
            page.on("pageerror", lambda e: problems.append(str(e)))
            resp = page.goto(link)
            csp = resp.headers.get("content-security-policy", "")
            print("weblayers: page policy:", csp)

            # The content policy: this server, and the street basemap's host.
            for d in ("connect-src", "img-src"):
                m = re.search(rf"{d} ([^;]*)", csp)
                assert m, csp
                hosts = [h for h in m.group(1).split() if h not in ("'self'", "data:", "blob:")]
                assert hosts == ["https://tiles.openfreemap.org"], f"{d} reaches {hosts}"
            assert "default-src 'none'" in csp, csp

            page.get_by_role("button", name=re.compile(r"^Web layers")).click()
            panel = page.locator("#web-panel")
            expect(panel).to_be_visible()
            expect(page.locator(".web-row")).to_have_count(len(kinds) + 2, timeout=15000)
            # Each starts off.
            for sid, *_ in kinds:
                expect(row(page, sid).get_by_role("checkbox")).not_to_be_checked()
            expect(panel.locator(".web-notice")).to_have_count(0)

            print("weblayers: the first layer on shows the privacy notice")
            row(page, "tiles").get_by_role("checkbox").check()
            expect(panel.locator(".web-notice")).to_contain_text(NOTICE)
            panel.locator(".web-notice").get_by_role("button", name="OK").click()
            expect(panel.locator(".web-notice")).to_have_count(0)
            for sid, *_ in kinds[1:]:
                row(page, sid).get_by_role("checkbox").check()
                expect(panel.locator(".web-notice")).to_have_count(0)
            print("weblayers: each kind draws")
            for sid, *_ in kinds:
                expect(row(page, sid)).to_have_attribute("data-drawn", "1", timeout=30000)
                expect(row(page, sid).locator(".error")).to_have_count(0)
            held = [x["id"] for x in web_state(page)]
            assert set(held) == {k[0] for k in kinds}, held
            # Every tile and feature came through this server; the services
            # saw the gateway asking (WMTS tiles on its own tile host).
            log = fake_log(base)
            for marker in ["/web/xyz/", "/web/wmts/tile/", "REQUEST=GetMap", "/Imagery/MapServer/tile/",
                           "/Dynamic/MapServer/export", "/Elevation/ImageServer/exportImage", "/Parcels/FeatureServer/0/query"]:
                assert any(marker.lower() in e["target"].lower() for e in log), f"no request for {marker}"
            assert any((e["host"] or "").startswith("cdn.fake.test") for e in log), "WMTS tiles on their own host"
            page.screenshot(path=str(OUT / "web-1-all-on.png"))

            print("weblayers: 12,000 features in view: 5,000 drawn, zoom in to see all")
            note = row(page, "parcels").locator(".note")
            expect(note).to_contain_text("Zoom in to see all", timeout=30000)
            expect(note).to_contain_text("5,000 of 12,000")

            print("weblayers: opacity")
            row(page, "tiles").get_by_label("Opacity of Fake tiles").fill("40")
            page.wait_for_function("JSON.parse(document.querySelector('.map').dataset.web || '[]')"
                                   ".some(x => x.id === 'tiles' && Math.abs(x.opacity - 0.4) < 1e-6)", timeout=10000)

            print("weblayers: order")
            before = [x["id"] for x in web_state(page) if not x["base"]]
            # The list is top first; the map holds bottom first.
            listed = page.locator(".web-row").evaluate_all("rs => rs.map(r => r.dataset.id)")
            on = {k[0] for k in kinds}
            assert before == [x for x in reversed(listed) if x in on], (before, listed)
            # Move the bottom one up: it now draws above the one it passed.
            last, above = [x for x in listed if x in on][-1], [x for x in listed if x in on][-2]
            title = [k[1] for k in kinds if k[0] == last][0]
            row(page, last).get_by_role("button", name=f"Move {title} up").click()
            page.wait_for_function(
                "([a, b]) => { const ids = JSON.parse(document.querySelector('.map').dataset.web || '[]').map(x => x.id);"
                " return ids.indexOf(a) > ids.indexOf(b); }", arg=[last, above], timeout=10000)
            order_after = [x["id"] for x in web_state(page) if not x["base"]]
            assert order_after != before, order_after

            print("weblayers: a web layer as the basemap")
            panel.get_by_label("Basemap").select_option("web:imagery")
            page.wait_for_function("JSON.parse(document.querySelector('.map').dataset.web || '[]')"
                                   ".some(x => x.id === 'imagery' && x.base)", timeout=10000)
            st = web_state(page)
            assert st[0] == {"id": "imagery", "base": True, "opacity": 1}, st
            assert not any(x["id"] == "imagery" and not x["base"] for x in st), st

            print("weblayers: zoom to a layer's extent")
            v0 = view(page)
            row(page, "parcels").get_by_role("button", name="Zoom to layer").click()
            v = settled(page, before=v0)
            assert v[0] <= -97.8 + 0.01 and v[2] >= -97.6 - 0.01 and v[1] <= 30.2 + 0.01 and v[3] >= 30.4 - 0.01, v
            assert v[2] - v[0] < 1.0, f"not fitted to the parcels: {v}"
            # And in the part of the map the panel leaves in view, in its
            # middle.
            vw, vs, ve, vn = in_view(page)
            assert vw <= -97.8 + 0.01 and ve >= -97.6 - 0.01, f"the parcels are not all in view {(vw, vs, ve, vn)}"
            assert abs((vw + ve) / 2 - -97.7) < (ve - vw) * 0.05, f"the parcels are not centred in view {(vw, vs, ve, vn)}"
            # Zoomed in further, everything in view draws.
            page.get_by_role("button", name="Zoom in").click()
            settled(page, before=v)
            page.get_by_role("button", name="Zoom in").click()
            settled(page)
            expect(note).to_have_count(0, timeout=15000)

            print("weblayers: services that do not answer")
            for sid in ("dead-tiles", "dead-map"):
                row(page, sid).get_by_role("checkbox").check()
                expect(row(page, sid).locator(".error")).to_contain_text(
                    "Web layer service at 127.0.0.1 could not be reached. Check the address or your network, then Test again.",
                    timeout=40000)
            # The map keeps working: it still moves, and the others still draw.
            v1 = view(page)
            page.get_by_role("button", name="Zoom out").click()
            settled(page, before=v1)
            for sid, *_ in kinds:
                expect(row(page, sid).locator(".error")).to_have_count(0)
            page.screenshot(path=str(OUT / "web-2-settings.png"))

            print("weblayers: a direct request to a service host is blocked")
            direct = f"http://127.0.0.1:{port}/web/xyz/1/1/1.png?direct=1"
            got = page.evaluate("u => fetch(u).then(() => 'fetched', e => 'blocked: ' + e)", direct)
            assert got.startswith("blocked"), got
            page.evaluate("u => { const i = new Image(); i.src = u; document.body.append(i); }", direct + "&img=1")
            page.wait_for_timeout(1000)
            csp_hits = page.evaluate("window.__csp")
            assert any("127.0.0.1:" + str(port) in (x["uri"] or "") for x in csp_hits), csp_hits
            assert not any("direct=1" in e["target"] for e in fake_log(base)), "a direct request reached the service"
            # Everything the page asked for: this server, or the basemap host.
            for u in requests:
                o = urlparse(u)
                if o.scheme in ("data", "blob"):
                    continue
                origin = f"{o.scheme}://{o.netloc}"
                assert origin in (gw, "https://tiles.openfreemap.org") or "direct=1" in u, f"the page reached {u}"
            # Web-layer requests carried no address and the token in a header only.
            assert not any("/api/web/" in u and ("url=" in u or "t=" in u.split("?")[-1]) for u in requests), requests

            print("weblayers: another window of the chat keeps every setting")
            settings_before = web_state(page)
            page2 = ctx.new_page()
            page2.on("pageerror", lambda e: problems.append(str(e)))
            page2.goto(open_link(gw_port, root))
            page2.get_by_role("button", name=re.compile(r"^Web layers")).click()
            expect(page2.locator(".web-row")).to_have_count(len(kinds) + 2, timeout=15000)
            for sid, *_ in kinds:
                expect(row(page2, sid).get_by_role("checkbox")).to_be_checked()
            expect(page2.locator("#web-panel").get_by_label("Basemap")).to_have_value("web:imagery")
            expect(row(page2, "tiles").get_by_label("Opacity of Fake tiles")).to_have_value("40")
            page2.wait_for_function(
                "n => (JSON.parse(document.querySelector('.map').dataset.web || '[]')).length >= n",
                arg=len(kinds), timeout=20000)
            after = web_state(page2)
            # The same order (the basemap one now below them all; the dead
            # services drawn as they were).
            kept = [x["id"] for x in after if not x["base"] and not x["id"].startswith("dead-")]
            assert kept == [x for x in order_after if x != "imagery"], (after, order_after)
            assert after == settings_before, (after, settings_before)
            assert after[0]["base"] and after[0]["id"] == "imagery", after
            assert [x for x in after if x["id"] == "tiles"][0]["opacity"] == 0.4, after
            for sid, *_ in kinds:
                expect(row(page2, sid)).to_have_attribute("data-drawn", "1", timeout=30000)
            # The notice was shown in this chat: not again.
            row(page2, "dead-tiles").get_by_role("checkbox").uncheck()
            row(page2, "dead-tiles").get_by_role("checkbox").check()
            page2.wait_for_timeout(500)
            expect(page2.locator(".web-notice")).to_have_count(0)
            page2.screenshot(path=str(OUT / "web-3-reloaded.png"))
            page2.close()

            print("weblayers: another chat starts with every web layer off, and its own notice")
            mcp2 = Mcp([str(exe), "serve"], env)
            s2 = mcp2.tool("whoami", {})["result"]["session"]
            p2 = json.loads(wait_file(root / "live" / f"{s2}.json"))["port"]
            page3 = ctx.new_page()
            page3.on("pageerror", lambda e: problems.append(str(e)))
            page3.goto(open_link(p2, root))
            page3.get_by_role("button", name=re.compile(r"^Web layers")).click()
            expect(page3.locator(".web-row")).to_have_count(len(kinds) + 2, timeout=15000)
            for sid, *_ in kinds:
                expect(row(page3, sid).get_by_role("checkbox")).not_to_be_checked()
            row(page3, "ortho").get_by_role("checkbox").check()
            expect(page3.locator(".web-notice")).to_contain_text(NOTICE)
            expect(row(page3, "ortho")).to_have_attribute("data-drawn", "1", timeout=30000)
            page3.close()

            # The assistant was told titles only.
            w = mcp.tool("whoami", {})["result"]
            assert sorted(w["web_layers"]) == sorted([k[1] for k in kinds] + ["Dead tiles", "Dead map service"]), w["web_layers"]
            assert "fake.test" not in json.dumps(w) and "/web/" not in json.dumps(w)

            assert not problems, problems
            browser.close()
        print("weblayers e2e: ok")
    finally:
        for m in (mcp, mcp2):
            if m:
                m.close()
        fake.terminate()


if __name__ == "__main__":
    main()
