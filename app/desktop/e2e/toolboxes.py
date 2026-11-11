"""Tools › Toolboxes in the main window, end to end on Linux through
WebDriver (tauri-driver and WebKitWebDriver, under xvfb and a D-Bus
session): build brief 3, T-29.

A toolbox is installed from Columbus's catalogue, its tools are searched
and read, one is run from a chat, and it is removed. The catalogue here is
a test build's own (COLUMBUS_TOOLBOX_TEST_CATALOGUE, which a release build
does not have): one entry whose download is an archive of the stand-in
runner (crates/gateway/examples/toolbox_standin.rs), served by this script
on this computer. A stand-in for the AI service plays the assistant.

Two sessions of the app:

1. Toolboxes turned off (the policy's default): the section says so, says
   how to turn them on, and offers nothing. Turned on, on a system that
   cannot confirm who is at the keyboard (Linux): Add tools shows what
   would be installed and the terminal's command, and no Install button.
2. With the tests' confirmation: Add tools shows the name, version, size,
   licence, publisher and address before anything is downloaded; Install
   downloads, checks, unpacks and lists, and says how many tools are ready;
   the tools are found by words and one opens in the panel on the right;
   the assistant finds one and runs it without being asked to approve, and
   its result is a layer on the chat's map; Remove takes the toolbox away,
   and the same chat's assistant is no longer offered its tools.

    cargo build -p columbus-mcp --example toolbox_standin
    dbus-run-session -- xvfb-run -a python app/desktop/e2e/toolboxes.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import hashlib
import io
import json
import os
import platform
import re
import sys
import tarfile
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support import expected_conditions as EC

import guard
from guard import step

WebDriverWait = guard.Wait  # its time-outs name the step that was waiting

REPO = Path(__file__).resolve().parents[3]
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
STANDIN = GATEWAY.parent / "examples" / "toolbox_standin"
OUT = Path(__file__).resolve().parent / "out"
TOOL = "geolibre.vector_copy"


# ---------- the download ----------

def archive():
    """The stand-in as its publisher would pack it: a folder with the
    program, a licence and the notices."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz", format=tarfile.USTAR_FORMAT) as tar:
        for name, data, mode in [("standin-1.8.1/geolibre", STANDIN.read_bytes(), 0o755),
                                 ("standin-1.8.1/LICENSE", b"MIT License (a stand-in for the tests)\n", 0o644),
                                 ("standin-1.8.1/THIRD-PARTY-NOTICES.txt", b"None.\n", 0o644)]:
            info = tarfile.TarInfo(name)
            info.size, info.mode, info.mtime = len(data), mode, 1_760_000_000
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


ARCHIVE = b""
HITS = []


