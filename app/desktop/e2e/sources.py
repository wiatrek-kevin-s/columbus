"""Folders of files, ArcGIS services, ArcGIS sign-in and Python
environments in the main window, end to end on Linux through WebDriver
(T-12, REQ-11; the settings.py pattern).

Adds a `files` connection (subfolders, CRS and coordinate columns kept
when it is edited again), a public `arcgis` connection, and one that signs
in; Test lists their datasets, a service that does not answer gives the
plain R-14 message, and Sign in opens the browser at the fake portal's
authorize address (a stand-in `xdg-open` plays the browser and follows
the redirect back to Columbus, so the sign-in completes and the signed-in
service then tests). Settings › Environments registers a test venv with a
stub arcpy and shows its status and arcpy line, confirms it again after a
change, removes one, and declines a proposal. Then, with the person
declining to confirm, nothing is registered, approved or signed in.

A debug build stands in for Touch ID / Windows Hello with
COLUMBUS_APP_TEST_CONFIRM (allow or deny); the fake portal is
crates/gateway/examples/fake_oauth_server.rs, reached over plain http to
127.0.0.1 only because debug builds honour COLUMBUS_FETCH_TEST_LOOPBACK.
Its sign-in is kept in the development secret store (a folder), never a
real one.

    cargo build -p columbus-mcp --example fake_oauth_server
    dbus-run-session -- xvfb-run -a python app/desktop/e2e/sources.py \\
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json
import os
import queue
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
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
FAKE = GATEWAY.parent / "examples" / "fake_oauth_server"
STUB = REPO / "crates/gateway/tests/fixtures/arcpy_stub/arcpy"
OUT = Path(__file__).resolve().parent / "out"
POLICY = """# The organization's settings.
connection_types: [files, arcgis]
hosts: ["127.0.0.1", "localhost"]
hide_fields: ["*ssn*"]
environments:
  allow: true
"""
MARK = "zq7k"  # in every fake token: none may reach the page

BROWSER = """#!{python}
# Plays the person's browser for the app (it runs `xdg-open <url>`): notes
# the address, then follows it and its redirect back to Columbus.
import sys, urllib.request
with open({log!r}, "a") as f:
    f.write(sys.argv[1] + "\\n")
try:
    urllib.request.urlopen(sys.argv[1], timeout=30).read()
except Exception as e:
    with open({log!r}, "a") as f:
        f.write("error: %s\\n" % e)
