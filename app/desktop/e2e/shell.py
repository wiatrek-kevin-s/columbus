"""The main window, end to end on Linux through WebDriver (tauri-driver and
WebKitWebDriver, under xvfb and a D-Bus session).

A real Columbus session runs beside the app, as an AI app would run it.
The test walks the rail; opens that session under Chats (its activity
from the audit log, and its companion page in the output panel's frame,
which must load and show the session's layer); lists a request waiting in
that session in Activity › Approvals (view only) and has the session open
its browser page to decide it; reads the audit log and checks its chain;
marks one of the person's notes reviewed; and opens a classic window.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/shell.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from selenium import webdriver
from selenium.common.exceptions import StaleElementReferenceException
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import WebDriverWait

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "app/ui/e2e"))
from mcp_client import Mcp, wait_file  # noqa: E402

GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
OUT = Path(__file__).resolve().parent / "out"

NOTE = """---
id: tiger.tracts
title: tracts
connection: tiger
status: draft   # set reviewed when it is right
owner: me
---

# Fields

- NAME: the tract's name
"""


def text(d, css):
    return d.execute_script("return document.querySelector(arguments[0])?.textContent || ''", css)


def click(d, how, what):
    """Click a control, scrolled into view first."""
    e = d.find_element(how, what)
    d.execute_script("arguments[0].scrollIntoView({block: 'center'})", e)
    e.click()


def go(d, wait, href, title):
    """Follow a rail link (the rail is redrawn as counts change: try again)."""
    for _ in range(5):
        try:
            click(d, By.CSS_SELECTOR, f"#rail a[href='{href}']")
            break
        except StaleElementReferenceException:
            time.sleep(0.3)
    wait.until(lambda drv: title in text(drv, "#page-title"))


def main():
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[box(0, 0, 1, 1), box(1, 0, 2, 1)], crs="EPSG:3081") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "outbox").mkdir()
    (root / "policy.yaml").write_text(
        f"connection_types: [gpkg]\nexports:\n  - {{ id: outbox, path: {json.dumps(str(root / 'outbox'))} }}\n")
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")
    note = root / "notes" / "datasets" / "tiger" / "tracts.md"
    note.parent.mkdir(parents=True)
    note.write_text(NOTE)

    files = dict(os.environ, COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
                 COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
                 COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_NOTES=str(root / "notes"),
                 XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"))
    gw_env = dict(files, COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"),
                  COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
                  COLUMBUS_APPROVALS_LAUNCH_FILE=str(root / "launch.txt"), COLUMBUS_LOG="warn")
    mcp = Mcp([str(GATEWAY), "serve"], gw_env)
    session = mcp.tool("whoami", {})["result"]["session"]
    port = json.loads(wait_file(root / "live" / f"{session}.json"))["port"]
    q = mcp.tool("query_features", {"connection": "tiger", "dataset": "tracts"})
    handle = q["result"]["layer"]["handle"]

    driver_proc = subprocess.Popen(["tauri-driver"], env=files, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(2)
    opts = webdriver.common.options.ArgOptions()
    opts.set_capability("tauri:options", {"application": str(APP)})
    opts.set_capability("browserName", "wry")
    d = webdriver.Remote("http://127.0.0.1:4444", options=opts)
    exporter = None
    try:
        wait = WebDriverWait(d, 30)
        wait.until(lambda drv: "Home" in text(drv, "#page-title"))
        main_window = d.current_window_handle
        d.save_screenshot(str(OUT / "shell-home.png"))

        # The rail reaches every page, and each says what it is.
        for href, title in [("#/chats", "Chats"), ("#/projects", "Projects"), ("#/documents", "Documents"),
                            ("#/datagroups", "Data groups"), ("#/skills", "Skills"), ("#/connections", "Connections"),
                            ("#/registry", "Registry"), ("#/tools", "Tools"), ("#/workflows", "Workflows"),
                            ("#/activity", "Approvals"), ("#/settings", "Models and accounts"), ("#/home", "Home")]:
            go(d, wait, href, title)
        go(d, wait, "#/skills", "Skills")
        wait.until(lambda drv: "working-with-columbus" in text(drv, "main"))
        go(d, wait, "#/registry", "Registry")
        assert "Coming" in text(d, "main"), "a page with no backend says it is coming"
        # The rail's foot: the policy's source, and the workflows.
        foot = text(d, ".rail-foot")
        assert "Workflows" in foot and "Policy" in foot, foot

        # The session another AI app runs, under Chats: its activity and its map.
        go(d, wait, "#/chats", "Chats")
        wait.until(lambda drv: "Running in other AI apps" in text(drv, "main"))
        click(d, By.CSS_SELECTOR, f"a[href='#/chats/s/{session}']")
        wait.until(lambda drv: 'Querying "tracts" on "tiger"' in text(drv, ".activity"))
        frame = WebDriverWait(d, 40).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".outpanel iframe")))
        assert frame.get_attribute("sandbox") == "allow-scripts allow-same-origin", frame.get_attribute("sandbox")
        src = frame.get_attribute("src")
        assert src.startswith(f"http://127.0.0.1:{port}/embed#t="), src
        d.switch_to.frame(frame)
        # The companion page signed in with its view link and shows the layer.
        WebDriverWait(d, 40).until(lambda drv: "tracts" in drv.execute_script("return document.body.textContent"))
        framed = d.execute_script("return { tauri: typeof window.__TAURI__, internals: typeof window.__TAURI_INTERNALS__, embedded: !!document.querySelector('.app.embedded') }")
        assert framed["tauri"] == "undefined" and framed["internals"] == "undefined", f"the frame can reach the app: {framed}"
        assert framed["embedded"], framed
        d.switch_to.default_content()
        time.sleep(2)
        d.save_screenshot(str(OUT / "shell-session.png"))
        # Its files.
        click(d, By.XPATH, "//div[@role='tablist']/button[normalize-space()='Files']")
        wait.until(lambda drv: ".gpkg" in text(drv, ".outpanel"))

        # A request waiting in that session: listed, view only; decided in the browser.
        result = {}
        exporter = threading.Thread(target=lambda: result.update(r=mcp.request(
            "tools/call", {"name": "export_output", "arguments": {"handle": handle, "folder": "outbox", "name": "tracts"}}, 120)))
        exporter.start()
        go(d, wait, "#/activity", "Approvals")
        WebDriverWait(d, 40).until(lambda drv: "tracts" in text(drv, "#approvals"))
        page = text(d, "#approvals")
        assert "Allow once" not in page and not d.find_elements(By.XPATH, "//*[@id='approvals']//button[normalize-space()='Allow once']")
        d.save_screenshot(str(OUT / "shell-approvals.png"))
        # The rail counts it.
        wait.until(lambda drv: "1 waiting" in (drv.find_element(By.CSS_SELECTOR, "#rail a[href='#/activity']").get_attribute("aria-label") or ""))
        # The session opened its browser page when the request came (the
        # test's stand-in for the browser is this file); the button asks
        # again, which the session limits to one page every 30 s.
        link = wait_file(root / "launch.txt")
        click(d, By.XPATH, "//*[@id='approvals']//button[normalize-space()='Decide in browser']")
        wait.until(lambda drv: "Decide there" in text(drv, "#toast") or "Nothing is waiting" in text(drv, "#toast"))
        assert link.startswith(f"http://127.0.0.1:{port}/#t="), f"the session opened its browser page: {link}"
        # Decide there, as the person would (decline).
        token = link.split("#t=")[1]
        own = f"http://127.0.0.1:{port}"
        r = urllib.request.urlopen(urllib.request.Request(f"{own}/api/session", data=json.dumps({"launch": token}).encode(),
                                                          headers={"Content-Type": "application/json", "Origin": own}))
        sess = json.loads(r.read())
        assert sess["scope"] == "approve", sess
        auth = {"Authorization": f"Bearer {sess['token']}"}
        pending = json.loads(urllib.request.urlopen(urllib.request.Request(f"{own}/api/approvals", headers=auth)).read())["pending"]
        assert len(pending) == 1, pending
        urllib.request.urlopen(urllib.request.Request(f"{own}/api/approvals/{pending[0]['id']}", data=json.dumps({"decision": "decline"}).encode(),
                                                      headers={**auth, "Content-Type": "application/json", "Origin": own}))
        exporter.join(60)
        assert not (root / "outbox" / "tracts.gpkg").exists()
        WebDriverWait(d, 30).until(lambda drv: "Nothing is waiting" in text(drv, "#approvals"))

        # The audit log: this session's calls, filtered, and its chain checked.
        go(d, wait, "#/activity/audit", "Audit log")
        wait.until(lambda drv: "query_features" in text(drv, "#audit-rows"))
        assert "approval_decision" in text(d, "#audit-rows")
        d.execute_script("const s = document.getElementById('audit-tool'); s.value = 'query_features'; s.dispatchEvent(new Event('change'));")
        wait.until(lambda drv: "whoami" not in text(drv, "#audit-rows") and "query_features" in text(drv, "#audit-rows"))
        click(d, By.ID, "audit-verify")
        wait.until(lambda drv: "holds" in text(drv, "#audit-verify-result"))
        d.save_screenshot(str(OUT / "shell-audit.png"))
        go(d, wait, "#/activity/exports", "Exports")

        # The person's note: read, then marked reviewed (their own file).
        go(d, wait, "#/documents", "Documents")
        wait.until(lambda drv: "tracts" in text(drv, "#notes-list"))
        click(d, By.CSS_SELECTOR, "#notes-list li")
        wait.until(lambda drv: "NAME: the tract's name" in text(drv, "main"))
        # Changed after the person read it (as an assistant allowed to edit
        # draft notes could): nothing is marked.
        note.write_text(NOTE.replace("the tract's name", "changed behind your back"))
        click(d, By.XPATH, "//button[normalize-space()='Mark reviewed']")
        wait.until(lambda drv: "changed after you read it" in text(drv, "main"))
        assert "status: draft" in note.read_text()
        # Read again, then marked: only the status word changes.
        click(d, By.CSS_SELECTOR, "#notes-list li")
        wait.until(lambda drv: "changed behind your back" in text(drv, "main"))
        click(d, By.XPATH, "//button[normalize-space()='Mark reviewed']")
        wait.until(lambda drv: "status: reviewed" in note.read_text())
        want = NOTE.replace("the tract's name", "changed behind your back").replace("status: draft", "status: reviewed")
        assert note.read_text() == want, note.read_text()

        # Ctrl+K opens the search.
        d.execute_script("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'k', ctrlKey: true}))")
        wait.until(lambda drv: drv.execute_script("return document.getElementById('palette').open"))
        d.execute_script("document.getElementById('palette').close()")

        # A classic window is still there.
        click(d, By.CSS_SELECTOR, ".rail-foot .classic")
        click(d, By.XPATH, "//dialog//button[normalize-space()='Session list']")
        wait.until(lambda drv: len(drv.window_handles) > 1)
        d.switch_to.window([h for h in d.window_handles if h != main_window][0])
        wait.until(lambda drv: session in drv.find_element(By.ID, "sessions").text)
        d.switch_to.window(main_window)
        print("desktop shell e2e: ok")
    finally:
        try:
            d.save_screenshot(str(OUT / "shell-last.png"))
        except Exception:
            pass
        d.quit()
        driver_proc.terminate()
        if exporter:
            exporter.join(5)
        mcp.close()


if __name__ == "__main__":
    main()
