"""Checks on the code of a personal tool an assistant proposes (propose_tool).

The code is only parsed (``ast``), never run. The sandbox is what keeps a
tool from the network and from files outside its own folder; these checks
refuse code that plainly tries anyway, so the person is never asked to
install it, and tell the assistant what to change:

* no network, subprocesses, native code or dynamic imports;
* no credentials or environment variables;
* no absolute paths, URLs or GDAL virtual file systems: data comes only
  through the tool's declared inputs, and goes only to its outputs;
* a ``run(ctx)`` function, and nothing done at import time.

It also reports which inputs, parameters, outputs and checks the code
names (``ctx.input_file("routes")``, ``ctx.param("tolerance")``,
``ctx.output_path("located")``, ``ctx.warn("lrs.off_route", ...)``), so the
gateway can compare them with the manifest.

Standard library only.
"""

from __future__ import annotations

import ast
import io
import re
import tokenize
from typing import Any

# Modules a tool has no business importing.
NETWORK = {
    "socket", "ssl", "http", "urllib", "urllib3", "requests", "httpx", "aiohttp", "ftplib", "smtplib",
    "poplib", "imaplib", "nntplib", "telnetlib", "xmlrpc", "websocket", "websockets", "paramiko",
    "asyncio", "selectors", "socketserver", "webbrowser", "smtpd", "mailbox", "boto3", "botocore",
    "google", "azure", "fsspec", "s3fs", "gcsfs",
}
PROCESSES = {"subprocess", "multiprocessing", "pty", "pexpect"}
NATIVE = {"ctypes", "cffi", "mmap", "_ctypes", "winreg", "_winapi", "msvcrt", "resource", "fcntl"}
DYNAMIC = {"importlib", "runpy", "code", "codeop", "pickle", "marshal", "shelve", "dill", "cloudpickle", "imp", "zipimport",
           # These run text as code, or look code up by name.
           "timeit", "profile", "cProfile", "pdb", "bdb", "trace", "doctest", "pydoc", "pkgutil", "logging.config"}
CREDENTIALS = {"getpass", "keyring", "netrc", "secrets"}
INTROSPECTION = {"__main__", "builtins", "gc", "inspect", "types", "ctypes", "sys.modules", "atexit", "threading", "_thread",
                 "faulthandler", "tracemalloc", "dis", "opcode", "codecs", "encodings", "site", "sitecustomize"}
REFUSED_MODULES = {
    **{m: "network access" for m in NETWORK},
    **{m: "starting other programs" for m in PROCESSES},
    **{m: "native code" for m in NATIVE},
    **{m: "loading code dynamically" for m in DYNAMIC},
    **{m: "credentials" for m in CREDENTIALS},
    **{m: "reaching into Python's own machinery" for m in INTROSPECTION},
}
# The only modules a tool may import (and their sub-modules, unless refused
# above). Everything else is refused: a list of what is allowed cannot leak
# the way a list of what is refused does.
ALLOWED_MODULES = {
    # Python's own: numbers, text, dates, data structures.
    "math", "cmath", "statistics", "decimal", "fractions", "numbers", "random", "re", "json", "csv",
    "datetime", "time", "calendar", "zoneinfo", "itertools", "functools", "operator", "collections",
    "heapq", "bisect", "array", "dataclasses", "enum", "typing", "abc", "contextlib", "copy", "string",
    "textwrap", "unicodedata", "uuid", "hashlib", "base64", "struct", "pathlib", "logging", "warnings",
    "zipfile",
    "__future__",
    # GIS and data.
    "geopandas", "shapely", "pyproj", "pandas", "numpy", "pyogrio", "fiona", "rasterio", "scipy", "networkx",
}
# Attribute names that lead to Python's machinery or the operating system,
# refused on any object (shutil.os.system, gzip.builtins.exec).
ESCAPE_ATTRS = {"os", "_os", "sys", "_sys", "posix", "nt", "builtins", "bltns", "subprocess", "importlib",
                "ctypes", "inspect", "types", "pickle", "marshal", "gc", "socket", "runpy", "six", "imp",
                "io", "shutil", "tempfile", "platform", "signal", "threading", "multiprocessing",
                "ctypeslib", "f2py", "distutils", "clipboard", "clipboards"}
# Parts of allowed packages that load native code, start programs, read
# from outside the tool's inputs or import modules named in text.
REFUSED_SUBMODULES = {"numpy.ctypeslib", "numpy.f2py", "numpy.distutils", "numpy.testing", "pandas.io.clipboard",
                      "pandas.io.clipboards", "pandas.util", "pandas.testing", "logging.handlers", "logging.config",
                      "scipy.weave", "networkx.utils.backends"}
