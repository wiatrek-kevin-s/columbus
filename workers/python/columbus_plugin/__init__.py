"""Columbus engine plugin SDK for Python.

A plugin is a folder with a ``plugin.yaml`` and an entry script built on
this SDK. The gateway starts the entry script as a worker process and
speaks the worker protocol (docs/worker-protocol.md) with it over
stdin/stdout. The SDK handles the protocol; a plugin only writes tools::

    from columbus_plugin import Plugin, Severity

    plugin = Plugin("contoso.lrs-check", "1.2.0")

    @plugin.tool("check_events", params=["events", "routes", "tolerance"], outputs=["findings", "report"])
    def check_events(ctx):
        events = ctx.dataset("events")
        for i, row in enumerate(events.rows()):
            ctx.progress(100 * i / events.row_count)   # also the cancellation point
            ...
        ctx.warn("lrs.gap", Severity.WARN, "3 gaps on IH0035")
        return {"events_checked": events.row_count}

    if __name__ == "__main__":
        plugin.serve()

Plugins receive datasets as read-only extracts the gateway produced through
its governed read path, and paths for their outputs inside the session
workspace. They never receive credentials.

Standard library only, so plugins run in any Python 3.10+ environment.
"""

from __future__ import annotations

import csv
import json
import math
import os
import pathlib
import posixpath
import queue
import sqlite3
import struct
import sys
import threading
import traceback
from dataclasses import dataclass, field
from typing import Any, Callable, Iterable, Iterator

__all__ = [
    "PROTOCOL_VERSION",
    "readonly_uri",
    "Plugin",
    "Context",
    "Extract",
    "Row",
    "Geometry",
    "Severity",
    "ToolError",
    "Cancelled",
    "write_csv",
    "write_geojson",
    "decode_geometry",
]

PROTOCOL_VERSION = 2

# Error codes (docs/worker-protocol.md).
PARSE_ERROR = -32700
UNKNOWN_METHOD = -32601
INVALID_PARAMS = -32602
UNSUPPORTED = -32001
ENGINE_ERROR = -32002
TIMEOUT = -32003
CANCELLED = -32004


class Severity:
    FAIL = "fail"
    WARN = "warn"
    INFO = "info"


class ToolError(Exception):
    """Raise from a tool to fail the run with a message the agent will see."""

    def __init__(self, message: str, code: int = ENGINE_ERROR):
        super().__init__(message)
        self.code = code
        self.message = message


class Cancelled(ToolError):
    def __init__(self) -> None:
        super().__init__("cancelled", CANCELLED)


# ---------------------------------------------------------------------------
# Geometry (a light WKB reader, not a geometry library)
# ---------------------------------------------------------------------------

_TYPES = {1: "Point", 2: "LineString", 3: "Polygon", 4: "MultiPoint", 5: "MultiLineString", 6: "MultiPolygon", 7: "GeometryCollection"}
_ENVELOPE_BYTES = {0: 0, 1: 32, 2: 48, 3: 48, 4: 64}


class GeometryError(ValueError):
    pass


@dataclass
class Geometry:
    """A decoded geometry: its type and coordinates as nested lists of (x, y)."""

    type: str
    parts: list = field(default_factory=list)  # Point: [(x, y)]; LineString: [[...]]; Polygon: [[ring], ...]; multi: list of those
    srid: int | None = None

    def points(self) -> Iterator[tuple[float, float]]:
        def walk(v):
            if isinstance(v, tuple):
                yield v
            else:
                for x in v:
                    yield from walk(x)

        yield from walk(self.parts)

    @property
    def is_empty(self) -> bool:
        return not any(True for p in self.points() if not (math.isnan(p[0]) or math.isnan(p[1])))

    def bbox(self) -> tuple[float, float, float, float] | None:
        pts = [p for p in self.points() if not (math.isnan(p[0]) or math.isnan(p[1]))]
        if not pts:
            return None
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        return (min(xs), min(ys), max(xs), max(ys))

    def is_valid_basic(self) -> bool:
        """Finite coordinates, lines of two or more points, closed rings of four or more."""
        if self.is_empty:
            return True
        if any(math.isinf(x) or math.isinf(y) for x, y in self.points()):
            return False

        def check(t: str, part) -> bool:
            if t == "LineString":
                return len(part) >= 2
            if t == "Polygon":
                return all(len(r) >= 4 and r[0] == r[-1] for r in part)
            return True

        if self.type in ("Point", "LineString", "Polygon"):
            return check(self.type, self.parts)
        if self.type in ("MultiLineString", "MultiPolygon"):
            single = self.type[5:]
            return all(check(single, p) for p in self.parts)
        if self.type == "GeometryCollection":
            return all(g.is_valid_basic() for g in self.parts)
        return True


