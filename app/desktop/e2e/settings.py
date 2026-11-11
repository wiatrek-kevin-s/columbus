"""Connections, Tools and the organization's switches in the main window,
end to end on Linux through WebDriver (ported from the Settings window,
whose classic copy stays reachable from Classic windows).

Adds a GeoPackage connection, makes it read-write, turns edits on and
removes it, checking each file as the server reads it; turns personal
tools on, reads a tool an assistant proposed, installs one, declines
another and removes the installed one (and checks that a read of the
list of tools, or of the switches there, which ends after a newer one is
not drawn over it); chooses Work freely on copies
(one confirmation, seven settings, the rest of the file kept) and goes
back to asking (scripts.approval each, nothing else); turns the switches
for toolboxes and for sync off and on, each by itself (its own line of
the policy file, nothing else); checks twice that a read of the settings
which ends after a newer one is not drawn over it (the mode, a switch);
then, with the person declining to confirm, checks that nothing that
widens access is saved (a connection, a switch, a tool, the mode), while
turning a switch off still works; and, with the policy file managed by
the organization, that the mode in force and the switches are shown and
cannot be changed. A debug build stands in for Touch ID / Windows Hello
with COLUMBUS_APP_TEST_CONFIRM (allow or deny), and for a managed policy
file with COLUMBUS_APP_TEST_MANAGED. A real Columbus session runs beside
the app on the same files: Test asks it to open a connection before it is
saved, and the saved notice says no restart is needed.

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
from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC

import guard
from guard import step

WebDriverWait = guard.Wait  # its time-outs name the step that was waiting

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


# The settings Work freely on copies turns on (F-15).
SEVEN = {"allow_scripts": True, "scripts": {"approval": "none"}, "environments": {"allow": True},
         "allow_personal_tools": True, "allow_toolboxes": True, "allow_project_files": True, "allow_sync": True}

# The switches for toolboxes and for sync (F-15), each with its title and
# what its words must say: toolbox tools run in the sandbox, and nothing
# syncs until the person presses Sync.
SWITCH_WORDS = [("allow_toolboxes", "Allow toolboxes.", "runs in the sandbox"),
                ("allow_sync", "Allow sync.", "nothing syncs until you press Sync")]


def policy(root):
    """The policy file as the server reads it, and as written."""
    text = (root / "policy.yaml").read_text()
    return yaml.safe_load(text), text


def session(root, confirm, **more):
    env = dict(files(root), COLUMBUS_APP_TEST_CONFIRM=confirm, **more)
    d, proc = guard.driver(env, APP)
    wait = WebDriverWait(d, 30)
    guard.ready(d, wait)
    nav(d, wait, "#/data", "add")
    return d, proc, wait


def nav(d, wait, where, ready):
    """Go to a page of the main window and wait for one of its elements."""
    d.execute_script("location.hash = arguments[0]", where)
    wait.until(EC.presence_of_element_located((By.ID, ready)))
    time.sleep(0.3)


def click(d, how, what):
    """Click a control, scrolled into view first (a dialog taller than the
    window scrolls inside itself)."""
    e = d.find_element(how, what)
    d.execute_script("arguments[0].scrollIntoView({block: 'center'})", e)
    e.click()


def open_source(d, wait, conn_id):
    """A connection's own page, from its row on Data."""
    nav(d, wait, "#/data", "add")
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, f"#connections li[data-id='{conn_id}'] a")))
    click(d, By.CSS_SELECTOR, f"#connections li[data-id='{conn_id}'] a")
    wait.until(lambda drv: drv.find_elements(By.ID, "source-edit"))
    time.sleep(0.3)


def fill(d, name, value):
    # Set directly: Selenium would read an existing path as a file upload.
    d.execute_script("document.querySelector(`[name='${arguments[0]}']`).value = arguments[1];", name, value)


