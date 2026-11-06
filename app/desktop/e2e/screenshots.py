"""The README's screenshots, taken from the real app on Linux (WebDriver,
under xvfb): sample Austin-area layers, a scripted assistant (a stand-in for
the AI service), and each page of the main window. Writes docs/images/*.png.

    dbus-run-session -- xvfb-run -a -s "-screen 0 1440x900x24" python app/desktop/e2e/screenshots.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop

The data is made up: a grid of road segments, crash points and district
polygons around Austin, Texas, with invented names.
"""

import json
import os
import random
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
OUT = REPO / "docs" / "images"
HANDLES = {}


def tool_use(i, name, args, text):
    return {"content": [{"type": "text", "text": text}, {"type": "tool_use", "id": i, "name": name, "input": args}],
            "stop_reason": "tool_use"}


def answer(text):
    return {"content": [{"type": "text", "text": text}], "stop_reason": "end_turn"}


SCRIPT = [
    tool_use("t1", "whoami", {}, "I'll check what this session may use."),
    tool_use("t2", "list_datasets", {"connection": "austin-sample"}, "Let me see the datasets."),
    tool_use("t3", "query_features", {"connection": "austin-sample", "dataset": "crashes", "where": "severity >= 2"}, "Reading the serious crashes."),
    tool_use("t4", "query_features", {"connection": "austin-sample", "dataset": "roads"}, "And the road segments."),
    "snapshot",
    answer("The **38 serious crashes** cluster along two corridors, and every one sits on a road segment: nothing falls off the network, so the coordinates line up.\n\n"
           "| Corridor | Crashes | Segments |\n|---|---:|---:|\n| IH0035 (north–south) | 21 | 9 |\n| US0290 (east–west) | 17 | 8 |\n\n"
           "Both layers are in EPSG:4326. The checks passed; the row filter limited the crashes to District 14."),
    "export",
    answer("Exported **38 crashes** to the review folder, with a provenance file beside it."),
]


