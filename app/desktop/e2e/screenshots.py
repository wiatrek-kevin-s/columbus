"""The README's pictures, taken from the real app on Linux (WebDriver, under
xvfb): public data of central Austin, Texas, a scripted assistant (a
stand-in for the AI service), and the app's pages. Writes docs/images/*.png
and the demo animation's frames (app/desktop/e2e/out/frames/).

    dbus-run-session -- xvfb-run -a -s "-screen 0 1920x1200x24" python app/desktop/e2e/screenshots.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop --root /home/analyst

`--root` is the stand-in person's home folder: their GIS files and Columbus
settings go there, and its paths are what the pictures show (so use a plain
one, never a temporary folder; it is emptied first). The street basemap
comes from tiles.openfreemap.org, so the pictures need internet; the
"README pictures" workflow takes them on GitHub's runners. Then
`sh app/desktop/e2e/demo/animate.sh` makes the animation.

The data: app/desktop/e2e/demo/austin.gpkg, a public extract (US Census
TIGER/Line roads and blocks, NHTSA FARS fatal crashes; see
docs/images/DATA-CREDITS.md), made by demo/fetch_austin.py. The answers the
stand-in gives are computed from that data here, so they are true. The
Oracle and ArcGIS Online connections are listed only: their hosts are not
on the organization's host list, so nothing is ever sent to them.
"""

import argparse
import json
import os
import shutil
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC

import guard
from guard import step

WebDriverWait = guard.Wait

REPO = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
OUT = REPO / "docs" / "images"
FRAMES = HERE / "out" / "frames"

ap = argparse.ArgumentParser()
ap.add_argument("gateway")
ap.add_argument("app")
ap.add_argument("--root", required=True, help="the stand-in person's home folder (emptied first)")
ap.add_argument("--data", default=str(HERE / "demo" / "austin.gpkg"))
ap.add_argument("--themes", default="light,dark")
ARGS = ap.parse_args()
GATEWAY = Path(ARGS.gateway).resolve()
APP = Path(ARGS.app).resolve()
ROOT = Path(ARGS.root)

W, H = 1600, 1000
QUESTION = "Which fatal crashes from 2020 to 2023 happened within 150 feet of IH 35 in central Austin? Count them by year."
EXPORT_ASK = "Export those crashes to the review folder for the safety team."
CRASHES = "fars_fatal_crashes_2020_2023"
IH35_WHERE = "street_name LIKE 'I- 35%' OR street_name LIKE 'I-35%' OR street_name LIKE 'Interstate 35%'"


# ---------------------------------------------------------------- the data