# Methods refused by name on any object: the clipboard (other programs,
# outside the inputs) and settings that make libraries load code.
REFUSED_METHODS = {"read_clipboard", "to_clipboard", "set_option", "option_context", "set_gdal_config_options",
                   "set_config_option"}
# Calls that read pickled data (which runs code as it loads).
PICKLE_CALLS = {"read_pickle", "to_pickle"}

# The SDK is added by Columbus; a tool may take its helpers, not its server.
SDK = "columbus_plugin"
SDK_HELPERS = {"ToolError", "Severity", "write_csv", "write_geojson", "decode_geometry"}

# Most blank lines in a row, comment-only lines in a row and lines in all:
# more could push code out of sight of a reader.
MAX_BLANK_RUN = 5
MAX_COMMENT_RUN = 40
MAX_LINES = 2000

# Special names a tool may use; every other __x__ is refused, as names,
# attributes and text (getattr(f, "__code__")).
DUNDERS_OK = {"__init__", "__name__", "__doc__", "__file__", "__len__", "__iter__", "__next__", "__enter__",
              "__exit__", "__repr__", "__str__", "__eq__", "__hash__", "__lt__", "__le__", "__gt__", "__ge__",
              "__contains__", "__getitem__", "__post_init__", "__all__", "__slots__", "__future__"}
NAMES_OK = {"__name__", "__doc__", "__file__", "__all__", "__slots__"}
FRAME_ATTRS = ("f_globals", "f_locals", "f_back", "f_builtins", "f_code", "tb_frame", "gi_frame", "cr_frame", "ag_frame",
               "gi_code", "cr_code")
REFUSED_SYS = {"sys.modules", "sys._getframe", "sys.settrace", "sys.setprofile", "sys.meta_path", "sys.path_hooks",
               "sys.addaudithook", "sys.excepthook", "sys.path", "sys.stdout", "sys.stderr"}


# Ways to reach attributes or code by a name written as text (stand-ins for
# getattr), refused wherever they are named.
REFUSED_NAMES = {"operator.attrgetter", "operator.methodcaller", "string.Formatter", "logging.config",
                 "pandas.eval", "typing.ForwardRef", "typing.get_type_hints", "functools.singledispatch",
                 "pandas.options", "pandas.set_option", "pandas.option_context", "pandas.read_clipboard"}
# GDAL settings can change where GDAL loads plugins and data from: rasterio
# and fiona's Env take no settings in a tool.
ENV_CALLS = {"rasterio.Env", "rasterio.env.Env", "fiona.Env", "fiona.env.Env"}
# Modules whose objects may only be used as `module.thing`, never passed
# around under another name (o = os; o.system(...)).
DIRECT_ONLY = {"os", "sys", "shutil", "operator", "string", "io", "logging", "functools", "pathlib", "tempfile"}
# A special name anywhere inside a text ("{0.__globals__}").
DUNDER_IN_TEXT = re.compile(r"__[a-z][a-z0-9_]*?__")


def _dunder(name: str) -> bool:
    return len(name) > 4 and name.startswith("__") and name.endswith("__")


REFUSED_CALLS = {
    "globals": "changing the module's names behind the reader's back",
    "locals": "changing names behind the reader's back",
    "vars": "changing names behind the reader's back",
    "setattr": "changing names behind the reader's back",
    "getattr": "reaching attributes by computed name",
    "hasattr": "reaching attributes by computed name",
    "delattr": "changing names behind the reader's back",
    "eval": "running text as code",
    "exec": "running text as code",
    "compile": "running text as code",
    "__import__": "loading code dynamically",
    "breakpoint": "the debugger",
    "input": "waiting for input that never comes",
}
OS_CALLS = {
    "system", "popen", "startfile", "fork", "forkpty", "kill", "killpg", "putenv", "unsetenv", "getenv",
    "getenvb", "execl", "execle", "execlp", "execlpe", "execv", "execve", "execvp", "execvpe", "spawnl",
    "spawnle", "spawnlp", "spawnlpe", "spawnv", "spawnve", "spawnvp", "spawnvpe", "posix_spawn",
    "posix_spawnp", "chdir", "chroot", "symlink", "link", "setuid", "setgid",
}
OS_ATTRS = {"environ", "environb"}

