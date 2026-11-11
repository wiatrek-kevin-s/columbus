"""Projects in the main window, end to end on Linux through WebDriver.

A project is made from the Projects page (name, instructions, only one of
the person's connections, an export folder), its file is written where the
server reads it, its Files section opens its folder and lists a file the
assistant wrote there (through a real Columbus session in the project) with
its size and when it changed, a read of the folder that ends after a newer
one does not put the older list back, a chat started in it says so, the
Chats page and the Projects list show it, and deleting it removes its file
(its chats and its files are kept).

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/projects.py \\
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
sys.path.insert(0, str(REPO / "app/ui/e2e"))
from mcp_client import Mcp  # noqa: E402

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
    guard.start("desktop projects e2e", minutes=10)
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    (root / "connections").mkdir()
    for cid in ["tiger", "payroll"]:
        (root / "connections" / f"{cid}.yaml").write_text(
            f"id: {cid}\ntype: gpkg\npath: {root / 'x.gpkg'}\ndatasets:\n  allow: [\"*\"]\n")
    (root / "policy.yaml").write_text(
        f"connection_types: [gpkg]\nallow_project_files: true\nexports:\n  - {{ id: outbox, path: \"{root / 'outbox'}\", granted: true }}\n")
    env = dict(os.environ,
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_TOOLS=str(root / "tools"),
               COLUMBUS_SERVER=str(GATEWAY), COLUMBUS_COMPANION="off",
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn",
               XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
               # A debug build writes the folder it would open here, instead
               # of starting a file manager.
               COLUMBUS_APP_TEST_OPEN=str(root / "opened.txt"))
    step("the app")
    d, proc = guard.driver(env, APP)
    wait = WebDriverWait(d, 30)
    try:
        guard.ready(d, wait)
        d.execute_script("location.hash = '#/projects'")
        wait.until(EC.element_to_be_clickable((By.ID, "pj-new"))).click()
        name = wait.until(EC.visibility_of_element_located((By.ID, "pj-name")))
        name.send_keys("Tract review 2027")
        d.find_element(By.ID, "pj-instructions").send_keys("Report areas in acres.")
        click(d, By.ID, "pj-conn-some")
        click(d, By.ID, "pj-c-tiger")
        d.execute_script("const s = document.getElementById('pj-folder'); s.value = 'outbox'; s.dispatchEvent(new Event('change'))")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "projects-new.png"))
        click(d, By.ID, "pj-save")
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/projects/tract-review-2027")
        f = root / "projects" / "tract-review-2027" / "project.yaml"
        saved = f.read_text()
        assert "name: Tract review 2027" in saved and "acres" in saved, saved
        assert "- tiger" in saved and "payroll" not in saved, saved
        assert "export_folder: outbox" in saved, saved
        step("Its project databases, as Columbus's server lists them for it")
        wait.until(lambda drv: "None yet" in text(drv, "pj-dbs") or "Couldn't" in text(drv, "pj-dbs"))
        assert "None yet" in text(d, "pj-dbs"), text(d, "pj-dbs")

        step("Its files: none yet, and the folder opens for the person's own")
        files = root / "projects" / "tract-review-2027" / "files"
        wait.until(lambda drv: "No files yet" in text(drv, "pj-files"))
        assert not files.exists(), "nothing is made before it is needed"
        click(d, By.ID, "pj-files-open")
        wait.until(lambda drv: (root / "opened.txt").exists())
        assert (root / "opened.txt").read_text() == str(files) and files.is_dir()
        step("A file the assistant wrote is listed, with its size and when it changed")
        # The assistant, in a chat of this project: a real Columbus session.
        script = "print('lane widths')\n"
        mcp = Mcp([str(GATEWAY), "serve"], dict(env, COLUMBUS_PROJECT="tract-review-2027"))
        try:
            made = mcp.tool("project_files", {"action": "write", "path": "scripts/widths.py", "text": script})
            assert made["result"]["status"] == "written", made
        finally:
            mcp.close()
        assert (files / "scripts" / "widths.py").read_text() == script
        # And one the person put there themselves.
        (files / "notes.md").write_text("# Notes\n")
        click(d, By.ID, "pj-files-refresh")
        row = "#pj-file-list li.file[data-path='scripts/widths.py']"
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, row))
        listed = d.find_element(By.CSS_SELECTOR, row).text
        assert "widths.py" in listed and f"{len(script)} B" in listed and "changed just now" in listed, listed
        rows = [e.text for e in d.find_elements(By.CSS_SELECTOR, "#pj-file-list li")]
        assert rows[0].startswith("notes.md") and rows[1] == "scripts/" and rows[2].startswith("widths.py"), rows
        assert "Files (2)" in text(d, "pj-files") and "2 files" in text(d, "pj-files"), text(d, "pj-files")
        assert "Your assistant can list, read and write" in text(d, "pj-files")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.execute_script("document.getElementById('pj-files').scrollIntoView({block: 'center'})")
        d.save_screenshot(str(OUT / "projects-files.png"))

        step("A read of the folder that ends after a newer one does not put the older list back")
        # Check again is pressed (the answer to that read is kept back), a
        # file is put in the folder, and Check again is pressed once more.
        guard.hold(d, "project_files")
        click(d, By.ID, "pj-files-refresh")
        wait.until(guard.held)
        (files / "plan.md").write_text("# Plan\n")
        click(d, By.ID, "pj-files-refresh")
        added = "#pj-file-list li.file[data-path='plan.md']"
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, added))
        # Now the first read ends: it was made before the file was there.
        older = guard.release(d)
        assert [x["path"] for x in older["files"]] == ["notes.md", "scripts/widths.py"], older
        assert d.find_elements(By.CSS_SELECTOR, added), "a read that ended late put back the list from before plan.md was added"
        assert "Files (3)" in text(d, "pj-files"), text(d, "pj-files")

        step("A chat in the project says so")
        wait.until(EC.element_to_be_clickable((By.ID, "pj-chat"))).click()
        # Under the message box, the new chat's project is this one (it can
        # still be changed until the first message).
        wait.until(lambda drv: drv.execute_script(
            "const s = document.getElementById('chat-project-pick'); return s ? [s.value, s.selectedOptions[0].textContent] : null") == ["tract-review-2027", "Tract review 2027"])
        assert "Tract review 2027" in text(d, "chat-sub")

        step("The list shows it, with its connections")
        d.execute_script("location.hash = '#/projects'")
        wait.until(lambda drv: "Tract review 2027" in text(drv, "pj-list"))
        assert "Uses tiger" in text(d, "pj-list"), text(d, "pj-list")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "projects-list.png"))

        step("A changed instruction is saved; deleting removes the file")
        click(d, By.XPATH, "//ul[@id='pj-list']/li[@data-id='tract-review-2027']//a")
        box = wait.until(EC.visibility_of_element_located((By.ID, "pj-instructions")))
        box.send_keys(" District 14 only.")
        click(d, By.ID, "pj-save")
        wait.until(lambda drv: "District 14 only." in f.read_text())
        click(d, By.ID, "pj-delete")
        wait.until(EC.element_to_be_clickable((By.XPATH, "//dialog//button[normalize-space()='Delete project']"))).click()
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/projects")
        assert not f.exists(), "the project file is removed"
        assert (files / "scripts" / "widths.py").exists() and (files / "notes.md").exists(), "its files are kept"
        print("desktop projects e2e: ok")
    except Exception:
        d.save_screenshot(str(OUT / "projects-failed.png"))
        raise
    finally:
        guard.stop(d, proc)
        subprocess.run(["pkill", "-f", str(APP)], check=False)


if __name__ == "__main__":
    main()