class Stand(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        last = body["messages"][-1]["content"]
        if isinstance(last, list) and last and last[0].get("type") == "tool_result":
            c = last[0]["content"]
            text = c if isinstance(c, str) else next((x["text"] for x in c if x.get("type") == "text"), "")
            try:
                h = json.loads(text)["result"]["layer"]["handle"]
                HANDLES[last[0]["tool_use_id"]] = h
            except Exception:
                pass
        nxt = SCRIPT.pop(0) if SCRIPT else answer("(end)")
        if nxt == "snapshot":
            nxt = tool_use("t5", "map_snapshot", {"layers": [HANDLES["t4"], HANDLES["t3"]]}, "I'll look at both layers on a map.")
        if nxt == "export":
            nxt = tool_use("t6", "export_output", {"handle": HANDLES["t3"], "folder": "review", "name": "serious-crashes"}, "I'll export them.")
        data = json.dumps(nxt).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def make_data(root):
    import geopandas as gpd
    from shapely.geometry import LineString, Point, box

    random.seed(7)
    x0, y0 = -97.80, 30.20
    roads, names = [], []
    for i in range(9):
        roads.append(LineString([(-97.745, y0 + i * 0.02), (-97.745, y0 + (i + 1) * 0.02)]))
        names.append("IH0035")
    for i in range(8):
        roads.append(LineString([(x0 + i * 0.03, 30.29), (x0 + (i + 1) * 0.03, 30.29)]))
        names.append("US0290")
    gpd.GeoDataFrame({"RTE_ID": names, "district": [14] * len(roads)}, geometry=roads, crs="EPSG:4326") \
        .to_file(root / "austin.gpkg", layer="roads", driver="GPKG", engine="pyogrio")
    pts, sev, rte = [], [], []
    for _ in range(21):
        pts.append(Point(-97.745, y0 + random.uniform(0.005, 0.175)))
        sev.append(random.choice([2, 3]))
        rte.append("IH0035")
    for _ in range(17):
        pts.append(Point(x0 + random.uniform(0.01, 0.23), 30.29))
        sev.append(random.choice([2, 3]))
        rte.append("US0290")
    for _ in range(12):
        pts.append(Point(x0 + random.uniform(0, 0.24), y0 + random.uniform(0, 0.18)))
        sev.append(1)
        rte.append("IH0035")
    gpd.GeoDataFrame({"crash_id": [f"C{i:03d}" for i in range(len(pts))], "severity": sev, "RTE_ID": rte, "district": [14] * len(pts)},
                     geometry=pts, crs="EPSG:4326").to_file(root / "austin.gpkg", layer="crashes", driver="GPKG", engine="pyogrio")
    gpd.GeoDataFrame({"name": ["North", "Central", "South"], "district": [14, 14, 14]},
                     geometry=[box(-97.81, 30.26, -97.55, 30.39), box(-97.81, 30.13, -97.55, 30.26), box(-97.81, 30.0, -97.55, 30.13)],
                     crs="EPSG:4326").to_file(root / "austin.gpkg", layer="maintenance_sections", driver="GPKG", engine="pyogrio")


WORKFLOW = """id: weekday-crash-check
name: Weekday crash check
purpose: Every weekday morning, read the serious crashes, check them and export them for the district share.
schedule: { days: weekdays, at: "06:00" }
scope: { connections: [austin-sample], run_minutes: 15 }
steps:
  - id: crashes
    tool: query_features
    args: { connection: austin-sample, dataset: crashes, where: "severity >= 2" }
  - id: checked
    tool: verify
    args: { layer: "${crashes}" }
outputs:
  folder: { export: outbox, steps: [crashes] }
"""

TOOL_YAML = """id: personal.count-by-route
name: Count by route
version: 1.0.0
runtime: python
entry: tool.py
tools:
  run:
    summary: Count features by route id.
    description: Counts a layer's features by its route id field and writes the counts as a table.
    not_for: Layers without a route id field.
    risk: read
    params:
      layer: { type: file, kind: layer, description: Any layer with a route id field. }
    outputs:
      counts: { type: file, format: csv, description: One row per route id with its count. }
"""
TOOL_PY = "def run(ctx):\n    import csv\n    import geopandas as gpd\n    df = gpd.read_file(ctx.input_file('layer'))\n    counts = df.groupby('RTE_ID').size()\n    with open(ctx.output_path('counts'), 'w', newline='') as f:\n        w = csv.writer(f)\n        w.writerow(['RTE_ID', 'count'])\n        for k, v in counts.items():\n            w.writerow([k, int(v)])\n    return {'routes': int(len(counts))}\n"


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="columbus-demo-"))
    for d in ["connections", "outbox", "review"]:
        (root / d).mkdir()
    make_data(root)
    (root / "connections/austin-sample.yaml").write_text(
        f"id: austin-sample\ntype: gpkg\npath: {json.dumps(str(root / 'austin.gpkg'))}\ndatasets:\n  allow: [\"*\"]\nrow_filter: \"district = 14\"\n")
    (root / "policy.yaml").write_text(
        "connection_types: [gpkg]\nallow_workflows: true\nallow_personal_tools: true\nallow_scripts: true\n"
        f"exports:\n  - {{ id: outbox, path: {json.dumps(str(root / 'outbox'))}, granted: true }}\n"
        f"  - {{ id: review, path: {json.dumps(str(root / 'review'))} }}\n")
    # A tool and a workflow the assistant proposed earlier.
    import hashlib
    td = root / "tools" / "proposals" / "personal.count-by-route"
    td.mkdir(parents=True)
    (td / "plugin.yaml").write_text(TOOL_YAML)
    (td / "tool.py").write_text(TOOL_PY)
    (td / "proposal.json").write_text(json.dumps({"proposed": "2026-11-06T07:40:00-06:00", "test": {"outputs": [{"kind": "table", "rows": 2}], "checks": []}}))
    wd = root / "workflows" / "proposals" / "weekday-crash-check"
    wd.mkdir(parents=True)
    (wd / "workflow.yaml").write_text(WORKFLOW)
    (wd / "proposal.json").write_text(json.dumps({"id": "weekday-crash-check", "proposed": "2026-11-06T07:45:00-06:00"}))

    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()
    env = dict(os.environ,
               COLUMBUS_APP_API_KEY="sk-ant-demo", COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{stand.server_port}",
               COLUMBUS_SERVER=str(GATEWAY), COLUMBUS_APP_TEST_CONFIRM="allow",
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_TOOLS=str(root / "tools"),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
               COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"), COLUMBUS_LOG="warn")
    proc = subprocess.Popen(["tauri-driver"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    opts = webdriver.common.options.ArgOptions()
    opts.set_capability("tauri:options", {"application": str(APP)})
    opts.set_capability("browserName", "wry")
    d = webdriver.Remote("http://127.0.0.1:4444", options=opts)
    d.set_window_size(1280, 800)
    wait = WebDriverWait(d, 60)

    def shot(name):
        time.sleep(1.2)
        # Focus as after a click (the page title takes it for screen readers),
        # and no notice only Linux shows.
        d.execute_script("""
            if (document.activeElement && document.activeElement.id === 'page-title') document.activeElement.blur();
            for (const n of document.querySelectorAll('.notice')) if (/no Touch ID or Windows Hello|Schedules are not available/.test(n.textContent)) n.remove();
        """)
        d.save_screenshot(str(OUT / f"{name}.png"))
        print("wrote", name)

    def go(hash_, ready_id=None):
        d.execute_script("location.hash = arguments[0]", hash_)
        if ready_id:
            wait.until(EC.presence_of_element_located((By.ID, ready_id)))
        time.sleep(1.0)

    try:
        wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))
        # Skip the first steps: this computer is set up.
        d.execute_script("window.__TAURI__.core.invoke('prefs_set', {prefs: {theme: 'light', basemap: false, skipped: ['ai','engine','connection'], setup_done: true}})")
        d.refresh()
        wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))

        # A chat: the assistant reads the layers and looks at them on a map.
        wait.until(EC.element_to_be_clickable((By.CSS_SELECTOR, "a.btn.primary[href='#/chats/new']"))).click()
        box = wait.until(EC.visibility_of_element_located((By.ID, "text")))
        time.sleep(1)
        box.send_keys("Where are the serious crashes on District 14's roads, and do they line up with the road network?")
        d.find_element(By.ID, "send").click()
        WebDriverWait(d, 120).until(lambda drv: "38 serious crashes" in drv.find_element(By.ID, "log").text)
        frame = WebDriverWait(d, 60).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#chatview .outpanel iframe")))
        d.switch_to.frame(frame)
        WebDriverWait(d, 60).until(lambda drv: "crashes" in drv.execute_script("return document.body.textContent"))
        # Both layers on the map.
        # One at a time: the list draws itself again after each.
        for _ in range(6):
            left = d.execute_script("""
                const boxes = [...document.querySelectorAll('input[type=checkbox]')]
                  .filter((b) => !/Basemap/.test((b.closest('label, li, div') || {}).textContent || ''));
                const off = boxes.filter((b) => !b.checked);
                if (off.length) off[0].click();
                return off.length;""")
            time.sleep(0.8)
            if not left:
                break
        time.sleep(1.5)
        d.switch_to.default_content()
        d.execute_script("const f = document.querySelector('#log figure.snapshot'); if (f) f.scrollIntoView({block: 'start'}); document.getElementById('log').scrollTop -= 40;")
        shot("chat")
        chat_hash = d.execute_script("return location.hash")

        # Map first.
        d.find_element(By.XPATH, "//button[normalize-space()='Map first']").click()
        shot("chat-map-first")
        d.find_element(By.XPATH, "//button[normalize-space()='Chat first']").click()

        # An export to a folder that isn't granted asks the person.
        box.send_keys("Export the serious crashes to the review folder.")
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
        d.find_element(By.ID, "send").click()
        card = WebDriverWait(d, 90).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".ask")))
        allow = card.find_element(By.XPATH, ".//button[normalize-space()='Allow once']")
        WebDriverWait(d, 5).until(lambda drv: allow.is_enabled())
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", card)
        shot("approval")
        allow.click()
        WebDriverWait(d, 60).until(lambda drv: "Exported 38 crashes" in drv.find_element(By.ID, "log").text)

        # Workflows: review the proposal, approve it, run it, read the report.
        go("#/workflows", "wf-waiting")
        WebDriverWait(d, 20).until(lambda drv: "Weekday crash check" in drv.find_element(By.ID, "wf-waiting").text)
        d.find_element(By.XPATH, "//ul[@id='wf-waiting']//button[normalize-space()='Review']").click()
        wait.until(EC.visibility_of_element_located((By.ID, "wf-review")))
        shot("workflow-review")
        d.find_element(By.ID, "wf-review-install").click()
        WebDriverWait(d, 20).until(lambda drv: "Weekday crash check" in drv.find_element(By.ID, "wf-installed").text)
        d.find_element(By.XPATH, "//ul[@id='wf-installed']//button[normalize-space()='Run now']").click()
        WebDriverWait(d, 180).until(lambda drv: drv.find_elements(By.ID, "wf-report") and "Succeeded" in drv.find_element(By.ID, "wf-report").text)
        d.execute_script("document.getElementById('wf-runs').scrollIntoView({block: 'start'})")
        shot("workflow-run")

        # Tools: a tool the assistant proposed, read before installing.
        go("#/tools", "tools-waiting")
        WebDriverWait(d, 20).until(lambda drv: "Count by route" in drv.find_element(By.ID, "tools-waiting").text)
        d.find_element(By.XPATH, "//ul[@id='tools-waiting']//button[normalize-space()='Review']").click()
        wait.until(EC.visibility_of_element_located((By.ID, "tool-review")))
        shot("tool-review")
        d.find_element(By.ID, "review-close").click()

        # Home, connections, documents, activity, settings.
        go("#/home")
        shot("home")
        go("#/connections")
        shot("connections")
        go("#/documents")
        shot("documents")
        go("#/activity/audit")
        shot("audit")
        go("#/activity/approvals")
        shot("approvals")
        go("#/settings/ai")
        shot("settings")
        print("screenshots: ok")
    finally:
        d.quit()
        proc.terminate()
        stand.shutdown()


if __name__ == "__main__":
    main()