ABSOLUTE = re.compile(r"^(?:/(?:[A-Za-z0-9_.~-]+)|[A-Za-z]:[\\/]|\\\\[^\\]|~[/\\])")
URL = re.compile(r"\b(?:https?|ftps?|s3|gs|gcs|az|abfss?|wasbs?|file|sftp|ssh)://", re.I)
VSI = re.compile(r"/vsi[a-z0-9_]+/", re.I)
SECRET_VALUE = re.compile(r"(?:AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY|ghp_[A-Za-z0-9]{20,}|xox[bpa]-[A-Za-z0-9-]{10,}|sk-[A-Za-z0-9]{20,})")
SECRET_NAME = re.compile(r"(?:passw(?:or)?d|pwd|secret|api_?key|access_?key|token|credential|auth)", re.I)
CHECK_ID = re.compile(r"^[a-z][a-z0-9_]*(?:\.[a-z0-9_]+)+$")


def _root(name: str) -> str:
    return name.split(".")[0]


def _const_str(node: ast.AST) -> str | None:
    return node.value if isinstance(node, ast.Constant) and isinstance(node.value, str) else None


def _dotted(node: ast.AST) -> str:
    parts = []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
    return ".".join(reversed(parts))


class _Lint(ast.NodeVisitor):
    def __init__(self) -> None:
        self.errors: list[dict[str, Any]] = []
        self.warnings: list[dict[str, Any]] = []
        self.uses: dict[str, set[str]] = {"inputs": set(), "params": set(), "outputs": set(), "checks": set()}
        self.aliases: dict[str, str] = {}  # local name -> module it stands for
        # Names bound by imports at the top level (the only names that may
        # shadow a refused builtin), and each node's parent.
        self.top_imports: set[str] = set()
        self.parents: dict[int, ast.AST] = {}
        self.ctx_names: set[str] = set()
        # Names the file binds at the top level other than by importing.
        self.local_names: set[str] = set()
        # Docstrings may mention a URL or path (as text, not as data).
        self.docstrings: set[int] = set()

    def err(self, node: ast.AST, rule: str, message: str) -> None:
        self.errors.append({"line": getattr(node, "lineno", 0), "rule": rule, "message": message})

    def warn(self, node: ast.AST, rule: str, message: str) -> None:
        self.warnings.append({"line": getattr(node, "lineno", 0), "rule": rule, "message": message})

    # imports ------------------------------------------------------------
    def _module(self, node: ast.AST, name: str) -> None:
        root = _root(name)
        why = REFUSED_MODULES.get(name) or REFUSED_MODULES.get(root)
        if any(name == r or name.startswith(r + ".") for r in REFUSED_SUBMODULES):
            self.err(node, "tool.import", f"import of {name}: this part of {root} loads native code, starts programs or loads modules named in text")
        if root != SDK and root not in ALLOWED_MODULES and not why:
            self.err(node, "tool.import", f"import of {name}: a tool may use only these modules: {', '.join(sorted(m for m in ALLOWED_MODULES if m != '__future__'))}")
        if root == SDK:
            if not isinstance(node, ast.ImportFrom) or any(a.name not in SDK_HELPERS for a in node.names):
                self.err(node, "tool.sdk", f"from columbus_plugin import only {', '.join(sorted(SDK_HELPERS))}: Columbus adds the rest")
        elif why:
            self.err(node, "tool.import", f"import of {name}: a tool may not use {why}; it reads only its inputs and writes only its outputs")

    def visit_Import(self, node: ast.Import) -> None:
        for a in node.names:
            self._module(node, a.name)
            self.aliases[(a.asname or a.name).split(".")[0]] = a.name if a.asname else _root(a.name)
        self.generic_visit(node)

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        if node.level:
            self.err(node, "tool.import", "relative imports: a tool is one file")
        mod = node.module or ""
        self._module(node, mod)
        for a in node.names:
            if a.name == "*":
                # It binds names the reader cannot see, `run` among them.
                self.err(node, "tool.import", f"from {mod or '.'} import *: name each thing you import, so the reader sees every name the tool uses")
                continue
            full = f"{mod}.{a.name}" if mod else a.name
            if full in REFUSED_MODULES or full in REFUSED_NAMES:
                self.err(node, "tool.import", f"from {mod} import {a.name}: a tool may not use {REFUSED_MODULES.get(full, 'ways to reach code or attributes by name')}")
            if mod == "os" and (a.name in OS_CALLS or a.name in OS_ATTRS):
                self.err(node, "tool.environment" if a.name in OS_ATTRS | {"getenv", "getenvb", "putenv", "unsetenv"} else "tool.process",
                         f"from os import {a.name}: a tool may not read the environment or start programs")
            self.aliases[a.asname or a.name] = full
        self.generic_visit(node)

    # calls and names ----------------------------------------------------
    def _resolve(self, node: ast.AST) -> str:
        d = _dotted(node)
        if not d:
            return ""
        head, _, rest = d.partition(".")
        base = self.aliases.get(head, head)
        return f"{base}.{rest}" if rest else base

    def visit_Attribute(self, node: ast.Attribute) -> None:
        full = self._resolve(node)
        if node.attr in ESCAPE_ATTRS:
            self.err(node, "tool.introspection", f".{node.attr}: a tool may not reach {node.attr} through another object; it uses only its allowed modules directly")
        if node.attr.startswith("_") and not _dunder(node.attr) \
                and not (isinstance(node.value, ast.Name) and node.value.id in ("self", "cls")):
            self.err(node, "tool.private", f".{node.attr}: a tool may use only the public parts of other objects (no names starting with _)")
        if any(full == r or full.startswith(r + ".") for r in REFUSED_SUBMODULES):
            self.err(node, "tool.import", f"{full}: this part of {_root(full)} loads native code, starts programs or loads modules named in text")
        if node.attr in REFUSED_METHODS:
            self.err(node, "tool.settings", f".{node.attr}: a tool may not use the clipboard or change a library's settings")
        if node.attr in PICKLE_CALLS:
            self.err(node, "tool.pickle", f".{node.attr}: pickled data runs code as it loads; read and write GeoPackage, CSV or JSON instead")
        if any(full == r or full.startswith(r + ".") for r in REFUSED_NAMES):
            self.err(node, "tool.introspection", f"{full}: a tool may not reach attributes or code by a name written as text")
        if full in ("os.environ", "os.environb"):
            self.err(node, "tool.environment", "os.environ: a tool may not read environment variables (no credentials, no settings); take what it needs as parameters")
        if full in REFUSED_SYS:
            self.err(node, "tool.introspection", f"{full}: a tool may not reach into Python's own machinery (it could change what runs)")
        if _dunder(node.attr) and node.attr not in DUNDERS_OK:
            self.err(node, "tool.introspection", f".{node.attr}: a tool may not use Python's special attributes (they can swap the code that runs){'; for a dataclass use dataclasses.asdict(obj)' if node.attr == '__dict__' else ''}")
        if node.attr.startswith(FRAME_ATTRS):
            self.err(node, "tool.introspection", f".{node.attr}: a tool may not reach into running frames")
        self.generic_visit(node)

    def visit_Name(self, node: ast.Name) -> None:
        if _dunder(node.id) and node.id not in NAMES_OK:
            self.err(node, "tool.introspection", f"{node.id}: a tool may not use Python's special names")
        if node.id == "__file__":
            self.warn(node, "tool.file", "__file__: a tool should not read its own folder")
        # Refused builtins, called or passed along (map(eval, ...),
        # functools.reduce(getattr, ...)), unless a top-level import gave the
        # name another meaning for the whole file (from re import compile).
        if node.id in REFUSED_CALLS and node.id not in self.top_imports:
            self.err(node, "tool.call", f"{node.id}: this is the name of a Python built-in a tool may not use ({REFUSED_CALLS[node.id]}); give anything of your own another name")
        target = self.aliases.get(node.id)
        if isinstance(node.ctx, ast.Load) and target:
            if any(target == r or target.startswith(r + ".") for r in REFUSED_NAMES):
                self.err(node, "tool.introspection", f"{node.id} ({target}): a tool may not reach attributes or code by a name written as text")
            parent = self.parents.get(id(node))
            if _root(target) in DIRECT_ONLY and target == _root(target) and not (isinstance(parent, ast.Attribute) and parent.value is node):
                self.err(node, "tool.module", f"{node.id}: use the {target} module directly (as {node.id}.something); do not pass it around or give it another name")
        if target in ("os.environ", "os.environb"):
            self.err(node, "tool.environment", "environ: a tool may not read environment variables")
        self.generic_visit(node)

    def visit_Call(self, node: ast.Call) -> None:
        f = node.func
        name = self._resolve(f)
        if name.startswith("os.") and name[3:] in OS_CALLS:
            rule = "tool.environment" if name[3:] in ("getenv", "getenvb", "putenv", "unsetenv") else "tool.process"
            self.err(node, rule, f"{name}(): a tool may not read the environment, start programs or change the file system outside its outputs")
        if name in ("shutil.rmtree", "os.remove", "os.unlink", "os.rmdir", "shutil.move", "os.rename", "os.replace"):
            self.warn(node, "tool.file", f"{name}(): a tool should only write its outputs, not move or delete files")
        # Expressions given as text (pandas query and eval) run code: only a
        # fixed text written in the tool, with no @names, is allowed.
        if isinstance(f, ast.Attribute) and f.attr in ("query", "eval"):
            expr = node.args[0] if node.args else next((k.value for k in node.keywords if k.arg == "expr"), None)
            text = _const_str(expr) if expr is not None else None
            if text is None or "@" in text or "__" in text:
                self.err(node, "tool.eval", f".{f.attr}(): only a fixed expression written in the tool, with no @names, is allowed; filter with a mask instead (gdf[gdf[field] > value])")
        if name in ENV_CALLS and (node.args or node.keywords):
            self.err(node, "tool.settings", f"{name}(...): a tool may not change GDAL's settings; use {name}() with no arguments if you need it at all")
        for k in node.keywords:
            if k.arg == "allow_pickle" and not (isinstance(k.value, ast.Constant) and k.value.value is False):
                self.err(node, "tool.pickle", "allow_pickle: pickled data runs code as it loads")
        # ctx.<method>("name") for the manifest check.
        if isinstance(f, ast.Attribute) and isinstance(f.value, ast.Name) and f.value.id in self.ctx_names:
            first = _const_str(node.args[0]) if node.args else None
            kind = {"input_file": "inputs", "param": "params", "output_path": "outputs", "set_output": "outputs", "warn": "checks"}.get(f.attr)
            if f.attr == "dataset":
                self.err(node, "tool.dataset", "ctx.dataset(): a personal tool takes workspace layers and tables; open them with ctx.input_file(name)")
            elif f.attr in ("_plugin", "_send", "_notify"):
                self.err(node, "tool.sdk", f"ctx.{f.attr}: use only ctx's documented methods")
            elif kind:
                if first is None:
                    self.warn(node, "tool.name", f"ctx.{f.attr}() with a name that is not written out: Columbus cannot check it against the manifest")
                else:
                    self.uses[kind].add(first)
                    if kind == "checks" and not CHECK_ID.match(first):
                        self.err(node, "tool.check_id", f"check id {first!r}: use dotted lower-case ids, like lrs.off_route")
        # open("/abs/path") and friends: caught as strings below.
        self.generic_visit(node)

    def visit_Constant(self, node: ast.Constant) -> None:
        v = node.value
        if isinstance(v, bytes):
            try:
                v = v.decode("utf-8", "replace")
            except Exception:  # noqa: BLE001
                v = ""
        if isinstance(v, str) and id(node) not in self.docstrings:
            bad = [m for m in DUNDER_IN_TEXT.findall(v) if m not in DUNDERS_OK]
            if bad:
                self.err(node, "tool.introspection", f"{bad[0]!r} in a text: a tool may not name Python's special attributes (format strings and getattr would reach them)")
        if isinstance(v, str) and id(node) in self.docstrings:
            if URL.search(v) or VSI.search(v):
                self.warn(node, "tool.url", "a docstring names a URL; the tool cannot reach it (no network)")
        elif isinstance(v, str):
            if URL.search(v):
                self.err(node, "tool.url", f"{v[:60]!r}: a tool may not name URLs; it has no network, and data comes only through its inputs")
            elif VSI.search(v):
                self.err(node, "tool.url", f"{v[:60]!r}: GDAL virtual file systems are refused")
            elif ABSOLUTE.match(v.strip()) and "\n" not in v and len(v) < 400:
                self.err(node, "tool.path", f"{v[:60]!r}: no absolute paths; read inputs with ctx.input_file(name) and write with ctx.output_path(name)")
            if SECRET_VALUE.search(v):
                self.err(node, "tool.credential", "this text looks like a key or token; a tool never holds credentials")
        self.generic_visit(node)

    def _assign_secret(self, targets: list[ast.AST], value: ast.AST) -> None:
        s = _const_str(value)
        if not s or len(s) < 4:
            return
        for t in targets:
            n = t.id if isinstance(t, ast.Name) else t.attr if isinstance(t, ast.Attribute) else None
            if n and SECRET_NAME.search(n):
                self.err(value, "tool.credential", f"{n} = '...': a tool never holds passwords, keys or tokens")

    def visit_Assign(self, node: ast.Assign) -> None:
        self._assign_secret(node.targets, node.value)
        self.generic_visit(node)

    def visit_AnnAssign(self, node: ast.AnnAssign) -> None:
        if node.value is not None:
            self._assign_secret([node.target], node.value)
        self.generic_visit(node)

    def visit_keyword(self, node: ast.keyword) -> None:
        if node.arg and SECRET_NAME.search(node.arg) and _const_str(node.value):
            self.err(node.value, "tool.credential", f"{node.arg}='...': a tool never holds passwords, keys or tokens")
        self.generic_visit(node)

    def visit_Global(self, node: ast.Global) -> None:
        self.err(node, "tool.state", "global: a tool may not rebind its top-level names; keep state in local variables")
        self.generic_visit(node)


