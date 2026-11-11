"""Shared guards for the main window's WebDriver scripts (Linux).

- `start(name, minutes)`: an overall time limit. When it runs out the script
  says which step it reached, kills everything it started (tauri-driver,
  WebKitWebDriver, the app, the fake servers, Columbus) and exits non-zero.
  Any other failure also names the last step, and whatever is still running
  is stopped when the script ends.
- `step(what)`: prints `step: <what>` so a CI log shows where a run stopped.
- `Wait`: a WebDriverWait whose time-out names the step that was waiting.
- `driver(env, app)` / `stop(d, proc)`: tauri-driver and one app session.
  The app is single-instance (tauri-plugin-single-instance holds a D-Bus
  name): a launch while an earlier app is still closing hands itself to that
  app and exits, and WebKitWebDriver then waits for a browser that never
  comes. So a session starts only once the earlier app has let go of the
  name, and every WebDriver command has a time limit.
- `stop` ends the app for every script, the same way. Ending a session
  closes the page; WebKitWebDriver then kills the app it started, just
  after it answers. tauri-driver, on SIGTERM, kills WebKitWebDriver at once,
  so stopping the driver straight after the answer sometimes killed
  WebKitWebDriver first and left the app running on its own, page closed
  and window open, holding the name for good (about 1 stop in 6 here). So
  `stop` waits for the app to go before it stops the driver; an app still
  there after APP_GRACE seconds is ended (SIGTERM, then SIGKILL) and the
  log says so.
"""

import atexit
import json
import os
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

from selenium import webdriver
from selenium.common.exceptions import StaleElementReferenceException
from selenium.webdriver.common.options import ArgOptions
from selenium.webdriver.remote.client_config import ClientConfig
from selenium.webdriver.remote.webelement import WebElement
from selenium.webdriver.support.ui import WebDriverWait

_CONF = Path(__file__).resolve().parents[1] / "src-tauri" / "tauri.conf.json"
BUS_NAME = json.loads(_CONF.read_text())["identifier"] + ".SingleInstance"
DRIVER_PORT, NATIVE_PORT = 4444, 4445
# One WebDriver command (not a wait): starting a session launches the app.
COMMAND_LIMIT = 120
# How long the app may take to go once its session has ended (it usually
# takes well under a second), and how long it gets after SIGTERM.
APP_GRACE, TERM_GRACE = 10, 10

_name = "e2e"
_last = "starting"
_lock = threading.Lock()


def step(what):
    global _last
    with _lock:
        _last = what
    print(f"step: {what}", flush=True)


def last():
    with _lock:
        return _last


def start(name, minutes=10):
    """The whole script's time limit."""
    global _name
    _name = name
    limit = minutes * 60

    def expired():
        sys.stderr.write(f"{_name}: TIMED OUT after {minutes} minutes; last step reached: {last()}. "
                         "Stopping the app, the driver and the servers.\n")
        sys.stderr.flush()
        kill_all()
        os._exit(124)

    t = threading.Timer(limit, expired)
    t.daemon = True
    t.start()

    hook = sys.excepthook

    def failed(kind, value, tb):
        hook(kind, value, tb)
        sys.stderr.write(f"{_name}: FAILED at step: {last()}\n")
        sys.stderr.flush()

    sys.excepthook = failed
    atexit.register(kill_all)


