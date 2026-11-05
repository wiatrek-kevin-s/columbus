"""A minimal MCP client over stdio for the end-to-end tests, and a helper to
wait for a file to appear."""

import json
import subprocess
import threading
import time
from pathlib import Path


class Mcp:
    """A minimal MCP client over stdio: requests in, responses by id."""

    def __init__(self, cmd, env):
        self.p = subprocess.Popen(cmd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        self.next = 0
        self.lock = threading.Lock()
        self.replies = {}
        self.cv = threading.Condition()
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                    "clientInfo": {"name": "companion-e2e", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _send(self, msg):
        with self.lock:
            self.p.stdin.write(json.dumps(msg) + "\n")
            self.p.stdin.flush()

    def _read(self):
        for line in self.p.stdout:
            m = json.loads(line)
            if "id" in m and ("result" in m or "error" in m):
                with self.cv:
                    self.replies[m["id"]] = m
                    self.cv.notify_all()

    def request(self, method, params, timeout=120):
        with self.lock:
            self.next += 1
            i = self.next
        self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        end = time.time() + timeout
        with self.cv:
            while i not in self.replies:
                left = end - time.time()
                assert left > 0, f"no reply to {method}"
                self.cv.wait(left)
            return self.replies.pop(i)

    def tool(self, name, args, timeout=120):
        r = self.request("tools/call", {"name": name, "arguments": args}, timeout)
        res = r["result"]
        assert not res.get("isError"), res
        return res["structuredContent"]

    def close(self):
        self.p.stdin.close()
        self.p.wait(10)


def wait_file(p: Path, t=20):
    end = time.time() + t
    while time.time() < end:
        if p.exists() and p.read_text().strip():
            return p.read_text().strip()
        time.sleep(0.1)
    raise AssertionError(f"{p} never appeared")