def _literal(node: ast.AST | None) -> bool:
    """A value written out in full: numbers, text, True/False/None, and
    lists, tuples, sets and dicts of those (a sign in front of a number is
    fine). Nothing that runs code when the module loads."""
    if node is None:
        return True
    if isinstance(node, ast.Constant):
        return True
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd)):
        return isinstance(node.operand, ast.Constant) and isinstance(node.operand.value, (int, float, complex))
    if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        return all(_literal(e) for e in node.elts)
    if isinstance(node, ast.Dict):
        return all(k is not None and _literal(k) for k in node.keys) and all(_literal(v) for v in node.values)
    return False


def _plain_annotation(node: ast.AST | None) -> bool:
    """An annotation that only names types: `int`, `gpd.GeoDataFrame`,
    `list[str]`, `"Row"`, `int | None`."""
    if node is None:
        return True
    if isinstance(node, (ast.Name, ast.Constant)):
        return True
    if isinstance(node, ast.Attribute):
        return _plain_annotation(node.value)
    if isinstance(node, ast.Subscript):
        return _plain_annotation(node.value) and _plain_annotation(node.slice)
    if isinstance(node, ast.Tuple):
        return all(_plain_annotation(e) for e in node.elts)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        return _plain_annotation(node.left) and _plain_annotation(node.right)
    return False