"""


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def files(root):
    return dict(os.environ, COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
                COLUMBUS_LIVE_DIR=str(root / "live"), XDG_CONFIG_HOME=str(root / "config"),
                COLUMBUS_TOOLS=str(root / "tools"), COLUMBUS_WORKSPACES=str(root / "ws"),
                COLUMBUS_AUDIT_DIR=str(root / "audit"), COLUMBUS_ENVIRONMENTS=str(root / "environments"),
                COLUMBUS_ENVS=str(root / "envs"), COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"),
                COLUMBUS_PYTHON=sys.executable, COLUMBUS_APPROVALS_BROWSER="none", COLUMBUS_LOG="warn",
                COLUMBUS_DEV_SECRETS="1", COLUMBUS_DEV_SECRETS_DIR=str(root / "secrets"),
                COLUMBUS_FETCH_TEST_LOOPBACK="1", COLUMBUS_SERVER=str(GATEWAY),
                PATH=f"{root / 'bin'}{os.pathsep}{os.environ.get('PATH', '')}")


def columbus(root):
    """A Columbus session on the same files, as Claude desktop would run it."""
    proc = subprocess.Popen([str(GATEWAY), "serve"], env=files(root), stdin=subprocess.PIPE,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(200):
        if list((root / "live").glob("*.key")):
            return proc
        time.sleep(0.05)
    raise SystemExit("the Columbus session did not announce itself")


def fake_portal():
    proc = subprocess.Popen([str(FAKE)], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    first = queue.Queue()
    threading.Thread(target=lambda: first.put(proc.stdout.readline()), daemon=True).start()
    try:
        line = first.get(timeout=30).strip()
    except queue.Empty:
        raise SystemExit("the fake portal did not say where it listens within 30 s")
    assert line.startswith("listening "), line
    return proc, line.split(" ", 1)[1]


def session(root, confirm):
    env = dict(files(root), COLUMBUS_APP_TEST_CONFIRM=confirm)
    d, proc = guard.driver(env, APP)
    wait = WebDriverWait(d, 60)
    guard.ready(d, wait)
    nav(d, wait, "#/data", "add")
    return d, proc, wait


def nav(d, wait, where, ready):
    d.execute_script("location.hash = arguments[0]", where)
    wait.until(EC.presence_of_element_located((By.ID, ready)))
    time.sleep(0.3)


def click(d, how, what):
    e = d.find_element(how, what)
    d.execute_script("arguments[0].scrollIntoView({block: 'center'})", e)
    e.click()


def fill(d, name, value):
    # Set directly: Selenium would read an existing path as a file upload.
    d.execute_script("document.querySelector(`[name='${arguments[0]}']`).value = arguments[1];", name, value)


def choose(d, name, value):
    d.execute_script("const s = document.querySelector(`[name='${arguments[0]}']`); s.value = arguments[1];"
                     "s.dispatchEvent(new Event('change'));", name, value)


def text(d, id_):
    return d.execute_script("return document.getElementById(arguments[0]).textContent", id_)


def test_says(d, wait, *words):
    click(d, By.ID, "test")
    try:
        wait.until(lambda drv: "Testing" not in drv.find_element(By.ID, "test").text
                   and drv.find_element(By.ID, "test-result").is_displayed())
    except Exception:
        d.save_screenshot(str(OUT / "sources-test-failed.png"))
        raise
    got = d.find_element(By.ID, "test-result").text
    for w in words:
        assert w in got, f"{w!r} not in Test's answer: {got}"
    return got


def new_connection(d, wait, kind, conn_id):
    nav(d, wait, "#/data", "add")
    d.find_element(By.ID, "add").click()
    wait.until(EC.visibility_of_element_located((By.NAME, "conn_id")))
    choose(d, "kind", kind)
    fill(d, "conn_id", conn_id)


def open_source(d, wait, conn_id):
    """A connection's own page, from its row on Data."""
    nav(d, wait, "#/data", "add")
    wait.until(EC.presence_of_element_located((By.CSS_SELECTOR, f"#connections li[data-id='{conn_id}'] a")))
    click(d, By.CSS_SELECTOR, f"#connections li[data-id='{conn_id}'] a")
    wait.until(lambda drv: drv.find_elements(By.ID, "source-edit") and text(drv, "page-title") == conn_id)
    time.sleep(0.3)


def page_button(d, label):
    """A button on the connection's page that is open."""
    return d.find_element(By.XPATH, f"//*[@id='source']//button[normalize-space()='{label}']")


# The words a connection's page says after Draft notes or Sign in.
STATUS = "//*[@id='source']//span[contains(@class, 'draft-status')]"


def portal_log(base):
    with urllib.request.urlopen(base + "/test/log", timeout=10) as r:
        return json.load(r)


def authorize_requests(base):
    return [e for e in portal_log(base) if e["target"].startswith("/sharing/rest/oauth2/authorize?")]


def env_row(d, env_id):
    return d.find_element(By.XPATH, f"//ul[@id='environments']/li[.//b[normalize-space()='{env_id}']]")


def env_text(d, env_id):
    # textContent: WebKit's driver leaves chips out of an element's text.
    return env_row(d, env_id).get_attribute("textContent")


def make_venv(root):
    venv = root / "venv"
    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(venv)], check=True, timeout=120)
    py = venv / "bin" / "python"
    purelib = subprocess.run([str(py), "-c", "import sysconfig; print(sysconfig.get_paths()['purelib'])"],
                             check=True, capture_output=True, text=True, timeout=60).stdout.strip()
    shutil.copytree(STUB, Path(purelib) / "arcpy")
    return py


