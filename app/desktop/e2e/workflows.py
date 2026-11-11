"""Workflows in the main window, end to end on Linux through WebDriver.

A workflow an assistant proposed (filed as propose_workflow files it) is
read in plain words and in full, approved (a debug build stands in for
Touch ID / Windows Hello with COLUMBUS_APP_TEST_CONFIRM), run with Run now
by the real server, and its report read; its result is exported to the
granted folder. From the run's report, Open in a chat starts a chat with
the run's layer on its map and in its table and the report in the
assistant's context; a stand-in AI service then exports that layer to a
folder that isn't granted, which asks for the person's approval as usual,
and the run's own files stay byte-for-byte as they were (REQ-27, R-20).
Another proposal is declined, and with the person
declining to confirm nothing is approved. Linux has no scheduler yet, so
the page says so and Run now is the way to run one.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/workflows.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import hashlib
import json
import os
import re
import threading
import subprocess
import sys
import tempfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC

import guard
from guard import step

WebDriverWait = guard.Wait  # its time-outs name the step that was waiting

REPO = Path(__file__).resolve().parents[3]
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
OUT = Path(__file__).resolve().parent / "out"

WORKFLOW = """id: tract-export
name: Tract export
purpose: Export the tracts every weekday for the district share.
schedule: { days: weekdays, at: "06:00" }
scope: { connections: [tiger] }
steps:
  - id: tracts
    tool: query_features
    args: { connection: tiger, dataset: tracts }
  - id: checked
    tool: verify
    args: { layer: "${tracts}" }
outputs:
  folder: { export: outbox, steps: [tracts] }
"""


SEEN = []


class Stand(BaseHTTPRequestHandler):
    """A stand-in for the AI service: the chat opened from the run exports
    the run's layer (named in its system prompt) to the review folder."""

    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        SEEN.append(body)
        last = body["messages"][-1]
        replied = isinstance(last["content"], list) and any(c.get("type") == "tool_result" for c in last["content"])
        if not replied:
            system = "".join(b["text"] for b in body["system"])
            handle = re.search(r"ws://\S+/results/tract-export-tracts\.gpkg", system).group(0)
            nxt = {"content": [{"type": "text", "text": "I'll export the reviewed tracts."},
                               {"type": "tool_use", "id": "x1", "name": "export_output",
                                "input": {"handle": handle, "folder": "review", "name": "reviewed-tracts"}}],
                   "stop_reason": "tool_use"}
        else:
            nxt = {"content": [{"type": "text", "text": "Exported the reviewed tracts."}], "stop_reason": "end_turn"}
        data = json.dumps(nxt).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def tree_hashes(*dirs):
    """Every file under these folders, with its SHA-256."""
    out = {}
    for d in dirs:
        for p in sorted(Path(d).rglob("*")):
            if p.is_file():
                out[str(p)] = hashlib.sha256(p.read_bytes()).hexdigest()
    return out


def propose(root, text):
    """A proposal as propose_workflow files it."""
    wid = text.split("\n", 1)[0].split(": ", 1)[1]
    d = root / "workflows" / "proposals" / wid
    d.mkdir(parents=True)
    (d / "workflow.yaml").write_text(text)
    (d / "proposal.json").write_text(json.dumps({"id": wid, "proposed": "2026-11-06T08:00:00-06:00"}))


STAND_PORT = [0]


def env_for(root, confirm):
    return dict(os.environ,
                COLUMBUS_APP_API_KEY="sk-ant-test-0123456789abcdef",
                COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{STAND_PORT[0]}",
                COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"),
                COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
                COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
                COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_TOOLS=str(root / "tools"),
                COLUMBUS_SERVER=str(GATEWAY),
                COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
                COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn",
                XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                COLUMBUS_APP_TEST_CONFIRM=confirm)


def session(root, confirm):
    d, proc = guard.driver(env_for(root, confirm), APP)
    wait = WebDriverWait(d, 30)
    guard.ready(d, wait)
    d.execute_script("location.hash = '#/workflows'")
    wait.until(EC.presence_of_element_located((By.ID, "wf-waiting")))
    time.sleep(0.5)
    return d, proc, wait


def click(d, how, what):
    e = d.find_element(how, what)
    d.execute_script("arguments[0].scrollIntoView({block: 'center'})", e)
    e.click()


def text(d, eid):
    # "" while the page is still drawing (a page is rendered after the hash changes).
    return d.execute_script("const e = document.getElementById(arguments[0]); return e ? e.textContent : ''", eid)


