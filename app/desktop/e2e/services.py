"""The app's chat with any AI service (T-38), end to end on Linux through
WebDriver (tauri-driver and WebKitWebDriver, under xvfb and a D-Bus
session), beside a real Columbus session.

One stand-in server plays three kinds of AI service, each in its own
request format, told apart by the address a request goes to:
`/v1/messages` (Anthropic Messages), `/responses` (OpenAI Responses,
answered as a stream) and `/chat/completions` (OpenAI Chat Completions).
No real service is contacted, and the keys here are made up.

The script first takes the first run's AI row through each state it has
(a service that needs a key with none saved, one on this computer, one the
organization's list refuses, one with its key saved, and the app's reads
failing), and Settings > Assistant with it: in none does anything read as a
value that was not there (the word "null" or "undefined"). Then it
chooses a service in the first run and saves its key; chats on
OpenAI (a tool call, an approval, the answer), changes the model in the
chat, and checks the chat shows and keeps its service while Settings moves
on to Google Gemini (its key, its own list of models, a typed model id);
then Anthropic, and Ollama, which needs no key; adds another service by
its address (confirmed with the tests' stand-in for Touch ID / Windows
Hello), chats on it at that address with its key in the header it was set
up with, and removes it; reads how to connect other AI apps, with this
computer's path to columbus-mcp, and copies an entry; has the
organization's assistant.hosts refuse a service in Settings, at the start
of a chat and in a running chat; and, in a second app with the
confirmation declined, is refused the adding of a service. No page is ever
given a key, and none is in any file. A debug build of the app keeps the
keys in memory (COLUMBUS_APP_TEST_KEYS) and reads the stand-in's address
from the environment.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/services.py \\
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
from selenium.webdriver.common.keys import Keys
from selenium.webdriver.support import expected_conditions as EC
from selenium.webdriver.support.ui import Select

import guard
from guard import step

Wait = guard.Wait  # its time-outs name the step that was waiting

REPO = Path(__file__).resolve().parents[3]
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
OUT = Path(__file__).resolve().parent / "out"

# Made up for this script: no service ever issued them. (No word in them:
# a piece of a key eight characters long is taken out of whatever a service
# sends, a model's name included.)
KEYS = {
    "openai": "tk-o1-7Qm2Xv9LpR4sWz81TnBc6Yd0HgEuJk5F",
    "gemini": "tk-g2-3Hc8Vb1NxK6mQw72RtLp9Zs4YdGfJe0A",
    "anthropic": "tk-a3-5Wd0Zk3MvP8qLx64CnTr1Bs7YhGjFe2U",
    "gateway": "tk-gw-9Lp2Qs7x",
}

# What "the model" does next, the same whatever the service: ("call", id,
# tool, arguments), "export_last" (export the layer the last result made),
# or ("say", words).
SCRIPT = []
SEEN = []  # (path, headers, body) of every request
LISTS = []  # (path, headers) of every request for a list of models


def last_result(kind, body):
    """The text of the last tool result in a request."""
    if kind == "messages":
        c = body["messages"][-1]["content"][0]["content"]
        return c if isinstance(c, str) else c[0]["text"]
    if kind == "responses":
        out = [i for i in body["input"] if i.get("type") == "function_call_output"][-1]["output"]
        return out if isinstance(out, str) else out[0]["text"]
    return [m for m in body["messages"] if m.get("role") == "tool"][-1]["content"]


def sse(name, data):
    return f"event: {name}\ndata: {json.dumps(dict(data, type=name))}\n\n"


def render(kind, text, calls):
    """One reply in the service's own format: (content type, body)."""
    if kind == "messages":
        content = ([{"type": "text", "text": text}] if text else []) + [
            {"type": "tool_use", "id": i, "name": n, "input": a} for i, n, a in calls]
        return "application/json", json.dumps({"content": content, "stop_reason": "tool_use" if calls else "end_turn",
                                               "usage": {"input_tokens": 120, "output_tokens": 9}})
    if kind == "responses":
        items = []
        if text:
            items.append({"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                          "content": [{"type": "output_text", "text": text, "annotations": []}]})
        for n, (i, name, a) in enumerate(calls):
            items.append({"type": "function_call", "id": f"fc_{n}", "call_id": i, "name": name,
                          "arguments": json.dumps(a), "status": "completed"})
        whole = {"id": "resp_1", "object": "response", "status": "completed", "output": items,
                 "usage": {"input_tokens": 120, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 9}}
        out = sse("response.created", {"response": {"id": "resp_1", "status": "in_progress", "output": []}})
        for n, item in enumerate(items):
            out += sse("response.output_item.done", {"output_index": n, "item": item})
        return "text/event-stream", out + sse("response.completed", {"response": whole})
    message = {"role": "assistant", "content": text or None}
    if calls:
        message["tool_calls"] = [{"id": i, "type": "function", "function": {"name": n, "arguments": json.dumps(a)}}
                                 for i, n, a in calls]
    return "application/json", json.dumps({
        "id": "chatcmpl-1", "object": "chat.completion",
        "choices": [{"index": 0, "message": message, "finish_reason": "tool_calls" if calls else "stop"}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 9, "total_tokens": 129}})


class Stand(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def kind(self):
        if self.path.endswith("/v1/messages"):
            return "messages"
        if self.path.endswith("/responses"):
            return "responses"
        if self.path.endswith("/chat/completions"):
            return "chat"
        return None

    def answer(self, code, ctype, data):
        data = data.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        LISTS.append((self.path, dict(self.headers)))
        if self.path.endswith("/models"):
            ids = ["models/gemini-3.8-flash", "models/gemini-4-argon", "gemini-list-only", "<b>not plain</b>"]
            return self.answer(200, "application/json", json.dumps({"object": "list", "data": [{"id": i, "object": "model"} for i in ids]}))
        self.answer(404, "text/plain", "not found")

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        kind = self.kind()
        SEEN.append((self.path, dict(self.headers), body))
        if kind is None:
            return self.answer(404, "application/json", json.dumps({"error": {"message": "nothing is served here"}}))
        nxt = SCRIPT.pop(0) if SCRIPT else ("say", "(end)")
        if nxt == "export_last":
            handle = json.loads(last_result(kind, body))["result"]["layer"]["handle"]
            nxt = ("call", "t3", "export_output", {"handle": handle, "folder": "outbox", "name": "tracts"})
        if nxt[0] == "say":
            ctype, data = render(kind, nxt[1], [])
        else:
            ctype, data = render(kind, f"I'll use {nxt[2]}.", [(nxt[1], nxt[2], nxt[3])])
        self.answer(200, ctype, data)


def header(headers, name):
    """A request header, whatever its letter case."""
    return next((v for k, v in headers.items() if k.lower() == name), None)


def settings_json(d):
    """What a page is told by chat_settings, as text and as a value."""
    got = d.execute_async_script(
        "const done = arguments[arguments.length - 1];"
        "window.__TAURI__.core.invoke('chat_settings').then(s => done(JSON.stringify(s))).catch(e => done('ERR ' + e));")
    assert not got.startswith("ERR"), got
    return got, json.loads(got)


def no_key_on_the_page(d):
    """No key, whole or the part after its made-up start, in what the page was told or shows."""
    told, _ = settings_json(d)
    shown = d.execute_script("return document.documentElement.outerHTML + [...document.querySelectorAll('input, textarea')].map(e => e.value).join(' ')")
    for name, key in KEYS.items():
        for where, text in (("chat_settings", told), ("the page", shown)):
            assert key not in text and key[8:] not in text, f"the {name} key is in {where}"


def reads_right(d, within):
    """Nothing in this part of the window reads as a value that was not
    there: no text, title, label, placeholder or field says "null",
    "undefined" or "NaN", alone or inside a sentence. (The words here are
    all the app's own. `Element.append(null)` writes the word "null".)"""
    bad = guard.stray(d, within, words=True) + guard.stray(d)
    assert not bad, bad


def ai_row(d):
    """The parts of the first run's AI row, in order: an element by its tag
    and id, a bare piece of text by what it says."""
    return d.execute_script(
        "return [...document.querySelector('#first-steps .checks > li .text').childNodes]"
        ".map(n => n.nodeType === 3 ? 'text: ' + n.data : n.tagName.toLowerCase() + (n.id ? '#' + n.id : ''))")


def first_steps(d):
    return d.execute_script("const e = document.getElementById('first-steps'); return e ? e.textContent : ''")


def choose(d, select_id, value):
    """Choose an option of a select as a person does: with the select in
    hand first. (WebKit sends `change` only when the choice differs from the
    one it noted when the select last took the focus or opened; a page that
    has since filled the select anew, with its own choice, is noted only
    then. The driver's click on an option alone does not make it look.)"""
    d.execute_script("const s = document.getElementById(arguments[0]); s.blur(); s.focus();", select_id)
    Select(d.find_element(By.ID, select_id)).select_by_value(value)


def chat_service(d):
    """The service the chat's foot names. (textContent: WebKit's driver
    leaves text that may be clipped out of an element's text.)"""
    return d.execute_script("return document.getElementById('chat-service').textContent")


def send(d, text, done, wait=90):
    box = d.find_element(By.ID, "text")
    box.send_keys(text)
    Wait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
    d.find_element(By.ID, "send").click()
    if done:
        Wait(d, wait).until(lambda drv: done in drv.find_element(By.ID, "log").text)
        # The reply is over once Send is back (the model boxes work again then).
        Wait(d, 20).until(lambda drv: not drv.execute_script("return document.getElementById('composer').classList.contains('busy')"))


def new_chat(d, ready=True):
    """A new chat. `ready`: an AI is set up, so the message box is there."""
    d.execute_script("location.hash = '#/chats/fresh'")
    Wait(d, 30).until(lambda drv: drv.execute_script("return document.getElementById('chat-title').textContent") == "New chat"
                      and drv.execute_script("return location.hash") == "#/chats/new")
    if ready:
        Wait(d, 10).until(EC.visibility_of_element_located((By.ID, "text")))
    time.sleep(0.5)


def open_assistant(d):
    d.execute_script("location.hash = '#/settings/ai'")
    Wait(d, 20).until(lambda drv: drv.execute_script(
        "const s = document.getElementById('service'); return !!s && s.options.length >= 7 && !!document.getElementById('apps-entry').textContent"))
    reads_right(d, "#settings")


def close_settings(d):
    d.find_element(By.ID, "settings-close").click()
    Wait(d, 10).until(lambda drv: not drv.execute_script("return document.getElementById('settings').open"))
    time.sleep(0.5)


def choose_service(d, service):
    """Choose a service under Settings > Assistant, and wait until it is the one in use."""
    choose(d, "service", service)
    Wait(d, 15).until(lambda drv: settings_json(drv)[1]["service"] == service)
    Wait(d, 10).until(lambda drv: drv.execute_script("return document.getElementById('service').value") == service)
    time.sleep(0.4)
    reads_right(d, "#settings")


def save_key(d, key):
    box = d.find_element(By.ID, "key")
    box.send_keys(key)
    d.find_element(By.CSS_SELECTOR, "#key-form button[type=submit]").click()
    Wait(d, 15).until(lambda drv: f"ending in {key[-4:]}" in drv.find_element(By.ID, "key-state").text)
    assert d.find_element(By.ID, "key").get_attribute("value") == "", "the key does not stay in the field"
    reads_right(d, "#settings")


def write_policy(root, hosts=None):
    text = f"connection_types: [gpkg]\nexports:\n  - {{ id: outbox, path: {json.dumps(str(root / 'outbox'))} }}\n"
    if hosts is not None:
        text += f"assistant:\n  hosts: {json.dumps(hosts)}\n"
    (root / "policy.yaml").write_text(text)
    # (The app reads the file at each check; the session, once it has kept still a second.)
    time.sleep(1.5)


def main():
    guard.start("desktop services e2e", minutes=12)
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box as square

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[square(0, 0, 1, 1), square(1, 0, 2, 1)], crs="EPSG:3081") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "outbox").mkdir()
    write_policy(root)
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")

    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()
    here = f"http://127.0.0.1:{stand.server_port}"

    env = dict(os.environ,
               COLUMBUS_APP_TEST_KEYS="memory", COLUMBUS_APP_API_BASE=here, COLUMBUS_APP_TEST_CONFIRM="allow",
               COLUMBUS_SERVER=str(GATEWAY),
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"))
    env.pop("COLUMBUS_APP_API_KEY", None)

    step("the app, with nothing set up at all: no AI, no data")
    (root / "no-connections").mkdir()
    bare = dict(env, COLUMBUS_CONNECTIONS=str(root / "no-connections"),
                XDG_CONFIG_HOME=str(root / "config-first"), XDG_DATA_HOME=str(root / "data-first"))
    d, driver_proc = guard.driver(bare, APP)
    try:
        wait = Wait(d, 60)
        d.set_window_size(1400, 900)
        guard.ready(d, wait)

        step("The first run's AI row, a service that needs a key and has none: the key's field, and nothing more to say")
        wait.until(EC.presence_of_element_located((By.ID, "fr-service")))
        assert d.find_element(By.ID, "fr-key").get_attribute("aria-label") == "API key for Anthropic"
        # (This is how every new install on Windows or a Mac begins.)
        assert ai_row(d) == ["b", "form#fr-ai-form", "p#fr-ai-error", "span#fr-ai-about"], ai_row(d)
        reads_right(d, "#first-steps")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "services-first-run-no-key.png"))
        open_assistant(d)
        assert "No key saved for Anthropic yet" in d.find_element(By.ID, "key-state").text
        assert "None added." in d.find_element(By.ID, "services-none").text
        close_settings(d)

        step("The first run's AI row, a service on this computer: no key field, and what is still missing")
        choose(d, "fr-service", "ollama")
        wait.until(lambda drv: "Ollama must be running" in drv.find_element(By.ID, "fr-ai-about").text)
        assert not d.find_elements(By.ID, "fr-key"), "a service that takes no key shows no key field"
        assert ai_row(d) == ["b", "form#fr-ai-form", "p#fr-ai-error", "span#fr-ai-need", "span#fr-ai-about"], ai_row(d)
        assert "Choose its model in Settings › Assistant" in d.find_element(By.ID, "fr-ai-need").text
        reads_right(d, "#first-steps")
        # One that takes a key only if it asks for one.
        choose(d, "fr-service", "lmstudio")
        wait.until(lambda drv: drv.find_elements(By.ID, "fr-key")
                   and drv.find_element(By.ID, "fr-key").get_attribute("aria-label") == "API key for LM Studio on this computer")
        assert ai_row(d) == ["b", "form#fr-ai-form", "p#fr-ai-error", "span#fr-ai-need", "span#fr-ai-about"], ai_row(d)
        reads_right(d, "#first-steps")
        open_assistant(d)
        assert "(only if it asks for one)" in d.find_element(By.CSS_SELECTOR, "label[for=key]").text
        assert "No model chosen yet" in d.find_element(By.ID, "model").text
        close_settings(d)

        step("The first run's AI row, the organization's list refusing the service: its sentence, in the row")
        choose(d, "fr-service", "openai")
        wait.until(lambda drv: "platform.openai.com" in drv.find_element(By.ID, "fr-ai-about").text)
        assert ai_row(d) == ["b", "form#fr-ai-form", "p#fr-ai-error", "span#fr-ai-about"], ai_row(d)
        reads_right(d, "#first-steps")
        write_policy(root, ["api.anthropic.com"])
        new_chat(d, ready=False)
        refused = "Your organization's rules do not let the app's chat use OpenAI: its address, api.openai.com, is not on the list of AI services allowed (assistant.hosts: api.anthropic.com)."
        wait.until(lambda drv: drv.find_elements(By.ID, "fr-ai-need") and drv.find_element(By.ID, "fr-ai-need").text == refused)
        assert ai_row(d) == ["b", "form#fr-ai-form", "p#fr-ai-error", "span#fr-ai-need", "span#fr-ai-about"], ai_row(d)
        assert Select(d.find_element(By.ID, "fr-service")).first_selected_option.text == "OpenAI (not allowed by your organization)"
        reads_right(d, "#first-steps")
        # Another that is not listed is refused when it is chosen, in words.
        choose(d, "fr-service", "gemini")
        wait.until(lambda drv: "do not let the app's chat use Google Gemini" in drv.find_element(By.ID, "fr-ai-error").text)
        assert d.execute_script("return document.getElementById('fr-service').value") == "openai"
        reads_right(d, "#first-steps")
        d.save_screenshot(str(OUT / "services-first-run-refused.png"))
        open_assistant(d)
        assert "allows the app's chat these services only: api.anthropic.com (assistant.hosts)" in d.find_element(By.ID, "service-org").text
        close_settings(d)
        write_policy(root)

        step("The first run's AI row, a key saved: the service is connected, and the row says so")
        new_chat(d, ready=False)
        wait.until(lambda drv: drv.find_elements(By.ID, "fr-key") and not drv.find_elements(By.ID, "fr-ai-need"))
        d.find_element(By.ID, "fr-key").send_keys(KEYS["openai"])
        d.find_element(By.CSS_SELECTOR, "#fr-ai-form button[type=submit]").click()
        # (No data is added yet, so the first steps stay, under the question.)
        wait.until(lambda drv: "OpenAI is connected for the chat." in first_steps(drv))
        wait.until(EC.visibility_of_element_located((By.ID, "text")))
        assert ai_row(d) == ["b", "span"], ai_row(d)
        reads_right(d, "#first-steps")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        no_key_on_the_page(d)
        d.save_screenshot(str(OUT / "services-first-run-connected.png"))
        open_assistant(d)
        assert f"ending in {KEYS['openai'][-4:]}" in d.find_element(By.ID, "key-state").text
        close_settings(d)

        step("The first run when the app cannot be read: each row says what it can, and nothing reads as a missing value")
        # How the assistant works is not known: its cards are left out.
        guard.answers(d, "settings_state", "fail")
        new_chat(d)
        wait.until(lambda drv: drv.find_elements(By.ID, "first-steps") and not drv.find_elements(By.ID, "fr-mode")
                   and "OpenAI is connected for the chat." in first_steps(drv))
        reads_right(d, "#first-steps")
        guard.answers(d, "settings_state", None)
        # The AI's settings are not known: the row points to Settings.
        guard.answers(d, "chat_settings", "fail")
        new_chat(d, ready=False)
        wait.until(lambda drv: "Set it up in Settings › Assistant." in first_steps(drv) and drv.find_elements(By.ID, "fr-mode"))
        assert ai_row(d) == ["b", "span"], ai_row(d)
        reads_right(d, "#first-steps")
        # An AI is set up, and Columbus's server is not found.
        guard.answers(d, "chat_settings", {"server_found": False})
        new_chat(d, ready=False)
        wait.until(lambda drv: "An AI is set up, but Columbus's server was not found." in first_steps(drv))
        assert ai_row(d) == ["b", "span"], ai_row(d)
        reads_right(d, "#first-steps")
        guard.answers(d, "chat_settings", None)
        # The check of the engine is not answered.
        guard.answers(d, "engine_check", "fail")
        new_chat(d)
        wait.until(lambda drv: "Columbus's server was not found" in first_steps(drv) and "OpenAI is connected for the chat." in first_steps(drv))
        reads_right(d, "#first-steps")
        guard.answers(d, "engine_check", None)
        new_chat(d)
        wait.until(lambda drv: "Found Columbus's server and its Python engine." in first_steps(drv))
        reads_right(d, "#first-steps")
    finally:
        guard.stop(d, driver_proc)

    step("the app, with data and no AI set up")
    d, driver_proc = guard.driver(env, APP)
    try:
        wait = Wait(d, 60)
        d.set_window_size(1400, 900)
        guard.ready(d, wait)

        step("The first run offers the same choice in small: the service, its key, Save")
        pick = wait.until(EC.presence_of_element_located((By.ID, "fr-service")))
        names = [o.text for o in Select(pick).options]
        assert names == ["Anthropic", "OpenAI", "xAI (Grok)", "Google Gemini", "OpenRouter",
                         "Ollama on this computer", "LM Studio on this computer"], names
        assert d.find_element(By.ID, "fr-key").get_attribute("aria-label") == "API key for Anthropic"
        assert "console.anthropic.com" in d.find_element(By.ID, "fr-ai-about").text
        # A service on this computer needs no key, and says what must be running.
        choose(d, "fr-service", "ollama")
        wait.until(lambda drv: "Ollama must be running" in drv.find_element(By.ID, "fr-ai-about").text)
        assert not d.find_elements(By.ID, "fr-key"), "a service that takes no key shows no key field"
        assert "Choose its model in Settings › Assistant" in d.find_element(By.ID, "fr-ai-need").text
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "services-first-run.png"))
        # OpenAI, with its key.
        choose(d, "fr-service", "openai")
        key = wait.until(EC.presence_of_element_located((By.ID, "fr-key")))
        wait.until(lambda drv: drv.find_element(By.ID, "fr-key").get_attribute("aria-label") == "API key for OpenAI")
        about = d.find_element(By.ID, "fr-ai-about").text
        assert "platform.openai.com" in about and "OpenAI bills it" in about and "never read it back" in about, about
        key = d.find_element(By.ID, "fr-key")
        key.send_keys("short")
        d.find_element(By.CSS_SELECTOR, "#fr-ai-form button[type=submit]").click()
        wait.until(lambda drv: "does not look like an API key" in drv.find_element(By.ID, "fr-ai-error").text)
        key = d.find_element(By.ID, "fr-key")
        key.clear()
        key.send_keys(KEYS["openai"])
        d.find_element(By.CSS_SELECTOR, "#fr-ai-form button[type=submit]").click()
        # With an AI connected, the first steps give way to the question.
        wait.until(lambda drv: not drv.find_elements(By.ID, "first-steps") and drv.find_element(By.ID, "text").is_displayed())
        _, s = settings_json(d)
        assert (s["service"], s["service_name"], s["has_key"], s["key_hint"], s["ready"]) == ("openai", "OpenAI", True, KEYS["openai"][-4:], True), s
        no_key_on_the_page(d)

        step("A chat on OpenAI: its own format, a tool call, an approval, the answer")
        wait.until(lambda drv: chat_service(drv) == "OpenAI")
        assert d.execute_script("return document.getElementById('chat-model').value") == "gpt-6.1-sol"
        assert "sends your messages and the results it reads to OpenAI" in d.find_element(By.ID, "chat-model").get_attribute("title")
        offered = d.execute_script("return [...document.getElementById('chat-model').options].map(o => o.value)")
        assert offered == ["gpt-6.1-sol", "gpt-6-astra", "gpt-6.1-terra", "gpt-6-luna"], offered
        levels = d.execute_script("return [...document.getElementById('chat-effort').options].map(o => o.value)")
        assert levels == ["", "low", "medium", "high"], levels
        SCRIPT[:] = [("call", "t1", "whoami", {}),
                     ("call", "t2", "query_features", {"connection": "tiger", "dataset": "tracts"}),
                     "export_last",
                     ("say", "Done. Exported 2 tracts to your outbox.")]
        send(d, "Export the tracts to my outbox.", None)
        card = Wait(d, 90).until(EC.presence_of_element_located((By.CSS_SELECTOR, ".ask")))
        assert "tracts" in card.text and "Columbus needs your OK" in card.text, card.text
        allow = card.find_element(By.XPATH, ".//button[normalize-space()='Allow once']")
        Wait(d, 5).until(lambda drv: allow.is_enabled())
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        allow.click()
        Wait(d, 60).until(lambda drv: "Exported 2 tracts" in drv.find_element(By.ID, "log").text)
        Wait(d, 20).until(lambda drv: not drv.execute_script("return document.getElementById('composer').classList.contains('busy')"))
        assert (root / "outbox" / "tracts.gpkg").exists(), "the export happened"
        assert len(SEEN) == 4 and all(p == "/responses" for p, _, _ in SEEN), [p for p, _, _ in SEEN]
        path, headers, first = SEEN[0]
        assert header(headers, "authorization") == f"Bearer {KEYS['openai']}"
        assert header(headers, "x-api-key") is None
        assert first["model"] == "gpt-6.1-sol" and first["store"] is False and first["stream"] is True, first.keys()
        assert "whoami" in first["instructions"] and first["input"][0]["role"] == "user"
        tools = {t["name"] for t in first["tools"]}
        assert "whoami" in tools and "export_output" in tools and all(t["type"] == "function" and t["strict"] is False for t in first["tools"])
        assert "reasoning" not in first, "the model's own thinking level: nothing is said of it"
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "services-chat-openai.png"))
        no_key_on_the_page(d)

        step("Another model for the rest of this chat, within its service")
        choose(d, "chat-model", "gpt-6-astra")
        Wait(d, 10).until(lambda drv: "From here on: GPT-6 Astra." in drv.find_element(By.ID, "log").text)
        choose(d, "chat-effort", "high")
        Wait(d, 10).until(lambda drv: "From here on: GPT-6 Astra, thinking high." in drv.find_element(By.ID, "log").text)
        SCRIPT[:] = [("say", "Still here, on another model.")]
        send(d, "And now?", "Still here, on another model.")
        assert SEEN[-1][0] == "/responses" and SEEN[-1][2]["model"] == "gpt-6-astra", SEEN[-1][2]["model"]
        assert SEEN[-1][2]["reasoning"] == {"effort": "high"}, SEEN[-1][2].get("reasoning")

        step("Settings > Assistant: Google Gemini, its key, its own list of models, a typed model id")
        open_assistant(d)
        assert d.execute_script("return document.getElementById('service').value") == "openai"
        assert "api.openai.com · OpenAI Responses" in d.find_element(By.ID, "service-about").text
        assert f"ending in {KEYS['openai'][-4:]}" in d.find_element(By.ID, "key-state").text
        choose_service(d, "gemini")
        about = d.find_element(By.ID, "service-about").text
        assert "generativelanguage.googleapis.com · OpenAI Chat Completions" in about and "aistudio.google.com" in about, about
        assert "No key saved for Google Gemini yet" in d.find_element(By.ID, "key-state").text
        assert d.find_element(By.CSS_SELECTOR, "label[for=key]").text == "API key for Google Gemini"
        assert not d.find_element(By.ID, "effort-field").is_displayed(), "a service with one thinking level offers no choice of it"
        # The chat that is open keeps the service it started with.
        assert "keeps the service it started with (OpenAI)" in d.find_element(By.ID, "service-running").text
        save_key(d, KEYS["gemini"])
        d.find_element(By.ID, "models-load").click()
        Wait(d, 20).until(lambda drv: "3 models listed by the service" in drv.find_element(By.ID, "models-loaded").text)
        assert LISTS[-1][0] == "/models" and header(LISTS[-1][1], "authorization") == f"Bearer {KEYS['gemini']}", LISTS[-1]
        offered = d.execute_script("return [...document.getElementById('model').options].map(o => o.value)")
        # Columbus's own three, then what the service listed that is a plain id.
        assert offered == ["gemini-3.8-flash", "gemini-4-argon", "gemini-3.6-flash", "models/gemini-3.8-flash", "models/gemini-4-argon", "gemini-list-only"], offered
        other = d.find_element(By.ID, "model-other")
        other.send_keys("not a model id")
        d.find_element(By.CSS_SELECTOR, "#prefs-form button[type=submit]").click()
        Wait(d, 10).until(lambda drv: "that is not a model id" in drv.find_element(By.ID, "server-state").text)
        other.clear()
        other.send_keys("gemini-9-typed")
        d.find_element(By.CSS_SELECTOR, "#prefs-form button[type=submit]").click()
        Wait(d, 15).until(lambda drv: settings_json(drv)[1]["model"] == "gemini-9-typed")
        Wait(d, 10).until(lambda drv: drv.execute_script("return document.getElementById('model').value") == "gemini-9-typed")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "services-settings.png"))
        no_key_on_the_page(d)
        # The first view names the service too.
        d.execute_script("location.hash = '#/settings'")
        Wait(d, 10).until(lambda drv: drv.find_elements(By.ID, "set-service-line") and "Google Gemini. A key ending in" in drv.find_element(By.ID, "set-service-line").text)
        assert d.execute_script("return document.getElementById('set-model').value") == "gemini-9-typed"
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        close_settings(d)

        step("The chat that was open kept its service and its model")
        assert chat_service(d) == "OpenAI"
        assert d.execute_script("return document.getElementById('chat-model').value") == "gpt-6-astra"
        SCRIPT[:] = [("say", "Same service as before.")]
        send(d, "Where am I?", "Same service as before.")
        assert SEEN[-1][0] == "/responses" and SEEN[-1][2]["model"] == "gpt-6-astra"

        step("A new chat uses Google Gemini: Chat Completions, its own key, the typed model")
        new_chat(d)
        Wait(d, 10).until(lambda drv: chat_service(drv) == "Google Gemini")
        assert d.execute_script("return document.getElementById('chat-model').value") == "gemini-9-typed"
        assert not d.find_element(By.ID, "chat-effort").is_displayed()
        before = len(SEEN)
        SCRIPT[:] = [("call", "g1", "whoami", {}), ("say", "Hello from the second service.")]
        send(d, "Who am I?", "Hello from the second service.")
        assert [p for p, _, _ in SEEN[before:]] == ["/chat/completions"] * 2, [p for p, _, _ in SEEN[before:]]
        path, headers, body = SEEN[before]
        assert header(headers, "authorization") == f"Bearer {KEYS['gemini']}", "each service is sent its own key"
        assert body["model"] == "gemini-9-typed" and body["messages"][0]["role"] == "system" and body["tools"][0]["type"] == "function"
        assert SEEN[-1][2]["messages"][-1]["role"] == "tool", "the tool's result went back as a tool message"
        choose(d, "chat-model", "gemini-4-argon")
        Wait(d, 10).until(lambda drv: "From here on: Gemini 4 Argon." in drv.find_element(By.ID, "log").text)
        d.save_screenshot(str(OUT / "services-chat-gemini.png"))

        step("Anthropic, as before, with its own key; then Ollama, which needs none")
        open_assistant(d)
        choose_service(d, "anthropic")
        save_key(d, KEYS["anthropic"])
        assert d.execute_script("return [...document.getElementById('effort').options].length") == 6
        close_settings(d)
        new_chat(d)
        Wait(d, 10).until(lambda drv: chat_service(drv) == "Anthropic")
        SCRIPT[:] = [("say", "Hello from the first service.")]
        send(d, "Hello", "Hello from the first service.")
        path, headers, body = SEEN[-1]
        assert path == "/v1/messages" and header(headers, "x-api-key") == KEYS["anthropic"] and header(headers, "authorization") is None
        assert body["model"] == "claude-sonnet-5-5" and "system" in body
        open_assistant(d)
        choose_service(d, "ollama")
        assert "Ollama on this computer needs no key." in d.find_element(By.ID, "key-state").text
        assert not d.find_element(By.ID, "key").is_displayed()
        assert "On this computer · OpenAI Chat Completions" in d.find_element(By.ID, "service-about").text
        assert d.execute_script("return document.getElementById('model').value") == "", "Columbus has no list of what is on the computer"
        _, s = settings_json(d)
        assert s["ready"] is False and s["key_needed"] is False and s["model"] == "", s
        other = d.find_element(By.ID, "model-other")
        other.send_keys("llama3.3:70b")
        d.find_element(By.CSS_SELECTOR, "#prefs-form button[type=submit]").click()
        Wait(d, 15).until(lambda drv: settings_json(drv)[1]["ready"] is True)
        close_settings(d)
        new_chat(d)
        Wait(d, 10).until(lambda drv: chat_service(drv) == "Ollama on this computer")
        SCRIPT[:] = [("say", "Hello from this computer.")]
        send(d, "Hello", "Hello from this computer.")
        path, headers, body = SEEN[-1]
        assert path == "/chat/completions" and body["model"] == "llama3.3:70b"
        assert header(headers, "authorization") is None and header(headers, "x-api-key") is None, "a service that takes no key is sent none"

        step("Another service, added by its address and confirmed; a bad address is refused first")
        open_assistant(d)
        assert "None added." in d.find_element(By.ID, "services-none").text
        d.find_element(By.ID, "service-add").click()
        Wait(d, 10).until(EC.visibility_of_element_located((By.ID, "svc-name")))
        d.find_element(By.ID, "svc-name").send_keys("My gateway")
        address = d.find_element(By.ID, "svc-address")
        for bad, why in [("http://gateway.example.org/v1", "https://"), ("https://me:secret@gateway.example.org/v1", "user name or a password"),
                         ("https://gateway.example.org/v1?key=abc", "nothing after a ?")]:
            address.clear()
            address.send_keys(bad)
            d.find_element(By.ID, "svc-save").click()
            Wait(d, 10).until(lambda drv: why in drv.find_element(By.ID, "svc-error").text)
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "services-add.png"))
        address.clear()
        address.send_keys(f"{here}/gw/v1/")
        choose(d, "svc-format", "chat")
        choose(d, "svc-key-header", "api-key")
        d.find_element(By.ID, "svc-save").click()
        Wait(d, 15).until(lambda drv: not drv.find_elements(By.ID, "svc-dialog"))
        row = Wait(d, 10).until(EC.presence_of_element_located((By.CSS_SELECTOR, "#services-added li[data-id='my-gateway']")))
        assert "My gateway" in row.text and f"{here}/gw/v1" in row.text and "OpenAI Chat Completions" in row.text, row.text
        _, s = settings_json(d)
        mine = next(v for v in s["services"] if v["id"] == "my-gateway")
        assert (mine["builtin"], mine["local"], mine["key"], mine["format_id"], mine["key_header"], mine["allowed"]) == (False, True, "optional", "chat", "api-key", True), mine
        # It is in the settings file as F-19 says, and no key is.
        saved = json.loads(next((root / "config").rglob("settings.json")).read_text())
        assert saved["services"] == [{"id": "my-gateway", "name": "My gateway", "address": f"{here}/gw/v1", "format": "chat", "key_header": "api-key", "pictures": False}], saved
        assert saved["service"] == "ollama" and saved["models"]["gemini"] == "gemini-9-typed" and saved["models"]["ollama"] == "llama3.3:70b", saved
        choose_service(d, "my-gateway")
        assert "(only if it asks for one)" in d.find_element(By.CSS_SELECTOR, "label[for=key]").text
        save_key(d, KEYS["gateway"])
        d.find_element(By.ID, "model-other").send_keys("deployment-1")
        d.find_element(By.CSS_SELECTOR, "#prefs-form button[type=submit]").click()
        Wait(d, 15).until(lambda drv: settings_json(drv)[1]["model"] == "deployment-1")
        close_settings(d)
        new_chat(d)
        Wait(d, 10).until(lambda drv: chat_service(drv) == "My gateway")
        SCRIPT[:] = [("say", "Hello from the gateway.")]
        send(d, "Hello", "Hello from the gateway.")
        path, headers, body = SEEN[-1]
        # At its own address, with its key in the header it was set up with.
        assert path == "/gw/v1/chat/completions" and body["model"] == "deployment-1", (path, body["model"])
        assert header(headers, "api-key") == KEYS["gateway"] and header(headers, "authorization") is None and header(headers, "x-api-key") is None
        no_key_on_the_page(d)

        step("The saved chat says which service and model it ran on")
        saved_chats = sorted((root / "data").rglob("chats/*.jsonl"))
        metas = [json.loads(p.read_text().splitlines()[0]) for p in saved_chats]
        ran_on = sorted((m["service"], m["model"]) for m in metas)
        assert ran_on == sorted([("openai", "gpt-6.1-sol"), ("gemini", "gemini-9-typed"), ("anthropic", "claude-sonnet-5-5"),
                                 ("ollama", "llama3.3:70b"), ("my-gateway", "deployment-1")]), ran_on
        first_chat = next(p for p, m in zip(saved_chats, metas) if m["service"] == "openai")
        d.execute_script(f"location.hash = '#/chats/c/{first_chat.stem}'")
        Wait(d, 20).until(lambda drv: drv.find_elements(By.ID, "saved-service") and "OpenAI, GPT-6.1 Sol" in drv.find_element(By.ID, "saved-service").text)
        assert "From here on: GPT-6 Astra, thinking high." in d.find_element(By.ID, "saved-log").text
        # A chat saved before the service and the model were written down
        # says when it was, and nothing in place of what it does not know.
        (first_chat.parent / "c-older-chat-0001.jsonl").write_text(
            json.dumps({"kind": "meta", "id": "c-older-chat-0001", "session": "s-old", "ts": "2026-11-05T10:00:00Z"}) + "\n"
            + json.dumps({"kind": "you", "text": "An older question.", "ts": "2026-11-05T10:00:01Z"}) + "\n")
        d.execute_script("location.hash = '#/chats/c/c-older-chat-0001'")
        Wait(d, 20).until(lambda drv: drv.find_elements(By.ID, "saved-log") and "An older question." in drv.find_element(By.ID, "saved-log").text)
        head = d.execute_script("return document.querySelector('.chathead .sub').textContent")
        assert not d.find_elements(By.ID, "saved-service") and "2026" in head and "·" not in head, head
        reads_right(d, ".chatcol[aria-label='Saved chat']")

        step("Activity names the app's own chat, and the map beside it, in plain words")
        d.execute_script("location.hash = '#/activity'")
        Wait(d, 30).until(lambda drv: "Columbus's own chat" in drv.execute_script("return document.querySelector('main').textContent"))
        assert "Columbus app ·" not in d.execute_script("return document.querySelector('main').textContent")
        # No line names one of Columbus's own programs as the log writes it.
        rows = d.execute_script("return document.getElementById('audit-rows').textContent")
        for program in ("companion-window", "columbus-app", "columbus-runner", "columbus-chat", "approvals-page"):
            assert program not in rows, f"the audit log's lines name a program, {program}: {rows}"
        # The filter offers each by its plain name, with the log's own after
        # it; the map and table beside the first chat read its layer to draw it.
        d.find_element(By.ID, "audit-filter").click()
        Wait(d, 10).until(lambda drv: drv.find_element(By.ID, "audit-client").is_displayed())
        offered = dict(d.execute_script("return [...document.getElementById('audit-client').options].map(o => [o.value, o.textContent])"))
        assert offered["Columbus app"] == "Columbus's own chat (Columbus app)", offered
        assert offered["companion-window (view)"] == "Columbus's map and table (companion-window (view))", offered
        choose(d, "audit-client", "companion-window (view)")
        Wait(d, 10).until(lambda drv: (lambda t: "Columbus's map and table" in t and "Columbus's own chat" not in t)(
            drv.execute_script("return document.getElementById('audit-rows').textContent")))
        assert "companion-window" not in d.execute_script("return document.getElementById('audit-rows').textContent")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it

        step("Removing the added service removes its key too")
        open_assistant(d)
        d.find_element(By.CSS_SELECTOR, "#services-added li[data-id='my-gateway'] button.danger").click()
        confirm = Wait(d, 10).until(EC.element_to_be_clickable((By.XPATH, "//dialog[@open]//button[normalize-space()='Remove service']")))
        confirm.click()
        Wait(d, 15).until(lambda drv: not drv.find_elements(By.CSS_SELECTOR, "#services-added li[data-id='my-gateway']"))
        _, s = settings_json(d)
        assert s["service"] == "anthropic" and all(v["id"] != "my-gateway" for v in s["services"]), s["service"]
        saved = json.loads(next((root / "config").rglob("settings.json")).read_text())
        assert "services" not in saved and "my-gateway" not in saved["models"], saved
        # Added again under the same name, it has no key: the old one is gone.
        d.find_element(By.ID, "service-add").click()
        Wait(d, 10).until(EC.visibility_of_element_located((By.ID, "svc-name")))
        d.find_element(By.ID, "svc-name").send_keys("My gateway")
        d.find_element(By.ID, "svc-address").send_keys(f"{here}/gw/v1")
        d.find_element(By.ID, "svc-save").click()
        Wait(d, 15).until(lambda drv: drv.find_elements(By.CSS_SELECTOR, "#services-added li[data-id='my-gateway']"))
        choose_service(d, "my-gateway")
        assert "No key saved for My gateway" in d.find_element(By.ID, "key-state").text
        choose_service(d, "openai")

        step("How to connect other AI apps: this computer's path, each app's own file and entry, Copy")
        apps = [o.get_attribute("value") for o in Select(d.find_element(By.ID, "apps-pick")).options]
        assert apps == ["claude-desktop", "copilot-vscode", "copilot-cli", "visual-studio", "jetbrains", "cursor", "codex", "gemini-cli", "claude-code", "lm-studio"], apps
        entries = {}
        for app in apps:
            choose(d, "apps-pick", app)
            time.sleep(0.2)
            entries[app] = (d.find_element(By.ID, "apps-file").text, d.find_element(By.ID, "apps-entry").text,
                            d.find_element(By.ID, "apps-command").get_attribute("textContent"),
                            d.find_element(By.ID, "apps-notes").get_attribute("textContent"))
            assert str(GATEWAY) in entries[app][1], f"{app}: the entry holds this computer's path to columbus-mcp"
            # Nothing belongs in the environment here (the server finds its engine): the entry has no line for it.
            assert "env" not in entries[app][1], f"{app}: {entries[app][1]}"
            reads_right(d, "#other-apps")
        assert json.loads(entries["claude-desktop"][1]) == {"mcpServers": {"columbus": {"command": str(GATEWAY), "args": []}}}
        assert entries["claude-desktop"][0].endswith("claude_desktop_config.json")
        assert json.loads(entries["copilot-vscode"][1])["mcpServers"]["columbus"] == {"type": "stdio", "command": str(GATEWAY), "args": [], "tools": ["*"]}
        assert "mcp-config.json" in entries["copilot-vscode"][0] and ".mcp.json" in entries["copilot-vscode"][0]
        # The Copilot CLI reads the file Copilot in VS Code reads: the same entry, and each says so.
        assert entries["copilot-cli"][1] == entries["copilot-vscode"][1] and entries["copilot-vscode"][0].startswith(entries["copilot-cli"][0])
        assert "one entry serves both" in entries["copilot-cli"][3] and "one entry serves both" in entries["copilot-vscode"][3]
        assert json.loads(entries["visual-studio"][1]) == {"servers": {"columbus": {"type": "stdio", "command": str(GATEWAY), "args": []}}}
        assert json.loads(entries["jetbrains"][1]) == {"servers": {"columbus": {"command": str(GATEWAY), "args": []}}}
        assert json.loads(entries["cursor"][1])["mcpServers"]["columbus"] == {"type": "stdio", "command": str(GATEWAY), "args": []} and ".cursor" in entries["cursor"][0]
        assert entries["codex"][1] == f"[mcp_servers.columbus]\ncommand = '{GATEWAY}'\nargs = []" and entries["codex"][0].endswith("config.toml")
        assert json.loads(entries["gemini-cli"][1]) == {"mcpServers": {"columbus": {"command": str(GATEWAY), "args": []}}} and "settings.json" in entries["gemini-cli"][0]
        assert entries["claude-code"][2] == f"claude mcp add --transport stdio columbus -- {GATEWAY}"
        assert json.loads(entries["lm-studio"][1]) == json.loads(entries["claude-desktop"][1]) and "mcp.json" in entries["lm-studio"][0]
        approvals = d.find_element(By.ID, "apps-approvals").text
        assert "opens its local approvals page in your browser" in approvals and "45 seconds" in approvals, approvals
        # Copy puts the entry on the clipboard: pasted back, it is the entry.
        choose(d, "apps-pick", "copilot-vscode")
        time.sleep(0.2)
        copy = d.find_element(By.ID, "apps-copy")
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", copy)
        copy.click()
        Wait(d, 10).until(lambda drv: drv.find_element(By.ID, "apps-copied").text == "Copied.")
        d.execute_script("const t = document.createElement('textarea'); t.id = 'paste-here'; t.setAttribute('aria-label', 'Paste'); document.getElementById('other-apps').append(t); t.focus();")
        d.find_element(By.ID, "paste-here").send_keys(Keys.CONTROL, "v")
        try:
            Wait(d, 10).until(lambda drv: drv.find_element(By.ID, "paste-here").get_attribute("value").strip() != "")
        except Exception:
            d.save_screenshot(str(OUT / "services-copy-failed.png"))
            raise SystemExit("Copy the entry put nothing on the clipboard")
        pasted = d.find_element(By.ID, "paste-here").get_attribute("value")
        assert json.loads(pasted) == json.loads(entries["copilot-vscode"][1]), pasted
        d.execute_script("document.getElementById('paste-here').remove()")
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "services-other-apps.png"))

        step("The organization's list refuses a service in Settings, and Organization rules says the limit")
        close_settings(d)
        write_policy(root, ["api.anthropic.com", "localhost"])
        open_assistant(d)
        assert "allows the app's chat these services only: api.anthropic.com, localhost (assistant.hosts)" in d.find_element(By.ID, "service-org").text
        labels = {o.get_attribute("value"): o.text for o in Select(d.find_element(By.ID, "service")).options}
        assert labels["gemini"].endswith("(not allowed by your organization)") and labels["anthropic"] == "Anthropic" and labels["ollama"] == "Ollama on this computer", labels
        # (OpenAI was chosen before the rule came; choosing another that is not listed is refused.)
        choose(d, "service", "gemini")
        sentence = "Your organization's rules do not let the app's chat use Google Gemini: its address, generativelanguage.googleapis.com, is not on the list of AI services allowed (assistant.hosts: api.anthropic.com, localhost)."
        Wait(d, 10).until(lambda drv: drv.find_element(By.ID, "service-error").text == sentence)
        assert settings_json(d)[1]["service"] == "openai", "the refused service was not saved as the one in use"
        Wait(d, 10).until(lambda drv: drv.execute_script("return document.getElementById('service').value") == "openai")
        d.save_screenshot(str(OUT / "services-org-refused.png"))
        d.execute_script("location.hash = '#/settings/org'")
        Wait(d, 20).until(lambda drv: "may use only AI services at these addresses: api.anthropic.com, localhost" in drv.execute_script("return document.getElementById('settings').textContent"))
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        close_settings(d)

        step("At the start of a chat: the service in use is no longer listed, so nothing is sent")
        openai_sentence = "Your organization's rules do not let the app's chat use OpenAI: its address, api.openai.com, is not on the list of AI services allowed (assistant.hosts: api.anthropic.com, localhost)."
        # The page: no AI can be used, so the first steps stand in for the
        # message box, with the organization's sentence.
        new_chat(d, ready=False)
        Wait(d, 15).until(lambda drv: drv.find_elements(By.ID, "fr-ai-need") and openai_sentence in drv.find_element(By.ID, "fr-ai-need").text)
        assert not d.find_element(By.ID, "text").is_displayed(), "there is nothing to ask a service that may not be used"
        assert Select(d.find_element(By.ID, "fr-service")).first_selected_option.text == "OpenAI (not allowed by your organization)"
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        d.save_screenshot(str(OUT / "services-org-start.png"))
        # The app's own code, whatever a page asks: the chat does not start.
        before = len(SEEN)
        refused = d.execute_async_script(
            "const done = arguments[arguments.length - 1];"
            "window.__TAURI__.core.invoke('chat_start', {project: null, run: null}).then(() => done('started'), e => done(String(e)));")
        assert refused == openai_sentence, refused
        _, s = settings_json(d)
        assert s["chatting"] is False and s["ready"] is False and len(SEEN) == before, "a chat on a service that is not listed sent nothing"

        step("In a running chat: the rule comes while it is open, and its next message is not sent")
        write_policy(root, None)
        new_chat(d)
        assert not d.find_elements(By.ID, "first-steps") and not d.find_elements(By.ID, "chat-not-ready")
        SCRIPT[:] = [("say", "Allowed for now.")]
        send(d, "Hello", "Allowed for now.")
        before = len(SEEN)
        write_policy(root, ["api.anthropic.com"])
        send(d, "And again?", None)
        Wait(d, 20).until(lambda drv: openai_sentence.replace("api.anthropic.com, localhost", "api.anthropic.com") in drv.find_element(By.ID, "log").text)
        assert len(SEEN) == before, "a running chat sent nothing more to a service that is no longer listed"
        Wait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled() and drv.find_element(By.ID, "send").is_displayed())
        # Listed again, the same chat goes on.
        write_policy(root, ["api.openai.com"])
        SCRIPT[:] = [("say", "Allowed again.")]
        send(d, "Now?", "Allowed again.")
        assert SEEN[-1][0] == "/responses"
        write_policy(root, None)

        step("No key is in any file the app or Columbus wrote")
        for p in root.rglob("*"):
            if p.is_file():
                data = p.read_bytes()
                for name, key in KEYS.items():
                    assert key.encode() not in data and key[8:].encode() not in data, f"the {name} key is in {p}"
        no_key_on_the_page(d)
    finally:
        guard.stop(d, driver_proc)

    step("the app again, the confirmation declined: a service is not added")
    env["COLUMBUS_APP_TEST_CONFIRM"] = "deny"
    d, driver_proc = guard.driver(env, APP)
    try:
        wait = Wait(d, 60)
        d.set_window_size(1400, 900)
        guard.ready(d, wait)
        open_assistant(d)
        # (Keys were kept in the first app's memory only: this one has none.)
        _, s = settings_json(d)
        assert s["has_key"] is False and [v["id"] for v in s["services"] if not v["builtin"]] == ["my-gateway"], s["services"]
        d.find_element(By.ID, "service-add").click()
        Wait(d, 10).until(EC.visibility_of_element_located((By.ID, "svc-name")))
        d.find_element(By.ID, "svc-name").send_keys("Another gateway")
        d.find_element(By.ID, "svc-address").send_keys(f"{here}/other/v1")
        d.find_element(By.ID, "svc-save").click()
        Wait(d, 15).until(lambda drv: "not confirmed" in drv.find_element(By.ID, "svc-error").text)
        saved = json.loads(next((root / "config").rglob("settings.json")).read_text())
        assert [a["id"] for a in saved["services"]] == ["my-gateway"], "nothing is added without the confirmation"
        d.find_element(By.XPATH, "//dialog[@id='svc-dialog']//button[normalize-space()='Cancel']").click()
        # A change of address needs it too; a change of name alone does not.
        d.find_element(By.CSS_SELECTOR, "#services-added li[data-id='my-gateway'] button:not(.danger)").click()
        Wait(d, 10).until(EC.visibility_of_element_located((By.ID, "svc-address")))
        address = d.find_element(By.ID, "svc-address")
        address.clear()
        address.send_keys(f"{here}/moved/v1")
        d.find_element(By.ID, "svc-save").click()
        Wait(d, 15).until(lambda drv: "not confirmed" in drv.find_element(By.ID, "svc-error").text)
        saved = json.loads(next((root / "config").rglob("settings.json")).read_text())
        assert saved["services"][0]["address"] == f"{here}/gw/v1", "the address is not changed without the confirmation"
        d.find_element(By.XPATH, "//dialog[@id='svc-dialog']//button[normalize-space()='Cancel']").click()
        # What the first app recorded as confirmed was in its memory: here
        # the service is one whose address nobody confirmed, and no chat
        # starts on it.
        choose(d, "service", "my-gateway")
        Wait(d, 10).until(lambda drv: "is not the one you confirmed in this app" in drv.find_element(By.ID, "service-error").text)
        assert settings_json(d)[1]["service"] != "my-gateway"
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        print("desktop services e2e: ok")
    finally:
        guard.stop(d, driver_proc)
        stand.shutdown()


if __name__ == "__main__":
    main()
