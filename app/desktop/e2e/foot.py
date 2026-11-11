"""The line under the chat's message box (its foot), in windows of every
width the app allows, end to end on Linux through WebDriver (tauri-driver
and WebKitWebDriver, under xvfb and a D-Bus session).

The foot says where a chat works (its project) and with what (the AI
service, the model, the thinking level). The middle of the window is narrow
when the panel on the right is open: from 1100 to about 1340 pixels of
window it is some 400 wide. This script looks at the foot in windows 1024
(the smallest the app allows), 1100, 1240 and 1440 wide, with the panel
open and closed: before a chat's first message (the project is a picker),
in a chat that has started (the project is a link), while the assistant is
working, with a short project name, with one of the longest a project may
have, with no project, and with a service and a model whose names are long.
Each time:

- the project's name shows whole, or at least its first eight characters,
  and where it is cut its whole name is in its title;
- the service's name and each picker show whole, or at least eight
  characters, with the whole of it in the title where it is cut;
- nothing in the foot reaches outside the foot, the foot stays inside the
  middle of the window, no two parts lie over each other, and nothing is
  under the panel (in a window under 1100 the panel floats over the middle:
  there the message box and its foot keep clear of it, Send included);
- with the panel closed everything is on one line; where one line cannot
  hold it, the AI goes to a line of its own under the project (and its
  pickers to another if they must), and nothing is cut to make room;
- the foot is as tall as its lines and stays in the window, also right
  after the panel opens or closes and the window changes its width.

The app uses the computer's own typeface, so widths differ from one system
to another (CI's is wider than most): the script counts lines only where
any typeface gives the same count.

A stand-in plays the AI service (Anthropic's format, and OpenAI Chat
Completions for the service on this computer). No real service is
contacted, and the key is made up. A picture of each width is saved in
`out/`.

    dbus-run-session -- xvfb-run -a -s "-screen 0 1600x1000x24" python app/desktop/e2e/foot.py \\
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

Wait = guard.Wait  # its time-outs name the step that was waiting

REPO = Path(__file__).resolve().parents[3]
GATEWAY = Path(sys.argv[1]).resolve()
APP = Path(sys.argv[2]).resolve()
OUT = Path(__file__).resolve().parent / "out"

# The smallest window the app allows, where the panel gets its own column,
# a window in the range where the middle is narrowest, and a wide one.
WIDTHS = [1024, 1100, 1240, 1440]
HEIGHT = 800
SHORT = ("tract-review-2027", "Tract review 2027")
# (A project's name is one line of at most 80 characters.)
LONG = ("north-slope", "North Slope survey 2026: parcels, rights of way and the access roads beside them")
LONG_MODEL = "qwen3-coder-30b-a3b-instruct-q4_k_m"

# Cleared, the stand-in's next reply waits (the assistant is working); set, it goes.
HOLD = threading.Event()
HOLD.set()


class Stand(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        if self.path.endswith("/chat/completions"):
            reply = {"id": "chatcmpl-1", "object": "chat.completion",
                     "choices": [{"index": 0, "message": {"role": "assistant", "content": "Here."}, "finish_reason": "stop"}],
                     "usage": {"prompt_tokens": 12, "completion_tokens": 2, "total_tokens": 14}}
        else:
            reply = {"content": [{"type": "text", "text": "Here."}], "stop_reason": "end_turn",
                     "usage": {"input_tokens": 12, "output_tokens": 2}}
        data = json.dumps(reply).encode()
        HOLD.wait(60)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


# What the foot shows, measured in the page. For each part: where it is,
# what it says in full, whether all of it shows, and how many of its first
# characters show where it does not (counted as the page cuts it: the
# characters that fit with the "…" after them, in the part's own type).
LOOK = """
const foot = document.getElementById('chat-foot');
const box = (e) => { const b = e.getBoundingClientRect(); return [b.left, b.top, b.right, b.bottom].map((x) => Math.round(x * 10) / 10); };
const shown = (e) => { if (!e) return false; const b = e.getBoundingClientRect(); const cs = getComputedStyle(e); return b.width > 0 && b.height > 0 && cs.visibility !== 'hidden' && cs.display !== 'none'; };
const wide = (like, text) => {
  const cs = getComputedStyle(like);
  const p = document.createElement('span');
  p.textContent = text;
  p.style.cssText = 'position:absolute;visibility:hidden;white-space:pre;left:-9999px;top:0';
  p.style.font = cs.font; p.style.letterSpacing = cs.letterSpacing; p.style.fontWeight = cs.fontWeight;
  document.body.append(p);
  const w = p.getBoundingClientRect().width;
  p.remove();
  return w;
};
const out = document.getElementById('outputs');
const panel = out && !out.hidden ? out.getBoundingClientRect() : null;
// Where the panel floats over the middle (a window under 1100), what is under it does not show.
const cover = panel && getComputedStyle(out).position === 'absolute' ? panel.left : Infinity;
const part = (name, e, text) => {
  if (!shown(e)) return null;
  const cs = getComputedStyle(e);
  const b = e.getBoundingClientRect();
  const left = b.left + parseFloat(cs.borderLeftWidth) + parseFloat(cs.paddingLeft);
  const right = Math.min(b.right - parseFloat(cs.borderRightWidth) - parseFloat(cs.paddingRight), cover);
  const room = right - left;
  const all = wide(e, text);
  let chars = text.length;
  if (all > room + 0.5) { chars = 0; while (chars < text.length && wide(e, text.slice(0, chars + 1) + '…') <= room + 0.5) chars += 1; }
  return { name, text, picker: e.tagName === 'SELECT', box: box(e), room: Math.round(room), needs: Math.round(all), whole: all <= room + 0.5, chars, title: e.getAttribute('title') || '' };
};
const pick = document.getElementById('chat-project-pick');
const link = document.querySelector('#chat-sub a:not(#chat-run)');
const sel = (id) => { const s = document.getElementById(id); return part(id, s, s && s.selectedOptions[0] ? s.selectedOptions[0].textContent : ''); };
const parts = [
  pick ? part('project', pick, pick.selectedOptions[0].textContent) : link ? part('project', link, link.textContent) : null,
  part('status', document.getElementById('status'), document.getElementById('status').textContent),
  part('service', document.getElementById('chat-service'), document.getElementById('chat-service').textContent),
  sel('chat-model'), sel('chat-effort'),
].filter(Boolean);
// Everything on show in the foot, whatever it is: none of it reaches outside.
const outside = [];
const f = foot.getBoundingClientRect();
for (const e of foot.querySelectorAll('*')) {
  if (!shown(e)) continue;
  const b = e.getBoundingClientRect();
  if (b.left < f.left - 0.6 || b.right > f.right + 0.6 || b.top < f.top - 0.6 || b.bottom > f.bottom + 0.6) outside.push((e.id || e.tagName.toLowerCase()) + ' ' + JSON.stringify(box(e)));
}
return {
  inner: window.innerWidth, height: window.innerHeight, foot: box(foot), scroll: [foot.scrollWidth, foot.clientWidth], main: box(document.getElementById('main')),
  composer: box(document.getElementById('composer')), panel: panel ? box(out) : null, floats: cover !== Infinity, parts, outside,
};
"""


def lines(parts):
    """The parts, line by line as they are drawn (top to bottom, left to right)."""
    rows = []
    for p in sorted(parts, key=lambda p: (p["box"][1], p["box"][0])):
        mid = (p["box"][1] + p["box"][3]) / 2
        for row in rows:
            if row["top"] <= mid <= row["bottom"]:
                row["parts"].append(p)
                row["top"], row["bottom"] = min(row["top"], p["box"][1]), max(row["bottom"], p["box"][3])
                break
        else:
            rows.append({"top": p["box"][1], "bottom": p["box"][3], "parts": [p]})
    return [sorted(r["parts"], key=lambda p: p["box"][0]) for r in rows]


def faults(m, what, width, panel, one_line=None, busy=False):
    """What is wrong with the foot as measured (nothing, when it reads well)."""
    say = f"{what}, window {width}, panel {'open' if panel else 'closed'}"
    bad = []
    if m["inner"] != width:
        bad.append(f"{say}: the window is {m['inner']} wide")
    if (m["panel"] is not None) != panel:
        bad.append(f"{say}: the panel is {'open' if m['panel'] else 'closed'}")
    parts = {p["name"]: p for p in m["parts"]}
    # Nothing reaches outside the foot; the foot stays in the middle of the
    # window, and clear of the panel when that is open.
    if m["outside"]:
        bad.append(f"{say}: outside the foot {m['foot']}: {m['outside']}")
    if m["scroll"][0] > m["scroll"][1] + 1:
        bad.append(f"{say}: the foot holds {m['scroll'][0]} px in {m['scroll'][1]}")
    if m["foot"][0] < m["main"][0] - 0.6 or m["foot"][2] > m["main"][2] + 0.6:
        bad.append(f"{say}: the foot {m['foot']} leaves the middle {m['main']}")
    if m["panel"] and m["foot"][2] > m["panel"][0] + 0.6:
        bad.append(f"{say}: the foot {m['foot']} is under the panel {m['panel']}")
    # Each part: whole, or at least its first eight characters, and then
    # the whole of it in its title.
    want = ["project", "status"] if busy else ["project", "service", "chat-model"] + (["chat-effort"] if "chat-effort" in parts else [])
    for name in want:
        if name not in parts:
            bad.append(f"{say}: {name} does not show (there is: {sorted(parts)})")
            continue
        p = parts[name]
        if not p["whole"] and p["chars"] < min(8, len(p["text"])):
            bad.append(f"{say}: {name} shows {p['chars']} of the {len(p['text'])} characters of {p['text']!r} ({p['room']} px for {p['needs']})")
        if not p["whole"] and p["text"] not in p["title"]:
            bad.append(f"{say}: {name} is cut ({p['text']!r}) and its title does not say it whole: {p['title']!r}")
    if busy:
        if "status" in parts and not parts["status"]["whole"]:
            bad.append(f"{say}: what the assistant is doing does not show whole: {parts['status']}")
        if "service" in parts or "chat-model" in parts:
            bad.append(f"{say}: the pickers show while the assistant works")
    # No two parts lie over each other.
    shown = list(parts.values())
    for i, a in enumerate(shown):
        for b in shown[i + 1:]:
            apart = a["box"][2] <= b["box"][0] + 0.6 or b["box"][2] <= a["box"][0] + 0.6 or a["box"][3] <= b["box"][1] + 0.6 or b["box"][3] <= a["box"][1] + 0.6
            if not apart:
                bad.append(f"{say}: {a['name']} {a['box']} lies over {b['name']} {b['box']}")
    rows = lines(shown)
    if len(rows) > 3:
        bad.append(f"{say}: the foot has {len(rows)} lines")
    # The foot is as tall as its lines (40 px with one, as it always was),
    # and all of it, with the message box, is in the window.
    tall = m["foot"][3] - m["foot"][1]
    if not (max(40, 26 * len(rows)) <= tall <= 14 + 28 * len(rows)):
        bad.append(f"{say}: the foot is {tall} px tall with {len(rows)} line(s)")
    if m["composer"][3] > m["height"] + 0.6 or m["foot"][3] > m["composer"][3] + 0.6:
        bad.append(f"{say}: the message box and its foot end at {m['composer'][3]} and {m['foot'][3]} in a window {m['height']} tall")
    if one_line is not None and (len(rows) == 1) != one_line:
        bad.append(f"{say}: the foot has {len(rows)} line(s): {[[p['name'] for p in r] for r in rows]}")
    # Where it takes more than one line, the first is where the chat works
    # and the AI comes under it; and a part is cut only where a whole line
    # could not hold it.
    if len(rows) > 1:
        if any(p["name"] in ("service", "chat-model", "chat-effort") for p in rows[0]) or any(p["name"] in ("project", "status") for r in rows[1:] for p in r):
            bad.append(f"{say}: the lines are {[[p['name'] for p in r] for r in rows]}")
        # (A picker keeps to a width of its own; a name that is a link does not.)
        line = m["foot"][2] - m["foot"][0] - 14
        for p in shown:
            if p["name"] == "project" and not p["picker"] and not p["whole"] and len(rows[0]) == 1 and p["needs"] + 14 <= line:
                bad.append(f"{say}: the project's name is cut ({p['room']} px for {p['needs']}) though a line holds {round(line)}")
    return bad


def look(d, what, width, panel, one_line=None, busy=False):
    """Check the foot as it is now, and return what was measured. (It is
    given a moment to be drawn; a foot that is wrong stays wrong.)"""
    end = time.monotonic() + 3
    while True:
        m = d.execute_script(LOOK)
        bad = faults(m, what, width, panel, one_line, busy)
        if not bad or time.monotonic() > end:
            break
        time.sleep(0.15)
    assert not bad, bad
    m["rows"] = len(lines(m["parts"]))
    return m


def size(d, width):
    d.set_window_size(width, HEIGHT)
    Wait(d, 10).until(lambda drv: drv.execute_script("return window.innerWidth") == width)
    time.sleep(0.15)


def panel(d, want):
    """Open or close the panel on the right, as the person does: the chat's
    Map button opens it, the panel's own button closes it (in a window
    under 1100 the panel floats over the chat's head, Map button included)."""
    is_open = lambda drv: drv.execute_script("const o = document.getElementById('outputs'); return !!o && !o.hidden")  # noqa: E731
    if is_open(d) != want:
        d.find_element(By.CSS_SELECTOR, "#chatview button.chat-map" if want else "#out-close").click()
        Wait(d, 10).until(lambda drv: is_open(drv) == want)
    time.sleep(0.15)