# Decorators a helper (never run) may have: they change nothing but the
# function they wrap.
# Named as imported (`from functools import cache` counts as functools.cache).
DECORATORS = {"staticmethod", "classmethod", "property", "dataclasses.dataclass", "functools.cache", "functools.lru_cache"}


def _decorator_ok(node: ast.AST, lint: "_Lint") -> bool:
    if isinstance(node, ast.Call):
        return _decorator_ok(node.func, lint) and all(_literal(a) for a in node.args) and all(_literal(k.value) for k in node.keywords)
    head = _dotted(node).split(".")[0]
    # A name the file itself defines is its own code, whatever it is called.
    return head not in lint.local_names and lint._resolve(node) in DECORATORS


def _check_def(node: ast.AST, lint: "_Lint") -> None:
    """What runs when a def or class statement runs: its decorators,
    default values, annotations, base classes and keywords. Only plain
    values there, so loading the module does nothing a reader does not see."""
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        for d in node.decorator_list:
            if node.name == "run":
                lint.err(d, "tool.decorator", "run may not have a decorator; Columbus calls it as written")
            elif not _decorator_ok(d, lint):
                lint.err(d, "tool.decorator", f"decorator on {node.name}: only {', '.join(sorted(DECORATORS))} (they run when the tool loads)")
        a = node.args
        for v in [*a.defaults, *[d for d in a.kw_defaults if d is not None]]:
            if not _literal(v):
                lint.err(v, "tool.default", f"a default value of {node.name} is not a plain value; defaults run when the tool loads (use None and set it inside)")
        for arg in [*a.posonlyargs, *a.args, *a.kwonlyargs, *([a.vararg] if a.vararg else []), *([a.kwarg] if a.kwarg else [])]:
            if not _plain_annotation(arg.annotation):
                lint.err(arg, "tool.annotation", f"the annotation of {arg.arg} in {node.name} is not a plain type; annotations run when the tool loads")
        if not _plain_annotation(node.returns):
            lint.err(node, "tool.annotation", f"the return annotation of {node.name} is not a plain type")
    elif isinstance(node, ast.ClassDef):
        for d in node.decorator_list:
            if not _decorator_ok(d, lint):
                lint.err(d, "tool.decorator", f"decorator on class {node.name}: only {', '.join(sorted(DECORATORS))}")
        for base in node.bases:
            if not isinstance(base, (ast.Name, ast.Attribute)):
                lint.err(base, "tool.class", f"the base classes of {node.name} must be plain names")
        if node.keywords:
            lint.err(node, "tool.class", f"class {node.name}: no metaclass or other keywords; they run code when the tool loads")
        for i, item in enumerate(node.body):
            if i == 0 and isinstance(item, ast.Expr) and isinstance(item.value, ast.Constant) and isinstance(item.value.value, str):
                continue
            if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                _check_def(item, lint)
                continue
            if isinstance(item, ast.Assign) and all(isinstance(t, ast.Name) for t in item.targets) and _literal(item.value):
                continue
            if isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name) and _literal(item.value) and _plain_annotation(item.annotation):
                continue
            if isinstance(item, ast.AnnAssign) and isinstance(item.target, ast.Name) and _plain_annotation(item.annotation) \
                    and _dataclass_field(item.value, lint):
                continue
            if isinstance(item, ast.Pass):
                continue
            lint.err(item, "tool.class", f"class {node.name} may hold only a docstring, functions and plain values; this runs when the tool loads")