def prepare(root):
    """The person's GIS files and Columbus settings under `root`; returns
    the facts the assistant's answer states, computed from the data."""
    import geopandas as gpd

    if root.exists():
        for p in root.iterdir():
            shutil.rmtree(p) if p.is_dir() and not p.is_symlink() else p.unlink()
    root.mkdir(parents=True, exist_ok=True)
    gis = root / "GIS"
    austin = gis / "Austin"
    crash_dir = gis / "Crash records"
    col = root / "Columbus"
    for d in [austin, crash_dir, col / "connections", col / "exports" / "Outbox", col / "exports" / "Review"]:
        d.mkdir(parents=True, exist_ok=True)

    roads = gpd.read_file(ARGS.data, layer="roads", engine="pyogrio").to_crs("EPSG:2277")
    blocks = gpd.read_file(ARGS.data, layer="census_blocks", engine="pyogrio").to_crs("EPSG:2277")
    crashes = gpd.read_file(ARGS.data, layer="fatal_crashes", engine="pyogrio")
    roads.to_file(austin / "Austin_Streets.gpkg", layer="roads", driver="GPKG", engine="pyogrio")
    blocks.to_file(austin / "Austin_Planning.gdb", layer="census_blocks", driver="OpenFileGDB", engine="pyogrio")
    crashes.drop(columns="geometry").to_csv(crash_dir / f"{CRASHES}.csv", index=False)

    # The answer, worked out the way the assistant's steps do it.
    ih35 = roads[roads["street_name"].fillna("").str.match(r"^(I- ?35|I-35|Interstate 35)")]
    if ih35.empty:
        sys.exit("no IH 35 segments in the extract (check the street names)")
    corridor = ih35.buffer(150).union_all()
    pts = crashes.to_crs("EPSG:2277")
    near = pts[pts.within(corridor)]
    by_year = near.groupby("year").agg(crashes=("case_id", "size"), deaths=("fatalities", "sum")).reset_index()
    facts = {"total": len(crashes), "near": len(near), "deaths": int(near["fatalities"].sum()),
             "segments": len(ih35), "rows": [(int(r.year), int(r.crashes), int(r.deaths)) for r in by_year.itertuples()]}

    conns = col / "connections"
    (conns / "austin-streets.yaml").write_text(
        f"id: austin-streets\ntype: gpkg\npath: {json.dumps(str(austin / 'Austin_Streets.gpkg'))}\n"
        "datasets:\n  allow: [\"*\"]\ndescription: Street centerlines of central Austin (US Census TIGER/Line 2024)\n")
    (conns / "planning-gdb.yaml").write_text(
        f"id: planning-gdb\ntype: filegdb\npath: {json.dumps(str(austin / 'Austin_Planning.gdb'))}\n"
        "datasets:\n  allow: [\"*\"]\ndescription: Census blocks of central Austin (US Census TIGER/Line 2024)\n")
    (conns / "crash-records.yaml").write_text(
        f"id: crash-records\ntype: files\npath: {json.dumps(str(crash_dir))}\n"
        "datasets:\n  allow: [\"*\"]\ndescription: Fatal crash records, 2020 to 2023 (NHTSA FARS)\n")
    # Listed, never reached: neither host is on the policy's host list.
    (conns / "enterprise-gdb.yaml").write_text(
        "id: enterprise-gdb\ntype: oracle\nhost: gisprod.agency.local\nport: 1521\ndatabase: GISPROD\nuser: gis_reader\n"
        "datasets:\n  allow: [\"TRANSPORTATION.*\"]\nrow_filter: \"DISTRICT = 14\"\n"
        "description: Enterprise geodatabase (roadway inventory), Austin district only\n")
    (conns / "bike-lanes-agol.yaml").write_text(
        "id: bike-lanes-agol\ntype: arcgis\nurl: https://services.arcgis.com/aUstInDemo0001/arcgis/rest/services/Bicycle_Facilities/FeatureServer\n"
        "datasets:\n  allow: [\"*\"]\ndescription: Bicycle facilities (ArcGIS Online, public layer)\n")
    (col / "policy.yaml").write_text(
        "connection_types: [gpkg, filegdb, files, oracle, arcgis]\nallow_workflows: true\nallow_personal_tools: true\nallow_scripts: true\n"
        f"exports:\n  - {{ id: outbox, path: {json.dumps(str(col / 'exports' / 'Outbox'))}, granted: true }}\n"
        f"  - {{ id: review, path: {json.dumps(str(col / 'exports' / 'Review'))} }}\n")

    td = col / "tools" / "proposals" / "personal.crashes-by-year"
    td.mkdir(parents=True)
    (td / "plugin.yaml").write_text(TOOL_YAML)
    (td / "tool.py").write_text(TOOL_PY)
    (td / "proposal.json").write_text(json.dumps({"proposed": "2026-11-06T07:40:00-06:00", "test": {"outputs": [{"kind": "table", "rows": 4}], "checks": []}}))
    wd = col / "workflows" / "proposals" / "weekly-ih35-crash-check"
    wd.mkdir(parents=True)
    (wd / "workflow.yaml").write_text(WORKFLOW)
    (wd / "proposal.json").write_text(json.dumps({"id": "weekly-ih35-crash-check", "proposed": "2026-11-06T07:45:00-06:00"}))
    return facts