def picture(d, name):
    d.save_screenshot(str(OUT / f"foot-{name}.png"))


def new_chat(d, project=None):
    d.execute_script("location.hash = arguments[0]", f"#/chats/fresh/{project}" if project else "#/chats/fresh")
    Wait(d, 30).until(lambda drv: drv.execute_script("return location.hash") == "#/chats/new"
                      and drv.execute_script("return document.getElementById('chat-title').textContent") == "New chat"
                      and drv.execute_script("const s = document.getElementById('chat-project-pick'); return s ? s.value : null") == (project or ""))
    Wait(d, 10).until(EC.visibility_of_element_located((By.ID, "text")))
    time.sleep(0.4)


def send(d, text, wait_for="Here."):
    d.find_element(By.ID, "text").send_keys(text)
    Wait(d, 10).until(lambda drv: drv.find_element(By.ID, "send").is_enabled())
    d.find_element(By.ID, "send").click()
    if wait_for:
        Wait(d, 90).until(lambda drv: wait_for in drv.find_element(By.ID, "log").text)
        Wait(d, 20).until(lambda drv: not drv.execute_script("return document.getElementById('composer').classList.contains('busy')"))
        time.sleep(0.3)


def invoke(d, cmd, args):
    got = d.execute_async_script(
        "const done = arguments[arguments.length - 1];"
        "window.__TAURI__.core.invoke(arguments[0], arguments[1]).then(() => done('ok'), e => done('ERR ' + e));", cmd, args)
    assert got == "ok", got


