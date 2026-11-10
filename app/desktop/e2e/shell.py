"""The main window, end to end on Linux through WebDriver (tauri-driver and
WebKitWebDriver, under xvfb and a D-Bus session).

A real Columbus session runs beside the app, as an AI app would run it.
The test opens on the start of a new chat; walks the four places and the
pages inside them; opens that session under Chats (its activity from the
audit log, and its companion page in the frame behind the panel, which
must load and show the session's layer; its files in the sheet beside the
panel); lists a request waiting in that session in Activity (view only)
and has the session open its browser page to decide it; reads the audit
log and checks its chain; marks one of the person's notes reviewed; and
opens a classic window.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/shell.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json
import os
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from selenium.common.exceptions import StaleElementReferenceException
from selenium.webdriver.common.by import By
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support import expected_conditions as EC

import guard
from guard import step

WebDriverWait = guard.Wait  # its time-outs name the step that was waiting

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


def named(d):
    """Every control on show has a name a screen reader can say, and Tab
    reaches it (the 0.14 audit left accessibility, B10, to this window)."""
    bad = guard.unnamed(d)
    assert not bad, bad


def place(d, wait, href, title):
    """Follow one of the four place links (the nav is redrawn as counts
    change: try again)."""
    for _ in range(5):
        try:
            click(d, By.CSS_SELECTOR, f"#nav a[href='{href}']")
            break
        except StaleElementReferenceException:
            time.sleep(0.3)
    try:
        wait.until(lambda drv: title == text(drv, "#page-title"))
    except Exception:
        d.save_screenshot(str(OUT / "shell-place-failed.png"))
        raise SystemExit(f"{href} did not show {title}: " + d.execute_script(
            "return JSON.stringify({hash: location.hash, title: document.querySelector('#page-title')?.textContent, body: document.body.className,"
            " dialog: !!document.querySelector('dialog[open]'), focus: document.activeElement && (document.activeElement.id || document.activeElement.tagName),"
            " hosts: [...document.querySelectorAll('#main > *')].map((e) => e.id + (e.hidden ? ' hidden' : ''))})"))
    named(d)


def inside(d, wait, href, title):
    """Follow a link on the page that is open, to a page inside the place."""
    link = f"#main a[href='{href}']"
    wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, link))
    click(d, By.CSS_SELECTOR, link)
    wait.until(lambda drv: title == text(drv, "#page-title"))
    named(d)


def back(d, wait, title):
    """The page's own way back."""
    click(d, By.CSS_SELECTOR, "#main .pagehead a.back")
    wait.until(lambda drv: title == text(drv, "#page-title"))