def _dataclass_field(value: ast.AST | None, lint: "_Lint") -> bool:
    """dataclasses.field(...) with plain values, and a default_factory of
    list, dict, set or tuple."""
    if not isinstance(value, ast.Call) or value.args or lint._resolve(value.func) != "dataclasses.field":
        return False
    for k in value.keywords:
        if k.arg == "default_factory":
            if not (isinstance(k.value, ast.Name) and k.value.id in ("list", "dict", "set", "tuple")):
                return False
        elif k.arg is None or not _literal(k.value):
            return False
    return True


def _bound_names(node: ast.AST) -> list[str]:
    """Names a top-level statement binds."""
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
        return [node.name]
    if isinstance(node, (ast.Import, ast.ImportFrom)):
        return [(a.asname or a.name).split(".")[0] for a in node.names]
    targets = node.targets if isinstance(node, ast.Assign) else [node.target] if isinstance(node, (ast.AnnAssign, ast.AugAssign)) else []
    out = []
    for t in targets:
        for n in ast.walk(t):
            if isinstance(n, ast.Name):
                out.append(n.id)
    return out


def _top_level(tree: ast.Module, lint: _Lint) -> None:
    """The top level is an allow-list: a docstring, imports, functions and
    classes, and names given plain values, each name bound once. Nothing else
    runs when Columbus loads the tool, so what a reader sees is what runs."""
    run = None
    bound: dict[str, int] = {}
    # Imports first, so decorators resolve to the modules they came from.
    for node in tree.body:
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            lint.visit(node)
        else:
            lint.local_names.update(_bound_names(node))
    for i, node in enumerate(tree.body):
        for name in _bound_names(node):
            if name in bound:
                lint.err(node, "tool.redefined", f"{name} is bound more than once at the top level (lines {bound[name]} and {node.lineno}); a reader sees one, Python uses the last")
            else:
                bound[name] = node.lineno
            if name in ("_plugin", "_ColumbusPlugin"):
                lint.err(node, "tool.sdk", f"{name}: this name is Columbus's")
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            continue
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            if node.name == "run":
                if isinstance(node, ast.ClassDef):
                    lint.err(node, "tool.run", "run must be a function")
                else:
                    run = node
            _check_def(node, lint)
            continue
        if i == 0 and isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
            continue  # the module's docstring
        if isinstance(node, ast.Assign) and all(isinstance(t, ast.Name) for t in node.targets) and _literal(node.value):
            if any(t.id == "run" for t in node.targets):
                lint.err(node, "tool.redefined", "run is the tool's function; do not assign to it")
            continue
        if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and node.value is not None \
                and _literal(node.value) and _plain_annotation(node.annotation):
            continue
        if isinstance(node, ast.If) and _dotted(node.test.left if isinstance(node.test, ast.Compare) else node.test) == "__name__":
            lint.err(node, "tool.main", "if __name__ == '__main__': Columbus adds the part that starts the tool; leave it out")
            continue
        lint.err(node, "tool.top_level", "only imports, functions, classes and names given plain values (numbers, text, lists and dicts of them) may be at the top level; anything else runs when the tool loads: put it in run(ctx)")
    # `run` changed from inside a function.
    for node in ast.walk(tree):
        if isinstance(node, (ast.Global, ast.Nonlocal)) and "run" in node.names:
            lint.err(node, "tool.redefined", "run is changed from inside a function; define it once, plainly")
    if run is None:
        lint.errors.append({"line": 0, "rule": "tool.run", "message": "the code must define run(ctx): Columbus calls it with the run's context"})
        return
    if isinstance(run, ast.AsyncFunctionDef):
        lint.err(run, "tool.run", "run(ctx) must be a plain function, not async")
    args = run.args
    if len(args.args) != 1 or args.vararg or args.kwarg or args.kwonlyargs or args.posonlyargs:
        lint.err(run, "tool.run", "run takes exactly one argument, ctx")
    else:
        lint.ctx_names.add(args.args[0].arg)
    # Helpers often take ctx too.
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.Lambda)):
            for a in node.args.args:
                if a.arg == "ctx":
                    lint.ctx_names.add("ctx")