def every_width(d, what, tag, started, one_line=None, pictures=()):
    """The foot at each width, the panel closed and (in a chat that has
    started, where the person can open it) open. `pictures`: True for one
    of each, or the (width, open) pairs to keep one of."""
    seen = {}
    for width in WIDTHS:
        size(d, width)
        for is_open in ([False, True] if started else [False]):
            panel(d, is_open)
            want = one_line(width, is_open) if callable(one_line) else one_line
            seen[(width, is_open)] = look(d, what, width, is_open, one_line=want)
            if pictures is True or (width, is_open) in pictures:
                picture(d, f"{tag}-{width}-{'open' if is_open else 'closed'}")
        if started:
            panel(d, False)
    return seen


def fixtures():
    """A connection, two projects and a stand-in AI service: (the app's environment, the stand-in)."""
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box as square

    gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[square(0, 0, 1, 1), square(1, 0, 2, 1)], crs="EPSG:3081") \
        .to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "policy.yaml").write_text("connection_types: [gpkg]\n")
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\ndatasets:\n  allow: [\"*\"]\n")
    for pid, name in (SHORT, LONG):
        (root / "projects" / pid).mkdir(parents=True)
        (root / "projects" / pid / "project.yaml").write_text(f"id: {pid}\nname: {json.dumps(name)}\n")

    stand = ThreadingHTTPServer(("127.0.0.1", 0), Stand)
    threading.Thread(target=stand.serve_forever, daemon=True).start()
    env = dict(os.environ,
               COLUMBUS_APP_TEST_KEYS="memory", COLUMBUS_APP_API_KEY="tk-ft-4Vn8Qw2LxR6sPz93",
               COLUMBUS_APP_API_BASE=f"http://127.0.0.1:{stand.server_port}",
               COLUMBUS_SERVER=str(GATEWAY), COLUMBUS_PROJECTS=str(root / "projects"),
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_TOOLS=str(root / "tools"),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn", XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"))
    return env, stand


