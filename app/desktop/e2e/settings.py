"""The settings window, end to end on Linux through WebDriver.

Adds a GeoPackage connection, makes it read-write, turns edits on and
removes it, checking each file as the server reads it; turns personal
tools on, reads a tool an assistant proposed, installs one, declines
another and removes the installed one; then, with the person declining to
confirm, checks that nothing that widens access is saved (a connection, a
switch, a tool). A debug build stands in for Touch ID / Windows Hello with
COLUMBUS_APP_TEST_CONFIRM (allow or deny). A real Columbus session runs
beside the app on the same files: Test asks it to open a connection
before it is saved, and the saved notice says no restart is needed.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/settings.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import hashlib
import json
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
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
OUT = Path(__file__).resolve().parent / "out"
POLICY = """# The organization's settings.
connection_types: [gpkg, postgis]
hide_fields: ["*ssn*"]
allow_scripts: false   # run_python
allow_edits: false     # edits
"""


TOOL_YAML = """id: personal.count-rows
name: Count rows
version: 1.0.0
runtime: python
entry: tool.py
tools:
  run:
    summary: Count a table's rows.
    description: Count a table's rows and write the count as a report.
    not_for: Anything but counting.
    risk: read
    params:
      table: { type: file, kind: table, description: Any table. }
    outputs:
      report: { type: file, format: json, description: The count. }
"""
TOOL_PY = "def run(ctx):\n    return {}\n"


def propose(root, tool_id, title):
    """A proposal as propose_tool files it; returns the hash the person installs."""
    d = root / "tools" / "proposals" / tool_id
    d.mkdir(parents=True)
    y = TOOL_YAML.replace("personal.count-rows", tool_id).replace("Count rows", title)
    (d / "plugin.yaml").write_text(y)
    (d / "tool.py").write_text(TOOL_PY)
    (d / "proposal.json").write_text(json.dumps({"proposed": "2026-11-05T10:00:00+00:00",
                                                 "test": {"outputs": [{"kind": "file", "rows": None}], "checks": []}}))
    return "sha256:" + hashlib.sha256(y.encode() + b"\0" + TOOL_PY.encode()).hexdigest()


def files(root):
    return dict(os.environ, COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
                COLUMBUS_LIVE_DIR=str(root / "live"), XDG_CONFIG_HOME=str(root / "config"),
                COLUMBUS_TOOLS=str(root / "tools"))


def columbus(root):
    """A Columbus session on the same files, as Claude desktop would run it."""
    env = dict(files(root), COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn")
    proc = subprocess.Popen([str(GATEWAY), "serve"], env=env, stdin=subprocess.PIPE,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(200):
        if list((root / "live").glob("*.key")):
            return proc
        time.sleep(0.05)
    raise SystemExit("the Columbus session did not announce itself")


def session(root, confirm):
    env = dict(files(root), COLUMBUS_APP_TEST_CONFIRM=confirm)
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
    subprocess.run([sys.executable, "-c", """
import sys, geopandas as gpd
from shapely.geometry import LineString
gpd.GeoDataFrame({"RTE": ["IH0035", "US0183"], "district": [14, 12]}, geometry=[LineString([(0, 0), (1000, 0)]), LineString([(0, 0), (0, 900)])],
    crs="EPSG:3081").to_file(sys.argv[1], layer="routes", driver="GPKG", engine="pyogrio")