def _source_checks(code: str, lint: _Lint) -> None:
    """What the parser hides: names spelled with other characters than the
    reader sees (Python folds full-width and other look-alike letters into
    ASCII, so the tree shows `run` where the text says `ｒｕｎ`), and long
    runs of blank lines that push code out of view."""
    try:
        for tok in tokenize.generate_tokens(io.StringIO(code).readline):
            if tok.type == tokenize.NAME and not tok.string.isascii():
                lint.errors.append({"line": tok.start[0], "rule": "tool.identifier",
                                    "message": f"the name {tok.string!r} uses characters other than ASCII letters, digits and _; names must be plain ASCII so they read as they run"})
    except (tokenize.TokenError, SyntaxError, IndentationError) as e:
        line = e.args[1][0] if isinstance(e, tokenize.TokenError) and len(e.args) > 1 and e.args[1] else getattr(e, "lineno", 0)
        lint.errors.append({"line": line or 0, "rule": "tool.syntax",
                            "message": f"the code cannot be read as Python tokens: {e.args[0] if e.args else e}"})
    lines = code.split("\n")
    if len(lines) > MAX_LINES:
        lint.errors.append({"line": MAX_LINES + 1, "rule": "tool.length",
                            "message": f"the code is {len(lines)} lines; a tool is at most {MAX_LINES}, so a person can read it all"})
    blank = comments = 0
    for n, line in enumerate(lines, 1):
        text = line.strip()
        blank = 0 if text else blank + 1
        comments = comments + 1 if text.startswith("#") else 0
        if blank == MAX_BLANK_RUN + 1:
            lint.errors.append({"line": n, "rule": "tool.blank_lines",
                                "message": f"more than {MAX_BLANK_RUN} blank lines in a row; keep the code together so a reader sees all of it"})
        if comments == MAX_COMMENT_RUN + 1:
            lint.errors.append({"line": n, "rule": "tool.comment_lines",
                                "message": f"more than {MAX_COMMENT_RUN} comment lines in a row; keep the code together so a reader sees all of it"})


