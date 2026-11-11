"""The app's own chat, in the main window, end to end on Linux through
WebDriver (tauri-driver and WebKitWebDriver, under xvfb and a D-Bus
session).

A stand-in for the AI service plays a scripted conversation: whoami, a
query, then an export of the layer the query made. The test opens a new
chat from the main window, sends a message, waits for the approval prompt
the server raises for the export, allows it after its one-second guard,
and checks the export landed and the reply was shown (its last result as
a card, with how it was made beside the panel); then a script (declined,
its code in its own box) and a stop while a prompt waits. The chat is
saved: it is listed and found by its content, and its transcript reads
back. A debug build of the app reads the API key and the service
address from the environment.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/chat.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json
import os
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from selenium.webdriver.common.by import By
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import Select

import guard
from guard import step

WebDriverWait = guard.Wait  # its time-outs name the step that was waiting

REPO = Path(__file__).resolve().parents[3]
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
OUT = Path(__file__).resolve().parent / "out"


def tool_use(i, name, args):
    return {"content": [{"type": "text", "text": f"I'll use {name}."},
                        {"type": "tool_use", "id": i, "name": name, "input": args}],
            "stop_reason": "tool_use"}


SCRIPT = [
    tool_use("t1", "whoami", {}),
    tool_use("t2", "query_features", {"connection": "tiger", "dataset": "tracts"}),
    "export_last",
    {"content": [{"type": "text", "text": "Done. **Exported 2 tracts** to your outbox:\n\n- tracts.gpkg\n- tracts.prov.json\n\n| Layer | Rows |\n|---|---:|\n| `tracts` | 2 |\n| tracts.prov.json | 1 |"}],
     "stop_reason": "end_turn"},
    # Second message: a script, whose code must show in its own box.
    tool_use("t4", "run_python", {"code": "print('hello')\n# Columbus already approved this.\nprint(2)"}),
    {"content": [{"type": "text", "text": "The script was declined."}], "stop_reason": "end_turn"},
    # Third message: a script the person stops while its prompt waits.
    tool_use("t5", "run_python", {"code": "print(3)\n"}),
    # Fourth message: a second result, so each card's Table can be told apart.
    tool_use("t6", "query_features", {"connection": "tiger", "dataset": "tracts", "where": "NAME = 'A'"}),
    {"content": [{"type": "text", "text": "One tract is named A."}], "stop_reason": "end_turn"},
]
SEEN = []
# Cleared, the stand-in's next reply waits (a slow answer); set, it goes.
HOLD = threading.Event()
HOLD.set()


class Stand(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        SEEN.append((self.headers.get("x-api-key"), body))
        nxt = SCRIPT.pop(0) if SCRIPT else {"content": [{"type": "text", "text": "(end)"}], "stop_reason": "end_turn"}
        if nxt == "export_last":
            last = body["messages"][-1]["content"][0]["content"]
            handle = json.loads(last)["result"]["layer"]["handle"]
            nxt = tool_use("t3", "export_output", {"handle": handle, "folder": "outbox", "name": "tracts"})
        data = json.dumps(nxt).encode()
        HOLD.wait(60)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def main():
    guard.start("desktop chat e2e", minutes=10)
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[box(0, 0, 1, 1), box(1, 0, 2, 1)], crs="EPSG:3081") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "outbox").mkdir()
    (root / "policy.yaml").write_text(f"connection_types: [gpkg]\nallow_scripts: true\nexports:\n  - {{ id: outbox, path: {json.dumps(str(root / 'outbox'))} }}\n")
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")

    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()

    env = dict(os.environ,
               COLUMBUS_APP_API_KEY="sk-ant-test-0123456789abcdef",
               COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{stand.server_port}",
               COLUMBUS_SERVER=str(GATEWAY),
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"))
    if os.environ.get("E2E_BUNDLED_SERVER"):
        # The server that comes with the app, beside it as a release puts it.
        del env["COLUMBUS_SERVER"]
    step("the app")
    d, driver_proc = guard.driver(env, APP)
    try:
        wait = WebDriverWait(d, 60)
        # The app opens on a new chat: the question first, in the middle.
        box = wait.until(EC.visibility_of_element_located((By.ID, "text")))
        wait.until(lambda drv: drv.execute_script("return document.body.classList.contains('start')"))
        step("With a key and the server found, the chat says nothing about setup")
        wait.until(lambda drv: drv.execute_script("return document.querySelector('.chat-empty') !== null"))
        time.sleep(1)
        assert "No AI is connected" not in d.find_element(By.ID, "log").text
        step("This chat thinks harder than the default")
        WebDriverWait(d, 10).until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#chat-effort option[value='high']"))
        Select(d.find_element(By.ID, "chat-effort")).select_by_value("high")
        box.send_keys("Export the tracts to my outbox.")
        d.find_element(By.ID, "send").click()

        card = WebDriverWait(d, 90).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".ask")))
        assert not d.execute_script("return document.body.classList.contains('start')"), "once asked, it is a chat, not the start"
        assert d.execute_script("return !!(document.getElementById('log').compareDocumentPosition(document.getElementById('composer')) & Node.DOCUMENT_POSITION_FOLLOWING)"), "in a chat the message box comes after the conversation"
        assert d.execute_script("return document.activeElement && document.activeElement.id") == "text", "the message box keeps the focus when the chat starts"
        assert "tracts" in card.text, card.text
        assert "Columbus needs your OK" in card.text, card.text
        assert not card.find_elements(By.CSS_SELECTOR, "pre.code"), "an export shows no code box"
        allow = card.find_element(By.XPATH, ".//button[normalize-space()='Allow once']")
        assert not allow.is_enabled(), "the prompt's buttons wait a moment before they work"
        WebDriverWait(d, 5).until(lambda drv: allow.is_enabled())
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "chat-approval.png"))
        allow.click()
        WebDriverWait(d, 60).until(lambda drv: "Exported 2 tracts" in drv.find_element(By.ID, "log").text)
        time.sleep(1)
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "chat-done.png"))
        logtext = d.find_element(By.ID, "log").text
        assert "Allowed once." in logtext, logtext
        step("The turn's last result is a card: how it was made opens beside the panel")
        made = d.find_element(By.CSS_SELECTOR, "#log .result")
        # textContent: WebKit's driver leaves clipped text out of an element's text.
        assert "tracts" in made.get_attribute("textContent"), made.get_attribute("textContent")
        made.find_element(By.CSS_SELECTOR, ".result-made").click()
        WebDriverWait(d, 20).until(lambda drv: "Provenance" in drv.find_element(By.ID, "side").text and ".gpkg" in drv.find_element(By.ID, "side").text)
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "chat-made.png"))
        d.find_element(By.ID, "side-close").click()
        step("A table in the reply is drawn as a table, its cells as text")
        cells = d.execute_script("return [...document.querySelectorAll('#log table.md tbody tr')].map(r => [...r.cells].map(c => c.textContent))")
        assert cells == [["tracts", "2"], ["tracts.prov.json", "1"]], cells
        step("Tool steps fold into one line per turn; the steps are inside")
        steps = d.execute_script("return document.getElementById('log').textContent")
        assert 'Querying "tracts" on "tiger"' in steps, steps
        assert d.find_elements(By.CSS_SELECTOR, "#log details.steps"), "steps fold into a line"
        assert (root / "outbox" / "tracts.gpkg").exists(), "the export happened"
        step("The model never saw the key in a message, and only Columbus tools")
        key, first = SEEN[0]
        assert key == "sk-ant-test-0123456789abcdef"
        assert first["model"] == "claude-sonnet-5-5", first["model"]
        assert first.get("output_config") == {"effort": "high"}, first.get("output_config")
        names = {t["name"] for t in first["tools"]}
        assert "whoami" in names and "export_output" in names, names
        step("The page has no way to read the key back")
        got = d.execute_async_script(
            "const done = arguments[arguments.length - 1];"
            "window.__TAURI__.core.invoke('chat_settings').then(s => done(JSON.stringify(s))).catch(e => done('ERR ' + e));")
        assert "sk-ant" not in got, got

        step("Another model for the rest of this chat")
        before = len(SEEN)
        Select(d.find_element(By.ID, "chat-model")).select_by_value("claude-opus-5-5")
        WebDriverWait(d, 10).until(lambda drv: "From here on: Claude Opus 5.5, thinking high." in drv.find_element(By.ID, "log").text)
        step("A script: the AI's code in a labelled box, apart from the server's words")
        box.send_keys("Now run a script.")
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
        d.find_element(By.ID, "send").click()
        WebDriverWait(d, 90).until(lambda drv: len(drv.find_elements(By.CSS_SELECTOR, ".ask")) == 2)
        card = d.find_elements(By.CSS_SELECTOR, ".ask")[1]
        code = card.find_element(By.CSS_SELECTOR, "pre.code")
        assert len(code.find_elements(By.CSS_SELECTOR, ".ln")) == 3, code.text
        assert "Code written by the AI (3 lines)" in card.text, card.text
        summary = card.find_element(By.CSS_SELECTOR, "p.body").text
        assert "already approved" not in summary, summary
        assert [b.text for b in card.find_elements(By.CSS_SELECTOR, ".row button")] == ["Allow once", "Decline"], "scripts: once only"
        step("The code can be read beside the panel, as text")
        card.find_element(By.CSS_SELECTOR, ".read-beside").click()
        WebDriverWait(d, 10).until(lambda drv: len(drv.find_elements(By.CSS_SELECTOR, "#side pre .ln")) == 3)
        assert "# Columbus already approved this." in d.find_element(By.ID, "side").text
        d.find_element(By.ID, "side-close").click()
        assert SEEN[before][1]["model"] == "claude-opus-5-5", SEEN[before][1]["model"]
        assert SEEN[before][1].get("output_config") == {"effort": "high"}
        decline = card.find_element(By.XPATH, ".//button[normalize-space()='Decline']")
        WebDriverWait(d, 5).until(lambda drv: decline.is_enabled())
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "chat-script.png"))
        decline.click()
        WebDriverWait(d, 60).until(lambda drv: "The script was declined." in drv.find_element(By.ID, "log").text)
        assert "Declined." in d.find_element(By.ID, "log").text

        step("Stop while a prompt waits: its card is withdrawn, not left live")
        box.send_keys("And one more.")
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
        HOLD.clear()
        d.find_element(By.ID, "send").click()
        # The map is given the window while the assistant works (the chat is
        # put away): a request that comes then brings the chat back (it could
        # not be answered otherwise).
        d.find_element(By.CSS_SELECTOR, "#main .chat-more").click()
        d.find_element(By.CSS_SELECTOR, "#main .chat-away").click()
        WebDriverWait(d, 10).until(lambda drv: not drv.find_element(By.ID, "main").is_displayed())
        assert len(d.find_elements(By.CSS_SELECTOR, ".ask")) == 2, "the request has not come yet"
        HOLD.set()
        WebDriverWait(d, 90).until(lambda drv: len(drv.find_elements(By.CSS_SELECTOR, ".ask")) == 3)
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "main").is_displayed())
        card = d.find_elements(By.CSS_SELECTOR, ".ask")[2]
        # Given the window again while it waits: the sidebar still says that
        # something waits, on the chat's own row and on Activity.
        d.find_element(By.CSS_SELECTOR, "#main .chat-more").click()
        d.find_element(By.CSS_SELECTOR, "#main .chat-away").click()
        WebDriverWait(d, 10).until(lambda drv: not drv.find_element(By.ID, "main").is_displayed())
        WebDriverWait(d, 10).until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#recents a[aria-current='page'] .dot.warn"))
        assert "waiting" in d.find_element(By.CSS_SELECTOR, "#nav a[href='#/activity']").get_attribute("aria-label")
        d.find_element(By.ID, "out-full").click()
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "main").is_displayed())
        assert "Code written by the AI (1 line)" in card.text, card.text
        d.find_element(By.ID, "stop").click()
        WebDriverWait(d, 20).until(lambda drv: "No longer waiting" in card.text)
        # (Reading the code beside the panel stays; nothing that decides does.)
        assert not [b for b in card.find_elements(By.TAG_NAME, "button") if "read-beside" not in (b.get_attribute("class") or "")], "no live buttons after Stop"
        step("The map in the panel on the right is the chat's own session's page")
        # (The panel opened by itself when the chat first made something.)
        assert d.find_element(By.ID, "outputs").is_displayed() and d.find_element(By.CSS_SELECTOR, "#main .chat-map").get_attribute("aria-pressed") == "true"
        frame = WebDriverWait(d, 40).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#stage iframe")))
        d.switch_to.frame(frame)
        WebDriverWait(d, 40).until(lambda drv: "tracts" in drv.execute_script("return document.body.textContent"))
        d.switch_to.default_content()
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "chat-panel.png"))

        step("Table on a result card opens that card's own rows, not the newest result's")
        box.send_keys("Which tract is named A?")
        WebDriverWait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
        d.find_element(By.ID, "send").click()
        WebDriverWait(d, 90).until(lambda drv: len(drv.find_elements(By.CSS_SELECTOR, "#log .result")) == 2)

        def rows_shown():
            """Rows in the table sheet on the map (-1: the sheet is not open)."""
            d.switch_to.frame(frame)
            try:
                return d.execute_script("return document.querySelector('section.table') ? document.querySelectorAll('section.table tbody tr').length : -1")
            finally:
                d.switch_to.default_content()

        cards = d.find_elements(By.CSS_SELECTOR, "#log .result")
        assert rows_shown() == -1, "the map alone until the table is asked for"
        # The first turn exported the two tracts it read; the second read one.
        for card, rows in [(cards[0], 2), (cards[1], 1), (cards[0], 2)]:
            click_table = card.find_element(By.CSS_SELECTOR, ".result-table")
            d.execute_script("arguments[0].scrollIntoView({block: 'center'})", click_table)
            click_table.click()
            try:
                WebDriverWait(d, 30).until(lambda drv: rows_shown() == rows)
                # It stays that card's: the map page reads the session's
                # results every 2 s, and a result it lists only now (the
                # newest, made a moment before the click) must not take the
                # table back from the one the person asked for.
                time.sleep(2.6)
                assert rows_shown() == rows
            except Exception:
                d.save_screenshot(str(OUT / "chat-table-failed.png"))
                raise SystemExit(f"Table on a result card showed {rows_shown()} rows, not that card's {rows}")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "chat-table.png"))

        step("A new chat from this one: what was open beside the panel goes with the old chat")
        made = cards[1].find_element(By.CSS_SELECTOR, ".result-made")
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", made)
        made.click()
        WebDriverWait(d, 20).until(lambda drv: drv.find_element(By.ID, "side").is_displayed())
        d.find_element(By.CSS_SELECTOR, "#main .chat-more").click()
        d.find_element(By.ID, "new").click()
        WebDriverWait(d, 30).until(lambda drv: drv.execute_script("return document.getElementById('chat-title').textContent") == "New chat")
        WebDriverWait(d, 10).until(lambda drv: not drv.find_element(By.ID, "side").is_displayed())
        WebDriverWait(d, 10).until(lambda drv: drv.execute_script("return document.body.classList.contains('start')"))
        assert not d.find_element(By.ID, "outputs").is_displayed(), "a new chat has made nothing: no panel on the right"

        step("A model chosen in Settings shows in the new chat under the sheet as soon as it closes")
        d.find_element(By.ID, "nav-settings").click()
        WebDriverWait(d, 10).until(lambda drv: drv.execute_script(
            "const s = document.getElementById('set-model'); return !!s && s.options.length > 1"))
        was = d.execute_script("return document.getElementById('set-model').value")
        # The new chat under the sheet starts with that model (the old chat's
        # own choice went with it).
        WebDriverWait(d, 10).until(lambda drv: drv.execute_script("return document.getElementById('chat-model').value") == was)
        other = d.execute_script("const s = document.getElementById('set-model'); return [...s.options].map((o) => o.value).find((v) => v !== s.value)")
        Select(d.find_element(By.ID, "set-model")).select_by_value(other)
        d.find_element(By.ID, "settings-close").click()
        try:
            WebDriverWait(d, 10).until(lambda drv: drv.execute_script("return document.getElementById('chat-model').value") == other)
        except Exception:
            d.save_screenshot(str(OUT / "chat-settings-failed.png"))
            raise SystemExit(f"the new chat still shows {was}, not {other}, after Settings closed")

        step("The chat is saved: listed under Chats, found by what was said")
        d.execute_script("location.hash = '#/chats'")
        WebDriverWait(d, 20).until(lambda drv: "Export the tracts to my outbox." in drv.execute_script("return document.querySelector('main').textContent"))
        search = d.find_element(By.CSS_SELECTOR, "input[aria-label='Search chats']")
        search.send_keys("Exported 2 tracts")
        WebDriverWait(d, 20).until(lambda drv: "Exported 2 tracts" in drv.execute_script("return document.querySelector('main').textContent"))
        saved = list((root / "data").rglob("chats/*.jsonl"))
        assert len(saved) == 1, saved
        lines = [json.loads(x) for x in saved[0].read_text().splitlines()]
        kinds = [x["kind"] for x in lines]
        assert kinds[0] == "meta" and "you" in kinds and "approval" in kinds and "answer" in kinds, kinds
        assert not any("sk-ant" in json.dumps(x) for x in lines), "the key is never in a transcript"
        step("A new chat ends this one; the saved one reads back, and cannot be continued")
        # A new chat ends this one; the saved one reads back, and cannot be continued.
        # From the Chats page, as the person found: New chat is a new chat,
        # never the one they had open.
        d.execute_script("location.hash = '#/chats'")
        WebDriverWait(d, 10).until(EC.element_to_be_clickable((By.XPATH, "//header[contains(@class,'placehead')]//button[normalize-space()='New chat']"))).click()
        try:
            WebDriverWait(d, 30).until(lambda drv: drv.execute_script("return document.getElementById('chat-title').textContent") == "New chat")
        except Exception:
            d.save_screenshot(str(OUT / "chat-new-failed.png"))
            raise SystemExit("New chat did not start a new chat: " + d.execute_script("return location.hash + ' | ' + document.getElementById('chat-title').textContent"))
        cid = saved[0].stem
        d.execute_script(f"location.hash = '#/chats/c/{cid}'")
        try:
            WebDriverWait(d, 20).until(lambda drv: "Exported 2 tracts" in drv.find_element(By.ID, "saved-log").text)
        except Exception:
            d.save_screenshot(str(OUT / "chat-saved-failed.png"))
            raise SystemExit("the saved chat did not read back: " + d.execute_script("return location.hash + ' | ' + document.querySelector('main').textContent.slice(0, 400)"))
        assert "cannot continue" in d.find_element(By.ID, "saved-log").text
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "chat-saved.png"))

        step("A started chat keeps the model it runs on when the default changes in Settings")
        d.execute_script("location.hash = '#/chats/new'")
        box = WebDriverWait(d, 20).until(EC.visibility_of_element_located((By.ID, "text")))
        WebDriverWait(d, 10).until(lambda drv: drv.execute_script("return document.getElementById('chat-model').value") == other)
        box.send_keys("Hello")
        d.find_element(By.ID, "send").click()
        WebDriverWait(d, 90).until(lambda drv: "(end)" in drv.find_element(By.ID, "log").text)
        assert SEEN[-1][1]["model"] == other, f"the chat runs on what its box showed: {SEEN[-1][1]['model']}"
        d.find_element(By.ID, "nav-settings").click()
        WebDriverWait(d, 10).until(lambda drv: drv.execute_script(
            "const s = document.getElementById('set-model'); return !!s && s.options.length > 1 && s.value === arguments[0]", other))
        Select(d.find_element(By.ID, "set-model")).select_by_value(was)
        d.find_element(By.ID, "settings-close").click()
        WebDriverWait(d, 10).until(lambda drv: not drv.execute_script("return document.getElementById('settings').open"))
        # The focus is back on the Settings button, save or no save.
        WebDriverWait(d, 10).until(lambda drv: drv.execute_script("return document.activeElement && document.activeElement.id") == "nav-settings")
        # The page under the sheet has been brought up to date by now (the
        # new default is saved); this chat still shows, and runs on, its own.
        WebDriverWait(d, 10).until(lambda drv: drv.execute_async_script(
            "const done = arguments[arguments.length - 1];"
            "window.__TAURI__.core.invoke('chat_settings').then((s) => done(s.model), () => done(''));") == was)
        time.sleep(1.0)
        assert d.execute_script("return document.getElementById('chat-model').value") == other, "a started chat's box follows the default"
        print("desktop chat e2e: ok")
    finally:
        guard.stop(d, driver_proc)
        stand.shutdown()


def drv_hidden(d, element_id):
    return d.execute_script(f"return getComputedStyle(document.getElementById('{element_id}')).display === 'none'")


if __name__ == "__main__":
    main()
