"""The desktop app's link handling, end to end on Linux (CI runs it under
xvfb and a D-Bus session): the app keeps its heartbeat, a session's server
then sends its window to the app as a columbus:// link, and a second launch
with that link hands it to the running app, whose window opens the page
(the page's script spends the launch token, which this test then finds
used). A link for a port that is not a live session is ignored.

    dbus-run-session -- xvfb-run -a python app/desktop/e2e/deeplink.py \
        target/debug/columbus-mcp app/desktop/src-tauri/target/debug/columbus-desktop
"""

import json, os, subprocess, sys, tempfile, time, urllib.request, urllib.error
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "app/ui/e2e"))
from companion import Mcp, wait_file  # noqa: E402

GATEWAY = Path(sys.argv[1]) if len(sys.argv) > 1 else REPO / "target/debug/columbus-mcp"
APP = Path(sys.argv[2]) if len(sys.argv) > 2 else REPO / "app/desktop/src-tauri/target/debug/columbus-desktop"
root = Path(tempfile.mkdtemp())
(root/"connections").mkdir()
(root/"policy.yaml").write_text("connection_types: [gpkg]\n")
env = dict(os.environ, COLUMBUS_POLICY=str(root/"policy.yaml"), COLUMBUS_CONNECTIONS=str(root/"connections"),
    COLUMBUS_WORKSPACES=str(root/"ws"), COLUMBUS_AUDIT_DIR=str(root/"audit"), COLUMBUS_LIVE_DIR=str(root/"live"),
    COLUMBUS_UI_DIR=str(REPO/"app/ui/dist"), COLUMBUS_WORKER=str(REPO/"workers/python/columbus_worker.py"),
    COLUMBUS_PYTHON=sys.executable, COLUMBUS_APPROVALS_LAUNCH_FILE=str(root/"launch.txt"), COLUMBUS_LOG="warn")
mcp = Mcp([str(GATEWAY), "serve"], env)
session = mcp.tool("whoami", {})["result"]["session"]
port = json.loads(wait_file(root/"live"/f"{session}.json"))["port"]
app_env = dict(os.environ, COLUMBUS_LIVE_DIR=str(root/"live"), WEBKIT_DISABLE_COMPOSITING_MODE="1")
exe = str(APP)
app = subprocess.Popen([exe], env=app_env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
hb = json.loads(wait_file(root/"live"/"app.json", 30))
assert hb["app"] == "columbus-desktop", hb
time.sleep(1)
req = urllib.request.Request(f"http://127.0.0.1:{port}/api/open", method="POST", headers={"X-Columbus-Open": "1"})
assert urllib.request.urlopen(req).status == 200
link = wait_file(root/"launch.txt")
assert link.startswith(f"columbus://open?port={port}#t="), f"the server sends the app's window as a link to the app: {link}"
token = link.split("#t=")[1]
deep = link
second = subprocess.run([exe, deep], env=app_env, timeout=30, capture_output=True, text=True)
assert second.returncode == 0, second
time.sleep(8)
# If the app opened the page, its script spent the launch token.
r = urllib.request.Request(f"http://127.0.0.1:{port}/api/session", method="POST", data=json.dumps({"launch": token}).encode(),
    headers={"Content-Type": "application/json", "Origin": f"http://127.0.0.1:{port}"})
try:
    urllib.request.urlopen(r)
    raise SystemExit("FAIL: the launch token is still unused, so the app did not open the page")
except urllib.error.HTTPError as e:
    assert e.code == 403, e.code
    print("ok: the app's window opened the session's page")
# A link for a port that is not a live session is ignored.
subprocess.run([exe, f"columbus://open?port=9#t={'a'*64}"], env=app_env, timeout=30)
app.terminate()
try: out = app.communicate(timeout=10)[0]
except Exception: app.kill(); out = app.communicate()[0]
mcp.close()
print("desktop deep link e2e: ok")