def _read_wkb(buf: bytes, off: int) -> tuple[Geometry, int]:
    if off + 5 > len(buf):
        raise GeometryError("truncated WKB")
    endian = "<" if buf[off] == 1 else ">"
    (code,) = struct.unpack_from(endian + "I", buf, off + 1)
    off += 5
    srid = None
    has_z = bool(code & 0x80000000)
    has_m = bool(code & 0x40000000)
    if code & 0x20000000:  # EWKB SRID
        (srid,) = struct.unpack_from(endian + "I", buf, off)
        off += 4
    code &= 0x0FFFFFFF
    if code >= 3000:
        has_z = has_m = True
        code -= 3000
    elif code >= 2000:
        has_m = True
        code -= 2000
    elif code >= 1000:
        has_z = True
        code -= 1000
    if code not in _TYPES:
        raise GeometryError(f"unknown WKB type {code}")
    dims = 2 + has_z + has_m
    pt = struct.Struct(endian + "d" * dims)

    def point(o):
        if o + pt.size > len(buf):
            raise GeometryError("truncated WKB")
        v = pt.unpack_from(buf, o)
        return (v[0], v[1]), o + pt.size

    def count(o):
        if o + 4 > len(buf):
            raise GeometryError("truncated WKB")
        return struct.unpack_from(endian + "I", buf, o)[0], o + 4

    def line(o):
        n, o = count(o)
        if n > (len(buf) - o) // pt.size + 1:
            raise GeometryError("truncated WKB")
        out = []
        for _ in range(n):
            p, o = point(o)
            out.append(p)
        return out, o

    def polygon(o):
        n, o = count(o)
        rings = []
        for _ in range(n):
            r, o = line(o)
            rings.append(r)
        return rings, o

    t = _TYPES[code]
    if t == "Point":
        p, off = point(off)
        return Geometry(t, [p], srid), off
    if t == "LineString":
        pts, off = line(off)
        return Geometry(t, pts, srid), off
    if t == "Polygon":
        rings, off = polygon(off)
        return Geometry(t, rings, srid), off
    n, off = count(off)
    members = []
    for _ in range(n):
        g, off = _read_wkb(buf, off)
        members.append(g if t == "GeometryCollection" else (g.parts[0] if g.type == "Point" else g.parts))
    return Geometry(t, members, srid), off


def decode_geometry(blob: bytes | memoryview | None, fmt: str = "auto") -> Geometry | None:
    """Decode GeoPackage binary, EWKB or WKB. Returns None for NULL."""
    if blob is None:
        return None
    buf = bytes(blob)
    if fmt in ("gpkg", "auto") and buf[:2] == b"GP":
        if len(buf) < 8:
            raise GeometryError("truncated GeoPackage header")
        flags = buf[3]
        env = _ENVELOPE_BYTES.get((flags >> 1) & 0x07)
        if env is None:
            raise GeometryError("bad GeoPackage envelope code")
        endian = "<" if flags & 1 else ">"
        (srid,) = struct.unpack_from(endian + "i", buf, 4)
        g, _ = _read_wkb(buf, 8 + env)
        g.srid = srid
        return g
    g, _ = _read_wkb(buf, 0)
    return g


# ---------------------------------------------------------------------------
# Extracts
# ---------------------------------------------------------------------------


def readonly_uri(path: str, windows: bool | None = None) -> str:
    """A SQLite URI that opens ``path`` read-only.

    Handles Windows drive letters, UNC shares and verbatim (``\\\\?\\``) paths,
    and percent-encodes characters such as ``?``, ``#`` and spaces.
    """
    if windows is None:
        windows = os.name == "nt"
    if windows:
        if path.startswith("\\\\?\\UNC\\"):
            path = "\\\\" + path[8:]
        elif path.startswith("\\\\?\\"):
            path = path[4:]
        uri = pathlib.PureWindowsPath(path).as_uri()
    else:
        uri = pathlib.PurePosixPath(posixpath.abspath(path)).as_uri()
    return uri + "?mode=ro"


class Row:
    __slots__ = ("_values", "_index", "_extract")

    def __init__(self, values, index, extract):
        self._values = values
        self._index = index
        self._extract = extract

    def __getitem__(self, column: str) -> Any:
        return self._values[self._index[column]]

    def get(self, column: str, default: Any = None) -> Any:
        i = self._index.get(column)
        return default if i is None else self._values[i]

    def geometry(self) -> Geometry | None:
        col = self._extract.geometry_column
        if not col:
            return None
        return decode_geometry(self.get(col), self._extract.geometry_format or "auto")

    def as_dict(self) -> dict[str, Any]:
        return {c: self._values[i] for c, i in self._index.items()}


