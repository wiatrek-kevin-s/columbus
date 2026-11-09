"""Web map layers in the main window, end to end on Linux through
WebDriver (T-25, REQ-31, REQ-33; the sources.py pattern).

On Connections › Map layers the person pastes one fake service of each
kind (XYZ, WMTS whose tiles come from another host, WMS, a cached ArcGIS
MapServer, a dynamic one, an ImageServer); Columbus reads its capabilities
and offers its layers. With no `map.hosts`, saving asks the keyboard
confirmation (a debug build stands in for Touch ID / Windows Hello with
COLUMBUS_APP_TEST_CONFIRM): declined, nothing is saved; allowed, each is
saved and listed, and Test reports it reachable. The ArcGIS feature layer
is added from the map itself (Add web layer in a chat's map opens the
app's dialog, and the map lists it at once). After a restart the layers
are still listed, and offered in two different chats (the app's own chat
and a session another AI app started); an Edit and a Remove on the Map
layers tab then take effect in both.

The fake services are crates/gateway/examples/fake_oauth_server.rs,
reached over plain http to 127.0.0.1 (and test host names standing for
it) only because debug builds honour COLUMBUS_FETCH_TEST_LOOPBACK and
COLUMBUS_WEB_TEST_HOSTS. The chat's AI is a local stand-in.

    cargo build -p columbus-mcp --example fake_oauth_server
    (cd app/ui && npm run build)
    dbus-run-session -- xvfb-run -a python app/desktop/e2e/weblayers.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC

import guard
from guard import step

WebDriverWait = guard.Wait  # its time-outs name the step that was waiting

REPO = Path(__file__).resolve().parents[3]
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
FAKE = GATEWAY.parent / "examples" / "fake_oauth_server"
OUT = Path(__file__).resolve().parent / "out"
TEST_HOSTS = "imagery.fake.test,cdn.fake.test"
NOTICE = "Web layers' services see which area you're viewing"


class Stand(BaseHTTPRequestHandler):
    """The chat's AI: answers every message with a line of text."""

    def log_message(self, *a):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        data = json.dumps({"content": [{"type": "text", "text": "Ready when you are."}], "stop_reason": "end_turn"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def fake_services():
    proc = subprocess.Popen([str(FAKE)], env=dict(os.environ, FAKE_WEB_FEATURES="12000"),
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    first = queue.Queue()
    threading.Thread(target=lambda: first.put(proc.stdout.readline()), daemon=True).start()
    try:
        line = first.get(timeout=30).strip()
    except queue.Empty:
        raise SystemExit("the fake web services did not say where they listen within 30 s")
    assert line.startswith("listening "), line
    return proc, line.split(" ", 1)[1]


def files(root, stand_port):
    return dict(os.environ, COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
                COLUMBUS_MAP_SOURCES=str(root / "map-sources"),
                COLUMBUS_LIVE_DIR=str(root / "live"), XDG_CONFIG_HOME=str(root / "config"),
                XDG_DATA_HOME=str(root / "data"),
                COLUMBUS_TOOLS=str(root / "tools"), COLUMBUS_WORKSPACES=str(root / "ws"),
                COLUMBUS_AUDIT_DIR=str(root / "audit"), COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"),
                COLUMBUS_PYTHON=sys.executable, COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn",
                COLUMBUS_DEV_SECRETS="1", COLUMBUS_DEV_SECRETS_DIR=str(root / "secrets"),
                COLUMBUS_FETCH_TEST_LOOPBACK="1", COLUMBUS_WEB_TEST_HOSTS=TEST_HOSTS,
                COLUMBUS_SERVER=str(GATEWAY), COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"),
                COLUMBUS_APP_API_KEY="sk-ant-test-0123456789abcdef",
                COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{stand_port}")


def columbus(env, root):
    """A Columbus session on the same files, as Claude desktop would run it."""
    before = set((root / "live").glob("*.json")) if (root / "live").exists() else set()
    proc = subprocess.Popen([str(GATEWAY), "serve"], env=env, stdin=subprocess.PIPE,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(400):
        new = [p for p in (root / "live").glob("*.json") if p not in before] if (root / "live").exists() else []
        if new and new[0].read_text().strip():
            return proc, new[0].stem
        time.sleep(0.05)
    raise SystemExit("the Columbus session did not announce itself")


def nav(d, wait, where, ready):
    d.execute_script("location.hash = arguments[0]", where)
    wait.until(EC.presence_of_element_located((By.ID, ready)))
    time.sleep(0.3)


def js_click(d, el):
    d.execute_script("arguments[0].scrollIntoView({block: 'center'}); arguments[0].click();", el)


def fill(d, name, value):
    d.execute_script("const e = document.querySelector(`[name='${arguments[0]}']`); e.value = arguments[1];"
                     "e.dispatchEvent(new Event('input')); e.dispatchEvent(new Event('change'));", name, value)


def choose(d, name, value):
    d.execute_script("const s = document.querySelector(`[name='${arguments[0]}']`); s.value = arguments[1];"
                     "s.dispatchEvent(new Event('change'));", name, value)


def dialog_open(d):
    return d.execute_script("const x = document.getElementById('wl-editor'); return !!(x && x.open);")


def fill_layer(d, wait, url, layer, title, sid):
    """In the open Add web layer dialog: paste, read, choose, name."""
    wait.until(lambda drv: dialog_open(drv))
    fill(d, "wl_url", url)
    js_click(d, d.find_element(By.ID, "wl-read"))
    wait.until(lambda drv: drv.execute_script(
        "const f = document.getElementById('wl-found'), e = document.getElementById('wl-error');"
        "return (!f.hidden && f.textContent) || (!e.hidden && e.textContent);"))
    err = d.execute_script("const e = document.getElementById('wl-error'); return e.hidden ? '' : e.textContent")
    assert not err, f"{url}: {err}"
    offered = d.execute_script("return [...document.querySelector('[name=wl_layer]').options].map(o => o.value)")
    if layer is not None:
        assert layer in offered, f"{url}: {layer!r} not offered in {offered}"
        choose(d, "wl_layer", layer)
    fill(d, "wl_title", title)
    fill(d, "wl_id", sid)
    return d.execute_script("return document.getElementById('wl-found').textContent + ' | ' + document.getElementById('wl-hint').textContent")


def save_dialog(d, wait):
    js_click(d, d.find_element(By.ID, "wl-save"))
    wait.until(lambda drv: not dialog_open(drv) or drv.execute_script(
        "const e = document.getElementById('wl-error'); return !e.hidden && e.textContent"))
    return d.execute_script("const e = document.getElementById('wl-error'); return e.hidden ? '' : e.textContent")


def listed(d):
    return d.execute_script("return [...document.querySelectorAll('#web-layers > li')].map(li => [li.dataset.id, li.querySelector('b').textContent])")


def row_button(d, sid, label):
    return d.find_element(By.XPATH, f"//ul[@id='web-layers']/li[@data-id='{sid}']//button[normalize-space()='{label}']")


def visible_frame(d, wait):
    """The map frame of the chat on screen."""
    def find(drv):
        for f in drv.find_elements(By.CSS_SELECTOR, ".outpanel iframe"):
            if drv.execute_script("return arguments[0].offsetParent !== null && arguments[0].getBoundingClientRect().width > 0", f):
                return f
        return False
    return wait.until(find)


def in_map(d, wait, fn):
    """Run `fn(d)` inside the visible chat's map frame, its Web layers open."""
    frame = visible_frame(d, wait)
    d.switch_to.frame(frame)
    try:
        wait.until(lambda drv: drv.execute_script(
            "return [...document.querySelectorAll('button')].some(b => b.textContent.startsWith('Web layers'))"))
        if not d.execute_script("return !!document.getElementById('web-panel')"):
            d.execute_script("[...document.querySelectorAll('button')].find(b => b.textContent.startsWith('Web layers')).click()")
            wait.until(lambda drv: drv.execute_script("return !!document.getElementById('web-panel')"))
        return fn(d)
    finally:
        d.switch_to.default_content()


def map_rows(d):
    return d.execute_script("return [...document.querySelectorAll('.web-row')].map(r => [r.dataset.id, r.querySelector('.web-title').textContent])")


def wait_rows(d, wait, want):
    """Inside a frame: until the map's list is `want` (id -> title)."""
    try:
        wait.until(lambda drv: dict(map_rows(drv)) == want)
    except Exception:
        raise AssertionError(f"the map lists {dict(map_rows(d))}, not {want}")
    return True


def start_chat(d, wait):
    """The app's own chat, with its Columbus session started."""
    d.execute_script("location.hash = '#/chats/fresh'")
    box = wait.until(EC.visibility_of_element_located((By.ID, "text")))
    box.send_keys("Show me the map.")
    js_click(d, d.find_element(By.ID, "send"))
    WebDriverWait(d, 90).until(lambda drv: "Ready when you are." in drv.find_element(By.ID, "log").text)
    WebDriverWait(d, 40).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#chatview .outpanel iframe")))
    return d.execute_script("return location.hash")


def main():
    guard.start("desktop web layers e2e", minutes=12)
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    for sub in ["connections", "secrets", "live"]:
        (root / sub).mkdir()
    # No `map` section: no map.hosts, so saving a layer is the person's
    # approval of its hosts, confirmed at the keyboard.
    (root / "policy.yaml").write_text("connection_types: [gpkg]\n")
    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()
    step("the fake web services")
    fake, base = fake_services()
    port = urlparse(base).port
    env = files(root, stand.server_port)
    svc = lambda p: f"http://127.0.0.1:{port}/web/arcgis/rest/services/{p}"  # noqa: E731
    kinds = [
        ("tiles", "Fake tiles", f"http://127.0.0.1:{port}/web/xyz/{{z}}/{{x}}/{{y}}.png", None),
        ("ortho", "Fake orthophoto", f"http://imagery.fake.test:{port}/web/wmts/cdn/WMTSCapabilities.xml", "ortho"),
        ("parcels-wms", "Fake parcels WMS", f"http://127.0.0.1:{port}/web/wms?service=WMS&request=GetCapabilities", "parcels"),
        ("imagery", "Fake imagery", svc("Imagery/MapServer"), ""),
        ("roads", "Fake roads", svc("Dynamic/MapServer"), "0"),
        ("elevation", "Fake elevation", svc("Elevation/ImageServer"), None),
    ]
    features = ("parcels", "Fake parcels", svc("Parcels/FeatureServer"), "0")
    saved = root / "map-sources"
    procs = []

    try:
        step("declined at the keyboard: nothing is saved")
        d, proc = guard.driver(dict(env, COLUMBUS_APP_TEST_CONFIRM="deny"), APP)
        try:
            wait = WebDriverWait(d, 60)
            wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))
            nav(d, wait, "#/connections/maplayers", "add-web-layer")
            js_click(d, d.find_element(By.ID, "add-web-layer"))
            said = fill_layer(d, wait, kinds[0][2], kinds[0][3], kinds[0][1], kinds[0][0])
            assert "confirm" in said, said
            err = save_dialog(d, wait)
            assert "not confirmed" in err, err
            assert not list(saved.glob("*.yaml")) if saved.exists() else True, "nothing is saved"
            d.save_screenshot(str(OUT / "weblayers-declined.png"))
        finally:
            guard.stop(d, proc)  # the app has gone: the next launch is a restart

        step("allowed: one service of each kind, pasted on Connections › Map layers")
        d, proc = guard.driver(dict(env, COLUMBUS_APP_TEST_CONFIRM="allow"), APP)
        try:
            wait = WebDriverWait(d, 60)
            wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))
            nav(d, wait, "#/connections/maplayers", "add-web-layer")
            for sid, title, url, layer in kinds:
                step(f"add {sid}")
                js_click(d, d.find_element(By.ID, "add-web-layer"))
                said = fill_layer(d, wait, url, layer, title, sid)
                if sid == "ortho":
                    assert "cdn.fake.test" in d.execute_script(
                        "return [...document.querySelector('[name=wl_layer]').options].map(o => o.textContent).join(' ')"), \
                        "the WMTS's tile host is named before saving"
                err = save_dialog(d, wait)
                assert not err, f"{sid}: {err}"
                wait.until(lambda drv: (saved / f"{sid}.yaml").exists())
                wait.until(lambda drv: sid in dict(listed(drv)))
            hosts = json.loads((saved / ".hosts.json").read_text())
            assert hosts["ortho"]["hosts"] == ["cdn.fake.test"], hosts
            assert dict(listed(d)) == {k[0]: k[1] for k in kinds}, listed(d)
            d.save_screenshot(str(OUT / "weblayers-tab.png"))

            step("Test says each is reachable")
            for sid, *_ in kinds:
                js_click(d, row_button(d, sid, "Test"))
                st = f"//ul[@id='web-layers']/li[@data-id='{sid}']//span[contains(@class,'wl-status')]"
                WebDriverWait(d, 60).until(lambda drv: drv.find_element(By.XPATH, st).get_attribute("textContent"))
                got = d.find_element(By.XPATH, st).get_attribute("textContent")
                assert got.startswith("Reachable"), f"{sid}: {got}"

            step("Add web layer on a chat's map opens the app's dialog")
            chat_hash = start_chat(d, wait)

            def add_from_map(drv):
                wait_rows(drv, wait, {k[0]: k[1] for k in kinds})
                drv.execute_script("[...document.querySelectorAll('#web-panel button')].find(b => b.textContent === 'Add web layer').click()")
            in_map(d, wait, add_from_map)
            fill_layer(d, wait, features[2], features[3], features[1], features[0])
            err = save_dialog(d, wait)
            assert not err, err
            want = {k[0]: k[1] for k in kinds + [features]}

            def turn_on(drv):
                wait_rows(drv, wait, want)
                # Each starts off; the first turned on shows the notice.
                assert not drv.execute_script("return [...document.querySelectorAll('.web-row input[type=checkbox]')].some(c => c.checked)")
                drv.execute_script("document.querySelector(\".web-row[data-id='parcels'] input[type=checkbox]\").click()")
                wait.until(lambda x: NOTICE in (x.execute_script("const n = document.querySelector('.web-notice'); return n ? n.textContent : ''")))
            in_map(d, wait, turn_on)
            d.save_screenshot(str(OUT / "weblayers-map.png"))
        finally:
            guard.stop(d, proc)  # the app has gone: the next launch is a restart

        step("after a restart: listed, and offered in two different chats")
        other, other_session = columbus(env, root)
        procs.append(other)
        d, proc = guard.driver(dict(env, COLUMBUS_APP_TEST_CONFIRM="allow"), APP)
        try:
            wait = WebDriverWait(d, 60)
            wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))
            nav(d, wait, "#/connections/maplayers", "add-web-layer")
            wait.until(lambda drv: dict(listed(drv)) == want)
            chat_hash = start_chat(d, wait)
            in_map(d, wait, lambda drv: wait_rows(drv, wait, want))
            session_hash = f"#/chats/s/{other_session}"
            d.execute_script("location.hash = arguments[0]", session_hash)
            in_map(d, wait, lambda drv: wait_rows(drv, wait, want))
            d.save_screenshot(str(OUT / "weblayers-other-chat.png"))

            step("Edit on the Map layers tab: both chats show it")
            nav(d, wait, "#/connections/maplayers", "add-web-layer")
            wait.until(lambda drv: dict(listed(drv)) == want)
            js_click(d, row_button(d, "tiles", "Edit"))
            wait.until(lambda drv: dialog_open(drv) and drv.execute_script(
                "return !document.getElementById('wl-found').hidden"))
            assert d.execute_script("return document.querySelector('[name=wl_id]').readOnly")
            # A new title, and tiles from another host: confirmed again.
            fill(d, "wl_url", f"http://imagery.fake.test:{port}/web/xyz/{{z}}/{{x}}/{{y}}.png")
            fill(d, "wl_title", "Fake tiles, edited")
            js_click(d, d.find_element(By.ID, "wl-read"))
            wait.until(lambda drv: "imagery.fake.test" in drv.execute_script("return document.getElementById('wl-found').textContent"))
            err = save_dialog(d, wait)
            assert not err, err
            wait.until(lambda drv: dict(listed(drv)).get("tiles") == "Fake tiles, edited")
            assert "imagery.fake.test" in (saved / "tiles.yaml").read_text()
            want["tiles"] = "Fake tiles, edited"
            js_click(d, row_button(d, "tiles", "Test"))
            WebDriverWait(d, 60).until(lambda drv: drv.find_element(
                By.XPATH, "//ul[@id='web-layers']/li[@data-id='tiles']//span[contains(@class,'wl-status')]").get_attribute("textContent").startswith("Reachable"))
            d.execute_script("location.hash = arguments[0]", chat_hash)
            in_map(d, wait, lambda drv: wait_rows(drv, wait, want))
            d.execute_script("location.hash = arguments[0]", session_hash)
            in_map(d, wait, lambda drv: wait_rows(drv, wait, want))

            step("Remove on the Map layers tab: gone from both chats")
            nav(d, wait, "#/connections/maplayers", "add-web-layer")
            wait.until(lambda drv: "tiles" in dict(listed(drv)))
            js_click(d, row_button(d, "tiles", "Remove"))
            wait.until(lambda drv: "tiles" not in dict(listed(drv)))
            assert not (saved / "tiles.yaml").exists()
            del want["tiles"]
            d.execute_script("location.hash = arguments[0]", chat_hash)
            in_map(d, wait, lambda drv: wait_rows(drv, wait, want))
            d.execute_script("location.hash = arguments[0]", session_hash)
            in_map(d, wait, lambda drv: wait_rows(drv, wait, want))
            d.save_screenshot(str(OUT / "weblayers-removed.png"))
        finally:
            guard.stop(d, proc)  # the app has gone: the next launch is a restart
        print("desktop web layers e2e: ok")
    finally:
        for p in procs:
            p.terminate()
        fake.terminate()
        stand.shutdown()


if __name__ == "__main__":
    main()