def main():
    guard.start("desktop foot e2e", minutes=10)
    OUT.mkdir(exist_ok=True)
    env, stand = fixtures()
    step("the app, an AI connected, two projects")
    d, driver_proc = guard.driver(env, APP)
    try:
        wait = Wait(d, 60)
        size(d, 1440)
        guard.ready(d, wait)
        wait.until(EC.visibility_of_element_located((By.ID, "text")))
        wait.until(lambda drv: drv.execute_script("return document.getElementById('chat-service').textContent") == "Anthropic")
        wait.until(lambda drv: drv.execute_script("return [...document.getElementById('chat-project-pick').options].map(o => o.value)") == ["", LONG[0], SHORT[0], "+"])

        step("A new chat, before its first message: the project is a picker beside the service and its pickers")
        # In no project, then in one, then in one with a long name. The
        # panel is closed (nothing is made yet): one line at every width.
        every_width(d, "a new chat in no project", "new-none", started=False, one_line=True)
        new_chat(d, SHORT[0])
        every_width(d, "a new chat in a project", "new", started=False, one_line=True, pictures={(1024, False), (1440, False)})
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        new_chat(d, LONG[0])
        seen = every_width(d, "a new chat in a project with a long name", "new-long", started=False, one_line=True)
        for (width, _), m in seen.items():
            p = next(x for x in m["parts"] if x["name"] == "project")
            assert not p["whole"] and p["chars"] >= 20 and LONG[1] in p["title"], f"window {width}: the long name's picker shows {p['chars']} characters: {p}"
        picture(d, "new-long-1440-closed")

        step("A chat that has started, in a project: its name is a link, whole at every width, the panel open and closed")
        new_chat(d, SHORT[0])
        send(d, "Which datasets can I use?")
        link = d.find_element(By.CSS_SELECTOR, "#chat-sub a")
        assert link.get_attribute("textContent") == SHORT[1] and link.get_attribute("href").endswith(f"#/projects/{SHORT[0]}")
        # With the panel closed everything is on one line. With it open the
        # middle is some 400 px wide until the window is a wide one: too
        # narrow for one line, so the AI has a line of its own.
        closed = lambda width, is_open: True if not is_open else None  # noqa: E731
        seen = every_width(d, "a chat in a project", "chat", started=True, pictures=True, one_line=closed)
        for (width, is_open), m in seen.items():
            for p in m["parts"]:
                assert p["whole"], f"window {width}, panel {'open' if is_open else 'closed'}: {p['name']} is cut: {p}"
        for width in (1024, 1100, 1240):
            assert seen[(width, True)]["rows"] >= 2, f"window {width}, panel open: the AI has a line of its own"
        # Under 1100 the panel floats over the middle: the message box and
        # its foot keep clear of it (look() checks that), Send included.
        size(d, 1024)
        panel(d, True)
        clear = d.execute_script("const s = document.getElementById('send').getBoundingClientRect(), o = document.getElementById('outputs').getBoundingClientRect(); return [s.right, o.left]")
        assert clear[0] <= clear[1], f"in a window 1024 wide Send is under the panel: {clear}"
        size(d, 1100)
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        # The pickers still work on their own line.
        d.execute_script("const s = document.getElementById('chat-model'); s.blur(); s.focus();")
        Select(d.find_element(By.ID, "chat-model")).select_by_value("claude-opus-5-5")
        Wait(d, 10).until(lambda drv: "From here on: Claude Opus 5.5." in drv.find_element(By.ID, "log").text)
        look(d, "a chat in a project, another model chosen", 1100, True)

        step("While the assistant works: the project and what it is doing, at the narrowest middle")
        HOLD.clear()
        send(d, "And their coordinate systems?", wait_for=None)
        Wait(d, 20).until(lambda drv: drv.execute_script("return document.getElementById('status').textContent") == "Working…")
        look(d, "a chat at work", 1100, True, one_line=True, busy=True)
        picture(d, "busy-1100-open")
        size(d, 1024)
        look(d, "a chat at work", 1024, True, busy=True)
        HOLD.set()
        Wait(d, 60).until(lambda drv: not drv.execute_script("return document.getElementById('composer').classList.contains('busy')"))
        panel(d, False)

        step("A chat in no project says so, whole, at every width")
        new_chat(d)
        send(d, "Hello")
        assert d.find_element(By.CSS_SELECTOR, "#chat-sub a").get_attribute("textContent") == "No project"
        seen = every_width(d, "a chat in no project", "none", started=True, pictures={(1240, True)}, one_line=closed)
        for (width, is_open), m in seen.items():
            for p in m["parts"]:
                assert p["whole"], f"window {width}, panel {'open' if is_open else 'closed'}: {p['name']} is cut: {p}"

        step("A project with one of the longest names: cut only where a whole line cannot hold it, and whole in its title")
        new_chat(d, LONG[0])
        send(d, "Hello")
        seen = every_width(d, "a chat in a project with a long name", "long", started=True, pictures={(1024, True), (1240, True), (1440, False)})
        for (width, is_open), m in seen.items():
            p = next(x for x in m["parts"] if x["name"] == "project")
            assert p["text"] == LONG[1] and LONG[1] in p["title"], p
            if is_open:
                assert m["rows"] >= 2, f"window {width}, panel open: a name this long has the first line to itself"
            assert p["whole"] or p["chars"] >= 20, f"window {width}, panel {'open' if is_open else 'closed'}: the long name shows {p['chars']} characters"
            for q in m["parts"]:
                assert q["name"] == "project" or q["whole"], f"window {width}, panel {'open' if is_open else 'closed'}: {q['name']} is cut beside a long project name: {q}"

        step("A service and a model with long names: LM Studio on this computer, a model id typed in Settings")
        invoke(d, "chat_use_service", {"service": "lmstudio"})
        invoke(d, "chat_save_settings", {"model": LONG_MODEL, "effort": None, "engine": ""})
        new_chat(d, SHORT[0])
        wait.until(lambda drv: drv.execute_script("return document.getElementById('chat-service').textContent") == "LM Studio on this computer")
        assert d.execute_script("return document.getElementById('chat-model').value") == LONG_MODEL
        every_width(d, "a new chat on LM Studio", "new-lmstudio", started=False, one_line=True)
        send(d, "Hello")
        seen = every_width(d, "a chat on LM Studio", "lmstudio", started=True, pictures={(1024, True), (1240, True), (1440, False)}, one_line=closed)
        for (width, is_open), m in seen.items():
            parts = {p["name"]: p for p in m["parts"]}
            assert parts["project"]["whole"] and parts["service"]["whole"], f"window {width}, panel {'open' if is_open else 'closed'}: {parts}"
            assert "chat-effort" not in parts, "a service with one thinking level shows no picker for it"
            # (A picker keeps to a width of its own: a long id shows its
            # first dozen characters or more, and all of it in its title.)
            assert not parts["chat-model"]["whole"] and parts["chat-model"]["chars"] >= 12 and LONG_MODEL in parts["chat-model"]["title"], parts["chat-model"]
        assert not guard.unnamed(d), guard.unnamed(d)  # every control has a name, and Tab reaches it
        print("desktop foot e2e: ok")
    except BaseException:
        try:
            d.save_screenshot(str(OUT / "foot-failed.png"))
        except Exception:
            pass
        raise
    finally:
        guard.stop(d, driver_proc)
        stand.shutdown()


if __name__ == "__main__":
    main()