class Downloads(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        HITS.append(self.path)
        if self.path != "/standin.tar.gz":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/gzip")
        self.send_header("Content-Length", str(len(ARCHIVE)))
        self.end_headers()
        self.wfile.write(ARCHIVE)


def target():
    arch = {"x86_64": "x86_64", "AMD64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}[platform.machine()]
    return f"{arch}-unknown-linux-gnu"


# ---------- the assistant ----------

def tool_use(i, name, args):
    return {"content": [{"type": "text", "text": f"I'll use {name}."},
                        {"type": "tool_use", "id": i, "name": name, "input": args}],
            "stop_reason": "tool_use"}


FIND = {"query": "copy the features of a vector layer"}
SCRIPT = [
    # First message: find a toolbox tool, read the tracts, run the tool on them.
    tool_use("t1", "search_tools", FIND),
    tool_use("t2", "query_features", {"connection": "tiger", "dataset": "tracts"}),
    "run_on_last",
    {"content": [{"type": "text", "text": "Copied the tracts with a toolbox tool."}], "stop_reason": "end_turn"},
    # Second message, once the toolbox is removed: look again, and try it.
    tool_use("t4", "search_tools", FIND),
    "run_again",
    {"content": [{"type": "text", "text": "That toolbox is no longer there."}], "stop_reason": "end_turn"},
]
SEEN = []
HANDLE = []


class Stand(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        SEEN.append(body)
        nxt = SCRIPT.pop(0) if SCRIPT else {"content": [{"type": "text", "text": "(end)"}], "stop_reason": "end_turn"}
        if nxt == "run_on_last":
            HANDLE.append(json.loads(last_result(body))["result"]["layer"]["handle"])
            nxt = tool_use("t3", "run_tool", {"name": TOOL, "args": {"input": HANDLE[0]}})
        elif nxt == "run_again":
            nxt = tool_use("t5", "run_tool", {"name": TOOL, "args": {"input": HANDLE[0]}})
        data = json.dumps(nxt).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def last_result(body):
    """The text of the tool result a request carries back to the assistant."""
    block = body["messages"][-1]["content"][0]
    assert block["type"] == "tool_result", block
    c = block["content"]
    return c if isinstance(c, str) else "".join(p.get("text", "") for p in c)


def result_of(tool_id):
    """What the assistant was told a tool call returned."""
    for body in SEEN:
        for m in body["messages"]:
            if m["role"] != "user" or isinstance(m["content"], str):
                continue
            for b in m["content"]:
                if b.get("type") == "tool_result" and b.get("tool_use_id") == tool_id:
                    c = b["content"]
                    return (c if isinstance(c, str) else "".join(p.get("text", "") for p in c)), bool(b.get("is_error"))
    raise SystemExit(f"the assistant was never told what {tool_id} returned")


# ---------- the window ----------

def nav(d, wait, where, ready):
    d.execute_script("location.hash = arguments[0]", where)
    wait.until(EC.presence_of_element_located((By.ID, ready)))
    time.sleep(0.3)


def text(d, css):
    """An element's words (textContent: WebKit's driver leaves clipped text out)."""
    return d.execute_script("const e = document.querySelector(arguments[0]); return e ? e.textContent : ''", css)


def shown(d, css):
    return d.execute_script(
        "const e = document.querySelector(arguments[0]); if (!e) return false;"
        "const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0 && getComputedStyle(e).visibility !== 'hidden';", css)


def named(d):
    """Every control on show has a name a screen reader can say, and Tab reaches it."""
    bad = guard.unnamed(d)
    assert not bad, bad


def send(d, words):
    box = WebDriverWait(d, 20).until(EC.visibility_of_element_located((By.ID, "text")))
    box.send_keys(words)
    WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
    d.find_element(By.ID, "send").click()


def main():
    global ARCHIVE
    guard.start("desktop toolboxes e2e", minutes=10)
    OUT.mkdir(exist_ok=True)
    if not STANDIN.exists():
        raise SystemExit(f"{STANDIN} is missing: cargo build -p columbus-mcp --example toolbox_standin")
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box as square

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[square(0, 0, 1, 1), square(1, 0, 2, 1)], crs="EPSG:3081") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")
    policy = root / "policy.yaml"
    # Toolboxes are off until the policy says otherwise.
    policy.write_text("connection_types: [gpkg]\n")

    ARCHIVE = archive()
    downloads = ThreadingHTTPServer(("127.0.0.1", 0), Downloads)
    threading.Thread(target=downloads.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{downloads.server_port}/standin.tar.gz"
    size = f"{len(ARCHIVE) / 1024 / 1024:.1f} MB" if len(ARCHIVE) >= 1024 * 1024 else f"{-(-len(ARCHIVE) // 1024)} KB"
    catalogue = root / "catalogue.json"
    catalogue.write_text(json.dumps([{
        "id": "geolibre", "name": "Stand-in GeoLibre", "about": "A few stand-in tools, for the tests.",
        "kind": "geolibre", "version": "1.8.1", "licence": "MIT", "publisher": "the tests",
        "homepage": "https://example.org/standin",
        "downloads": [{"target": target(), "packing": "tar_gz", "url": url,
                       "sha256": hashlib.sha256(ARCHIVE).hexdigest(), "bytes": len(ARCHIVE),
                       "program": "standin-1.8.1/geolibre",
                       "keep": ["standin-1.8.1/LICENSE", "standin-1.8.1/THIRD-PARTY-NOTICES.txt"]}]}]))

    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()

    env = dict(os.environ,
               COLUMBUS_APP_API_KEY="sk-ant-test-0123456789abcdef",
               COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{stand.server_port}",
               COLUMBUS_SERVER=str(GATEWAY),
               COLUMBUS_POLICY=str(policy), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_TOOLS=str(root / "tools"),
               COLUMBUS_TOOLBOXES=str(root / "toolboxes"), COLUMBUS_TOOLBOX_TEST_CATALOGUE=str(catalogue),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"))
    env.pop("COLUMBUS_APP_TEST_CONFIRM", None)

    # ---------- 1. turned off; and a system that cannot confirm ----------
    step("the app, with toolboxes turned off")
    d, driver_proc = guard.driver(env, APP)
    try:
        wait = WebDriverWait(d, 30)
        guard.ready(d, wait)
        nav(d, wait, "#/tools", "toolbox-card")
        wait.until(lambda drv: shown(drv, "#tbx-off"))
        off = text(d, "#tbx-off")
        assert "Toolboxes are turned off" in off and "Work freely on copies" in off and "allow_toolboxes" in off, off
        step("Turned off, nothing is offered: no Add tools, no search, no tools")
        assert not shown(d, "#tbx-add") and not shown(d, "#tbx-find"), "nothing is offered while toolboxes are off"
        assert not d.find_elements(By.CSS_SELECTOR, "#tbx-tools li")
        named(d)
        d.save_screenshot(str(OUT / "toolboxes-off.png"))

        step("Turned on: Add tools is offered")
        policy.write_text("connection_types: [gpkg]\nallow_toolboxes: true\n")
        nav(d, wait, "#/workflows", "page-title")
        nav(d, wait, "#/tools", "toolbox-card")
        wait.until(lambda drv: shown(drv, "#tbx-add") and not shown(drv, "#tbx-off"))
        assert "No toolboxes yet" in text(d, "#no-toolboxes")
        step("Where the app cannot confirm who is at the keyboard, it shows the terminal's command")
        d.find_element(By.ID, "tbx-add").click()
        wait.until(lambda drv: drv.execute_script("const x = document.getElementById('tbx-dialog'); return !!x && x.open"))
        dialog = text(d, "#tbx-dialog")
        for part in ["Stand-in GeoLibre", "A few stand-in tools", "1.8.1", "MIT", "the tests", url, size,
                     "columbus-mcp toolbox install geolibre", "columbus-mcp toolbox add PROGRAM"]:
            assert part in dialog, f"{part!r} is not in the dialog: {dialog}"
        assert not d.find_elements(By.CSS_SELECTOR, "#tbx-dialog .tbx-install"), "no Install where it could not be confirmed"
        assert not d.find_elements(By.ID, "tbx-form"), "nor a form to add a program"
        named(d)
        d.save_screenshot(str(OUT / "toolboxes-terminal.png"))
        d.find_element(By.ID, "tbx-add-close").click()
        assert not HITS, f"nothing is downloaded until the person installs: {HITS}"
    finally:
        guard.stop(d, driver_proc)

    # ---------- 2. install, find, read, run, remove ----------
    step("the app again, with the tests' confirmation")
    d, driver_proc = guard.driver(dict(env, COLUMBUS_APP_TEST_CONFIRM="allow"), APP)
    try:
        wait = WebDriverWait(d, 30)
        guard.ready(d, wait)
        nav(d, wait, "#/tools", "toolbox-card")
        wait.until(lambda drv: shown(drv, "#tbx-add"))
        step("Add tools says what would be installed before anything is downloaded")
        d.find_element(By.ID, "tbx-add").click()
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#tbx-dialog .tbx-install"))
        dialog = text(d, "#tbx-dialog")
        for part in ["Stand-in GeoLibre", "A few stand-in tools", "Version", "1.8.1", "Size", size, "Licence", "MIT",
                     "Publisher", "the tests", "Address", url, "A program on this computer"]:
            assert part in dialog, f"{part!r} is not in the dialog: {dialog}"
        assert "columbus-mcp toolbox install" not in dialog, "the terminal's command is for systems that cannot confirm"
        assert d.find_elements(By.ID, "tbx-form"), "a program on this computer can be added too"
        named(d)
        d.save_screenshot(str(OUT / "toolboxes-add.png"))
        assert not HITS, f"nothing is downloaded until the person installs: {HITS}"

        step("Install: downloaded, checked against its pin, unpacked, its tools listed in the sandbox")
        install = d.find_element(By.CSS_SELECTOR, "#tbx-dialog .tbx-install")
        assert install.text == "Install", install.text
        install.click()
        WebDriverWait(d, 180).until(lambda drv: re.fullmatch(r"[\d,]+ tools ready\.", text(drv, "#tbx-dialog .tbx-step.ok").strip() or "")
                                    or text(drv, "#tbx-dialog .tbx-install-error").strip())
        assert not text(d, "#tbx-dialog .tbx-install-error").strip(), text(d, "#tbx-dialog .tbx-install-error")
        ready = int(re.match(r"([\d,]+) tools ready", text(d, "#tbx-dialog .tbx-step.ok").strip()).group(1).replace(",", ""))
        assert ready >= 10, ready
        assert HITS == ["/standin.tar.gz"], HITS
        named(d)
        d.save_screenshot(str(OUT / "toolboxes-installed.png"))
        d.find_element(By.ID, "tbx-add-close").click()

        step("The toolbox is listed: its name, version, how many tools, where it came from")
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#toolboxes li[data-id='geolibre']"))
        row = text(d, "#toolboxes li[data-id='geolibre']")
        listed = int(re.search(r"([\d,]+) tools", row).group(1).replace(",", ""))
        assert listed >= ready, (listed, ready)
        for part in ["Stand-in GeoLibre", "version 1.8.1", f"From {url}", "licence: MIT", "In use"]:
            assert part in row, f"{part!r} is not in the row: {row}"
        assert "Installed Stand-in GeoLibre 1.8.1" in text(d, "#tbx-notice"), text(d, "#tbx-notice")
        program = root / "toolboxes" / "geolibre" / "bin" / "geolibre"
        assert program.exists() and os.access(program, os.X_OK), "the program is in the toolbox's own folder"
        assert (root / "toolboxes" / "geolibre" / "toolbox.yaml").exists()

        step("Its tools are found by words: name, summary, toolbox and category")
        wait.until(lambda drv: shown(drv, "#tbx-q"))
        wait.until(lambda drv: len(drv.find_elements(By.CSS_SELECTOR, "#tbx-tools li")) >= 10)
        assert re.search(r"Showing 40 of [\d,]+ tools|[\d,]+ tools\.", text(d, "#tbx-count")), text(d, "#tbx-count")
        q = d.find_element(By.ID, "tbx-q")
        q.send_keys("copy vector features")
        wait.until(lambda drv: (drv.find_elements(By.CSS_SELECTOR, "#tbx-tools li") or [None])[0] is not None
                   and drv.find_elements(By.CSS_SELECTOR, "#tbx-tools li")[0].get_attribute("data-tool") == TOOL)
        hit = text(d, f"#tbx-tools li[data-tool='{TOOL}']")
        for part in [TOOL, "Stand-in GeoLibre", "vector"]:
            assert part in hit, f"{part!r} is not in the row: {hit}"
        named(d)
        d.save_screenshot(str(OUT / "toolboxes-search.png"))

        step("A tool's row opens its description in the panel on the right, by the keyboard too")
        first = d.find_element(By.CSS_SELECTOR, f"#tbx-tools li[data-tool='{TOOL}']")
        assert first.get_attribute("tabindex") == "0", "Tab reaches a tool's row"
        first.send_keys(Keys.ENTER)
        wait.until(lambda drv: shown(drv, "#side") and "Parameters" in text(drv, "#side"))
        side = text(d, "#side")
        for part in [TOOL, "Stand-in GeoLibre 1.8.1", "input", "a vector layer", "required", "output", "written by default"]:
            assert part in side, f"{part!r} is not in the description: {side}"
        named(d)
        d.save_screenshot(str(OUT / "toolboxes-tool.png"))
        d.find_element(By.ID, "side-close").click()
        wait.until(lambda drv: not shown(drv, "#side"))

        step("A tool Columbus cannot run yet is listed, and says so")
        q.clear()
        q.send_keys("lidar")
        wait.until(lambda drv: "Cannot run yet" in text(drv, "#tbx-tools li[data-tool='geolibre.lidar_info']"))
        q.clear()
        q.send_keys("zzzzqqqq")
        wait.until(lambda drv: "No tool matches" in text(drv, "#tbx-count"))
        assert not d.find_elements(By.CSS_SELECTOR, "#tbx-tools li")

        step("In a chat, the assistant finds the tool and runs it: no approval, the result a layer on the map")
        d.execute_script("location.hash = '#/chats/new'")
        send(d, "Copy the tracts with a toolbox tool.")
        WebDriverWait(d, 120).until(lambda drv: "Copied the tracts with a toolbox tool." in drv.find_element(By.ID, "log").text)
        assert not d.find_elements(By.CSS_SELECTOR, ".ask"), "a toolbox tool runs without an approval"
        found, _ = result_of("t1")
        assert TOOL in found, f"search_tools did not offer {TOOL}: {found[:600]}"
        ran, failed = result_of("t3")
        assert not failed, ran[:800]
        out = json.loads(ran)
        assert out["result"]["status"] == "done", ran[:800]
        saved = out["result"]["saved"]
        assert len(saved) == 1 and saved[0]["kind"] == "layer" and "/results/geolibre-vector_copy-" in saved[0]["handle"], saved
        assert out["result"]["external"]["untrusted"] is True, "what the tool printed is marked as text not to obey"
        assert out["provenance"], ran[:300]
        step("Its result is a card in the chat; how it was made names the toolbox's tool")
        made = WebDriverWait(d, 20).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#log .result")))
        made.find_element(By.CSS_SELECTOR, ".result-made").click()
        WebDriverWait(d, 20).until(lambda drv: TOOL in text(drv, "#side"))
        named(d)
        d.save_screenshot(str(OUT / "toolboxes-made.png"))
        d.find_element(By.ID, "side-close").click()
        step("The layer the tool made is on the chat's map, in the panel on the right")
        assert d.find_element(By.ID, "outputs").is_displayed()
        frame = WebDriverWait(d, 40).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#stage iframe")))
        d.switch_to.frame(frame)
        try:
            WebDriverWait(d, 40).until(lambda drv: "geolibre-vector_copy" in drv.execute_script("return document.body.textContent"))
        finally:
            d.switch_to.default_content()
        d.save_screenshot(str(OUT / "toolboxes-map.png"))
        chat = d.execute_script("return location.hash")

        step("Remove: the toolbox and its program go")
        nav(d, wait, "#/tools", "toolbox-card")
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#toolboxes li[data-id='geolibre']"))
        remove = d.find_element(By.XPATH, "//ul[@id='toolboxes']/li[@data-id='geolibre']//button[normalize-space()='Remove']")
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", remove)
        remove.click()
        wait.until(lambda drv: not drv.find_elements(By.CSS_SELECTOR, "#toolboxes li"))
        wait.until(lambda drv: "Removed geolibre" in text(drv, "#tbx-notice"))
        assert not (root / "toolboxes" / "geolibre").exists(), "the toolbox's folder is gone"
        assert not shown(d, "#tbx-find"), "no toolbox, nothing to search"
        named(d)

        step("The same chat's assistant is no longer offered its tools")
        d.execute_script("location.hash = arguments[0]", chat)
        WebDriverWait(d, 30).until(lambda drv: drv.find_elements(By.ID, "log") and "Copied the tracts with a toolbox tool." in drv.find_element(By.ID, "log").text)
        send(d, "Find that tool again and run it.")
        WebDriverWait(d, 120).until(lambda drv: "That toolbox is no longer there." in drv.find_element(By.ID, "log").text)
        found, _ = result_of("t4")
        assert "geolibre." not in found, f"a removed toolbox's tools are still offered: {found[:600]}"
        ran, failed = result_of("t5")
        assert failed or '"status":"done"' not in ran.replace(" ", ""), f"a removed toolbox's tool still ran: {ran[:600]}"
        assert "geolibre-vector_copy" not in ran, ran[:600]

        step("Installing and removing are in the audit log")
        audit = "".join(p.read_text() for p in (root / "audit").rglob("*") if p.is_file())
        for part in ['"toolbox_install"', '"toolbox_remove"', '"run_tool"']:
            assert part in audit, f"{part} is not in the audit log"
        print("desktop toolboxes e2e: ok")
    finally:
        guard.stop(d, driver_proc)
        stand.shutdown()
        downloads.shutdown()


if __name__ == "__main__":
    main()