WORKFLOW = f"""id: weekly-ih35-crash-check
name: Weekly IH 35 crash check
purpose: Every Monday morning, read the fatal crash records, check them and put a copy in the outbox for the safety team.
schedule: {{ days: [mon], at: "06:30" }}
scope: {{ connections: [crash-records], run_minutes: 15 }}
steps:
  - id: crashes
    tool: query_features
    args: {{ connection: crash-records, dataset: {CRASHES} }}
  - id: checked
    tool: verify
    args: {{ layer: "${{crashes}}" }}
outputs:
  folder: {{ export: outbox, steps: [crashes] }}
"""

TOOL_YAML = """id: personal.crashes-by-year
name: Crashes by year
version: 1.0.0
runtime: python
entry: tool.py
tools:
  run:
    summary: Count crashes and deaths by year.
    description: Counts a crash layer's rows and sums its fatalities by its year field, as a table.
    not_for: Layers without year and fatalities fields.
    risk: read
    params:
      layer: { type: file, kind: layer, description: A crash layer with year and fatalities fields. }
    outputs:
      counts: { type: file, format: csv, description: One row per year with crashes and deaths. }
"""
TOOL_PY = ("def run(ctx):\n    import csv\n    import geopandas as gpd\n    df = gpd.read_file(ctx.input_file('layer'))\n"
           "    by = df.groupby('year').agg(crashes=('year', 'size'), deaths=('fatalities', 'sum'))\n"
           "    with open(ctx.output_path('counts'), 'w', newline='') as f:\n        w = csv.writer(f)\n"
           "        w.writerow(['year', 'crashes', 'deaths'])\n        for year, r in by.iterrows():\n"
           "            w.writerow([int(year), int(r.crashes), int(r.deaths)])\n    return {'years': int(len(by))}\n")


# ---------------------------------------------------------- the assistant

def tool_use(i, name, args, text):
    return {"content": [{"type": "text", "text": text}, {"type": "tool_use", "id": i, "name": name, "input": args}],
            "stop_reason": "tool_use"}


def answer(text):
    return {"content": [{"type": "text", "text": text}], "stop_reason": "end_turn"}


class Assistant:
    """The scripted conversation, one reply per request, a little slowly
    (as a real one), so the animation sees each step arrive."""

    def __init__(self, facts):
        f = facts
        table = "\n".join(f"| {y} | {c} | {d} |" for y, c, d in f["rows"])
        self.handles = {}
        self.script = [
            tool_use("t1", "query_features", {"connection": "crash-records", "dataset": CRASHES},
                     "I'll read the fatal crash records first."),
            tool_use("t2", "query_features", {"connection": "austin-streets", "dataset": "roads", "where": IH35_WHERE},
                     "Now the IH 35 segments from the street centerlines."),
            lambda h: tool_use("t3", "vector_op", {"op": "buffer", "inputs": {"layer": h["t2"]},
                                                   "params": {"distance": 150, "unit": "ft", "dissolve": True}},
                               "I'll draw a 150-foot corridor around them. The streets are in Texas Central State Plane (feet), so the distance is true."),
            lambda h: tool_use("t4", "vector_op", {"op": "clip", "inputs": {"layer": h["t1"], "mask": h["t3"]}},
                               "And keep the crashes inside that corridor."),
            lambda h: tool_use("t5", "map_snapshot", {"layers": [h["t3"], h["t4"]]}, "Let me look at the result on a map."),
            answer(f"**{f['near']} of the {f['total']} fatal crashes** in this part of Austin from 2020 to 2023 were within 150 feet of IH 35. "
                   f"They took **{f['deaths']} lives**.\n\n| Year | Crashes | Deaths |\n|---|---:|---:|\n{table}\n\n"
                   "The crashes sit on the corridor, not beside it, so the two layers line up. "
                   "The checks passed. The highlighted points on the map are the ones I counted."),
            lambda h: tool_use("t6", "export_output", {"handle": h["t4"], "folder": "review", "name": "ih35_fatal_crashes"},
                               "I'll export them to the review folder. It is not pre-approved, so Columbus will ask you first."),
            answer(f"Exported **{f['near']} crashes** to the review folder as `ih35_fatal_crashes.gpkg`, with a provenance file beside it."),
        ]
        self.delay = 1.6

    def reply(self, body):
        last = body["messages"][-1]["content"]
        if isinstance(last, list) and last and last[0].get("type") == "tool_result":
            c = last[0]["content"]
            text = c if isinstance(c, str) else next((x["text"] for x in c if x.get("type") == "text"), "")
            try:
                self.handles[last[0]["tool_use_id"]] = json.loads(text)["result"]["layer"]["handle"]
            except Exception:
                pass
        time.sleep(self.delay)
        nxt = self.script.pop(0) if self.script else answer("(end)")
        return nxt(self.handles) if callable(nxt) else nxt


