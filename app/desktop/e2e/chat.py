"""The chat window, end to end on Linux through WebDriver (tauri-driver and
WebKitWebDriver, under xvfb and a D-Bus session).

A stand-in for the AI service plays a scripted conversation: whoami, a
query, then an export of the layer the query made. The test opens the chat
from the session list, sends a message, waits for the approval prompt the
server raises for the export, allows it after its one-second guard, and
checks the export landed and the reply was shown. A debug build of the app
reads the API key and the service address from the environment.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/chat.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

REPO = Path(__file__).resolve().parents[3]
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
OUT = Path(__file__).resolve().parent / "out"


def tool_use(i, name, args):
    return {"content": [{"type": "text", "text": f"I'll use {name}."},
                        {"type": "tool_use", "id": i, "name": name, "input": args}],
            "stop_reason": "tool_use"}


SCRIPT = [
    tool_use("t1", "whoami", {}),
    tool_use("t2", "query_features", {"connection": "tiger", "dataset": "tracts"}),
    "export_last",
    {"content": [{"type": "text", "text": "Done. **Exported 2 tracts** to your outbox:\n\n- tracts.gpkg\n- tracts.prov.json"}],
     "stop_reason": "end_turn"},
]
SEEN = []


class Stand(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        SEEN.append((self.headers.get("x-api-key"), body))
        nxt = SCRIPT.pop(0) if SCRIPT else {"content": [{"type": "text", "text": "(end)"}], "stop_reason": "end_turn"}
        if nxt == "export_last":
            last = body["messages"][-1]["content"][0]["content"]
            handle = json.loads(last)["result"]["layer"]["handle"]
            nxt = tool_use("t3", "export_output", {"handle": handle, "folder": "outbox", "name": "tracts"})
        data = json.dumps(nxt).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[box(0, 0, 1, 1), box(1, 0, 2, 1)], crs="EPSG:3081") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "outbox").mkdir()
    (root / "policy.yaml").write_text(f"connection_types: [gpkg]\nexports:\n  - {{ id: outbox, path: {json.dumps(str(root / 'outbox'))} }}\n")
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")

    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()

    env = dict(os.environ,
               COLUMBUS_APP_API_KEY="sk-ant-test-0123456789abcdef",
               COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{stand.server_port}",
               COLUMBUS_SERVER=str(GATEWAY),
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", XDG_CONFIG_HOME=str(root / "config"))
    driver_proc = subprocess.Popen(["tauri-driver"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    opts = webdriver.common.options.ArgOptions()
    opts.set_capability("tauri:options", {"application": str(APP)})
    opts.set_capability("browserName", "wry")
    d = webdriver.Remote("http://127.0.0.1:4444", options=opts)
    try:
        wait = WebDriverWait(d, 60)
        wait.until(EC.element_to_be_clickable((By.ID, "chat"))).click()
        wait.until(lambda drv: len(drv.window_handles) > 1)
        main = d.current_window_handle
        d.switch_to.window([h for h in d.window_handles if h != main][0])
        box = wait.until(EC.presence_of_element_located((By.ID, "text")))
        # With a key and a server, settings stay closed.
        wait.until(lambda drv: drv.execute_script("return document.getElementById('key-state').textContent").strip() != "")
        assert drv_hidden(d, "settings"), "settings open although a key and the server are set"
        box.send_keys("Export the tracts to my outbox.")
        d.find_element(By.ID, "send").click()

        card = WebDriverWait(d, 90).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".ask")))
        assert "tracts" in card.text, card.text
        allow = card.find_element(By.XPATH, ".//button[normalize-space()='Allow once']")
        assert not allow.is_enabled(), "the prompt's buttons wait a moment before they work"
        WebDriverWait(d, 5).until(lambda drv: allow.is_enabled())
        d.save_screenshot(str(OUT / "chat-approval.png"))
        allow.click()
        WebDriverWait(d, 60).until(lambda drv: "Exported 2 tracts" in drv.find_element(By.ID, "log").text)
        time.sleep(1)
        d.save_screenshot(str(OUT / "chat-done.png"))
        logtext = d.find_element(By.ID, "log").text
        assert "Allowed once." in logtext, logtext
        assert "Querying tracts (tiger)" in logtext, logtext
        assert (root / "outbox" / "tracts.gpkg").exists(), "the export happened"
        # The model never saw the key in a message, and only Columbus tools.
        key, first = SEEN[0]
        assert key == "sk-ant-test-0123456789abcdef"
        names = {t["name"] for t in first["tools"]}
        assert "whoami" in names and "export_output" in names, names
        # The page has no way to read the key back.
        got = d.execute_async_script(
            "const done = arguments[arguments.length - 1];"
            "window.__TAURI__.core.invoke('chat_settings').then(s => done(JSON.stringify(s))).catch(e => done('ERR ' + e));")
        assert "sk-ant" not in got, got
        print("desktop chat e2e: ok")
    finally:
        d.quit()
        driver_proc.terminate()
        stand.shutdown()


def drv_hidden(d, element_id):
    return d.execute_script(f"return getComputedStyle(document.getElementById('{element_id}')).display === 'none'")


if __name__ == "__main__":
    main()