def main():
    guard.start("desktop settings e2e", minutes=10)
    step("fixtures")
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
""", str(gpkg)], check=True, timeout=120)
    broken = root / "broken.gpkg"
    broken.write_bytes(b"not a GeoPackage")

    count_hash = propose(root, "personal.count-rows", "Count rows")
    propose(root, "personal.other-tool", "Other tool")
    # Two more, declined one after the other further down.
    propose(root, "personal.late-one", "Late tool one")
    propose(root, "personal.late-two", "Late tool two")

    step("a Columbus session")
    server = columbus(root)
    step("the app, confirming allowed")
    d, proc, wait = session(root, "allow")
    try:
        # Add a GeoPackage connection; Test before saving.
        step("connections: Test and save a GeoPackage")
        d.find_element(By.ID, "add").click()
        wait.until(EC.visibility_of_element_located((By.NAME, "conn_id")))
        fill(d, "conn_id", "roads")
        fill(d, "path", str(broken))
        click(d, By.ID, "test")
        wait.until(lambda drv: "could not open" in drv.find_element(By.ID, "test-result").text)
        assert "test-bad" in d.find_element(By.ID, "test-result").get_attribute("class")
        fill(d, "path", str(gpkg))
        click(d, By.ID, "test")
        wait.until(lambda drv: "Opened" in drv.find_element(By.ID, "test-result").text)
        assert "routes" in d.find_element(By.ID, "test-result").text
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-test.png"))
        assert not (root / "connections" / "roads.yaml").exists(), "Test saves nothing"
        fill(d, "deny", "*audit*")
        # A filter on a hidden field would reveal it, row count by row count.
        step("connections: a filter on a hidden field")
        fill(d, "row_filter", "owner_ssn LIKE '4%'")
        click(d, By.ID, "save")
        wait.until(lambda drv: "a field your organization hides" in drv.find_element(By.ID, "form-error").text)
        fill(d, "row_filter", "district = 14")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-add.png"))
        click(d, By.ID, "save")
        wait.until(lambda drv: drv.find_element(By.ID, "restart").is_displayed())
        # The running session picks it up by itself.
        step("connections: the running session picks it up")
        assert "no need to restart" in d.find_element(By.ID, "restart").text, d.find_element(By.ID, "restart").text
        c = yaml.safe_load((root / "connections" / "roads.yaml").read_text())
        assert c["id"] == "roads" and c["type"] == "gpkg" and c["path"] == str(gpkg), c
        assert c["mode"] == "read" and c["datasets"] == {"allow": ["*"], "deny": ["*audit*"]}, c
        assert c["row_filter"] == "district = 14", c
        wait.until(lambda drv: "roads" in drv.execute_script("return document.getElementById('connections').textContent"))

        # Draft notes from the database: the running session writes them.
        step("the list says what it is in a line; its own page says its rules in plain words")
        row = d.execute_script("return document.querySelector(\"#connections li[data-id='roads']\").textContent")
        assert "GeoPackage" in row and "read only" in row and "rows where district = 14" in row, row
        open_source(d, wait, "roads")
        page = d.execute_script("return document.getElementById('source').textContent")
        assert "Read only." in page and "never *audit*" in page and "Only rows where district = 14" in page, page
        assert "can read part of it, and change nothing" in page, page
        step("connections: Test on its page opens it as Columbus would")
        click(d, By.ID, "source-test-btn")
        wait.until(lambda drv: drv.find_element(By.ID, "source-test").is_displayed() and "Opened" in drv.find_element(By.ID, "source-test").text)
        assert "routes" in d.find_element(By.ID, "source-test").text
        step("connections: draft notes")
        d.find_element(By.XPATH, "//*[@id='source']//button[normalize-space()='Draft notes']").click()
        try:
            WebDriverWait(d, 60).until(lambda drv: "Drafted" in drv.find_element(By.CSS_SELECTOR, ".draft-status").text)
        except Exception:
            d.save_screenshot(str(OUT / "settings-notes-failed.png"))
            raise SystemExit("Draft notes said: " + d.find_element(By.CSS_SELECTOR, ".draft-status").get_attribute("textContent"))
        note = root / "notes" / "datasets" / "roads" / "routes.md"
        assert note.exists(), list((root / "notes").rglob("*"))
        text = note.read_text()
        assert "status: draft" in text and "RTE" in text, text
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-notes.png"))

        # Make it read-write, and turn edits on: the rest of the policy file stays.
        step("its notes are listed on its page")
        wait.until(lambda drv: "routes" in (drv.execute_script("const e = document.getElementById('source-datasets'); return e ? e.textContent : ''")))
        step("settings: read-write and edits on")
        click(d, By.ID, "source-edit")
        # What the assistant may do is a choice between two cards.
        wait.until(EC.visibility_of_element_located((By.CSS_SELECTOR, "#editor .choice[data-mode='read-write']")))
        assert d.execute_script("return document.querySelector('#editor input[name=mode_pick]:checked').value") == "read"
        # The focus is on the card that is chosen, never on the unseen select
        # that holds the value (arrow keys there would change it unseen).
        assert d.execute_script("const a = document.activeElement; return [a.name, a.value]") == ["mode_pick", "read"]
        click(d, By.CSS_SELECTOR, "#editor .choice[data-mode='read-write']")
        assert d.execute_script("return document.querySelector('#editor [name=mode]').value") == "read-write"
        click(d, By.ID, "save")
        wait.until(lambda drv: "Read and write." in drv.execute_script("return document.getElementById('source').textContent"))
        assert "propose edits you approve" in d.execute_script("return document.getElementById('source').textContent")
        assert yaml.safe_load((root / "connections" / "roads.yaml").read_text())["mode"] == "read-write"
        nav(d, wait, "#/data", "add")
        wait.until(lambda drv: "read and write" in drv.execute_script("return document.getElementById('connections').textContent"))
        nav(d, wait, "#/settings/org", "allow_edits")
        d.find_element(By.ID, "allow_edits").click()
        wait.until(lambda drv: "allow_edits: true" in (root / "policy.yaml").read_text())
        text = (root / "policy.yaml").read_text()
        assert text == POLICY.replace("allow_edits: false     # edits", "allow_edits: true     # edits"), text
        time.sleep(0.5)
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings.png"))

        # Personal tools: turned on (confirmed) on the Tools page, then a proposal is read and installed.
        step("tools: turned on, a proposal reviewed and installed")
        nav(d, wait, "#/tools", "allow_personal_tools")
        wait.until(lambda drv: drv.find_element(By.ID, "tools-off").is_displayed())
        d.find_element(By.ID, "allow_personal_tools").click()
        wait.until(lambda drv: "allow_personal_tools: true" in (root / "policy.yaml").read_text())
        wait.until(lambda drv: not drv.find_element(By.ID, "tools-off").is_displayed())
        waiting = "//ul[@id='tools-waiting']//li[contains(., '{}')]//button[normalize-space()='{}']"
        d.find_element(By.XPATH, waiting.format("Count rows", "Review")).click()
        wait.until(EC.visibility_of_element_located((By.ID, "review-code")))
        assert "def run(ctx):" in d.find_element(By.ID, "review-code").text
        assert d.find_element(By.ID, "review-code-lines").text == "2 lines", d.find_element(By.ID, "review-code-lines").text
        assert len(d.find_elements(By.CSS_SELECTOR, "#review-code span.ln")) == 2
        assert "id: personal.count-rows" in d.find_element(By.ID, "review-manifest").text
        assert count_hash in d.find_element(By.ID, "review-name").text
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-tool-review.png"))
        click(d, By.ID, "review-install")
        installed = root / "tools" / "personal.count-rows"
        # The tool's files, then its approval: written one after the other.
        wait.until(lambda drv: (installed / "tool.py").exists() and (root / "tools" / "approved.yaml").exists())
        approved = yaml.safe_load((root / "tools" / "approved.yaml").read_text())
        assert approved == {"personal.count-rows": count_hash}, approved
        assert not (root / "tools" / "proposals" / "personal.count-rows").exists()
        wait.until(lambda drv: "In use" in drv.execute_script("return document.getElementById('tools-installed').textContent"))
        # Declining needs no confirmation.
        step("tools: decline and remove")
        d.find_element(By.XPATH, waiting.format("Other tool", "Decline")).click()
        wait.until(lambda drv: not (root / "tools" / "proposals" / "personal.other-tool").exists())
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-tools.png"))
        # Nor does removing.
        d.find_element(By.XPATH, "//ul[@id='tools-installed']//button[normalize-space()='Remove']").click()
        wait.until(lambda drv: not installed.exists())
        assert yaml.safe_load((root / "tools" / "approved.yaml").read_text()) in (None, {})

        step("tools: a read of the list that ends after a newer one does not put a declined tool back")
        # One tool is declined (the answer to the page's read that follows is
        # kept back), then the other. The window reads the list too, on a
        # timer and when it gets the focus: the read kept back is the page's
        # own, the first one after the decline was answered.
        proposals = root / "tools" / "proposals"
        guard.hold(d, "settings_tools", after="settings_tool_decline")
        click(d, By.XPATH, waiting.format("Late tool one", "Decline"))
        wait.until(lambda drv: not (proposals / "personal.late-one").exists() and guard.held(drv))
        click(d, By.XPATH, waiting.format("Late tool two", "Decline"))
        wait.until(lambda drv: not (proposals / "personal.late-two").exists())
        wait.until(lambda drv: drv.find_element(By.ID, "no-waiting").is_displayed())  # the newer read was drawn
        # Now the first read ends: it was made while the second tool waited.
        older = guard.release(d)
        assert [x["title"] for x in older["waiting"]] == ["Late tool two"], older["waiting"]
        assert "Late tool two" not in d.execute_script("return document.getElementById('tools-waiting').textContent"), \
            "a read that ended late put a declined tool back in the list"
        assert d.find_element(By.ID, "no-waiting").is_displayed()

        step("tools: a read of the settings that ends after a newer one does not put the switch back")
        # Allow personal tools is turned off (the answer to the read that
        # follows is kept back), then on again.
        d.execute_script("document.getElementById('tools-about').open = true")
        drawn_before = d.find_element(By.ID, "allow_personal_tools")
        assert drawn_before.is_selected()
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", drawn_before)
        guard.hold(d, "settings_state")
        drawn_before.click()
        wait.until(lambda drv: policy(root)[0].get("allow_personal_tools") is False and guard.held(drv))
        # The row drawn before is still there: nothing was drawn since.
        wait.until(lambda drv: drawn_before.is_enabled())
        drawn_before.click()
        wait.until(lambda drv: policy(root)[0].get("allow_personal_tools") is True)
        wait.until(EC.staleness_of(drawn_before))  # the newer read was drawn
        # Now the first read ends: it was made while the switch was off.
        older = guard.release(d)
        assert older["policy"]["allow_personal_tools"] is False, older["policy"]
        box = d.find_element(By.ID, "allow_personal_tools")
        assert box.is_selected() and box.is_enabled(), "a read that ended late put the switch back to off, while the file says on"
        wait.until(lambda drv: not drv.find_element(By.ID, "tools-off").is_displayed())

        # Removing needs no confirmation.
        open_source(d, wait, "roads")
        click(d, By.ID, "source-edit")
        wait.until(EC.presence_of_element_located((By.ID, "delete")))
        click(d, By.ID, "delete")
        wait.until(lambda drv: not (root / "connections" / "roads.yaml").exists())
        step("removed: back on Data, without it")
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/data")
        wait.until(lambda drv: drv.find_elements(By.ID, "no-conn") and drv.find_element(By.ID, "no-conn").is_displayed())
        assert not d.find_elements(By.CSS_SELECTOR, "#connections li[data-id='roads']")

        # How the assistant works (T-27, REQ-37): one choice in Settings.
        step("work freely on copies: the mode in force is shown")
        before, _ = policy(root)
        assert before["allow_scripts"] is False and "scripts" not in before, before
        nav(d, wait, "#/settings", "work-free")
        wait.until(lambda drv: "Python scripts are off" in drv.find_element(By.ID, "work-state").text)
        assert d.find_element(By.ID, "work-ask").is_selected() and not d.find_element(By.ID, "work-free").is_selected()
        assert d.find_element(By.ID, "work-free").is_enabled()
        words = d.find_element(By.ID, "work").text
        assert "Work freely on copies (recommended)" in words and "Ask me before each script" in words, words
        assert "cannot reach your connections, your passwords, your other files or the internet" in words, words
        # R-104: the cards say what the first choice turns on besides
        # scripts, that the second changes scripts only, and exactly what
        # still asks when something leaves the workspace.
        assert "Choosing this also turns on Python environments you register, your own tools, toolboxes, project files and sync" in words, words
        assert "This changes scripts only: tools from a toolbox and your own installed tools still run without asking" in words, words
        assert "Columbus asks you before it changes a source" in words, words
        assert ("An export to a folder that is not pre-approved asks; an export to a pre-approved folder, and what your "
                "assistant writes into a project's files, do not") in words, words
        assert "before anything leaves the workspace" not in words, words
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-work-mode.png"))
        step("work freely on copies: on, after one confirmation, writes the seven settings")
        click(d, By.ID, "work-free")
        wait.until(lambda drv: "approval: none" in (root / "policy.yaml").read_text())
        on, text = policy(root)
        assert {k: on.get(k) for k in SEVEN} == SEVEN, on
        # Nothing else changed, and the comments stayed where they were.
        assert {k: v for k, v in on.items() if k not in SEVEN} == {k: v for k, v in before.items() if k not in SEVEN}, (before, on)
        assert text.startswith("# The organization's settings.\n") and "allow_scripts: true   # run_python\n" in text, text
        assert "allow_edits: true     # edits\n" in text, text
        wait.until(lambda drv: "On now: Work freely on copies." in drv.find_element(By.ID, "work-state").text)
        assert d.find_element(By.ID, "work-free").is_selected()
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-work-freely.png"))
        step("a read of the settings that ends after a newer one does not put the older mode back")
        # Ask me before each script is chosen (the answer to the read that
        # follows is kept back), then Work freely on copies again.
        drawn_before = d.find_element(By.ID, "work-free")
        guard.hold(d, "settings_state")
        click(d, By.ID, "work-ask")
        wait.until(lambda drv: "approval: each" in (root / "policy.yaml").read_text() and guard.held(drv))
        # The cards drawn before are still there: nothing was drawn since.
        wait.until(lambda drv: drawn_before.is_enabled())
        drawn_before.click()
        wait.until(lambda drv: "approval: none" in (root / "policy.yaml").read_text())
        wait.until(EC.staleness_of(drawn_before))  # the newer read was drawn
        wait.until(lambda drv: "On now: Work freely on copies." in drv.find_element(By.ID, "work-state").text)
        # Now the first read ends: it was made while the mode was Ask me.
        older = guard.release(d)
        assert older["policy"]["work_mode"] == "ask", older["policy"]
        assert "On now: Work freely on copies." in d.find_element(By.ID, "work-state").text, \
            "a read that ended late put the mode back to Ask me, while the file says Work freely on copies"
        assert d.find_element(By.ID, "work-free").is_selected() and d.find_element(By.ID, "work-free").is_enabled()
        assert policy(root) == (on, text), "the file is as it was before the two choices"
        # Organization rules says the same in plain words.
        nav(d, wait, "#/settings/org", "org-work-free")
        assert d.find_element(By.ID, "org-work-free").is_selected()
        wait.until(lambda drv: "without asking you each time" in drv.find_element(By.ID, "settings").text)
        assert d.find_element(By.ID, "allow_scripts").is_selected()
        # Project files have their own switch here, on with the rest.
        assert d.find_element(By.ID, "allow_project_files").is_selected()
        step("ask me before each script: scripts.approval goes back to each, nothing else changes")
        click(d, By.ID, "org-work-ask")
        wait.until(lambda drv: "approval: each" in (root / "policy.yaml").read_text())
        off, text = policy(root)
        assert off == dict(on, scripts={"approval": "each"}), (on, off)
        assert "allow_scripts: true   # run_python\n" in text and "# The organization's settings." in text, text
        wait.until(lambda drv: "On now: Ask me before each script." in drv.find_element(By.ID, "org-work-state").text)
        assert d.find_element(By.ID, "org-work-ask").is_selected()
        # The other switches stay where they are; each is turned off in its
        # own place (a tightening: no confirmation).
        wait.until(lambda drv: drv.find_element(By.ID, "allow_scripts").is_selected())
        click(d, By.ID, "allow_scripts")
        wait.until(lambda drv: "allow_scripts: false" in (root / "policy.yaml").read_text())
        wait.until(lambda drv: "Python scripts are off" in drv.find_element(By.ID, "org-work-state").text)

        # Toolboxes and sync have their own switches too (F-15): on with the
        # rest since Work freely on copies, and each one changed by itself.
        step("toolboxes and sync: each has its own switch, saying what it allows")
        for name, title, words in SWITCH_WORDS:
            box = wait.until(EC.presence_of_element_located((By.ID, name)))
            assert box.is_selected() and box.is_enabled(), name
            row = d.execute_script("return arguments[0].closest('label').textContent", box)
            assert title in row and words in row, row
        d.execute_script("document.getElementById('allow_sync').scrollIntoView({block: 'center'})")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-switches.png"))
        step("toolboxes and sync: turning one off, then on, changes its own line of the policy file and nothing else")
        for name, _, _ in SWITCH_WORDS:
            for want in [False, True]:
                before, before_text = policy(root)
                assert before[name] is not want, (name, before)
                wait.until(lambda drv: drv.find_element(By.ID, name).is_enabled())
                click(d, By.ID, name)
                wait.until(lambda drv: policy(root)[0].get(name) is want)
                after, after_text = policy(root)
                assert after == dict(before, **{name: want}), (before, after)
                was, now = before_text.splitlines(), after_text.splitlines()
                assert len(was) == len(now) and [(a, b) for a, b in zip(was, now) if a != b] == \
                    [(f"{name}: {str(not want).lower()}", f"{name}: {str(want).lower()}")], (before_text, after_text)
                # The page shows the switch as the file has it.
                wait.until(lambda drv: drv.find_element(By.ID, name).is_enabled() and drv.find_element(By.ID, name).is_selected() is want)

        step("a read of the settings that ends after a newer one does not put an older switch back")
        # Allow toolboxes is turned off (the answer to the read that follows
        # is kept back), then on again.
        drawn_before = d.find_element(By.ID, "allow_toolboxes")
        assert drawn_before.is_selected()
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", drawn_before)
        guard.hold(d, "settings_state")
        drawn_before.click()
        wait.until(lambda drv: policy(root)[0].get("allow_toolboxes") is False and guard.held(drv))
        # The row drawn before is still there: nothing was drawn since.
        wait.until(lambda drv: drawn_before.is_enabled())
        drawn_before.click()
        wait.until(lambda drv: policy(root)[0].get("allow_toolboxes") is True)
        wait.until(EC.staleness_of(drawn_before))  # the newer read was drawn
        # Now the first read ends: it was made while the switch was off.
        older = guard.release(d)
        assert older["policy"]["allow_toolboxes"] is False, older["policy"]
        box = d.find_element(By.ID, "allow_toolboxes")
        assert box.is_selected() and box.is_enabled(), "a read that ended late put the switch back to off, while the file says on"
    finally:
        try:
            d.save_screenshot(str(OUT / "settings-last.png"))
        except Exception:
            pass
        step("closing the first app")
        guard.stop(d, proc)

    # The person declines: nothing that widens access is saved.
    propose(root, "personal.third-tool", "Third tool")
    step("the app again, confirming denied (once the first app has gone)")
    d, proc, wait = session(root, "deny")
    try:
        step("denied: save a connection, turn scripts on, install a tool")
        d.find_element(By.ID, "add").click()
        wait.until(EC.visibility_of_element_located((By.NAME, "conn_id")))
        fill(d, "conn_id", "roads2")
        fill(d, "path", str(gpkg))
        click(d, By.ID, "save")
        wait.until(lambda drv: drv.find_element(By.ID, "form-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "form-error").text
        assert not (root / "connections" / "roads2.yaml").exists()
        click(d, By.ID, "cancel")
        nav(d, wait, "#/settings/org", "allow_scripts")
        box = d.find_element(By.ID, "allow_scripts")
        box.click()
        wait.until(lambda drv: drv.find_element(By.ID, "error").is_displayed())
        assert "allow_scripts: false" in (root / "policy.yaml").read_text()
        assert not d.find_element(By.ID, "allow_scripts").is_selected()
        # A tool is not installed without the person's confirmation.
        nav(d, wait, "#/tools", "tools-waiting")
        wait.until(lambda drv: "Third tool" in drv.find_element(By.ID, "tools-waiting").text)
        d.find_element(By.XPATH, "//ul[@id='tools-waiting']//li[contains(., 'Third tool')]//button[normalize-space()='Review']").click()
        wait.until(EC.presence_of_element_located((By.ID, "review-install")))
        click(d, By.ID, "review-install")
        wait.until(lambda drv: drv.find_element(By.ID, "review-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "review-error").text
        assert not (root / "tools" / "personal.third-tool").exists()
        assert (root / "tools" / "proposals" / "personal.third-tool").exists()
        click(d, By.ID, "review-close")
        # Turning a switch off is a tightening: no confirmation needed.
        nav(d, wait, "#/settings/org", "allow_edits")
        d.find_element(By.ID, "allow_edits").click()
        wait.until(lambda drv: "allow_edits: false" in (root / "policy.yaml").read_text())
        # Work freely on copies widens: not saved without the confirmation.
        step("denied: work freely on copies")
        _, text = policy(root)
        nav(d, wait, "#/settings", "work-free")
        click(d, By.ID, "work-free")
        wait.until(lambda drv: drv.find_element(By.ID, "work-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "work-error").text
        assert (root / "policy.yaml").read_text() == text
        assert d.find_element(By.ID, "work-ask").is_selected() and not d.find_element(By.ID, "work-free").is_selected()
        # Toolboxes and sync: turning one off takes access away, so it needs
        # no confirmation; turning one on asks, and here the answer is no.
        step("denied: toolboxes and sync are turned off without the confirmation, and not turned on without it")
        for name, _, _ in SWITCH_WORDS:
            nav(d, wait, "#/settings/org", name)
            before, _ = policy(root)
            assert before[name] is True, before
            wait.until(lambda drv: drv.find_element(By.ID, name).is_enabled() and drv.find_element(By.ID, name).is_selected())
            click(d, By.ID, name)
            wait.until(lambda drv: policy(root)[0].get(name) is False)
            off, text = policy(root)
            assert off == dict(before, **{name: False}), (before, off)
            assert not d.find_element(By.ID, "error").is_displayed(), d.find_element(By.ID, "error").text
            wait.until(lambda drv: drv.find_element(By.ID, name).is_enabled() and not drv.find_element(By.ID, name).is_selected())
            click(d, By.ID, name)
            wait.until(lambda drv: drv.find_element(By.ID, "error").is_displayed())
            assert "not confirmed" in d.find_element(By.ID, "error").text, d.find_element(By.ID, "error").text
            assert (root / "policy.yaml").read_text() == text, "nothing is saved without the confirmation"
            wait.until(lambda drv: drv.find_element(By.ID, name).is_enabled() and not drv.find_element(By.ID, name).is_selected())
            # (The part is opened afresh for the next one, so its error line starts empty.)
            nav(d, wait, "#/settings", "work-free")
    finally:
        step("closing the second app")
        guard.stop(d, proc)

    # The organization's file: the mode in force is shown, not offered.
    managed = "# Managed by IT.\nconnection_types: [gpkg]\nallow_scripts: true\nscripts: { approval: none }\n"
    (root / "policy.yaml").write_text(managed)
    step("the app once more, the policy file managed by the organization")
    d, proc, wait = session(root, "allow", COLUMBUS_APP_TEST_MANAGED="1")
    try:
        step("managed: the mode in force is shown as set by the organization")
        nav(d, wait, "#/settings", "work-free")
        wait.until(lambda drv: "On now: Work freely on copies." in drv.find_element(By.ID, "work-state").text)
        assert "set by your organization" in d.find_element(By.ID, "work-managed").text
        assert d.find_element(By.ID, "work-free").is_selected()
        assert not d.find_element(By.ID, "work-free").is_enabled() and not d.find_element(By.ID, "work-ask").is_enabled()
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "settings-work-managed.png"))
        # The app's own code refuses too, whatever a page asks.
        d.set_script_timeout(30)
        answer = d.execute_async_script(
            "const done = arguments[arguments.length - 1];"
            "window.__TAURI__.core.invoke('settings_set_work_mode', { free: false }).then(() => done('saved'), (e) => done(String(e)));")
        assert "managed by your organization" in answer, answer
        assert (root / "policy.yaml").read_text() == managed
        step("managed: the switches for toolboxes and sync are shown, not offered, and the app's own code refuses")
        nav(d, wait, "#/settings/org", "allow_toolboxes")
        assert d.find_element(By.ID, "org-managed").is_displayed()
        for name, _, _ in SWITCH_WORDS:
            box = d.find_element(By.ID, name)
            assert not box.is_enabled() and not box.is_selected(), name
            answer = d.execute_async_script(
                "const done = arguments[arguments.length - 1];"
                "window.__TAURI__.core.invoke('settings_set_switch', { name: arguments[0], on: true }).then(() => done('saved'), (e) => done(String(e)));", name)
            assert "managed by your organization" in answer, answer
        assert (root / "policy.yaml").read_text() == managed
        print("desktop settings e2e (main window): ok")
    finally:
        step("closing the app and Columbus")
        try:
            guard.stop(d, proc)
        finally:
            server.stdin.close()
            server.wait(timeout=30)


if __name__ == "__main__":
    main()