class Extract:
    """A dataset the gateway extracted for this run. Read-only."""

    def __init__(self, path: str):
        self.path = path
        self._db = sqlite3.connect(readonly_uri(path), uri=True)
        self.meta = dict(self._db.execute("SELECT key, value FROM columbus_extract"))
        self.columns = [r[1] for r in self._db.execute('PRAGMA table_info("data")')]
        self._index = {c: i for i, c in enumerate(self.columns)}

    @property
    def row_count(self) -> int:
        return int(self.meta.get("rows") or self._db.execute('SELECT count(*) FROM "data"').fetchone()[0])

    @property
    def geometry_column(self) -> str | None:
        return self.meta.get("geometry_column") or None

    @property
    def geometry_format(self) -> str | None:
        return self.meta.get("geometry_format") or None

    @property
    def srid(self) -> int | None:
        s = self.meta.get("srid")
        return int(s) if s not in (None, "") else None

    @property
    def labels(self) -> list[str]:
        return json.loads(self.meta.get("labels") or "[]")

    def rows(self) -> Iterator[Row]:
        for values in self._db.execute('SELECT * FROM "data"'):
            yield Row(values, self._index, self)

    def close(self) -> None:
        self._db.close()


# ---------------------------------------------------------------------------
# Output writers
# ---------------------------------------------------------------------------