def main():
    guard.start("desktop workflows e2e", minutes=10)
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[box(-97.8, 30.2, -97.7, 30.3), box(-97.7, 30.2, -97.6, 30.3)], crs="EPSG:4326") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    for d in ["connections", "outbox", "review"]:
        (root / d).mkdir()
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")
    (root / "policy.yaml").write_text(
        "connection_types: [gpkg]\nallow_workflows: true\n"
        f"exports:\n  - {{ id: outbox, path: {json.dumps(str(root / 'outbox'))}, granted: true }}\n"
        f"  - {{ id: review, path: {json.dumps(str(root / 'review'))} }}\n")
    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()
    STAND_PORT[0] = stand.server_port
    propose(root, WORKFLOW)
    propose(root, WORKFLOW.replace("id: tract-export", "id: tract-copy").replace("name: Tract export", "name: Tract copy"))

    # The person declines to confirm: nothing is approved.
    step("the app, confirming denied: nothing is approved")
    d, proc, wait = session(root, "deny")
    try:
        wait.until(lambda drv: "Tract export" in text(drv, "wf-waiting"))
        click(d, By.XPATH, "//ul[@id='wf-waiting']/li[.//b[text()='Tract export']]//button[normalize-space()='Review']")
        wait.until(EC.visibility_of_element_located((By.ID, "wf-review")))
        click(d, By.ID, "wf-review-install")
        wait.until(lambda drv: not drv.find_element(By.ID, "wf-review-error").get_attribute("hidden"))
        assert not (root / "workflows" / "approved.yaml").exists(), "nothing approved without the person"
        click(d, By.ID, "wf-review-close")
    finally:
        step("closing the first app")
        guard.stop(d, proc)
        # The app is single-instance: the next session needs this one gone
        # (the next session also waits until it has let go of its name).
        subprocess.run(["pkill", "-f", str(APP)], check=False)

    step("the app again, confirming allowed")
    d, proc, wait = session(root, "allow")
    try:
        step("No scheduler on Linux: the page says so")
        assert d.find_element(By.ID, "wf-nosched").is_displayed()
        click(d, By.XPATH, "//ul[@id='wf-waiting']/li[.//b[text()='Tract export']]//button[normalize-space()='Review']")
        dlg = wait.until(EC.visibility_of_element_located((By.ID, "wf-review")))
        plain = text(d, "wf-review-plain")
        assert "every weekday (Monday to Friday) at 06:00" in plain, plain
        assert "Reads only: tiger" in plain, plain
        assert "export folder outbox" in plain, plain
        assert "query_features" in text(d, "wf-review-file")
        # R-104: where the app cannot confirm who is at the keyboard, the
        # Approve button says so, in words.
        tip = d.find_element(By.ID, "wf-review-install").get_attribute("title")
        assert "asks for not available" not in tip, tip
        assert tip == ("Approve it. On this system the app cannot confirm it is you (there is no Touch ID or Windows Hello), "
                       "so a workflow cannot be approved here."), tip
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "workflows-review.png"))
        click(d, By.ID, "wf-review-install")
        wait.until(lambda drv: not dlg.is_displayed())
        wait.until(lambda drv: "Tract export" in text(drv, "wf-installed"))
        assert "tract-export" in (root / "workflows" / "approved.yaml").read_text()
        step("Open a shared file is offered on the list")
        # Open a shared file is offered on the list (the file dialog itself
        # is the system's).
        assert d.find_element(By.ID, "wf-open-shared").is_displayed()
        # A search that matches nothing says so, and no more.
        find = d.find_element(By.CSS_SELECTOR, "input[aria-label='Search workflows']")
        find.send_keys("zzzz")
        wait.until(lambda drv: drv.find_element(By.ID, "wf-no-installed").is_displayed())
        assert d.find_element(By.ID, "wf-no-installed").text == "Nothing matches.", d.find_element(By.ID, "wf-no-installed").text
        d.execute_script("arguments[0].value = ''; arguments[0].dispatchEvent(new Event('input', {bubbles: true}))", find)
        wait.until(lambda drv: not drv.find_element(By.ID, "wf-no-installed").is_displayed())

        step("The list says when it runs; its own page says so in a sentence")
        row = d.execute_script("return document.querySelector(\"#wf-installed li[data-id='tract-export']\").textContent")
        assert "weekdays at 06:00" in row and "not run yet" in row, row
        assert "Runs by itself" in text(d, "wf-installed")
        click(d, By.XPATH, "//ul[@id='wf-installed']/li[@data-id='tract-export']//a")
        wait.until(EC.element_to_be_clickable((By.ID, "wf-run-now")))
        assert "Weekdays at 06:00." in text(d, "page-host"), text(d, "page-host")
        # No scheduler on Linux: its page says so too.
        assert "Schedules are not available on this system yet" in text(d, "wf-entry"), text(d, "wf-entry")
        step("Run now: the real server runs it, and its report shows")
        click(d, By.ID, "wf-run-now")
        try:
            WebDriverWait(d, 120).until(lambda drv: drv.find_elements(By.ID, "wf-report") and "Succeeded" in text(drv, "wf-report"))
        except Exception:
            d.save_screenshot(str(OUT / "workflows-run-failed.png"))
            raise SystemExit("the run did not succeed: " + d.execute_script("return (document.getElementById('wf-error')||{}).textContent + ' | ' + ((document.getElementById('wf-report')||{}).textContent || '')"))
        report = text(d, "wf-report")
        assert "exported to the outbox folder" in report, report
        assert any(p.name.startswith("tract-export-tracts-") for p in (root / "outbox").iterdir()), list((root / "outbox").iterdir())
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "workflows-run.png"))

        step("Open in a chat: the run's layer on the chat's map and table, its report for the assistant")
        run_id = d.execute_script("return decodeURIComponent(location.hash.split('/').pop())")
        rec = json.loads((root / "workflows" / "runs" / "tract-export" / run_id / "run.json").read_text())
        run_files = [root / "workflows" / "runs" / "tract-export" / run_id, Path(rec["workspace"]), root / "outbox"]
        before = tree_hashes(*run_files)
        click(d, By.ID, "wf-open-chat")
        WebDriverWait(d, 30).until(EC.presence_of_element_located((By.ID, "chat-run-note")))
        try:
            frame = WebDriverWait(d, 90).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#stage iframe")))
        except Exception:
            d.save_screenshot(str(OUT / "workflows-open-failed.png"))
            raise SystemExit("the chat did not open with the run: " + d.execute_script(
                "return (document.getElementById('log')||{}).textContent + ' | ' + ((document.getElementById('stage')||{}).textContent || '')"))
        assert "Tract export run" in text(d, "chat-sub"), text(d, "chat-sub")
        d.switch_to.frame(frame)
        try:
            # On the map: drawn (the layer's own colour swatch is checked on);
            # in the table: its rows.
            WebDriverWait(d, 60).until(lambda drv: "tract-export-tracts.gpkg" in drv.execute_script("return document.body.textContent"))
            WebDriverWait(d, 60).until(lambda drv: drv.execute_script(
                "const t = document.querySelector('section.table'); return !!t && t.textContent.includes('NAME') && t.textContent.includes('B')"))
            on_map = d.execute_script("return [...document.querySelectorAll('aside.outputs input[type=checkbox]')].filter(c => c.checked).length")
            assert on_map >= 1, "the run's layer is on the map"
        finally:
            d.switch_to.default_content()
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "workflows-open-in-chat.png"))

        step("The export from that chat asks for approval as usual; allowed, it lands")
        box = d.find_element(By.ID, "text")
        # Focus back in the chat's own page (it was in the map's frame).
        box.click()
        box.send_keys("These look right. Export them for review.")
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
        d.find_element(By.ID, "send").click()
        try:
            card = WebDriverWait(d, 90).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".ask")))
        except Exception:
            d.save_screenshot(str(OUT / "workflows-chat-export-failed.png"))
            raise SystemExit("no approval was asked: " + d.execute_script("return document.getElementById('log').textContent"))
        assert "review" in card.text, card.text
        allow = card.find_element(By.XPATH, ".//button[normalize-space()='Allow once']")
        WebDriverWait(d, 5).until(lambda drv: allow.is_enabled())
        allow.click()
        WebDriverWait(d, 60).until(lambda drv: "Exported the reviewed tracts." in drv.find_element(By.ID, "log").text)
        assert (root / "review" / "reviewed-tracts.gpkg").exists(), "the export from the chat happened"
        system = "".join(b["text"] for b in SEEN[0]["system"])
        assert "# Tract export" in system and "tract-export-tracts.gpkg" in system, "the report is in context"
        assert tree_hashes(*run_files) == before, "the run's files are unchanged"
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "workflows-chat-export.png"))
        d.execute_script(f"location.hash = '#/workflows/tract-export/{run_id}'")
        WebDriverWait(d, 30).until(lambda drv: drv.find_elements(By.ID, "wf-report") and "Succeeded" in text(drv, "wf-report"))

        step("The report is its own page; the workflow's page lists the run")
        click(d, By.ID, "wf-back")
        wait.until(lambda drv: drv.find_elements(By.XPATH, "//ul[@id='wf-run-list']/li"))
        assert "Started by you" in text(d, "wf-run-list")
        d.execute_script("location.hash = '#/workflows'")
        wait.until(lambda drv: "last run" in text(drv, "wf-installed"))

        step("The other is declined")
        click(d, By.XPATH, "//ul[@id='wf-waiting']/li[.//b[text()='Tract copy']]//button[normalize-space()='Decline']")
        wait.until(lambda drv: "Tract copy" not in text(drv, "wf-waiting"))
        assert not (root / "workflows" / "proposals" / "tract-copy").exists()

        step("Removing it, from its own page: off, its runs kept")
        click(d, By.XPATH, "//ul[@id='wf-installed']/li[@data-id='tract-export']//a")
        step("Share is offered for an approved workflow, under More")
        wait.until(EC.element_to_be_clickable((By.CSS_SELECTOR, "#main .wf-more"))).click()
        assert wait.until(EC.presence_of_element_located((By.ID, "wf-share"))).is_enabled()
        wait.until(EC.element_to_be_clickable((By.ID, "wf-remove"))).click()
        wait.until(lambda drv: drv.find_elements(By.ID, "wf-installed") and "Tract export" not in text(drv, "wf-installed"))
        assert "tract-export" not in (root / "workflows" / "approved.yaml").read_text()
        assert list((root / "workflows" / "runs" / "tract-export").iterdir()), "runs are kept"
        print("desktop workflows e2e: ok")
    finally:
        guard.stop(d, proc)
        stand.shutdown()


if __name__ == "__main__":
    main()
