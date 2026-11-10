"""Data groups in the main window, end to end on Linux through WebDriver.

A data group is made on the Data groups page (two datasets from a
connection, one with a filter), its file is written where the server reads
it, a project attaches it, and deleting the group removes its file.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/datagroups.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import os
import subprocess
import sys
import tempfile
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


def text(d, eid):
    # "" while the page is still drawing.
    return d.execute_script("const e = document.getElementById(arguments[0]); return e ? e.textContent : ''", eid)


def click(d, how, what):
    e = d.find_element(how, what)
    d.execute_script("arguments[0].scrollIntoView({block: 'center'})", e)
    e.click()


def main():
    guard.start("desktop data groups e2e", minutes=10)
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    (root / "connections").mkdir()
    for cid in ["tiger"]:
        (root / "connections" / f"{cid}.yaml").write_text(
            f"id: {cid}\ntype: gpkg\npath: {root / 'x.gpkg'}\ndatasets:\n  allow: [\"*\"]\n")
    (root / "policy.yaml").write_text(
        f"connection_types: [gpkg]\nexports:\n  - {{ id: outbox, path: \"{root / 'outbox'}\", granted: true }}\n")
    env = dict(os.environ,
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_TOOLS=str(root / "tools"),
               COLUMBUS_SERVER=str(GATEWAY), COLUMBUS_COMPANION="off",
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn",
               XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"))
    step("the app")
    d, proc = guard.driver(env, APP)
    wait = WebDriverWait(d, 30)
    try:
        guard.ready(d, wait)
        d.execute_script("location.hash = '#/datagroups'")
        wait.until(EC.element_to_be_clickable((By.ID, "dg-new"))).click()
        wait.until(EC.visibility_of_element_located((By.ID, "dg-name"))).send_keys("Texas tracts")
        d.find_element(By.ID, "dg-description").send_keys("The state's tracts and counties.")
        d.find_element(By.CSS_SELECTOR, "#dg-rows tr:nth-child(1) .dg-dataset").send_keys("tracts")
        d.find_element(By.CSS_SELECTOR, "#dg-rows tr:nth-child(1) .dg-filter").send_keys("STATEFP = '48'")
        click(d, By.ID, "dg-add")
        d.find_element(By.CSS_SELECTOR, "#dg-rows tr:nth-child(2) .dg-dataset").send_keys("counties")
        d.save_screenshot(str(OUT / "datagroups-new.png"))
        click(d, By.ID, "dg-save")
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/datagroups/texas-tracts")
        f = root / "data-groups" / "texas-tracts.yaml"
        saved = f.read_text()
        assert "dataset: tracts" in saved and "STATEFP = '48'" in saved and "dataset: counties" in saved, saved

        step("A filter Columbus wouldn't run is refused, with the reason")
        box = wait.until(EC.visibility_of_element_located((By.CSS_SELECTOR, "#dg-rows tr:nth-child(2) .dg-filter")))
        box.send_keys("1=1; DROP TABLE x")
        click(d, By.ID, "dg-save")
        wait.until(lambda drv: "not one Columbus runs" in text(drv, "dg-error"))
        assert "DROP" not in f.read_text()

        step("A project attaches it")
        d.execute_script("location.hash = '#/projects/new'")
        wait.until(EC.visibility_of_element_located((By.ID, "pj-name"))).send_keys("Tract work")
        click(d, By.ID, "pj-g-texas-tracts")
        click(d, By.ID, "pj-save")
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/projects/tract-work")
        p = (root / "projects" / "tract-work" / "project.yaml").read_text()
        assert "data_groups:" in p and "- texas-tracts" in p, p

        step("The list says which project uses it; deleting removes its file")
        d.execute_script("location.hash = '#/datagroups'")
        wait.until(lambda drv: "used by Tract work" in text(drv, "dg-list"))
        d.save_screenshot(str(OUT / "datagroups-list.png"))
        click(d, By.XPATH, "//ul[@id='dg-list']/li[@data-id='texas-tracts']//a")
        wait.until(EC.element_to_be_clickable((By.ID, "dg-delete"))).click()
        wait.until(EC.element_to_be_clickable((By.XPATH, "//dialog//button[normalize-space()='Delete data group']"))).click()
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/datagroups")
        assert not f.exists(), "the group's file is removed"
        print("desktop data groups e2e: ok")
    except Exception:
        d.save_screenshot(str(OUT / "datagroups-failed.png"))
        raise
    finally:
        guard.stop(d, proc)
        subprocess.run(["pkill", "-f", str(APP)], check=False)


if __name__ == "__main__":
    main()