def main():
    guard.start("desktop shell e2e", minutes=10)
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

    step("the app")
    d, driver_proc = guard.driver(files, APP)
    exporter = None
    try:
        wait = WebDriverWait(d, 30)
        step("Nothing is set up for the app's own chat: the first run stands in for the question")
        guard.ready(d, wait)
        wait.until(lambda drv: drv.find_elements(By.ID, "first-steps"))
        first = text(d, "#first-steps")
        assert "Start with your data." in first and "Connect an AI for the chat" in first, first
        assert "Your first connection is added" in first, first
        assert not d.find_element(By.ID, "text").is_displayed(), "no question to ask before an AI is connected"
        named(d)
        d.save_screenshot(str(OUT / "shell-first-run.png"))
        click(d, By.ID, "fr-skip")
        step("The app opens on a question: a new chat, its panel in the middle")
        wait.until(EC.visibility_of_element_located((By.ID, "text")))
        wait.until(lambda drv: drv.execute_script("return document.body.classList.contains('start')"))
        wait.until(lambda drv: drv.execute_script("return document.activeElement && document.activeElement.id") == "text")
        assert d.execute_script("return document.activeElement && document.activeElement.id") == "text", "the question has the focus"
        main_window = d.current_window_handle
        named(d)
        # Tab and a screen reader follow the document: the question comes
        # first on the start, as it is drawn, and the panel before the map.
        follows = "return !!(document.getElementById(arguments[0]).compareDocumentPosition(document.getElementById(arguments[1])) & Node.DOCUMENT_POSITION_FOLLOWING)"
        assert d.execute_script(follows, "composer", "log"), "on the start the question is first in the document"
        assert d.execute_script(follows, "panel", "stage"), "the panel comes before the map"
        d.save_screenshot(str(OUT / "shell-start.png"))

        step("The four places, and the pages inside them: each says what it is")
        assert [a.get_attribute("href").split("#")[1] for a in d.find_elements(By.CSS_SELECTOR, "#nav a.tab")] == ["/chats", "/data", "/workflows", "/activity"]
        place(d, wait, "#/chats", "Chats")
        assert not d.execute_script("return document.body.classList.contains('start')"), "a list sits at the side, over the map"
        inside(d, wait, "#/projects", "Projects")
        back(d, wait, "Chats")
        place(d, wait, "#/data", "Data")
        for href, title in [("#/documents", "Notes"), ("#/data/datasets", "Datasets"), ("#/datagroups", "Data groups"),
                            ("#/data/maplayers", "Map layers"), ("#/data/projectdbs", "Project databases")]:
            inside(d, wait, href, title)
            back(d, wait, "Data")
        inside(d, wait, "#/registry", "Registry")
        wait.until(lambda drv: drv.find_elements(By.ID, "rg-q"))
        back(d, wait, "Data")
        step("An export folder's path reads left to right, its first character first")
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#export-folders li .when bdi"))
        shown = d.execute_script("""
            const t = document.querySelector('#export-folders li .when bdi').firstChild;
            const left = (i) => { const r = document.createRange(); r.setStart(t, i); r.setEnd(t, i + 1); return r.getBoundingClientRect().left; };
            return [t.data, left(0) < left(1) && left(1) < left(t.data.length - 1)];""")
        # (The folder as the app names it: a system may spell the temporary folder its own way.)
        assert shown[0].replace("\\", "/").endswith("/outbox") and shown[1] is True, shown
        step("Skip to content moves the focus into the page, and opens nothing")
        d.execute_script("document.querySelector('a.skip').focus()")
        d.switch_to.active_element.send_keys(Keys.ENTER)
        wait.until(lambda drv: drv.execute_script("return document.activeElement && document.activeElement.id") == "page-title")
        assert d.execute_script("return location.hash") == "#/data" and text(d, "#page-title") == "Data"
        step("Nothing unbuilt is offered: no part of Data says it is coming")
        assert "oming" not in text(d, "#page-host"), text(d, "#page-host")
        assert not d.find_elements(By.CSS_SELECTOR, "#main a[href='#/data/accounts']")
        step("Notes: the organization's pack, proposals and import are tabs of it")
        inside(d, wait, "#/documents", "Notes")
        click(d, By.CSS_SELECTOR, "#main nav.tabs a[href='#/documents/import']")
        wait.until(lambda drv: drv.find_elements(By.ID, "imp-choose"))
        click(d, By.CSS_SELECTOR, "#main nav.tabs a[href='#/documents/proposals']")
        wait.until(lambda drv: drv.find_elements(By.ID, "pr-check"))
        wait.until(lambda drv: drv.find_elements(By.ID, "sg-list"))
        step("Workflows holds tools and skills; it says workflows are off here")
        place(d, wait, "#/workflows", "Workflows")
        wait.until(lambda drv: drv.find_element(By.ID, "wf-off").is_displayed())
        inside(d, wait, "#/tools", "Tools")
        wait.until(lambda drv: drv.find_elements(By.ID, "team-choose"))
        back(d, wait, "Workflows")
        inside(d, wait, "#/skills", "Skills")
        wait.until(lambda drv: "working-with-columbus" in text(drv, "main"))
        back(d, wait, "Workflows")
        place(d, wait, "#/activity", "Activity")
        step("Settings is one sheet over the page: it names the policy's source, and its parts open inside it")
        click(d, By.ID, "nav-settings")
        wait.until(lambda drv: text(drv, "#settings-title") == "Settings")
        assert d.execute_script("return document.getElementById('settings').open")
        wait.until(lambda drv: "policy file on this computer" in text(drv, "#set-policy"))
        named(d)
        for part, title, ready in [("ai", "Assistant", "key-form"), ("environments", "Python environments", "env-add"),
                                   ("privacy", "Privacy and files", None), ("org", "Organization rules", "allow_edits"),
                                   ("keyboard", "Keyboard shortcuts", None)]:
            click(d, By.CSS_SELECTOR, f"#settings a[href='#/settings/{part}']")
            wait.until(lambda drv: text(drv, "#settings-title") == title)
            if ready:
                wait.until(lambda drv: drv.find_elements(By.ID, ready))
            named(d)
            click(d, By.ID, "settings-back")
            wait.until(lambda drv: text(drv, "#settings-title") == "Settings")
        click(d, By.ID, "settings-close")
        wait.until(lambda drv: not drv.execute_script("return document.getElementById('settings').open"))
        assert d.execute_script("return location.hash") == "#/activity", "closing the sheet returns to the page under it"
        wait.until(lambda drv: drv.execute_script("return document.activeElement && document.activeElement.id") == "nav-settings")
        step("Closing Settings puts back the address the page has, not the link that led to it")
        click(d, By.ID, "nav-new")  # "#/chats/fresh", which the new chat rewrites to "#/chats/new"
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/chats/new")
        click(d, By.ID, "nav-settings")
        wait.until(lambda drv: drv.execute_script("return document.getElementById('settings').open"))
        click(d, By.ID, "settings-close")
        wait.until(lambda drv: not drv.execute_script("return document.getElementById('settings').open"))
        assert d.execute_script("return location.hash") == "#/chats/new", d.execute_script("return location.hash")

        step("The session another AI app runs, under Chats: its activity and its map")
        place(d, wait, "#/chats", "Chats")
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, f"#chat-list a[href='#/chats/s/{session}']"))
        assert "The conversation is in that app" in text(d, "#chat-list")
        click(d, By.CSS_SELECTOR, f"#chat-list a[href='#/chats/s/{session}']")
        wait.until(lambda drv: 'Querying "tracts" on "tiger"' in text(drv, ".activity"))
        frame = WebDriverWait(d, 40).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#stage iframe")))
        assert frame.get_attribute("sandbox") == "allow-scripts allow-same-origin", frame.get_attribute("sandbox")
        src = frame.get_attribute("src")
        assert src.startswith(f"http://127.0.0.1:{port}/embed#t="), src
        size = d.execute_script("const r = arguments[0].getBoundingClientRect(); return [r.left, r.top, r.width, r.height, innerWidth, innerHeight]", frame)
        assert size[:2] == [0, 0] and size[2:4] == size[4:], f"the map fills the window behind the panel: {size}"
        # Beside the panel, a click lands on the map; on the panel, it does not.
        hit = "const e = document.elementFromPoint(arguments[0], arguments[1]); return e === arguments[2] ? 'map' : e && e.closest('#panel') ? 'panel' : String(e && e.tagName)"
        assert d.execute_script(hit, size[4] - 40, size[5] - 40, frame) == "map"
        assert d.execute_script(hit, 60, size[5] / 2, frame) == "panel"
        d.switch_to.frame(frame)
        step("The companion page signed in with its view link and shows the layer")
        WebDriverWait(d, 40).until(lambda drv: "tracts" in drv.execute_script("return document.body.textContent"))
        framed = d.execute_script("return { tauri: typeof window.__TAURI__, internals: typeof window.__TAURI_INTERNALS__, embedded: !!document.querySelector('.app.embedded') }")
        assert framed["tauri"] == "undefined" and framed["internals"] == "undefined", f"the frame can reach the app: {framed}"
        assert framed["embedded"], framed
        d.switch_to.default_content()
        time.sleep(2)
        named(d)
        d.save_screenshot(str(OUT / "shell-session.png"))
        step("Its files, in the sheet beside the panel")
        click(d, By.CSS_SELECTOR, "#main .chat-more")
        click(d, By.CSS_SELECTOR, "#main .chat-files")
        wait.until(lambda drv: ".gpkg" in text(drv, "#side"))
        named(d)
        d.save_screenshot(str(OUT / "shell-files.png"))
        click(d, By.ID, "side-close")
        step("The panel put away leaves the map alone, and comes back")
        click(d, By.CSS_SELECTOR, "#main .chat-more")
        click(d, By.CSS_SELECTOR, "#main .chat-away")
        wait.until(lambda drv: not drv.find_element(By.ID, "panel").is_displayed())
        named(d)
        d.save_screenshot(str(OUT / "shell-map-only.png"))
        click(d, By.ID, "pill-show")
        wait.until(lambda drv: drv.find_element(By.ID, "panel").is_displayed())

        step("A request waiting in that session: listed, view only; decided in the browser")
        result = {}
        exporter = threading.Thread(target=lambda: result.update(r=mcp.request(
            "tools/call", {"name": "export_output", "arguments": {"handle": handle, "folder": "outbox", "name": "tracts"}}, 120)))
        exporter.start()
        place(d, wait, "#/activity", "Activity")
        WebDriverWait(d, 40).until(lambda drv: "tracts" in text(drv, "#approvals"))
        page = text(d, "#approvals")
        assert "Allow once" not in page and not d.find_elements(By.XPATH, "//*[@id='approvals']//button[normalize-space()='Allow once']")
        assert "decided in that app or in your browser, not here" in page, page
        named(d)
        d.save_screenshot(str(OUT / "shell-approvals.png"))
        step("The nav counts it")
        wait.until(lambda drv: "1 waiting" in (drv.find_element(By.CSS_SELECTOR, "#nav a[href='#/activity']").get_attribute("aria-label") or ""))
        step("The session opened its browser page when the request came")
        # The session opened its browser page when the request came (the
        # test's stand-in for the browser is this file); the button asks
        # again, which the session limits to one page every 30 s.
        link = wait_file(root / "launch.txt")
        # The page polls every few seconds; a poll that changes nothing the
        # person sees keeps the very button they are about to click.
        held = d.find_element(By.XPATH, "//*[@id='approvals']//button[normalize-space()='Decide in browser']")
        time.sleep(6)  # past one inbox poll (5 s) and one session poll (4 s)
        try:
            still = d.execute_script("return arguments[0].isConnected", held)
        except StaleElementReferenceException:
            still = False
        assert still, "a poll redrew the button under the person's click"
        click(d, By.XPATH, "//*[@id='approvals']//button[normalize-space()='Decide in browser']")
        wait.until(lambda drv: "Decide there" in text(drv, "#toast") or "Nothing is waiting" in text(drv, "#toast"))
        assert link.startswith(f"http://127.0.0.1:{port}/#t="), f"the session opened its browser page: {link}"
        step("Decide there, as the person would (decline)")
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

        step("The audit log, under what waits: this session's calls, filtered, and its chain checked")
        wait.until(lambda drv: "query_features" in text(drv, "#audit-rows"))
        assert "approval_decision" in text(d, "#audit-rows")
        assert "You declined" in text(d, "#audit-rows"), text(d, "#audit-rows")
        click(d, By.ID, "audit-filter")
        wait.until(lambda drv: drv.find_element(By.ID, "audit-tool").is_displayed())
        d.execute_script("const s = document.getElementById('audit-tool'); s.value = 'query_features'; s.dispatchEvent(new Event('change'));")
        wait.until(lambda drv: "whoami" not in text(drv, "#audit-rows") and "query_features" in text(drv, "#audit-rows"))
        click(d, By.ID, "audit-verify")
        wait.until(lambda drv: "holds" in text(drv, "#audit-verify-result"))
        d.save_screenshot(str(OUT / "shell-audit.png"))
        step("Decisions alone, then the exports it records (none: the export was declined)")
        click(d, By.CSS_SELECTOR, "#main .histhead button[data-view='decisions']")
        wait.until(lambda drv: "approval_decision" in text(drv, "#audit-rows") and "query_features" not in text(drv, "#audit-rows"))
        click(d, By.CSS_SELECTOR, "#main .histhead button[data-view='exports']")
        wait.until(lambda drv: drv.find_element(By.ID, "exports").is_displayed() and "No exports yet" in text(drv, "#exports"))

        step("The person's note: read beside the panel, then marked reviewed (their own file)")
        place(d, wait, "#/data", "Data")
        wait.until(lambda drv: "1 is a draft" in text(drv, "#data-notes"))
        click(d, By.ID, "data-notes")
        wait.until(lambda drv: "Notes" == text(drv, "#page-title"))
        wait.until(lambda drv: "tracts" in text(drv, "#notes-list"))
        click(d, By.CSS_SELECTOR, "#notes-list li")
        wait.until(lambda drv: "NAME: the tract's name" in text(drv, "#side"))
        step("A note changed after the person read it: nothing is marked")
        # Changed after the person read it (as an assistant allowed to edit
        # draft notes could): nothing is marked.
        note.write_text(NOTE.replace("the tract's name", "changed behind your back"))
        click(d, By.XPATH, "//button[normalize-space()='Mark reviewed']")
        wait.until(lambda drv: "changed after you read it" in text(drv, "#side"))
        assert "status: draft" in note.read_text()
        step("Read again, then marked: only the status word changes")
        click(d, By.CSS_SELECTOR, "#notes-list li")
        wait.until(lambda drv: "changed behind your back" in text(drv, "#side"))
        click(d, By.XPATH, "//button[normalize-space()='Mark reviewed']")
        wait.until(lambda drv: "status: reviewed" in note.read_text())
        want = NOTE.replace("the tract's name", "changed behind your back").replace("status: draft", "status: reviewed")
        assert note.read_text() == want, note.read_text()

        step("Esc closes the sheet beside the panel")
        assert d.find_element(By.ID, "side").is_displayed()
        d.execute_script("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true, cancelable: true}))")
        wait.until(lambda drv: not drv.find_element(By.ID, "side").is_displayed())

        step("Ctrl+K opens the search; a connection is found by its name and opens its page")
        d.execute_script("document.dispatchEvent(new KeyboardEvent('keydown', {key: 'k', ctrlKey: true}))")
        wait.until(lambda drv: drv.execute_script("return document.getElementById('palette').open"))
        d.find_element(By.ID, "palette-q").send_keys("tig")
        row = "//ul[@id='palette-list']/li[span[normalize-space()='tiger'] and span[normalize-space()='Data']]"
        wait.until(lambda drv: drv.find_elements(By.XPATH, row))
        named(d)
        d.find_element(By.XPATH, row).click()
        wait.until(lambda drv: drv.execute_script("return location.hash") == "#/data/c/tiger")
        wait.until(lambda drv: text(drv, "#page-title") == "tiger" and drv.find_elements(By.ID, "source-edit"))
        named(d)
        assert not d.execute_script("return document.getElementById('palette').open")

        step("A classic window is still there, from Settings")
        click(d, By.ID, "nav-settings")
        wait.until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#settings .classic"))
        click(d, By.CSS_SELECTOR, "#settings .classic")
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
        guard.stop(d, driver_proc)
        if exporter:
            exporter.join(5)
        mcp.close()


if __name__ == "__main__":
    main()