def stand_in(assistant):
    class Stand(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            data = json.dumps(assistant.reply(body)).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

    srv = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


# ----------------------------------------------------------------- shots

def shrink(path, budget_kb, size=None):
    """Fit a PNG in its budget: resize if asked, then fewer colours if needed."""
    from PIL import Image

    im = Image.open(path).convert("RGB")
    if size:
        im = im.resize(size, Image.LANCZOS)
    im.save(path, optimize=True)
    if path.stat().st_size > budget_kb * 1024:
        im.quantize(colors=256, method=Image.Quantize.FASTOCTREE, dither=Image.Dither.FLOYDSTEINBERG).save(path, optimize=True)
    kb = path.stat().st_size // 1024
    print(f"wrote {path.name} ({kb} KB)")
    if kb > budget_kb:
        sys.exit(f"{path.name} is {kb} KB, over its {budget_kb} KB budget")


class Session:
    def __init__(self, theme, frames):
        self.theme = theme
        self.frames = frames  # list of (file, seconds), or None
        self.n = 0

    def tidy(self):
        # Focus as after a click, and no notice only Linux shows.
        self.d.execute_script("""
            if (document.activeElement && document.activeElement.id === 'page-title') document.activeElement.blur();
            for (const n of document.querySelectorAll('.notice')) if (/no Touch ID or Windows Hello|Schedules are not available|cannot be made here/.test(n.textContent)) n.remove();
        """)

    def shot(self, name, budget_kb, size=None, settle=1.2):
        time.sleep(settle)
        self.tidy()
        p = OUT / f"{name}.png"
        self.d.save_screenshot(str(p))
        shrink(p, budget_kb, size)

    def frame(self, hold=0.6):
        if self.frames is None:
            return
        self.tidy()
        p = FRAMES / f"{self.n:03d}.png"
        self.d.save_screenshot(str(p))
        self.frames.append((p.name, hold))
        self.n += 1

    def frames_until(self, cond, limit=120, every=0.7):
        """Frames while waiting for `cond` (or just wait, with no frames)."""
        end = time.monotonic() + limit
        while not cond(self.d):
            if time.monotonic() > end:
                raise SystemExit(f"step '{guard.last()}' waited {limit}s in vain")
            if self.frames is not None:
                self.frame(0.7)
            time.sleep(every if self.frames is None else 0.1)


def layers_on(d, names=None):
    """Turn on the output panel's layers (all, or those whose name has one
    of `names`), one at a time: the list draws itself again after each."""
    for _ in range(12):
        left = d.execute_script("""
            const want = arguments[0];
            const items = [...document.querySelectorAll('li.out')];
            const off = items.filter((li) => {
              const b = li.querySelector('input[type=checkbox]');
              const name = (li.querySelector('.out-name') || {}).textContent || '';
              return b && !b.checked && (!want || want.some((w) => name.includes(w)));
            });
            if (off.length) off[0].querySelector('input[type=checkbox]').click();
            return off.length;""", names)
        time.sleep(0.8)
        if not left:
            return


def run(theme, facts, frames):
    step(f"{theme}: setting up")
    assistant = Assistant(facts)
    srv = stand_in(assistant)
    col = ROOT / "Columbus"
    env = dict(os.environ, HOME=str(ROOT),
               COLUMBUS_APP_API_KEY="sk-ant-demo", COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{srv.server_port}",
               COLUMBUS_SERVER=str(GATEWAY), COLUMBUS_APP_TEST_CONFIRM="allow",
               COLUMBUS_POLICY=str(col / "policy.yaml"), COLUMBUS_CONNECTIONS=str(col / "connections"),
               COLUMBUS_WORKSPACES=str(col / "workspaces"), COLUMBUS_AUDIT_DIR=str(col / "audit"),
               COLUMBUS_LIVE_DIR=str(col / "live"), COLUMBUS_TOOLS=str(col / "tools"),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", XDG_CONFIG_HOME=str(ROOT / ".config"), XDG_DATA_HOME=str(ROOT / ".local/share"),
               COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"), COLUMBUS_LOG="warn")
    d, proc = guard.driver(env, APP)
    s = Session(theme, frames)
    s.d = d
    sfx = f"-{theme}"
    try:
        d.set_window_size(W, H)
        wait = WebDriverWait(d, 60)
        wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))
        d.execute_script("window.__TAURI__.core.invoke('prefs_set', {prefs: {theme: arguments[0], basemap: true, skipped: ['ai','engine','connection'], setup_done: true}})", theme)
        d.refresh()
        wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))

        step(f"{theme}: the chat")
        wait.until(EC.element_to_be_clickable((By.CSS_SELECTOR, "a.btn.primary[href='#/chats/fresh']"))).click()
        box = wait.until(EC.visibility_of_element_located((By.ID, "text")))
        time.sleep(2)
        s.frame(1.5)
        words = QUESTION.split(" ")
        for i in range(0, len(words), 6):
            box.send_keys(" ".join(words[i:i + 6]) + (" " if i + 6 < len(words) else ""))
            s.frame(0.35)
        s.frame(1.0)
        d.find_element(By.ID, "send").click()
        frame_el = WebDriverWait(d, 60).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#chatview .outpanel iframe")))

        def in_map(fn):
            d.switch_to.frame(frame_el)
            try:
                return fn()
            finally:
                d.switch_to.default_content()

        # The steps arrive; each layer goes on the map as it comes.
        seen = 0

        def progress(drv):
            nonlocal seen
            n = in_map(lambda: drv.execute_script("return document.querySelectorAll('li.out').length"))
            if n > seen:
                seen = n
                in_map(lambda: layers_on(drv))
            return "lives" in drv.find_element(By.ID, "log").text

        s.frames_until(progress, limit=180)
        in_map(lambda: layers_on(d))
        time.sleep(3)  # basemap tiles
        d.execute_script("const l = document.getElementById('log'); l.scrollTop = l.scrollHeight;")
        s.frame(1.0)
        s.frame(3.5)
        s.shot(f"hero{sfx}", 400, settle=2)

        step(f"{theme}: checks and provenance (map first)")
        d.find_element(By.XPATH, "//button[normalize-space()='Map first']").click()
        time.sleep(1.5)
        in_map(lambda: d.execute_script("""
            const b = [...document.querySelectorAll('li.out .out-name')].pop(); if (b) b.click();"""))
        time.sleep(2)
        s.shot(f"tour-checks{sfx}", 250, size=(1280, 800))
        if theme == "light":
            s.shot("chat-map-first", 250, size=(1280, 800), settle=0.2)
        d.find_element(By.XPATH, "//button[normalize-space()='Chat first']").click()
        time.sleep(1)

        step(f"{theme}: an export waits for the person")
        box.send_keys(EXPORT_ASK)
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
        s.frame(1.0)
        d.find_element(By.ID, "send").click()
        s.frames_until(lambda drv: drv.find_elements(By.CSS_SELECTOR, ".ask"), limit=90)
        card = d.find_element(By.CSS_SELECTOR, ".ask")
        allow = card.find_element(By.XPATH, ".//button[normalize-space()='Allow once']")
        WebDriverWait(d, 5).until(lambda drv: allow.is_enabled())
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", card)
        s.frame(3.5)
        s.shot(f"tour-approval{sfx}", 250, size=(1280, 800))
        allow.click()
        s.frames_until(lambda drv: "Exported" in drv.find_element(By.ID, "log").text and "provenance file" in drv.find_element(By.ID, "log").text, limit=60)
        d.execute_script("const l = document.getElementById('log'); l.scrollTop = l.scrollHeight;")
        s.frame(4.0)
        if theme == "light":
            s.shot("chat", 250, size=(1280, 800))

        step(f"{theme}: a workflow, approved and run")
        d.execute_script("location.hash = '#/workflows'")
        WebDriverWait(d, 30).until(lambda drv: drv.find_elements(By.ID, "wf-waiting") and "Weekly IH 35" in drv.find_element(By.ID, "wf-waiting").text)
        d.find_element(By.XPATH, "//ul[@id='wf-waiting']//button[normalize-space()='Review']").click()
        wait.until(EC.visibility_of_element_located((By.ID, "wf-review")))
        if theme == "light":
            s.shot("workflow-review", 250, size=(1280, 800))
        d.find_element(By.ID, "wf-review-install").click()
        WebDriverWait(d, 20).until(lambda drv: "Weekly IH 35" in drv.find_element(By.ID, "wf-installed").text)
        d.find_element(By.XPATH, "//ul[@id='wf-installed']//button[normalize-space()='Run now']").click()
        WebDriverWait(d, 180).until(lambda drv: drv.find_elements(By.ID, "wf-report") and "Succeeded" in drv.find_element(By.ID, "wf-report").text)
        d.execute_script("document.getElementById('wf-runs').scrollIntoView({block: 'start'})")
        s.shot(f"tour-workflow-run{sfx}", 250, size=(1280, 800))

        step(f"{theme}: connections")
        d.execute_script("location.hash = '#/connections'")
        WebDriverWait(d, 30).until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#connections li.conn") and len(drv.find_elements(By.CSS_SELECTOR, "#connections li.conn")) >= 5)
        s.shot(f"tour-connections{sfx}", 250, size=(1280, 800))

        if theme == "light":
            step("light: the other pages, for docs/app-tour.md")
            d.execute_script("location.hash = '#/tools'")
            WebDriverWait(d, 20).until(lambda drv: drv.find_elements(By.ID, "tools-waiting") and "Crashes by year" in drv.find_element(By.ID, "tools-waiting").text)
            d.find_element(By.XPATH, "//ul[@id='tools-waiting']//button[normalize-space()='Review']").click()
            wait.until(EC.visibility_of_element_located((By.ID, "tool-review")))
            s.shot("tool-review", 250, size=(1280, 800))
            d.find_element(By.ID, "review-close").click()
            for page, name in [("#/home", "home"), ("#/documents", "documents"), ("#/activity/audit", "audit"),
                               ("#/activity/approvals", "approvals"), ("#/settings/ai", "settings")]:
                d.execute_script("location.hash = arguments[0]", page)
                time.sleep(1.5)
                s.shot(name, 250, size=(1280, 800))
            # The old name of the connections picture, for the app tour.
            shutil.copyfile(OUT / "tour-connections-light.png", OUT / "connections.png")
            shutil.copyfile(OUT / "tour-approval-light.png", OUT / "approval.png")
            shutil.copyfile(OUT / "tour-workflow-run-light.png", OUT / "workflow-run.png")
    finally:
        guard.stop(d, proc)
        srv.shutdown()


def main():
    guard.start("README pictures", minutes=25)
    OUT.mkdir(parents=True, exist_ok=True)
    if FRAMES.exists():
        shutil.rmtree(FRAMES)
    FRAMES.mkdir(parents=True)
    for theme in ARGS.themes.split(","):
        facts = prepare(ROOT)
        print("facts:", json.dumps(facts))
        frames = [] if theme == "light" else None
        run(theme, facts, frames)
        if frames is not None:
            # ffmpeg's concat list: each frame and how long it shows.
            lines = []
            for name, secs in frames:
                lines += [f"file '{name}'", f"duration {secs:.2f}"]
            lines.append(f"file '{frames[-1][0]}'")
            (FRAMES / "frames.txt").write_text("\n".join(lines) + "\n")
            print(f"frames: {len(frames)}, {sum(s for _, s in frames):.1f} s")
    print("screenshots: ok")


if __name__ == "__main__":
    main()
