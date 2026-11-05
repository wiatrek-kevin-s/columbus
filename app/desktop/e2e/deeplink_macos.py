"""The desktop app's link handling on macOS, where links arrive as Apple
events on the app's main thread: the bundled app is started with `open`, a
session's server sends its window to the app as a columbus:// link, the
link is opened with `open` as the server would, and the app must open the
session's page (its script spends the launch token) and stay responsive
(it answers AppleScript) instead of hanging.

    python app/desktop/e2e/deeplink_macos.py target/debug/columbus-mcp \\
        app/desktop/src-tauri/target/debug/bundle/macos/Columbus.app
"""

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "app/ui/e2e"))
from mcp_client import Mcp, wait_file  # noqa: E402

GATEWAY = Path(sys.argv[1])
APP = Path(sys.argv[2])


def responsive() -> bool:
    """The app answers an Apple event within 10 s (a hung main thread does not)."""
    r = subprocess.run(["osascript", "-e", 'with timeout of 10 seconds', "-e",
                        'tell application "Columbus" to get name', "-e", "end timeout"],
                       capture_output=True, text=True, timeout=30)
    return r.returncode == 0


def main():
    root = Path(tempfile.mkdtemp())
    (root / "connections").mkdir()
    (root / "policy.yaml").write_text("connection_types: [gpkg]\n")
    live = root / "live"
    env = dict(os.environ, COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"), COLUMBUS_LIVE_DIR=str(live),
               COLUMBUS_UI_DIR=str(REPO / "app/ui/dist"), COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"),
               COLUMBUS_PYTHON=sys.executable, COLUMBUS_APPROVALS_LAUNCH_FILE=str(root / "launch.txt"), COLUMBUS_LOG="warn")
    mcp = Mcp([str(GATEWAY), "serve"], env)
    try:
        session = mcp.tool("whoami", {})["result"]["session"]
        port = json.loads(wait_file(live / f"{session}.json"))["port"]
        subprocess.run(["open", "-n", "--env", f"COLUMBUS_LIVE_DIR={live}", str(APP)], check=True)
        wait_file(live / "app.json", 60)
        time.sleep(2)
        assert responsive(), "the app does not answer after starting"

        req = urllib.request.Request(f"http://127.0.0.1:{port}/api/open", method="POST", headers={"X-Columbus-Open": "1"})
        assert urllib.request.urlopen(req).status == 200
        link = wait_file(root / "launch.txt")
        assert link.startswith(f"columbus://open?port={port}#t="), link
        token = link.split("#t=")[1]
        subprocess.run(["open", link], check=True)
        time.sleep(10)
        assert responsive(), "the app hangs after a link arrives"
        r = urllib.request.Request(f"http://127.0.0.1:{port}/api/session", method="POST",
                                   data=json.dumps({"launch": token}).encode(),
                                   headers={"Content-Type": "application/json", "Origin": f"http://127.0.0.1:{port}"})
        try:
            urllib.request.urlopen(r)
            raise SystemExit("FAIL: the launch token is still unused, so the app did not open the page")
        except urllib.error.HTTPError as e:
            assert e.code == 403, e.code
        print("macOS deep link e2e: ok")
    finally:
        subprocess.run(["osascript", "-e", 'tell application "Columbus" to quit'], capture_output=True, timeout=30)
        mcp.close()


if __name__ == "__main__":
    main()