def write_csv(path: str, header: list[str], rows: Iterable[Iterable[Any]]) -> int:
    """Write a CSV (RFC 4180). Returns the number of data rows written."""
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    n = 0
    with open(path, "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(header)
        for r in rows:
            w.writerow(list(r))
            n += 1
    return n


def write_geojson(path: str, features: Iterable[dict]) -> int:
    """Stream a GeoJSON FeatureCollection. Returns the number of features."""
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    n = 0
    with open(path, "w", encoding="utf-8") as f:
        f.write('{"type":"FeatureCollection","features":[')
        for feat in features:
            if n:
                f.write(",")
            f.write(json.dumps(feat, separators=(",", ":"), default=str))
            n += 1
        f.write("]}")
    return n


# ---------------------------------------------------------------------------
# Runs
# ---------------------------------------------------------------------------


class Context:
    """Everything a tool sees for one run."""

    def __init__(self, plugin: "Plugin", request: dict[str, Any]):
        self._plugin = plugin
        self.run_id: str = request.get("run") or ""
        self.tool: str = request.get("tool") or ""
        self.params: dict[str, Any] = request.get("params") or {}
        self.inputs: dict[str, Any] = request.get("inputs") or {}
        self.output_specs: dict[str, Any] = request.get("outputs") or {}
        self.workspace: str | None = request.get("workspace")
        self.timeout_s: float = float(request.get("timeout_s") or 0) or math.inf
        self.warnings: list[dict[str, Any]] = []
        self.outputs: dict[str, dict[str, Any]] = {}
        self._cancel = threading.Event()
        self._extracts: list[Extract] = []

    # parameters -----------------------------------------------------------
    def param(self, name: str, default: Any = None) -> Any:
        return self.params.get(name, default)

    def dataset(self, name: str) -> Extract:
        spec = self.inputs.get(name)
        if not isinstance(spec, dict) or spec.get("kind") != "dataset":
            raise ToolError(f"input {name!r} is not a dataset", INVALID_PARAMS)
        ex = Extract(spec["path"])
        self._extracts.append(ex)
        return ex

    def input_file(self, name: str) -> str:
        spec = self.inputs.get(name)
        if not isinstance(spec, dict) or spec.get("kind") != "file":
            raise ToolError(f"input {name!r} is not a file", INVALID_PARAMS)
        return spec["path"]

    def output_path(self, name: str) -> str:
        spec = self.output_specs.get(name)
        if not isinstance(spec, dict) or not spec.get("path"):
            raise ToolError(f"no output named {name!r}", INVALID_PARAMS)
        return spec["path"]

    def set_output(self, name: str, **counts: Any) -> None:
        self.outputs.setdefault(name, {}).update(counts)

    # reporting ------------------------------------------------------------
    def progress(self, percent: float | None = None, message: str = "") -> None:
        """Report progress. Raises Cancelled if the run was cancelled."""
        if self._cancel.is_set():
            raise Cancelled()
        self._plugin._notify("progress", {"run": self.run_id, "percent": percent, "message": message})

    @property
    def cancelled(self) -> bool:
        return self._cancel.is_set()

    def check_cancelled(self) -> None:
        if self._cancel.is_set():
            raise Cancelled()

    def warn(self, id: str, severity: str, message: str, detail: Any = None, fix: str | None = None) -> None:
        w: dict[str, Any] = {"id": id, "severity": severity, "message": message}
        if detail is not None:
            w["detail"] = detail
        if fix:
            w["fix"] = fix
        self.warnings.append(w)

    def log(self, message: str, level: str = "info") -> None:
        self._plugin._notify("log", {"level": level, "message": message})

    def _close(self) -> None:
        for e in self._extracts:
            try:
                e.close()
            except Exception:  # noqa: BLE001
                pass


@dataclass
class _Tool:
    name: str
    params: list[str]
    outputs: list[str]
    fn: Callable[[Context], Any]


class Plugin:
    def __init__(self, id: str, version: str):
        self.id = id
        self.version = version
        self._tools: dict[str, _Tool] = {}
        self._out = threading.Lock()
        self._active: Context | None = None
        self._active_lock = threading.Lock()
        self._stdout = sys.stdout
        self._threads: list[threading.Thread] = []

    def tool(self, name: str, params: list[str] | None = None, outputs: list[str] | None = None):
        def register(fn: Callable[[Context], Any]):
            self._tools[name] = _Tool(name, list(params or []), list(outputs or []), fn)
            return fn

        return register

    # protocol -------------------------------------------------------------
    def _send(self, msg: dict[str, Any]) -> None:
        line = json.dumps(msg, separators=(",", ":"), default=str)
        with self._out:
            self._stdout.write(line + "\n")
            self._stdout.flush()

    def _notify(self, method: str, params: dict[str, Any]) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def _reply(self, rid: Any, result: Any = None, error: tuple[int, str] | None = None) -> None:
        if error:
            self._send({"jsonrpc": "2.0", "id": rid, "error": {"code": error[0], "message": error[1]}})
        else:
            self._send({"jsonrpc": "2.0", "id": rid, "result": result})

    def describe(self) -> dict[str, Any]:
        return {
            "protocol": PROTOCOL_VERSION,
            "plugin": {"id": self.id, "version": self.version},
            "tools": [{"name": t.name, "params": t.params, "outputs": t.outputs} for t in self._tools.values()],
        }

    def _execute(self, rid: Any, ctx: Context) -> None:
        result: Any = None
        error: tuple[int, str] | None = None
        try:
            t = self._tools.get(ctx.tool)
            if t is None:
                raise ToolError(f"no tool named {ctx.tool!r}", INVALID_PARAMS)
            summary = t.fn(ctx)
            result = {"outputs": ctx.outputs, "warnings": ctx.warnings, "summary": summary if summary is not None else {}}
        except ToolError as e:
            error = (e.code, e.message)
        except Exception as e:  # noqa: BLE001 - report, never crash the loop
            traceback.print_exc(file=sys.stderr)
            error = (ENGINE_ERROR, f"{type(e).__name__}: {e}")
        finally:
            ctx._close()
        # Free the slot and decide cancel-or-finish together, before
        # replying: the gateway may send its next run the moment the reply
        # arrives, and must not find this one still marked active.
        with self._active_lock:
            if self._active is ctx:
                self._active = None
            if ctx._cancel.is_set():
                error = (CANCELLED, "cancelled")
        self._reply(rid, result, error)

    def handle(self, line: str, spawn: bool = True) -> None:
        try:
            req = json.loads(line)
            if not isinstance(req, dict):
                raise ValueError
        except ValueError:
            self._reply(None, error=(PARSE_ERROR, "parse error"))
            return
        rid = req.get("id")
        method = req.get("method")
        params = req.get("params") or {}
        if method == "describe":
            self._reply(rid, self.describe())
        elif method == "cancel":
            with self._active_lock:
                if self._active is not None and self._active.run_id == params.get("run"):
                    self._active._cancel.set()
            self._reply(rid, {})
        elif method == "run":
            with self._active_lock:
                if self._active is not None:
                    self._reply(rid, error=(ENGINE_ERROR, "busy: a run is already active"))
                    return
                ctx = Context(self, params)
                self._active = ctx
            if spawn:
                th = threading.Thread(target=self._execute, args=(rid, ctx), daemon=True)
                self._threads.append(th)
                th.start()
            else:
                self._execute(rid, ctx)
        else:
            self._reply(rid, error=(UNKNOWN_METHOD, "unknown method"))

    def serve(self) -> None:
        """Read requests from stdin until EOF. Runs execute on a separate
        thread so that ``cancel`` can be read while a tool works."""
        lines: "queue.Queue[str | None]" = queue.Queue()
        self._threads = []

        def reader():
            for line in sys.stdin:
                if line.strip():
                    lines.put(line)
            lines.put(None)

        threading.Thread(target=reader, daemon=True).start()
        while True:
            line = lines.get()
            if line is None:
                break
            self.handle(line)
        # EOF: let an active run finish its reply, briefly.
        for th in self._threads:
            th.join(timeout=10)