def _children():
    """Every process below this one, deepest last."""
    kids = {}
    for p in Path("/proc").iterdir():
        if not p.name.isdigit():
            continue
        try:
            stat = (p / "stat").read_text()
            ppid = int(stat.rsplit(")", 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            continue
        kids.setdefault(ppid, []).append(int(p.name))
    out, todo = [], [os.getpid()]
    while todo:
        for c in kids.get(todo.pop(), []):
            out.append(c)
            todo.append(c)
    return out


def _app_pid():
    """The process holding the app's single-instance name, if any."""
    try:
        r = subprocess.run(["dbus-send", "--session", "--print-reply", "--dest=org.freedesktop.DBus",
                            "/org/freedesktop/DBus", "org.freedesktop.DBus.GetConnectionUnixProcessID",
                            f"string:{BUS_NAME}"], capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if r.returncode != 0:
        return None
    try:
        return int(r.stdout.split()[-1])
    except (ValueError, IndexError):
        return None


def kill_all():
    """Stop everything this script started, and the app if it is still up."""
    pids = _children()
    app = _app_pid()
    if app and app not in pids:
        pids.append(app)
    for pid in pids:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass


class Wait(WebDriverWait):
    """WebDriverWait whose time-out says which step was waiting. A condition
    that reads an element just as the page draws it again is asked again:
    the element it found is gone, and the next look finds the new one. (The
    condition must still come true within the time.)"""

    def __init__(self, driver, timeout, poll_frequency=0.5, ignored_exceptions=None):
        ignored = [StaleElementReferenceException, *(ignored_exceptions or [])]
        super().__init__(driver, timeout, poll_frequency=poll_frequency, ignored_exceptions=ignored)

    def until(self, method, message=""):
        return super().until(method, message or f"step '{last()}' waited {self._timeout}s in vain")

    def until_not(self, method, message=""):
        return super().until_not(method, message or f"step '{last()}' waited {self._timeout}s in vain")


def app_running():
    """Whether an app instance holds the single-instance name on this bus."""
    try:
        r = subprocess.run(["dbus-send", "--session", "--print-reply", "--dest=org.freedesktop.DBus",
                            "/org/freedesktop/DBus", "org.freedesktop.DBus.NameHasOwner", f"string:{BUS_NAME}"],
                           capture_output=True, text=True, timeout=10)
    except FileNotFoundError:
        # No dbus-send: fall back to the process name (cut to 15 letters).
        return subprocess.run(["pgrep", "-x", "columbus-deskto"], capture_output=True).returncode == 0
    except subprocess.TimeoutExpired:
        return True
    return "boolean true" in r.stdout


def _listening(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=1).close()
        return True
    except OSError:
        return False


def _until(cond, seconds, what):
    end = time.monotonic() + seconds
    while not cond():
        if time.monotonic() > end:
            raise SystemExit(f"{_name}: step '{last()}': {what} (waited {seconds}s)")
        time.sleep(0.1)


def driver(env, app):
    """tauri-driver and a session on a fresh app; returns (driver, proc)."""
    _until(lambda: not app_running(), 30, "the app from the previous session is still running")
    _until(lambda: not _listening(DRIVER_PORT) and not _listening(NATIVE_PORT), 30,
           f"ports {DRIVER_PORT} and {NATIVE_PORT} are still in use")
    proc = subprocess.Popen(["tauri-driver"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _until(lambda: proc.poll() is None and _listening(DRIVER_PORT) and _listening(NATIVE_PORT), 30,
           "tauri-driver and WebKitWebDriver did not start listening")
    opts = ArgOptions()
    opts.set_capability("tauri:options", {"application": str(app)})
    opts.set_capability("browserName", "wry")
    url = f"http://127.0.0.1:{DRIVER_PORT}"
    d = webdriver.Remote(url, options=opts, client_config=ClientConfig(url, timeout=COMMAND_LIMIT))
    return d, proc


def ready(d, wait):
    """Wait until the main window has drawn itself: its places are in the
    sidebar, and the page it opens on (a new chat) is shown."""
    wait.until(lambda drv: drv.execute_script(
        "return document.querySelectorAll('#nav a.tab').length === 7 && !!document.getElementById('text')"))




AT_REST = """
const parts = ['sidebar', 'main', 'outputs'].map((id) => document.getElementById(id)).filter(Boolean);
if (!parts.length) return 'none';
return parts.some((p) => typeof p.getAnimations === 'function' && p.getAnimations().some((a) => a.playState === 'running' || a.playState === 'pending')) ? 'moving' : 'still';
"""


def at_rest(d, seconds=5):
    """Wait until the main window's columns (the sidebar, the middle and the
    panel on the right) are not in the middle of a move: a click aimed while
    one glides lands where the thing was. They change at once today, so this
    is one look; it waits only if a move is ever given a transition. A window
    without them (the classic windows, a browser page, a frame) is at rest."""
    try:
        WebDriverWait(d, seconds, poll_frequency=0.03).until(lambda drv: drv.execute_script(AT_REST) != "moving")
    except Exception:
        # Not this wait's to report: the click that follows says what is wrong.
        pass


# Every click a script makes waits for the columns to be at rest first, as a
# person would.
_click = WebElement.click


def _click_at_rest(self):
    at_rest(self.parent)
    return _click(self)


WebElement.click = _click_at_rest


def unnamed(d):
    """The controls on show that a screen reader could not name, or that Tab
    cannot reach: in the dialog that is open, else in the whole window. A
    name is an aria-label, a label, the control's own text or its title; a
    placeholder alone is not one. (The script is shared with the map page's
    tests: app/ui/e2e/a11y.py.)"""
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "ui" / "e2e"))
    from a11y import UNNAMED
    return d.execute_script(UNNAMED)


def stop(d, proc):
    """End the session, the app and the driver (bounded).

    The driver stays up until the app has gone (see the module's notes); an
    app that outlives APP_GRACE is ended here and reported."""
    try:
        d.quit()
    finally:
        try:
            _until_quiet(lambda: not app_running(), APP_GRACE)
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
        if app_running():
            end_app()


def _until_quiet(cond, seconds):
    """Wait up to `seconds` for `cond`; whether it came true."""
    end = time.monotonic() + seconds
    while not cond():
        if time.monotonic() > end:
            return False
        time.sleep(0.05)
    return True


def end_app():
    """End an app that did not go when its session ended: SIGTERM, then
    SIGKILL, each reported. Fails the script if the name is still held."""
    pid = _app_pid()
    if pid is None:
        _until(lambda: not app_running(), TERM_GRACE, "the app from the previous session is still running")
        return
    why = {signal.SIGTERM: f"was still running {APP_GRACE}s after its session ended",
           signal.SIGKILL: f"was still running {TERM_GRACE}s after SIGTERM"}
    for sig in (signal.SIGTERM, signal.SIGKILL):
        print(f"{_name}: guard: the app (pid {pid}) {why[sig]}; sending {sig.name}", flush=True)
        try:
            os.kill(pid, sig)
        except OSError:
            pass
        if _until_quiet(lambda: not app_running(), TERM_GRACE):
            print(f"{_name}: guard: the app (pid {pid}) ended after {sig.name}", flush=True)
            return
    raise SystemExit(f"{_name}: step '{last()}': the app (pid {pid}) is still running after SIGKILL")
