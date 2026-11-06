"""A page in the main window's frame cannot reach the app, on every system
CI runs (Windows, macOS, Linux): no WebDriver needed.

The companion page shown in the output panel is another origin
(http://127.0.0.1:<port>/embed) in a sandboxed frame. On Windows, wry
injects Tauri's scripts into frames too, so this checks what stands
between such a frame and the app's commands: a debug build of the app is
started with COLUMBUS_APP_TEST_FRAME, which shows this test's own page in
the main window's frame exactly as a session's page is shown. The page
tries every way in: `__TAURI_INTERNALS__.invoke` for a read
(settings_state) and for changes (a new connection, with the debug
confirmation stand-in set to allow, and the app's preferences),
`window.ipc.postMessage` with the same requests, and fetches to the IPC
protocol's addresses. It reports what happened to this test's server.
Every attempt must be refused or go unanswered, and nothing may change.

    python app/desktop/e2e/frame_ipc.py app/desktop/src-tauri/target/debug/columbus-desktop
    (on Linux under: dbus-run-session -- xvfb-run -a)
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

APP = Path(sys.argv[1]).resolve()
if os.name == "nt" and APP.suffix != ".exe":
    APP = APP.with_suffix(".exe")
OUT = Path(__file__).resolve().parent / "out"

PAGE = """<!doctype html><meta charset="utf-8"><title>frame test</title>
<body>frame test</body>
<script>
(async () => {
  const out = {
    internals: typeof window.__TAURI_INTERNALS__,
    tauri: typeof window.__TAURI__,
    ipc: typeof window.ipc,
    webview: typeof (window.chrome && window.chrome.webview),
    results: {},
  };
  const conn = { form: { existing: false, id: "fromframe", kind: "gpkg", path: GPKG, mode: "read", allow: ["*"], deny: [] } };
  const prefs = { prefs: { theme: "dark", basemap: true, skipped: [], setup_done: true } };
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  async function invoke(name, cmd, args) {
    const I = window.__TAURI_INTERNALS__;
    if (!I || typeof I.invoke !== "function") { out.results[name] = "no invoke"; return; }
    try {
      const r = await Promise.race([I.invoke(cmd, args), wait(6000).then(() => { throw new Error("no answer"); })]);
      out.results[name] = "RESOLVED " + JSON.stringify(r).slice(0, 300);
    } catch (e) {
      out.results[name] = "refused: " + String(e && e.message || e).slice(0, 300);
    }
  }
  await invoke("invoke settings_state", "settings_state", {});
  await invoke("invoke save_connection", "settings_save_connection", conn);
  await invoke("invoke prefs_set", "prefs_set", prefs);
  // The raw message channel, as Tauri's own script would use it.
  try {
    const key = window.__TAURI_INVOKE_KEY__ || "";
    const send = (cmd, payload) => window.ipc.postMessage(JSON.stringify({ cmd, callback: 1, error: 2, payload, options: null, __TAURI_INVOKE_KEY__: key }));
    if (window.ipc && window.ipc.postMessage) {
      send("settings_save_connection", { form: { ...conn.form, id: "fromframe2" } });
      send("prefs_set", prefs);
      out.results["postMessage"] = "sent";
    } else out.results["postMessage"] = "no window.ipc";
  } catch (e) { out.results["postMessage"] = "threw: " + String(e).slice(0, 200); }
  try {
    if (window.chrome && window.chrome.webview) {
      window.chrome.webview.postMessage(JSON.stringify({ cmd: "prefs_set", callback: 1, error: 2, payload: prefs, options: null, __TAURI_INVOKE_KEY__: "" }));
      out.results["chrome.webview"] = "sent";
    } else out.results["chrome.webview"] = "none";
  } catch (e) { out.results["chrome.webview"] = "threw: " + String(e).slice(0, 200); }
  // The IPC protocol's addresses, directly: a read, and a change.
  for (const [base, cmd, body] of [["ipc://localhost", "settings_state", "{}"], ["http://ipc.localhost", "settings_state", "{}"],
    ["ipc://localhost", "prefs_set", JSON.stringify(prefs)], ["http://ipc.localhost", "prefs_set", JSON.stringify(prefs)]]) {
    try {
      const r = await Promise.race([fetch(base + "/" + cmd, {
        method: "POST", body,
        headers: { "Content-Type": "application/json", "Tauri-Callback": "1", "Tauri-Error": "2", "Tauri-Invoke-Key": window.__TAURI_INVOKE_KEY__ || "x" },
      }), wait(6000).then(() => { throw new Error("no answer"); })]);
      out.results["fetch " + base + " " + cmd] = "status " + r.status + " " + (await r.text()).slice(0, 300);
    } catch (e) { out.results["fetch " + base + " " + cmd] = "refused: " + String(e && e.message || e).slice(0, 200); }
  }
  await wait(1500);
  await fetch("/report", { method: "POST", body: JSON.stringify(out) });
})();
</script>
"""

REPORT = {}


class Page(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        if self.path.split("?")[0] != "/embed":
            self.send_response(404)
            self.end_headers()
            return
        body = PAGE.replace("GPKG", json.dumps(str(GPKG))).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        data = self.rfile.read(n)
        if self.path == "/report":
            REPORT.update(json.loads(data))
        self.send_response(204)
        self.end_headers()


def main():
    global GPKG
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    (root / "connections").mkdir()
    (root / "policy.yaml").write_text("connection_types: [gpkg]\nallow_scripts: false\n")
    GPKG = root / "roads.gpkg"
    GPKG.write_bytes(b"")
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Page)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{srv.server_port}/embed"
    env = dict(os.environ, COLUMBUS_APP_TEST_FRAME=url, COLUMBUS_APP_TEST_CONFIRM="allow",
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_LIVE_DIR=str(root / "live"), XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), WEBKIT_DISABLE_COMPOSITING_MODE="1")
    # The app's preferences file, wherever this system keeps it: the frame
    # must not change it.
    prefs = prefs_file(root)
    before = prefs.read_bytes() if prefs and prefs.exists() else None
    policy_before = (root / "policy.yaml").read_text()
    app = subprocess.Popen([str(APP)], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        deadline = time.time() + 120
        while not REPORT and time.time() < deadline and app.poll() is None:
            time.sleep(0.5)
        time.sleep(3)  # anything a message started has time to land
        if not REPORT:
            raise SystemExit(f"FAIL: the test page in the main window's frame never reported (did the frame load?); app exited: {app.poll()}")
        print(json.dumps(REPORT, indent=2))
        (OUT / f"frame_ipc-{sys.platform}.json").write_text(json.dumps(REPORT, indent=2))
        bad = [k for k, v in REPORT["results"].items() if "RESOLVED" in v or "connections_dir" in v]
        assert not bad, f"FAIL: the frame reached the app: {bad}: {REPORT['results']}"
        assert REPORT["tauri"] == "undefined", REPORT
        assert not list((root / "connections").iterdir()), "FAIL: a connection was written from the frame"
        assert (root / "policy.yaml").read_text() == policy_before
        after = prefs.read_bytes() if prefs and prefs.exists() else None
        assert after == before, f"FAIL: the frame changed the app's preferences ({prefs})"
        if sys.platform != "win32":
            # Tauri's scripts are injected into the main frame only here.
            assert REPORT["internals"] == "undefined", REPORT
        print(f"frame IPC e2e ({sys.platform}): ok")
    finally:
        app.terminate()
        try:
            app.wait(15)
        except Exception:
            app.kill()
        srv.shutdown()


def prefs_file(root):
    ident = "org.columbus-gis.desktop"
    if sys.platform == "win32":
        base = os.environ.get("APPDATA")
        return Path(base) / ident / "preferences.json" if base else None
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support" / ident / "preferences.json"
    return root / "config" / ident / "preferences.json"


if __name__ == "__main__":
    main()
