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
DYNAMIC = {"importlib", "runpy", "code", "codeop", "pickle", "marshal", "shelve", "dill", "cloudpickle", "imp", "zipimport"}
CREDENTIALS = {"getpass", "keyring", "netrc", "secrets"}
REFUSED_MODULES = {
    **{m: "network access" for m in NETWORK},
    **{m: "starting other programs" for m in PROCESSES},
    **{m: "native code" for m in NATIVE},
    **{m: "loading code dynamically" for m in DYNAMIC},
    **{m: "credentials" for m in CREDENTIALS},
}
# The SDK is added by Columbus; a tool may take its helpers, not its server.
SDK = "columbus_plugin"
SDK_HELPERS = {"ToolError", "Severity", "write_csv", "write_geojson", "decode_geometry"}

# Most blank lines in a row: more could push code out of sight of a reader.
MAX_BLANK_RUN = 5

REFUSED_CALLS = {
    "globals": "changing the module's names behind the reader's back",
    "locals": "changing names behind the reader's back",
    "vars": "changing names behind the reader's back",
    "setattr": "changing names behind the reader's back",
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
        self.ctx_names: set[str] = set()
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
            full = f"{mod}.{a.name}" if mod else a.name
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
        if full in ("os.environ", "os.environb"):
            self.err(node, "tool.environment", "os.environ: a tool may not read environment variables (no credentials, no settings); take what it needs as parameters")
        if full in ("sys.path", "sys.modules", "sys.meta_path"):
            self.warn(node, "tool.import_path", f"{full}: a tool should not change how Python imports")
        self.generic_visit(node)

    def visit_Name(self, node: ast.Name) -> None:
        if node.id == "__file__":
            self.warn(node, "tool.file", "__file__: a tool should not read its own folder")
        target = self.aliases.get(node.id)
        if target in ("os.environ", "os.environb"):
            self.err(node, "tool.environment", "environ: a tool may not read environment variables")
        self.generic_visit(node)

    def visit_Call(self, node: ast.Call) -> None:
        f = node.func
        name = self._resolve(f)
        if isinstance(f, ast.Name) and f.id in REFUSED_CALLS and f.id not in self.aliases:
            self.err(node, "tool.call", f"{f.id}(): a tool may not use {REFUSED_CALLS[f.id]}")
        if name.startswith("os.") and name[3:] in OS_CALLS:
            rule = "tool.environment" if name[3:] in ("getenv", "getenvb", "putenv", "unsetenv") else "tool.process"
            self.err(node, rule, f"{name}(): a tool may not read the environment, start programs or change the file system outside its outputs")
        if name in ("shutil.rmtree", "os.remove", "os.unlink", "os.rmdir", "shutil.move", "os.rename", "os.replace"):
            self.warn(node, "tool.file", f"{name}(): a tool should only write its outputs, not move or delete files")
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
        self.warn(node, "tool.state", "global: a tool should keep nothing between runs, so every run gives the same answer for the same inputs")
        self.generic_visit(node)


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
    run = None
    # A reader sees the first definition; Python runs the last. Every name a
    # function or class defines at the top level is bound exactly once.
    defined: dict[str, int] = {}
    bound: dict[str, int] = {}
    for node in tree.body:
        for name in _bound_names(node):
            is_def = isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
            if (is_def and name in bound) or name in defined:
                lint.err(node, "tool.redefined", f"{name} is defined more than once at the top level (line {defined.get(name) or bound.get(name)} and line {node.lineno}); a reader sees one, Python runs the last")
            if is_def:
                defined.setdefault(name, node.lineno)
            bound.setdefault(name, node.lineno)
    # `run` defined or rebound in top-level code (inside an if, a try, a
    # loop), or changed from inside a function, is refused too.
    for top in tree.body:
        top_def = isinstance(top, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
        for node in ast.walk(top):
            if isinstance(node, (ast.Global, ast.Nonlocal)) and "run" in node.names:
                lint.err(node, "tool.redefined", "run is changed from inside a function; define it once, plainly")
            if top_def or node is top:
                continue
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and node.name == "run":
                lint.err(node, "tool.redefined", "run is defined inside a block; define it once, plainly, at the top level")
            if isinstance(node, ast.Name) and node.id == "run" and isinstance(node.ctx, (ast.Store, ast.Del)):
                lint.err(node, "tool.redefined", "run is rebound at the top level; define it once, plainly")
    for i, node in enumerate(tree.body):
        if isinstance(node, (ast.Import, ast.ImportFrom, ast.ClassDef)):
            continue
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            if node.name == "run":
                run = node
            if node.name in ("_plugin", "_ColumbusPlugin"):
                lint.err(node, "tool.sdk", f"{node.name}: this name is Columbus's")
            continue
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str) and i == 0:
            continue  # the module's docstring
        if isinstance(node, (ast.Assign, ast.AnnAssign)):
            value = node.value
            simple = value is None or isinstance(value, (ast.Constant, ast.Tuple, ast.List, ast.Dict, ast.Set, ast.Name, ast.Attribute, ast.UnaryOp, ast.BinOp))
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            if any(isinstance(t, ast.Name) and t.id in ("_plugin", "_ColumbusPlugin", "run") for t in targets):
                lint.err(node, "tool.sdk", "this name is Columbus's (or the tool's run function)")
            if simple:
                continue
        if isinstance(node, ast.If) and _dotted(node.test.left if isinstance(node.test, ast.Compare) else node.test) == "__name__":
            lint.err(node, "tool.main", "if __name__ == '__main__': Columbus adds the part that starts the tool; leave it out")
            continue
        lint.warn(node, "tool.import_time", "code at the top level runs every time Columbus starts the tool; put the work in run(ctx)")
    if run is None:
        lint.errors.append({"line": 0, "rule": "tool.run", "message": "the code must define run(ctx): Columbus calls it with the run's context"})
        return
    if isinstance(run, ast.AsyncFunctionDef):
        lint.err(run, "tool.run", "run(ctx) must be a plain function, not async")
    args = run.args
    if len(args.args) != 1 or args.vararg or args.kwarg or args.kwonlyargs:
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
    except (tokenize.TokenError, SyntaxError, IndentationError):
        pass  # the parser reports it
    blank = 0
    for n, line in enumerate(code.split("\n"), 1):
        if line.strip():
            blank = 0
            continue
        blank += 1
        if blank == MAX_BLANK_RUN + 1:
            lint.errors.append({"line": n, "rule": "tool.blank_lines",
                                "message": f"more than {MAX_BLANK_RUN} blank lines in a row; keep the code together so a reader sees all of it"})


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