""", str(gpkg)], check=True)
    broken = root / "broken.gpkg"
    broken.write_bytes(b"not a GeoPackage")

    count_hash = propose(root, "personal.count-rows", "Count rows")
    propose(root, "personal.other-tool", "Other tool")

    server = columbus(root)
    d, proc, wait = session(root, "allow")
    try:
        # Add a GeoPackage connection; Test before saving.
        d.find_element(By.ID, "add").click()
        wait.until(EC.visibility_of_element_located((By.NAME, "conn_id")))
        fill(d, "conn_id", "roads")
        fill(d, "path", str(broken))
        d.find_element(By.ID, "test").click()
        wait.until(lambda drv: "could not open" in drv.find_element(By.ID, "test-result").text)
        assert "test-bad" in d.find_element(By.ID, "test-result").get_attribute("class")
        fill(d, "path", str(gpkg))
        d.find_element(By.ID, "test").click()
        wait.until(lambda drv: "Opened" in drv.find_element(By.ID, "test-result").text)
        assert "routes" in d.find_element(By.ID, "test-result").text
        d.save_screenshot(str(OUT / "settings-test.png"))
        assert not (root / "connections" / "roads.yaml").exists(), "Test saves nothing"
        fill(d, "deny", "*audit*")
        # A filter on a hidden field would reveal it, row count by row count.
        fill(d, "row_filter", "owner_ssn LIKE '4%'")
        d.find_element(By.ID, "save").click()
        wait.until(lambda drv: "a field your organization hides" in drv.find_element(By.ID, "form-error").text)
        fill(d, "row_filter", "district = 14")
        d.save_screenshot(str(OUT / "settings-add.png"))
        d.find_element(By.ID, "save").click()
        wait.until(lambda drv: drv.find_element(By.ID, "restart").is_displayed())
        # The running session picks it up by itself.
        assert "no need to restart" in d.find_element(By.ID, "restart").text, d.find_element(By.ID, "restart").text
        c = yaml.safe_load((root / "connections" / "roads.yaml").read_text())
        assert c["id"] == "roads" and c["type"] == "gpkg" and c["path"] == str(gpkg), c
        assert c["mode"] == "read" and c["datasets"] == {"allow": ["*"], "deny": ["*audit*"]}, c
        assert c["row_filter"] == "district = 14", c
        wait.until(lambda drv: "roads" in drv.find_element(By.ID, "connections").text)

        # Draft notes from the database: the running session writes them.
        d.find_element(By.XPATH, "//ul[@id='connections']//button[normalize-space()='Draft notes']").click()
        try:
            WebDriverWait(d, 60).until(lambda drv: "Drafted" in drv.find_element(By.CSS_SELECTOR, ".draft-status").text)
        except Exception:
            d.save_screenshot(str(OUT / "settings-notes-failed.png"))
            raise SystemExit("Draft notes said: " + d.find_element(By.CSS_SELECTOR, ".draft-status").get_attribute("textContent"))
        note = root / "notes" / "datasets" / "roads" / "routes.md"
        assert note.exists(), list((root / "notes").rglob("*"))
        text = note.read_text()
        assert "status: draft" in text and "RTE" in text, text
        d.save_screenshot(str(OUT / "settings-notes.png"))

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

        # Personal tools: turned on (confirmed), then a proposal is read and installed.
        d.find_element(By.ID, "allow_personal_tools").click()
        wait.until(lambda drv: "allow_personal_tools: true" in (root / "policy.yaml").read_text())
        wait.until(lambda drv: drv.find_element(By.ID, "tools-off").get_attribute("hidden") is not None)
        waiting = "//ul[@id='tools-waiting']//li[contains(., '{}')]//button[normalize-space()='{}']"
        d.find_element(By.XPATH, waiting.format("Count rows", "Review")).click()
        wait.until(EC.visibility_of_element_located((By.ID, "review-code")))
        assert "def run(ctx):" in d.find_element(By.ID, "review-code").text
        assert d.find_element(By.ID, "review-code-lines").text == "2 lines", d.find_element(By.ID, "review-code-lines").text
        assert len(d.find_elements(By.CSS_SELECTOR, "#review-code span.ln")) == 2
        assert "id: personal.count-rows" in d.find_element(By.ID, "review-manifest").text
        assert count_hash in d.find_element(By.ID, "review-name").text
        d.save_screenshot(str(OUT / "settings-tool-review.png"))
        d.find_element(By.ID, "review-install").click()
        installed = root / "tools" / "personal.count-rows"
        wait.until(lambda drv: (installed / "tool.py").exists())
        approved = yaml.safe_load((root / "tools" / "approved.yaml").read_text())
        assert approved == {"personal.count-rows": count_hash}, approved
        assert not (root / "tools" / "proposals" / "personal.count-rows").exists()
        wait.until(lambda drv: "In use" in drv.find_element(By.ID, "tools-installed").text)
        # Declining needs no confirmation.
        d.find_element(By.XPATH, waiting.format("Other tool", "Decline")).click()
        wait.until(lambda drv: not (root / "tools" / "proposals" / "personal.other-tool").exists())
        d.save_screenshot(str(OUT / "settings-tools.png"))
        # Nor does removing.
        d.find_element(By.XPATH, "//ul[@id='tools-installed']//button[normalize-space()='Remove']").click()
        wait.until(lambda drv: not installed.exists())
        assert yaml.safe_load((root / "tools" / "approved.yaml").read_text()) in (None, {})

        # Removing needs no confirmation.
        d.find_element(By.XPATH, "//ul[@id='connections']//button[normalize-space()='Edit']").click()
        wait.until(EC.visibility_of_element_located((By.ID, "delete"))).click()
        wait.until(lambda drv: not (root / "connections" / "roads.yaml").exists())
    finally:
        d.quit()
        proc.terminate()
        proc.wait()

    # The person declines: nothing that widens access is saved.
    propose(root, "personal.third-tool", "Third tool")
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
        # A tool is not installed without the person's confirmation.
        d.find_element(By.XPATH, "//ul[@id='tools-waiting']//li[contains(., 'Third tool')]//button[normalize-space()='Review']").click()
        wait.until(EC.visibility_of_element_located((By.ID, "review-install"))).click()
        wait.until(lambda drv: drv.find_element(By.ID, "review-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "review-error").text
        assert not (root / "tools" / "personal.third-tool").exists()
        assert (root / "tools" / "proposals" / "personal.third-tool").exists()
        d.find_element(By.ID, "review-close").click()
        # Turning a switch off is a tightening: no confirmation needed.
        d.find_element(By.ID, "allow_edits").click()
        wait.until(lambda drv: "allow_edits: false" in (root / "policy.yaml").read_text())
        print("desktop settings e2e: ok")
    finally:
        d.quit()
        proc.terminate()
        server.stdin.close()
        server.wait(timeout=30)


if __name__ == "__main__":
    main()