def _imports(tree: ast.Module, lint: _Lint) -> None:
    """Every import in the file, before anything is checked, so what a name
    stands for does not depend on the order the checks visit the code. A
    name imported as two different things anywhere is refused; an import
    inside a function may not take a refused built-in's name."""
    for node in ast.walk(tree):
        for child in ast.iter_child_nodes(node):
            lint.parents[id(child)] = node
    seen: dict[str, tuple[str, int]] = {}
    top = set(id(n) for n in tree.body)
    for node in ast.walk(tree):
        if not isinstance(node, (ast.Import, ast.ImportFrom)):
            continue
        for a in node.names:
            if a.name == "*":
                continue
            if isinstance(node, ast.Import):
                name = (a.asname or a.name).split(".")[0]
                target = a.name if a.asname else _root(a.name)
            else:
                name = a.asname or a.name
                target = f"{node.module}.{a.name}" if node.module else a.name
            if name in seen and seen[name][0] != target:
                lint.err(node, "tool.import", f"{name} is imported as two different things (lines {seen[name][1]} and {node.lineno}); give each its own name")
            seen.setdefault(name, (target, node.lineno))
            lint.aliases.setdefault(name, target)
            if id(node) in top:
                lint.top_imports.add(name)
            elif name in REFUSED_CALLS:
                lint.err(node, "tool.call", f"{name}: an import inside a function may not take the name of a Python built-in a tool may not use")


def lint_tool(code: str) -> dict[str, Any]:
    """Check a tool's code. Returns errors (each refuses the tool), warnings,
    and the names the code uses from ctx."""
    if not isinstance(code, str):
        return {"errors": [{"line": 0, "rule": "tool.code", "message": "code must be text"}], "warnings": [], "uses": {}}
    try:
        tree = ast.parse(code, filename="tool.py", mode="exec")
    except SyntaxError as e:
        return {"errors": [{"line": e.lineno or 0, "rule": "tool.syntax", "message": f"syntax error: {e.msg}"}], "warnings": [], "uses": {}}
    except (ValueError, RecursionError, MemoryError) as e:
        return {"errors": [{"line": 0, "rule": "tool.syntax", "message": f"the code cannot be read: {type(e).__name__}"}], "warnings": [], "uses": {}}
    lint = _Lint()
    _source_checks(code, lint)
    _imports(tree, lint)
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and node.body:
            first = node.body[0]
            if isinstance(first, ast.Expr) and isinstance(first.value, ast.Constant) and isinstance(first.value.value, str):
                lint.docstrings.add(id(first.value))
    _top_level(tree, lint)
    try:
        lint.visit(tree)
    except RecursionError:
        lint.errors.append({"line": 0, "rule": "tool.syntax", "message": "the code is nested too deeply to check"})
    # One message per rule and line is enough.
    def uniq(items):
        seen = set()
        out = []
        for it in sorted(items, key=lambda x: (x["line"], x["rule"])):
            k = (it["line"], it["rule"], it["message"])
            if k not in seen:
                seen.add(k)
                out.append(it)
        return out[:50]

    return {
        "errors": uniq(lint.errors),
        "warnings": uniq(lint.warnings),
        "uses": {k: sorted(v) for k, v in lint.uses.items()},
    }