def propose(root, env_id):
    d = root / "environments" / ".proposals"
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{env_id}.json").write_text(json.dumps({
        "id": env_id, "python_version": None, "packages": ["six==1.17.0"],
        "reason": "Count words in the crash narratives.", "proposed": "2026-11-08T10:00:00+00:00",
        "session": "s1", "client": "Claude"}))


def main():
    guard.start("desktop sources e2e", minutes=10)
    step("fixtures: folders, the stand-in browser, a venv with a stub arcpy")
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    for sub in ["connections", "bin", "secrets", "environments", "envs"]:
        (root / sub).mkdir()
    (root / "policy.yaml").write_text(POLICY)
    drops = root / "drops"
    (drops / "2026").mkdir(parents=True)
    (drops / "crashes.csv").write_text("id,EASTING,NORTHING,severity\n1,3100000,10070000,minor\n2,3100500,10070200,major\n")
    (drops / "2026" / "more.csv").write_text("id,EASTING,NORTHING\n3,3101000,10071000\n")
    browser_log = root / "browser.log"
    xdg = root / "bin" / "xdg-open"
    xdg.write_text(BROWSER.format(python=sys.executable, log=str(browser_log)))
    xdg.chmod(0o755)
    python = make_venv(root)

    step("the fake portal and a Columbus session")
    portal, base = fake_portal()
    dead = free_port()
    server = columbus(root)
    step("the app, confirming allowed")
    d, proc, wait = session(root, "allow")
    try:
        # ---- A folder of files: Test, save, edit again (F-2 fields kept). ----
        step("files: Test and save")
        new_connection(d, wait, "files", "drops")
        assert d.find_element(By.NAME, "recursive").is_displayed()
        assert not d.find_element(By.NAME, "url").is_displayed()
        assert d.find_element(By.ID, "browse").text == "Choose folder…"
        assert d.execute_script("return document.querySelector(\"#editor input[name=mode_pick][value='read-write']\").disabled"), "a folder of files is read only here"
        assert d.execute_script("return document.querySelector('#editor input[name=mode_pick]:checked').value") == "read"
        fill(d, "path", str(drops))
        d.execute_script("document.querySelector('[name=recursive]').checked = true;")
        fill(d, "crs", "EPSG:3081")
        fill(d, "x", "EASTING")
        fill(d, "y", "NORTHING")
        test_says(d, wait, "Opened", "crashes", "2026/more")
        assert not (root / "connections" / "drops.yaml").exists(), "Test saves nothing"
        d.save_screenshot(str(OUT / "sources-files.png"))
        click(d, By.ID, "save")
        wait.until(lambda drv: (root / "connections" / "drops.yaml").exists())
        c = yaml.safe_load((root / "connections" / "drops.yaml").read_text())
        assert c["type"] == "files" and c["path"] == str(drops) and c["mode"] == "read", c
        assert c["recursive"] is True and c["crs"] == "EPSG:3081" and (c["x"], c["y"]) == ("EASTING", "NORTHING"), c
        wait.until(lambda drv: "drops" in text(drv, "connections"))
        assert "Folder of files" in text(d, "connections") and "read only" in text(d, "connections"), text(d, "connections")
        open_source(d, wait, "drops")
        assert "With subfolders" in text(d, "source") and "EPSG:3081" in text(d, "source"), text(d, "source")
        # Editing keeps recursive, crs, x and y (T-2's loose end).
        step("files: edit again")
        click(d, By.ID, "source-edit")
        wait.until(EC.visibility_of_element_located((By.NAME, "crs")))
        assert d.execute_script("return document.querySelector('[name=recursive]').checked")
        assert d.find_element(By.NAME, "crs").get_attribute("value") == "EPSG:3081"
        assert d.find_element(By.NAME, "x").get_attribute("value") == "EASTING"
        fill(d, "description", "Crash drops from the districts")
        click(d, By.ID, "save")
        wait.until(lambda drv: "Crash drops" in (root / "connections" / "drops.yaml").read_text())
        c = yaml.safe_load((root / "connections" / "drops.yaml").read_text())
        assert c["recursive"] is True and c["crs"] == "EPSG:3081" and (c["x"], c["y"]) == ("EASTING", "NORTHING"), c

        # ---- A public ArcGIS service: Test needs no sign-in. ----
        step("arcgis: a public service")
        public = f"{base}/arcgis/rest/services/Public/FeatureServer"
        new_connection(d, wait, "arcgis", "assets")
        assert d.find_element(By.NAME, "url").is_displayed()
        assert not d.find_element(By.NAME, "path").is_displayed()
        assert not d.find_element(By.NAME, "portal").is_displayed(), "portal shows only for OAuth"
        fill(d, "url", public)
        test_says(d, wait, "Opened", "Assets")
        click(d, By.ID, "save")
        wait.until(lambda drv: (root / "connections" / "assets.yaml").exists())
        c = yaml.safe_load((root / "connections" / "assets.yaml").read_text())
        assert c["type"] == "arcgis" and c["url"] == public and "auth" not in c and "path" not in c, c
        wait.until(lambda drv: "assets" in text(drv, "connections"))
        open_source(d, wait, "assets")
        assert "Public (no sign-in)" in text(d, "source"), text(d, "source")
        assert not d.find_elements(By.XPATH, "//*[@id='source']//button[normalize-space()='Sign in']")
        assert not d.find_elements(By.XPATH, "//*[@id='source']//button[normalize-space()='Draft notes']"), "drafted notes don't apply to a service"

        # ---- A service that does not answer: the plain R-14 message. ----
        step("arcgis: a service that does not answer")
        new_connection(d, wait, "arcgis", "gone")
        fill(d, "url", f"http://127.0.0.1:{dead}/arcgis/rest/services/Gone/FeatureServer")
        got = test_says(d, wait, "ArcGIS service at 127.0.0.1", "Check the address or your network, then Test again.")
        assert "test-bad" in d.find_element(By.ID, "test-result").get_attribute("class"), got
        d.save_screenshot(str(OUT / "sources-network-error.png"))
        click(d, By.ID, "cancel")

        # ---- A service that signs in: Sign in opens the portal's page. ----
        step("arcgis: a service that signs in, saved")
        secure = f"{base}/arcgis/rest/services/Secure/FeatureServer"
        new_connection(d, wait, "arcgis", "secure")
        fill(d, "url", secure)
        choose(d, "auth", "oauth")
        assert d.find_element(By.NAME, "portal").is_displayed()
        fill(d, "portal", base)
        fill(d, "client_id", "FakeClientId123")
        fill(d, "page_size", "500")
        test_says(d, wait, "needs an ArcGIS sign-in")
        click(d, By.ID, "save")
        wait.until(lambda drv: (root / "connections" / "secure.yaml").exists())
        c = yaml.safe_load((root / "connections" / "secure.yaml").read_text())
        assert c["auth"] == {"type": "oauth", "portal": base, "client_id": "FakeClientId123"} and c["page_size"] == 500, c
        wait.until(lambda drv: "secure" in text(drv, "connections"))
        open_source(d, wait, "secure")
        assert "Signs in at" in text(d, "source"), text(d, "source")
        assert authorize_requests(base) == []
        step("arcgis: Sign in, through the stand-in browser")
        btn = page_button(d, "Sign in")
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", btn)
        btn.click()
        status = STATUS
        try:
            WebDriverWait(d, 90).until(lambda drv: "Signed in" in drv.find_element(By.XPATH, status).text
                                       or "error-text" in drv.find_element(By.XPATH, status).get_attribute("class"))
        finally:
            d.save_screenshot(str(OUT / "sources-signin.png"))
        said = d.find_element(By.XPATH, status).text
        assert "Signed in" in said and MARK not in said, said
        asked = authorize_requests(base)
        assert len(asked) == 1, asked
        target = asked[0]["target"]
        for part in ["client_id=FakeClientId123", "response_type=code", "code_challenge_method=S256", "redirect_uri=http%3A%2F%2F127.0.0.1%3A"]:
            assert part in target, target
        opened = browser_log.read_text().splitlines()
        assert opened and opened[0] == base + target, (opened, target)
        assert (root / "secrets" / "COLUMBUS_SECRET_ARCGIS_SECURE").exists(), list((root / "secrets").iterdir())
        assert MARK not in d.page_source
        # Signed in, the service tests.
        step("arcgis: the signed-in service tests, on its page and in its editor")
        click(d, By.ID, "source-test-btn")
        wait.until(lambda drv: drv.find_element(By.ID, "source-test").is_displayed() and "Testing" not in drv.find_element(By.ID, "source-test-btn").text)
        said = d.find_element(By.ID, "source-test").text
        assert "Opened" in said and "Assets" in said and MARK not in said, said
        click(d, By.ID, "source-edit")
        wait.until(EC.visibility_of_element_located((By.NAME, "portal")))
        test_says(d, wait, "Opened", "Assets")
        click(d, By.ID, "cancel")

        # ---- Settings › Environments: register, status and arcpy line. ----
        step("environments: register the stub-arcpy venv")
        nav(d, wait, "#/settings/environments", "env-add")
        wait.until(lambda drv: drv.find_element(By.ID, "no-env").is_displayed())
        click(d, By.ID, "env-add")
        wait.until(EC.visibility_of_element_located((By.NAME, "env_id")))
        fill(d, "env_id", "stub-pro")
        fill(d, "env_python", str(python))
        fill(d, "env_description", "A test venv with a stub arcpy")
        click(d, By.ID, "env-save")
        try:
            wait.until(lambda drv: "stub-pro" in text(drv, "environments"))
        except Exception:
            d.save_screenshot(str(OUT / "sources-env-failed.png"))
            raise SystemExit("Add said: " + d.execute_script("const e = document.getElementById('env-form-error'); return e ? e.textContent : ''"))
        row = env_text(d, "stub-pro")
        assert "Ready" in row, row
        assert "arcpy: yes (experimental); ArcGIS Pro licence: Advanced" in row, row
        assert "Governed, level A" in row and "Python 3." in row, row
        reg = yaml.safe_load((root / "environments" / "stub-pro.yaml").read_text())
        assert reg["python"] == str(python) and reg["trust"] == "governed" and reg["probe"]["arcpy"] is True, reg
        assert reg["created_by"] == "person", reg
        d.save_screenshot(str(OUT / "sources-environments.png"))
        # Edited by hand: changed, until confirmed again.
        step("environments: edited by hand, confirmed again")
        f = root / "environments" / "stub-pro.yaml"
        f.write_text(f.read_text().replace("probed_at: ", "probed_at: '2020-01-01T00:00:00+00:00' # was ", 1))
        nav(d, wait, "#/settings/org", "allow_edits")
        nav(d, wait, "#/settings/environments", "env-add")
        wait.until(lambda drv: "Changed: confirm it again" in env_text(drv, "stub-pro"))
        assert "edited after it was confirmed" in env_text(d, "stub-pro")
        d.save_screenshot(str(OUT / "sources-env-changed.png"))
        env_row(d, "stub-pro").find_element(By.XPATH, ".//button[normalize-space()='Confirm']").click()
        wait.until(lambda drv: "Ready" in env_text(drv, "stub-pro"))
        assert "Confirmed stub-pro" in d.find_element(By.ID, "env-notice").text
        # Another, removed again: its interpreter folder stays (the person registered it).
        step("environments: add one and remove it")
        click(d, By.ID, "env-add")
        wait.until(EC.visibility_of_element_located((By.NAME, "env_id")))
        fill(d, "env_id", "spare")
        fill(d, "env_python", str(python))
        click(d, By.ID, "env-save")
        wait.until(lambda drv: "spare" in text(drv, "environments"))
        env_row(d, "spare").find_element(By.XPATH, ".//button[normalize-space()='Remove']").click()
        wait.until(lambda drv: not (root / "environments" / "spare.yaml").exists())
        wait.until(lambda drv: "Removed spare" in drv.find_element(By.ID, "env-notice").text)
        assert python.exists()
        # A proposal from the assistant, declined (no confirmation needed).
        step("environments: decline a proposal")
        propose(root, "words")
        nav(d, wait, "#/settings/org", "allow_edits")
        nav(d, wait, "#/settings/environments", "env-add")
        wait.until(lambda drv: "words" in text(drv, "env-proposals"))
        assert "six==1.17.0" in text(d, "env-proposals") and "crash narratives" in text(d, "env-proposals")
        d.save_screenshot(str(OUT / "sources-env-proposal.png"))
        d.find_element(By.XPATH, "//ul[@id='env-proposals']/li[.//b[normalize-space()='words']]//button[normalize-space()='Decline']").click()
        wait.until(lambda drv: not (root / "environments" / ".proposals" / "words.json").exists())
    finally:
        try:
            d.save_screenshot(str(OUT / "sources-last.png"))
        except Exception:
            pass
        step("closing the first app")
        guard.stop(d, proc)

    # ---- The person declines: nothing is registered, approved or signed in. ----
    propose(root, "words2")
    step("the app again, confirming denied (once the first app has gone)")
    d, proc, wait = session(root, "deny")
    try:
        step("denied: Sign in")
        before = len(authorize_requests(base))
        open_source(d, wait, "secure")
        btn = page_button(d, "Sign in")
        d.execute_script("arguments[0].scrollIntoView({block: 'center'})", btn)
        btn.click()
        wait.until(lambda drv: "not confirmed" in drv.find_element(By.XPATH, STATUS).text)
        assert len(authorize_requests(base)) == before, "no sign-in page without confirmation"
        step("denied: save a connection")
        new_connection(d, wait, "arcgis", "assets2")
        fill(d, "url", f"{base}/arcgis/rest/services/Public/FeatureServer")
        click(d, By.ID, "save")
        wait.until(lambda drv: drv.find_element(By.ID, "form-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "form-error").text
        assert not (root / "connections" / "assets2.yaml").exists()
        click(d, By.ID, "cancel")
        step("denied: register an environment, approve a proposal")
        nav(d, wait, "#/settings/environments", "env-add")
        wait.until(lambda drv: "words2" in text(drv, "env-proposals"))
        click(d, By.ID, "env-add")
        wait.until(EC.visibility_of_element_located((By.NAME, "env_id")))
        fill(d, "env_id", "other")
        fill(d, "env_python", str(python))
        click(d, By.ID, "env-save")
        wait.until(lambda drv: drv.find_element(By.ID, "env-form-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "env-form-error").text
        assert not (root / "environments" / "other.yaml").exists()
        d.execute_script("document.getElementById('env-dialog').close()")
        d.find_element(By.XPATH, "//ul[@id='env-proposals']/li[.//b[normalize-space()='words2']]//button[normalize-space()='Approve']").click()
        wait.until(lambda drv: drv.find_element(By.ID, "env-error").is_displayed())
        assert "not confirmed" in d.find_element(By.ID, "env-error").text
        assert (root / "environments" / ".proposals" / "words2.json").exists()
        assert not (root / "envs" / "words2").exists()
        print("desktop sources e2e (files, arcgis, sign-in, environments): ok")
    finally:
        step("closing the app, Columbus and the fake portal")
        try:
            guard.stop(d, proc)
        finally:
            server.stdin.close()
            server.wait(timeout=30)
            portal.terminate()
            portal.wait(timeout=30)


if __name__ == "__main__":
    main()
