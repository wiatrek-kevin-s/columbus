"""Workflows in the main window, end to end on Linux through WebDriver.

A workflow an assistant proposed (filed as propose_workflow files it) is
read in plain words and in full, approved (a debug build stands in for
Touch ID / Windows Hello with COLUMBUS_APP_TEST_CONFIRM), run with Run now
by the real server, and its report read; its result is exported to the
granted folder. Another proposal is declined, and with the person
declining to confirm nothing is approved. Linux has no scheduler yet, so
the page says so and Run now is the way to run one.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/workflows.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

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


def propose(root, text):
    """A proposal as propose_workflow files it."""
    wid = text.split("\n", 1)[0].split(": ", 1)[1]
    d = root / "workflows" / "proposals" / wid
    d.mkdir(parents=True)
    (d / "workflow.yaml").write_text(text)
    (d / "proposal.json").write_text(json.dumps({"id": wid, "proposed": "2026-11-06T08:00:00-06:00"}))


def env_for(root, confirm):
    return dict(os.environ,
                COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
                COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
                COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_TOOLS=str(root / "tools"),
                COLUMBUS_SERVER=str(GATEWAY), COLUMBUS_COMPANION="off",
                COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
                COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn",
                XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                COLUMBUS_APP_TEST_CONFIRM=confirm)


def session(root, confirm):
    proc = subprocess.Popen(["tauri-driver"], env=env_for(root, confirm), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    opts = webdriver.common.options.ArgOptions()
    opts.set_capability("tauri:options", {"application": str(APP)})
    opts.set_capability("browserName", "wry")
    d = webdriver.Remote("http://127.0.0.1:4444", options=opts)
    wait = WebDriverWait(d, 30)
    wait.until(lambda drv: drv.execute_script("return !!document.getElementById('page-title')"))
    d.execute_script("location.hash = '#/workflows'")
    wait.until(EC.presence_of_element_located((By.ID, "wf-waiting")))
    time.sleep(0.5)
    return d, proc, wait


def click(d, how, what):
    e = d.find_element(how, what)
    d.execute_script("arguments[0].scrollIntoView({block: 'center'})", e)
    e.click()


def text(d, eid):
    return d.execute_script("return document.getElementById(arguments[0]).textContent", eid)


def main():
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[box(-97.8, 30.2, -97.7, 30.3), box(-97.7, 30.2, -97.6, 30.3)], crs="EPSG:4326") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    for d in ["connections", "outbox"]:
        (root / d).mkdir()
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")
    (root / "policy.yaml").write_text(
        "connection_types: [gpkg]\nallow_workflows: true\n"
        f"exports:\n  - {{ id: outbox, path: {json.dumps(str(root / 'outbox'))}, granted: true }}\n")
    propose(root, WORKFLOW)
    propose(root, WORKFLOW.replace("id: tract-export", "id: tract-copy").replace("name: Tract export", "name: Tract copy"))

    # The person declines to confirm: nothing is approved.
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
        d.quit()
        proc.terminate()
        # The app is single-instance: the next session needs this one gone.
        subprocess.run(["pkill", "-f", str(APP)], check=False)
        time.sleep(1)

    d, proc, wait = session(root, "allow")
    try:
        # No scheduler on Linux: the page says so.
        assert d.find_element(By.ID, "wf-nosched").is_displayed()
        click(d, By.XPATH, "//ul[@id='wf-waiting']/li[.//b[text()='Tract export']]//button[normalize-space()='Review']")
        dlg = wait.until(EC.visibility_of_element_located((By.ID, "wf-review")))
        plain = text(d, "wf-review-plain")
        assert "every weekday (Monday to Friday) at 06:00" in plain, plain
        assert "Reads only: tiger" in plain, plain
        assert "export folder outbox" in plain, plain
        assert "query_features" in text(d, "wf-review-file")
        d.save_screenshot(str(OUT / "workflows-review.png"))
        click(d, By.ID, "wf-review-install")
        wait.until(lambda drv: not dlg.is_displayed())
        wait.until(lambda drv: "Tract export" in text(drv, "wf-installed"))
        assert "tract-export" in (root / "workflows" / "approved.yaml").read_text()

        # Run now: the real server runs it, and its report shows.
        click(d, By.XPATH, "//ul[@id='wf-installed']/li[@data-id='tract-export']//button[normalize-space()='Run now']")
        try:
            WebDriverWait(d, 120).until(lambda drv: drv.find_elements(By.ID, "wf-report") and "Succeeded" in text(drv, "wf-report"))
        except Exception:
            d.save_screenshot(str(OUT / "workflows-run-failed.png"))
            raise SystemExit("the run did not succeed: " + d.execute_script("return (document.getElementById('wf-error')||{}).textContent + ' | ' + ((document.getElementById('wf-report')||{}).textContent || '')"))
        report = text(d, "wf-report")
        assert "exported to the outbox folder" in report, report
        assert any(p.name.startswith("tract-export-tracts-") for p in (root / "outbox").iterdir()), list((root / "outbox").iterdir())
        d.save_screenshot(str(OUT / "workflows-run.png"))
        wait.until(lambda drv: "Last run" in text(drv, "wf-installed"))

        # The other is declined.
        click(d, By.XPATH, "//ul[@id='wf-waiting']/li[.//b[text()='Tract copy']]//button[normalize-space()='Decline']")
        wait.until(lambda drv: "Tract copy" not in text(drv, "wf-waiting"))
        assert not (root / "workflows" / "proposals" / "tract-copy").exists()

        # Removing it: off, its runs kept.
        click(d, By.XPATH, "//ul[@id='wf-installed']/li[@data-id='tract-export']//button[normalize-space()='Remove']")
        wait.until(lambda drv: "Tract export" not in text(drv, "wf-installed"))
        assert "tract-export" not in (root / "workflows" / "approved.yaml").read_text()
        assert list((root / "workflows" / "runs" / "tract-export").iterdir()), "runs are kept"
        print("desktop workflows e2e: ok")
    finally:
        d.quit()
        proc.terminate()


if __name__ == "__main__":
    main()
