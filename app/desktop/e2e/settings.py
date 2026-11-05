"""The settings window, end to end on Linux through WebDriver.

Adds a GeoPackage connection, makes it read-write, turns edits on and
removes it, checking each file as the server reads it; then, with the
person declining to confirm, checks that nothing that widens access is
saved. A debug build stands in for Touch ID / Windows Hello with
COLUMBUS_APP_TEST_CONFIRM (allow or deny).

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/settings.py \\
        app/desktop/src-tauri/target/debug/columbus-desktop
"""

import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import yaml
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

REPO = Path(__file__).resolve().parents[3]
APP = Path(sys.argv[1]).resolve()
OUT = Path(__file__).resolve().parent / "out"
POLICY = """# The organization's settings.
connection_types: [gpkg, postgis]
allow_scripts: false   # run_python
allow_edits: false     # edits
"""


def session(root, confirm):
    env = dict(os.environ, COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_APP_TEST_CONFIRM=confirm,
               XDG_CONFIG_HOME=str(root / "config"))
    proc = subprocess.Popen(["tauri-driver"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    opts = webdriver.common.options.ArgOptions()
    opts.set_capability("tauri:options", {"application": str(APP)})
    opts.set_capability("browserName", "wry")
    d = webdriver.Remote("http://127.0.0.1:4444", options=opts)
    wait = WebDriverWait(d, 30)
    wait.until(EC.element_to_be_clickable((By.ID, "settings"))).click()
    wait.until(lambda drv: len(drv.window_handles) > 1)
    main = d.current_window_handle
    d.switch_to.window([h for h in d.window_handles if h != main][0])
    wait.until(EC.presence_of_element_located((By.ID, "add")))
    return d, proc, wait


def fill(d, name, value):
    # Set directly: Selenium would read an existing path as a file upload.
    d.execute_script("document.querySelector(`[name='${arguments[0]}']`).value = arguments[1];", name, value)


def main():
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    (root / "connections").mkdir()
    (root / "policy.yaml").write_text(POLICY)
    gpkg = root / "roads.gpkg"
    gpkg.write_bytes(b"not read by this test")

    d, proc, wait = session(root, "allow")
    try:
        # Add a GeoPackage connection.
        d.find_element(By.ID, "add").click()
        wait.until(EC.visibility_of_element_located((By.NAME, "conn_id")))
        fill(d, "conn_id", "roads")
        fill(d, "path", str(gpkg))
        fill(d, "deny", "*audit*")
        fill(d, "row_filter", "district = 14")
        d.save_screenshot(str(OUT / "settings-add.png"))
        d.find_element(By.ID, "save").click()
        wait.until(lambda drv: drv.find_element(By.ID, "restart").is_displayed())
        c = yaml.safe_load((root / "connections" / "roads.yaml").read_text())
        assert c["id"] == "roads" and c["type"] == "gpkg" and c["path"] == str(gpkg), c
        assert c["mode"] == "read" and c["datasets"] == {"allow": ["*"], "deny": ["*audit*"]}, c
        assert c["row_filter"] == "district = 14", c
        wait.until(lambda drv: "roads" in drv.find_element(By.ID, "connections").text)

        # Make it read-write, and turn edits on: the rest of the policy file stays.
        d.find_element(By.XPATH, "//ul[@id='connections']//button[normalize-space()='Edit']").click()
        wait.until(EC.visibility_of_element_located((By.NAME, "mode")))
        d.execute_script("const s = document.querySelector('[name=mode]'); s.value = 'read-write';")
        d.find_element(By.ID, "save").click()
        wait.until(lambda drv: "Read and write" in drv.find_element(By.ID, "connections").text)
        assert yaml.safe_load((root / "connections" / "roads.yaml").read_text())["mode"] == "read-write"
        d.find_element(By.ID, "allow_edits").click()
        wait.until(lambda drv: "allow_edits: true" in (root / "policy.yaml").read_text())
        text = (root / "policy.yaml").read_text()
        assert text == POLICY.replace("allow_edits: false     # edits", "allow_edits: true     # edits"), text
        time.sleep(0.5)
        d.save_screenshot(str(OUT / "settings.png"))

        # Removing needs no confirmation.
        d.find_element(By.XPATH, "//ul[@id='connections']//button[normalize-space()='Edit']").click()
        wait.until(EC.visibility_of_element_located((By.ID, "delete"))).click()
        wait.until(lambda drv: not (root / "connections" / "roads.yaml").exists())
    finally:
        d.quit()
        proc.terminate()
        proc.wait()

    # The person declines: nothing that widens access is saved.
    d, proc, wait = session(root, "deny")
    try:
        d.find_element(By.ID, "add").click()
        wait.until(EC.visibility_of_element_located((By.NAME, "conn_id")))
        fill(d, "conn_id", "roads2")
        fill(d, "path", str(gpkg))
        d.find_element(By.ID, "save").click()
        wait.until(lambda drv: drv.find_element(By.ID, "form-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "form-error").text
        assert not (root / "connections" / "roads2.yaml").exists()
        d.find_element(By.ID, "cancel").click()
        box = d.find_element(By.ID, "allow_scripts")
        box.click()
        wait.until(lambda drv: drv.find_element(By.ID, "error").is_displayed())
        assert "allow_scripts: false" in (root / "policy.yaml").read_text()
        assert not d.find_element(By.ID, "allow_scripts").is_selected()
        # Turning a switch off is a tightening: no confirmation needed.
        d.find_element(By.ID, "allow_edits").click()
        wait.until(lambda drv: "allow_edits: false" in (root / "policy.yaml").read_text())
        print("desktop settings e2e: ok")
    finally:
        d.quit()
        proc.terminate()


if __name__ == "__main__":
    main()
