"""Project databases: a local DuckDB file per project, built from governed
results and queried with SQL.

The gateway decides everything that matters for governance: which file
(always one it resolved inside the session workspace or a named project
folder), which layers may be loaded (only workspace layers Columbus's own
tools wrote, which already leave hidden fields out), the labels the file
carries, and the audit. This module enforces the rest inside the engine:

- Every connection is opened with the extensions Columbus ships with
  loaded first (the spatial extension, when it is installed beside DuckDB,
  from its own package and checked against DuckDB's signature), and then
  locked: ``enable_external_access = false`` (no other file, URL, ATTACH,
  COPY, INSTALL or LOAD), Python replacement scans off, autoloading and
  autoinstalling of extensions off, and ``lock_configuration = true`` so no
  statement can change any of that.
- Statements are parsed by DuckDB itself, on the locked connection, and
  only these kinds may run: CREATE (tables, views, indexes, sequences,
  macros, schemas, types; never secrets), INSERT, UPDATE, DELETE, MERGE,
  ALTER (COMMENT ON included), DROP, ANALYZE/VACUUM. ``query`` takes
  exactly one SELECT (DESCRIBE, SUMMARIZE, SHOW and EXPLAIN included).
  Functions that read files or run SQL given as text are refused by name
  as well, before DuckDB sees them.
- ``query`` and ``describe`` open the file read-only, so DuckDB itself
  refuses any change they might make (EXPLAIN ANALYZE runs what it
  explains, so EXPLAIN may only wrap a SELECT as well).
- Error messages keep only their first line and, unless they are about a
  name, nothing quoted in them: a conversion or key error can quote a
  stored value.
- A batch of statements runs in one transaction: all of it or none.
- Each call has a time limit (the connection is interrupted) and a memory
  limit; work beyond memory spills into the database's own ``.tmp``
  folder.
"""

from __future__ import annotations

import contextvars
import csv
import importlib.util
import math
import os
import re
import sqlite3
import threading
import time
from typing import Any

from columbus_plugin import readonly_uri

FETCH = 2000
INSERT_BATCH = 50_000
NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,62}$")

# Statement kinds `execute` runs; everything else is refused.
EXECUTE_TYPES = {"CREATE", "INSERT", "UPDATE", "DELETE", "MERGE_INTO", "ALTER", "DROP", "VACUUM"}
QUERY_TYPES = {"SELECT", "EXPLAIN"}

# Functions that reach files, URLs or other databases, run SQL given as
# text or as a serialized plan, change or read the engine's settings and
# logs, or raise their argument as an error: refused by name in every
# statement (the engine settings refuse the file and network ones too; this
# is the second line). Names come from DuckDB's own tokenizer, so strings
# (E'' escapes and $$ quotes included), comments and quoted names are read
# as DuckDB reads them.
_DENY_NAMES = re.compile(
    r"(?i)(read_\w+|\w+_scan|\w+_scan_\w+|glob|sniff_csv|parquet_\w+|st_read\w*|st_drivers|"
    r"query|query_table|getenv|load_extension|duckdb_secrets|which_secret|"
    r"current_setting|duckdb_settings|duckdb_extensions|duckdb_databases|duckdb_memory|"
    r"duckdb_temporary_files|duckdb_external_file_cache|duckdb_variables|getvariable|"
    r"enable_logging|disable_logging|truncate_duckdb_logs|duckdb_logs\w*|duckdb_log_contexts|"
    r"write_log|parse_duckdb_log_message|"
    r"json_execute_serialized_sql|json_deserialize_sql|json_serialize_sql|json_serialize_plan|"
    r"verify_serializer|disable_verify_serializer|duckdb_prepared_statements|"
    r"checkpoint|force_checkpoint|enable_profiling|disable_profiling|python_map_function|"
    # error() raises its argument as the error text: a way to carry stored
    # values out in a message.
    r"error)")
_WORD = re.compile(r"[A-Za-z_\u0080-\uffff][\w$]*")
# One token at the end of a statement, for telling that the tokenizer read
# all of it (it stops early on syntax it does not know, such as U&"…").
_LAST_TOKEN = re.compile(r"""(?sx)\s*(?:
    "(?:[^"]|"")*"
  | [eE]'(?:[^'\\]|\\.|'')*'
  | '(?:[^']|'')*'
  | \$([A-Za-z_]*)\$.*?\$\1\$
  | [A-Za-z_\u0080-\uffff][\w$]*
  | [0-9][0-9_.]*(?:[eE][+-]?[0-9]+)?
  | [^\s\w]{1,3}
)""")
_COMMENTS = re.compile(r"(?s)--[^\n]*|/\*.*?\*/")
_EXPLAIN = re.compile(r"(?is)^\s*explain\s+(analy[sz]e\s+)?(\([^)]*\)\s*)?")

# DuckDB error classes whose messages name things in the statement (a
# column, a table, a function), not stored values: kept as they are. Other
# messages (conversion, constraint, out of range, invalid input…) can quote
# a value, so anything quoted in them is removed.
_NAME_ERRORS = ("Parser Error", "Binder Error", "Catalog Error", "Syntax Error", "Permission Error")


class ProjectDbError(Exception):
    """A refusal or failure to pass on as is."""


def _duckdb():
    try:
        import duckdb

        return duckdb
    except ImportError as e:
        raise ProjectDbError(
            f"project databases need DuckDB in the engine environment (pip install --require-hashes --no-deps --only-binary :all: -r workers/python/requirements-projectdb.txt): {e}"
        ) from None


def spatial_extension() -> str | None:
    """The spatial extension file installed beside DuckDB (the
    duckdb-extension-spatial package), for this DuckDB version, or None.
    Never downloaded."""
    duckdb = _duckdb()
    try:
        spec = importlib.util.find_spec("duckdb_extension_spatial")
    except (ImportError, ValueError):
        return None
    if spec is None or not spec.origin:
        return None
    path = os.path.join(os.path.dirname(spec.origin), "extensions", f"v{duckdb.__version__}", "spatial.duckdb_extension")
    return path if os.path.isfile(path) else None


def versions() -> dict[str, Any]:
    try:
        duckdb = _duckdb()
    except ProjectDbError:
        return {"duckdb": None, "spatial": None}
    return {"duckdb": duckdb.__version__, "spatial": spatial_extension() is not None}


def _plain_path(path: Any) -> str:
    if not isinstance(path, str) or not path or path.startswith(("/vsi", "\\vsi")) or "://" in path:
        raise ProjectDbError("the project database path is missing or not a plain file path")
    # Windows verbatim paths (\\?\C:\...), as the gateway's canonical paths
    # are, in the ordinary form every library reads.
    if os.name == "nt":
        if path.startswith("\\\\?\\UNC\\"):
            path = "\\\\" + path[8:]
        elif path.startswith("\\\\?\\"):
            path = path[4:]
    return path


def _open(path: str, memory_mb: int, create: bool = False, read_only: bool = False, max_mb: int | None = None):
    """A locked connection to the project database at ``path`` (and whether
    the spatial functions are loaded). ``read_only`` for query and describe:
    DuckDB itself then refuses every change, whatever a statement does
    (EXPLAIN ANALYZE of a DELETE included). ``max_mb`` caps the disk the
    engine may spill to."""
    duckdb = _duckdb()
    path = _plain_path(path)
    if not create and not os.path.isfile(path):
        raise ProjectDbError("this project database does not exist; create it first")
    folder = os.path.dirname(path)
    temp = path + ".tmp"
    ext_dir = os.path.join(folder, ".duckdb-extensions")  # never written: installs are off
    config = {
        "autoinstall_known_extensions": "false",
        "autoload_known_extensions": "false",
        "allow_community_extensions": "false",
        "allow_unsigned_extensions": "false",
        "memory_limit": f"{max(64, int(memory_mb))}MB",
        "temp_directory": temp,
        "extension_directory": ext_dir,
        "threads": str(max(1, min(8, os.cpu_count() or 1))),
    }
    if max_mb:
        config["max_temp_directory_size"] = f"{max(64, int(max_mb))}MB"
    try:
        con = duckdb.connect(path, read_only=read_only, config=config)
    except duckdb.Error as e:
        raise ProjectDbError(f"could not open the project database: {_msg(e)}") from None
    spatial = False
    ext = spatial_extension()
    if ext is not None:
        try:
            con.execute("LOAD " + _lit(ext))
            spatial = True
        except duckdb.Error:
            spatial = False  # signature or version mismatch: work without it
    try:
        for setting in ("python_enable_replacements = false", "python_scan_all_frames = false"):
            try:
                con.execute("SET " + setting)
            except duckdb.Error:
                pass  # not in this DuckDB
        con.execute("SET enable_external_access = false")
        con.execute("SET allow_persistent_secrets = false")
        con.execute("SET lock_configuration = true")
    except duckdb.Error as e:
        con.close()
        raise ProjectDbError(f"could not lock the project database's settings, so it is not used: {_msg(e)}") from None
    return con, spatial


def _lit(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def _qi(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


# Whether this call may show stored values (the session's return_rows is
# above 0). Off: an error says only its class.
_DETAILS = contextvars.ContextVar("project_db_details", default=True)


def _set_details(params: dict[str, Any]) -> None:
    rows = params.get("return_rows")
    _DETAILS.set(not (isinstance(rows, int) and rows <= 0))


def _msg(e: Exception) -> str:
    """A DuckDB error, as safe to pass on as can be made: its first line,
    and, unless it is about a name in the statement (parser, binder,
    catalog, syntax and permission errors), everything from its first quote
    to its last cut out and every number replaced, since conversion,
    constraint, range and JSON errors print the value that failed. When the
    session may not see rows at all, only the error's class."""
    text = str(e).strip()
    if not text:
        return type(e).__name__
    first = text.splitlines()[0][:500]
    kind, sep, _rest = first.partition(":")
    if not _DETAILS.get():
        return (kind if sep else type(e).__name__) + " (details withheld: this session may not see rows)"
    if first.startswith(_NAME_ERRORS):
        return first
    # DuckDB's own apostrophes ("can't") are not quotes around a value.
    first = re.sub(r"(?<=[A-Za-z])'(?=(t|s|re|ve|ll|d)\b)", "\u2019", first)
    quotes = [i for i, c in enumerate(first) if c in "'\""]
    if quotes:
        first = first[:quotes[0]] + "\"…\"" + first[quotes[-1] + 1:]
    first = re.sub(r"(?<![A-Za-z0-9_.])[0-9][0-9.,_]*", "#", first)
    if sep and kind.startswith("Invalid Input") and not quotes:
        # Some functions raise their input as is, with no quotes.
        first = f"{kind}: (details withheld: they may hold stored values)"
    return first


def db_bytes(path: str) -> int:
    """The project database's size on disk: the file, its write-ahead log
    and the engine's spill folder."""
    path = _plain_path(path)
    total = 0
    for p in (path, path + ".wal"):
        try:
            total += os.path.getsize(p)
        except OSError:
            pass
    for dirpath, _dirs, files in os.walk(path + ".tmp"):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(dirpath, f))
            except OSError:
                pass
    return total


def used_bytes(con) -> int:
    """What the database holds: its used blocks and its write-ahead log.
    Space freed by DROP TABLE is reused, so it does not count (DELETE frees
    little, and the file never shrinks)."""
    try:
        _n, _size, block, _total, used, _free, wal, *_ = con.execute("SELECT * FROM pragma_database_size()").fetchone()
        return int(block) * int(used) + _parse_bytes(wal)
    except Exception:  # noqa: BLE001 - fall back to the file
        return 0


def _parse_bytes(text: Any) -> int:
    m = re.match(r"^\s*([0-9.]+)\s*([KMGT]?i?B|bytes?)\s*$", str(text or ""))
    if not m:
        return 0
    unit = m.group(2).upper().replace("I", "")
    mult = {"BYTE": 1, "BYTES": 1, "B": 1, "KB": 1 << 10, "MB": 1 << 20, "GB": 1 << 30, "TB": 1 << 40}.get(unit, 1)
    return int(float(m.group(1)) * mult)


def _cap_bytes(max_mb: Any) -> int | None:
    try:
        return int(float(max_mb) * 1024 * 1024) if max_mb else None
    except (TypeError, ValueError):
        return None


def _check_size(con, max_mb: Any, what: str) -> int:
    """What the database holds now; refuses ``what`` when that is over the
    cap."""
    used = used_bytes(con)
    cap = _cap_bytes(max_mb)
    if cap is not None and used > cap:
        raise ProjectDbError(
            f"the project database holds {used / 1024 / 1024:,.0f} MB, over the organization's cap of {float(max_mb):,.0f} MB "
            f"(policy project_db.max_mb), so {what} is refused. DROP TABLE what is no longer needed (DROP frees its space "
            "for reuse; DELETE frees little and the file does not shrink), or start a new database")
    return used


class _SizeGuard:
    """Interrupts the connection (which rolls the call back) once what the
    call has added on disk, the spill folder included, would take the
    database past the cap. Checked twice a second: a burst between two
    looks can pass the cap briefly before it is stopped."""

    def __init__(self, con, path: str, max_mb: Any, used: int):
        self.fired = False
        self.cap = _cap_bytes(max_mb)
        self.path = path
        self.con = con
        self.used = used
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._watch, name="project-db-size", daemon=True)

    def _watch(self):
        start = db_bytes(self.path)
        while not self.stop.wait(0.5):
            if self.used + max(0, db_bytes(self.path) - start) > self.cap:
                self.fired = True
                try:
                    self.con.interrupt()
                except Exception:  # noqa: BLE001
                    pass
                return

    def message(self) -> str:
        return (f"the call was stopped and nothing in it was kept: it would have taken the project database past the "
                f"organization's cap of {self.cap / 1024 / 1024:,.0f} MB (policy project_db.max_mb, the spill folder included). "
                "Load or build less at once, DROP TABLE what is no longer needed, or start a new database")

    def __enter__(self):
        if self.cap is not None:
            self.thread.start()
        return self

    def __exit__(self, *exc):
        self.stop.set()
        return False


class _Deadline:
    """Interrupts the connection when the time limit passes."""

    def __init__(self, con, seconds: float):
        self.fired = False
        self.seconds = seconds

        def fire():
            self.fired = True
            try:
                con.interrupt()
            except Exception:  # noqa: BLE001
                pass

        self.timer = threading.Timer(max(1.0, seconds), fire)
        self.timer.daemon = True

    def __enter__(self):
        self.timer.start()
        return self

    def __exit__(self, *exc):
        self.timer.cancel()
        return False


def _words(sql: str) -> list[tuple[str, bool]]:
    """The names and keywords of a statement in order, quoted names
    unquoted, each with whether a ( follows it (a call), as DuckDB's own
    tokenizer reads them. Refuses a statement the tokenizer does not read to
    its end, since what it skips could hide a call."""
    duckdb = _duckdb()
    try:
        toks = duckdb.tokenize(sql)
    except Exception:  # noqa: BLE001
        raise ProjectDbError("the statement could not be read for checking, so it is refused") from None
    significant = (duckdb.token_type.identifier, duckdb.token_type.keyword)
    out: list[tuple[str, bool]] = []
    for k, (start, typ) in enumerate(toks):
        if typ not in significant:
            continue
        if sql.startswith('"', start):
            j, name = start + 1, []
            while j < len(sql):
                if sql.startswith('""', j):
                    name.append('"')
                    j += 2
                    continue
                if sql[j] == '"':
                    break
                name.append(sql[j])
                j += 1
            word = "".join(name)
        else:
            m = _WORD.match(sql, start)
            if not m:
                continue
            word = m.group(0)
        nxt = toks[k + 1][0] if k + 1 < len(toks) else None
        out.append((word, nxt is not None and sql.startswith("(", nxt)))
    # Everything after the last token must be that token, comments and
    # space: otherwise the tokenizer stopped early.
    tail = sql[toks[-1][0]:] if toks else sql
    m = _LAST_TOKEN.match(tail)
    rest = _COMMENTS.sub("", tail[m.end():] if m else tail).strip()
    if rest:
        raise ProjectDbError("the statement holds syntax Columbus cannot check (for example U&\"…\" escapes), so it is refused")
    return out


def _first_words(words: list[tuple[str, bool]], n: int) -> list[str]:
    return [w.lower() for w, _ in words[:n]]


def _denied_call(words: list[tuple[str, bool]]) -> str | None:
    for w, call in words:
        if call and _DENY_NAMES.fullmatch(w):
            return w
    return None


def check_statements(con, sql: str, allowed: set[str], pivot: bool = False) -> list[dict[str, Any]]:
    """The statements in ``sql`` as DuckDB parses them, each checked:
    [{index, type, sql}]. Raises with the first refused one. With ``pivot``,
    a PIVOT that DuckDB expands into helper statements (a CREATE TYPE and a
    SELECT, with no text of their own) comes back as one SELECT."""
    duckdb = _duckdb()
    if not isinstance(sql, str) or not sql.strip():
        raise ProjectDbError("sql is required")
    if len(sql) > 1_000_000:
        raise ProjectDbError("sql is over 1,000,000 characters; split it into several calls")
    try:
        parsed = con.extract_statements(sql)
    except duckdb.Error as e:
        raise ProjectDbError(f"the SQL does not parse: {_msg(e)}") from None
    kinds = [str(st.type).split(".")[-1] for st in parsed]
    denied = ("reads files, other databases, settings or logs, runs SQL given as text, or raises stored values as errors, "
              "which a project database does not allow; load data with action load instead")
    if (pivot and len(parsed) > 1 and kinds[-1] == "SELECT" and all(k == "CREATE" for k in kinds[:-1])
            and not any(st.query.strip() for st in parsed)):
        words = _words(sql)
        if _first_words(words, 1) in (["pivot"], ["unpivot"]):
            bad = _denied_call(words)
            if bad:
                raise ProjectDbError(f"statement 1 is refused: {bad}() {denied}")
            return [{"index": 1, "type": "SELECT", "sql": sql.strip(), "pivot": True}]
    out = []
    for i, st in enumerate(parsed):
        kind = kinds[i]
        text = st.query.strip()
        if not text:
            raise ProjectDbError(f"statement {i + 1} is refused: it cannot be read on its own (a PIVOT goes to action query, alone)")
        words = _words(text)
        first = _first_words(words, 6)
        if kind == "VACUUM" and first[:1] in (["analyze"], ["analyse"]):
            kind = "ANALYZE"
        if kind == "ANALYZE" and "VACUUM" in allowed:
            pass
        elif kind not in allowed:
            what = {"ATTACH": "attaching other databases", "DETACH": "detaching databases", "COPY": "COPY to or from files",
                    "COPY_DATABASE": "copying databases", "EXPORT": "exporting the database to files", "LOAD": "installing or loading extensions",
                    "PRAGMA": "PRAGMA statements", "SET": "changing settings", "VARIABLE_SET": "changing settings",
                    "CALL": "CALL statements", "TRANSACTION": "transaction statements (each call is one transaction)",
                    "PREPARE": "prepared statements", "EXECUTE": "prepared statements", "SELECT": "a SELECT here (use action query)",
                    "EXPLAIN": "EXPLAIN here (use action query)"}.get(kind, f"{kind} statements")
            raise ProjectDbError(f"statement {i + 1} is refused: a project database does not allow {what}. "
                                 f"Allowed here: {', '.join(sorted(allowed))}")
        if kind == "CREATE" and "secret" in [w for w in first if w not in ("create", "or", "replace", "persistent", "temporary", "temp")][:1]:
            raise ProjectDbError(f"statement {i + 1} is refused: secrets are not allowed in a project database")
        bad = _denied_call(words)
        if bad:
            raise ProjectDbError(f"statement {i + 1} is refused: {bad}() {denied}")
        if kind == "EXPLAIN":
            # EXPLAIN ANALYZE runs what it explains: only a SELECT may be inside.
            inner = _EXPLAIN.sub("", text, count=1)
            try:
                inside = con.extract_statements(inner)
            except duckdb.Error:
                inside = []
            if len(inside) != 1 or str(inside[0].type).split(".")[-1] != "SELECT":
                raise ProjectDbError(f"statement {i + 1} is refused: EXPLAIN is allowed only of a SELECT")
        out.append({"index": i + 1, "type": kind, "sql": text})
    if not out:
        raise ProjectDbError("sql holds no statement")
    return out


# ---------- actions ----------


def create(params: dict[str, Any]) -> dict[str, Any]:
    _set_details(params)
    path = _plain_path(params.get("path"))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    existed = os.path.isfile(path)
    con, spatial = _open(path, int(params.get("memory_mb") or 2048), create=True)
    con.close()
    return {"created": not existed, "spatial": spatial, **versions()}


def _source_layer(path: str):
    """(kind, table, geometry column, columns [(name, declared type)], crs
    code) of a workspace layer."""
    if path.lower().endswith(".csv"):
        with open(path, encoding="utf-8", newline="") as f:
            header = next(csv.reader(f), [])
        return "csv", None, None, [(h, "TEXT") for h in header], None
    if not path.lower().endswith(".gpkg"):
        raise ProjectDbError("only GeoPackage and CSV layers can be loaded")
    with open(path, "rb") as f:
        head = f.read(100)
    if len(head) < 100 or head[:16] != b"SQLite format 3\x00" or int.from_bytes(head[68:72], "big") != 0x47504B47:
        raise ProjectDbError("the layer is not a GeoPackage")
    db = sqlite3.connect(readonly_uri(path), uri=True)
    try:
        db.execute("PRAGMA query_only = ON")
        rows = db.execute("SELECT table_name, data_type, srs_id FROM gpkg_contents WHERE data_type IN ('features', 'attributes')").fetchall()
        if len(rows) != 1:
            raise ProjectDbError("the GeoPackage must hold exactly one layer")
        table, _dtype, srs_id = rows[0]
        g = db.execute("SELECT column_name FROM gpkg_geometry_columns WHERE table_name = ?", (table,)).fetchone()
        geom = g[0] if g else None
        cols = [(r[1], (r[2] or "").upper()) for r in db.execute(f"PRAGMA table_info({_qi(table)})")]
        crs = None
        if srs_id is not None and geom:
            s = db.execute("SELECT organization, organization_coordsys_id FROM gpkg_spatial_ref_sys WHERE srs_id = ?", (srs_id,)).fetchone()
            if s and s[0] and s[0].upper() == "EPSG" and s[1]:
                crs = f"EPSG:{int(s[1])}"
        return "gpkg", table, geom, cols, crs
    finally:
        db.close()


def _duck_type(declared: str) -> str:
    d = declared.upper()
    if d in ("INTEGER", "INT", "BIGINT", "MEDIUMINT", "SMALLINT", "TINYINT", "INT64"):
        return "BIGINT"
    if d in ("REAL", "DOUBLE", "FLOAT", "NUMERIC", "DOUBLE PRECISION"):
        return "DOUBLE"
    if d == "BOOLEAN":
        return "BOOLEAN"
    if d == "BLOB":
        return "BLOB"
    return "VARCHAR"  # TEXT, DATE, DATETIME and anything else: kept as written


def _wkb(blob) -> bytes | None:
    """ISO WKB from a GeoPackage geometry blob."""
    if blob is None:
        return None
    raw = bytes(blob)
    if raw[:2] != b"GP" or len(raw) < 8:
        return None
    flags = raw[3]
    env = {0: 0, 1: 32, 2: 48, 3: 48, 4: 64}.get((flags >> 1) & 0x07, 0)
    return raw[8 + env:]


def load(params: dict[str, Any]) -> dict[str, Any]:
    """Stream a workspace layer into a table, in batches, in one
    transaction. ``mode``: create (the table must not exist), replace or
    append. Columns the policy hides are never loaded."""
    _set_details(params)
    import fnmatch

    import pandas as pd

    path = _plain_path(params.get("path"))
    source = _plain_path(params.get("source"))
    table = params.get("table")
    mode = params.get("mode") or "create"
    if not isinstance(table, str) or not NAME.match(table) or table.lower().startswith(("duckdb_", "sqlite_", "pg_")):
        raise ProjectDbError("table must be a plain name: a letter or _, then letters, digits and _ (at most 63)")
    if mode not in ("create", "replace", "append"):
        raise ProjectDbError("mode must be create, replace or append")
    pk = params.get("primary_key") or []
    if not isinstance(pk, list) or not all(isinstance(c, str) for c in pk):
        raise ProjectDbError("primary_key must be a list of column names")
    hide = [p.lower() for p in params.get("hide_fields") or [] if isinstance(p, str)]
    kind, src_table, geom, cols, crs = _source_layer(source)
    hidden = [c for c, _ in cols if any(fnmatch.fnmatchcase(c.lower(), h) for h in hide)]
    cols = [(c, t) for c, t in cols if c not in hidden]
    names = [c for c, _ in cols]
    missing = [c for c in pk if c not in names]
    if missing:
        raise ProjectDbError(f"primary_key names columns the layer does not have: {', '.join(missing)}")
    timeout = float(params.get("timeout_s") or 120)
    max_mb = params.get("max_mb")
    con, spatial = _open(path, int(params.get("memory_mb") or 2048), max_mb=max_mb)
    started = time.monotonic()
    try:
        used = _check_size(con, max_mb, "loading more")
        with _Deadline(con, timeout) as dl, _SizeGuard(con, path, max_mb, used) as sg:
            try:
                con.execute("BEGIN TRANSACTION")
                exists = con.execute(
                    "SELECT count(*) FROM duckdb_tables() WHERE database_name = current_database() AND schema_name = 'main' AND lower(table_name) = lower(?)",
                    [table]).fetchone()[0] > 0
                if exists and mode == "create":
                    raise ProjectDbError(f"table {table} already exists; use mode replace or append, or another name")
                if not exists and mode == "append":
                    raise ProjectDbError(f"table {table} does not exist; use mode create")
                types = {c: ("GEOMETRY" if c == geom else _duck_type(t)) for c, t in cols}
                if exists and mode == "replace":
                    con.execute(f"DROP TABLE {_qi(table)}")
                if not exists or mode == "replace":
                    defs = [f"{_qi(c)} {types[c]}" + (" NOT NULL" if c in pk else "") for c in names]
                    if pk:
                        defs.append(f"PRIMARY KEY ({', '.join(_qi(c) for c in pk)})")
                    con.execute(f"CREATE TABLE {_qi(table)} ({', '.join(defs)})")
                else:
                    have = [r[0] for r in con.execute(
                        "SELECT column_name FROM duckdb_columns() WHERE database_name = current_database() AND schema_name = 'main' AND lower(table_name) = lower(?) ORDER BY column_index",
                        [table]).fetchall()]
                    if [h.lower() for h in have] != [n.lower() for n in names]:
                        raise ProjectDbError(f"table {table} has columns {have}, the layer {names}: append needs the same columns")
                select = ", ".join(
                    (f"ST_GeomFromWKB({_qi(c)})" if c == geom else _qi(c)) + f" AS {_qi(c)}" for c in names)
                insert = f"INSERT INTO {_qi(table)} SELECT {select} FROM columbus_batch"
                rows = 0

                def push(batch):
                    nonlocal rows
                    if not batch:
                        return
                    frame = pd.DataFrame(batch, columns=names, dtype=object)
                    con.register("columbus_batch", frame)
                    try:
                        con.execute(insert)
                    finally:
                        con.unregister("columbus_batch")
                    rows += len(batch)

                if kind == "gpkg":
                    src = sqlite3.connect(readonly_uri(source), uri=True)
                    try:
                        src.execute("PRAGMA query_only = ON")
                        cur = src.execute(f"SELECT {', '.join(_qi(c) for c in names)} FROM {_qi(src_table)}")
                        gi = names.index(geom) if geom in names else None
                        batch = []
                        while True:
                            chunk = cur.fetchmany(FETCH)
                            if not chunk:
                                break
                            for r in chunk:
                                r = list(r)
                                if gi is not None:
                                    r[gi] = _wkb(r[gi])
                                batch.append(r)
                            if len(batch) >= INSERT_BATCH:
                                push(batch)
                                batch = []
                        push(batch)
                    finally:
                        src.close()
                else:
                    with open(source, encoding="utf-8", newline="") as f:
                        reader = csv.reader(f)
                        header = next(reader, [])
                        idx = [header.index(c) for c in names]
                        batch = []
                        for r in reader:
                            batch.append([(r[i] if i < len(r) and r[i] != "" else None) for i in idx])
                            if len(batch) >= INSERT_BATCH:
                                push(batch)
                                batch = []
                        push(batch)
                    if params.get("types", "infer") != "text":
                        _infer_csv_types(con, table, names, pk)
                comment = params.get("comment")
                if isinstance(comment, str) and comment:
                    con.execute(f"COMMENT ON TABLE {_qi(table)} IS {_lit(comment[:2000])}")
                if geom and crs:
                    con.execute(f"COMMENT ON COLUMN {_qi(table)}.{_qi(geom)} IS {_lit('CRS ' + crs)}")
                con.execute("COMMIT")
            except BaseException as e:
                try:
                    con.execute("ROLLBACK")
                except Exception:  # noqa: BLE001
                    pass
                if sg.fired:
                    raise ProjectDbError(sg.message()) from None
                if dl.fired:
                    raise ProjectDbError(f"loading took longer than {timeout:g} s and was stopped; nothing was loaded") from None
                if isinstance(e, ProjectDbError):
                    raise
                duckdb = _duckdb()
                if isinstance(e, duckdb.Error):
                    raise ProjectDbError(f"the load failed and nothing was loaded: {_msg(e)}") from None
                raise
        described = _columns(con, table)
        held = used_bytes(con)
    finally:
        con.close()
    return {"table": table, "rows": rows, "columns": described, "geometry_column": geom, "crs": crs, "bytes": db_bytes(path),
            "used_bytes": held,
            "hidden_columns": hidden, "primary_key": pk, "spatial": spatial, "seconds": round(time.monotonic() - started, 3)}


def _infer_csv_types(con, table: str, names: list[str], pk: list[str]) -> None:
    """A CSV's columns become BIGINT or DOUBLE where every value reads back
    exactly as written ("0042" stays text, so codes keep their padding)."""
    for c in names:
        q = _qi(c)
        for target in ("BIGINT", "DOUBLE"):
            bad = con.execute(
                f"SELECT count(*) FROM {_qi(table)} WHERE {q} IS NOT NULL AND (TRY_CAST({q} AS {target}) IS NULL "
                f"OR CAST(TRY_CAST({q} AS {target}) AS VARCHAR) <> {q})").fetchone()[0]
            some = con.execute(f"SELECT count({q}) FROM {_qi(table)}").fetchone()[0]
            if some and not bad:
                con.execute(f"ALTER TABLE {_qi(table)} ALTER {q} TYPE {target} USING CAST({q} AS {target})")
                break


def _columns(con, table: str) -> list[dict[str, Any]]:
    return [{"name": r[0], "type": r[1], "nullable": bool(r[2]), "comment": r[3]} for r in con.execute(
        "SELECT column_name, data_type, is_nullable, comment FROM duckdb_columns() "
        "WHERE database_name = current_database() AND schema_name = 'main' AND table_name = ? ORDER BY column_index",
        [table]).fetchall()]


def execute(params: dict[str, Any]) -> dict[str, Any]:
    """DDL and DML in the project database, all statements in one
    transaction. Returns each statement's kind and the rows it changed."""
    _set_details(params)
    path = _plain_path(params.get("path"))
    timeout = float(params.get("timeout_s") or 120)
    max_mb = params.get("max_mb")
    con, spatial = _open(path, int(params.get("memory_mb") or 2048), max_mb=max_mb)
    try:
        stmts = check_statements(con, params.get("sql"), EXECUTE_TYPES)
        # Over the size cap, only statements that free or tidy space run.
        frees = all(st["type"] in ("DROP", "DELETE", "VACUUM", "ANALYZE") for st in stmts)
        used = used_bytes(con) if frees else _check_size(con, max_mb, "anything but DROP, DELETE, VACUUM and ANALYZE")
        started = time.monotonic()
        done = []
        with _Deadline(con, timeout) as dl, _SizeGuard(con, path, max_mb, used) as sg:
            try:
                con.execute("BEGIN TRANSACTION")
                for st in stmts:
                    try:
                        cur = con.execute(st["sql"])
                    except Exception as e:  # noqa: BLE001
                        duckdb = _duckdb()
                        if sg.fired:
                            raise ProjectDbError(sg.message()) from None
                        if dl.fired:
                            raise ProjectDbError(f"statement {st['index']} ran past {timeout:g} s and was stopped; nothing in this call was kept") from None
                        if isinstance(e, duckdb.Error):
                            raise ProjectDbError(f"statement {st['index']} ({st['type']}) failed, so nothing in this call was kept: {_msg(e)}") from None
                        raise
                    changed = None
                    if st["type"] in ("INSERT", "UPDATE", "DELETE", "MERGE_INTO"):
                        try:
                            r = cur.fetchone()
                            changed = int(r[0]) if r and isinstance(r[0], int) else None
                        except Exception:  # noqa: BLE001
                            changed = None
                    done.append({**st, "rows": changed})
                if sg.fired:
                    raise ProjectDbError(sg.message())
                con.execute("COMMIT")
            except BaseException:
                try:
                    con.execute("ROLLBACK")
                except Exception:  # noqa: BLE001
                    pass
                raise
        held = used_bytes(con)
    finally:
        con.close()
    return {"statements": done, "spatial": spatial, "seconds": round(time.monotonic() - started, 3), "bytes": db_bytes(path),
            "used_bytes": held}


def _cell(v: Any) -> Any:
    if isinstance(v, (bytes, bytearray, memoryview)):
        return {"blob": True, "bytes": len(bytes(v))}
    if v is None or isinstance(v, (bool, int, str)):
        return v
    if isinstance(v, float):
        return v if math.isfinite(v) else None
    return str(v)


def query(params: dict[str, Any]) -> dict[str, Any]:
    """One SELECT on the project database, its rows written as a layer
    (GeoPackage when it has a GEOMETRY column, CSV otherwise) in batches as
    they arrive. Stops at ``max_rows`` and says so."""
    _set_details(params)
    import columbus_gis as g
    import shapely

    path = _plain_path(params.get("path"))
    max_rows = params.get("max_rows")
    if not isinstance(max_rows, int) or max_rows < 1:
        raise ProjectDbError("a positive max_rows is required")
    return_rows = max(0, int(params.get("return_rows") or 0))
    stem = params.get("output_stem")
    if not isinstance(stem, str) or not stem:
        raise ProjectDbError("output_stem is required")
    timeout = float(params.get("timeout_s") or 120)
    crs = g.crs_from_spec({"code": params["crs"]}) if params.get("crs") else None
    con, spatial = _open(path, int(params.get("memory_mb") or 2048), read_only=True, max_mb=params.get("max_mb"))
    stream = None
    started = time.monotonic()
    try:
        stmts = check_statements(con, params.get("sql"), QUERY_TYPES, pivot=True)
        if len(stmts) != 1:
            raise ProjectDbError("query takes exactly one SELECT; use execute for statements that change the database")
        sql = stmts[0]["sql"].rstrip().rstrip(";")
        # A limit around the query, so the engine never builds more rows than
        # can be written; statements that cannot be wrapped run as they are
        # and are cut off while fetching.
        bare = (stmts[0]["type"] == "EXPLAIN" or stmts[0].get("pivot")
                or _first_words(_words(sql), 1) in (["describe"], ["show"], ["summarize"]))
        # The newline ends a -- comment the statement may end with.
        run = sql if bare else f"SELECT * FROM ({sql}\n) AS columbus_q LIMIT {max_rows + 1}"
        with _Deadline(con, timeout) as dl, _SizeGuard(con, path, params.get("max_mb"), used_bytes(con)) as sg:
            try:
                cur = con.execute(run)
                desc = cur.description or []
                columns = [d[0] for d in desc]
                types = [str(d[1]).upper() for d in desc]
                geo = [i for i, t in enumerate(types) if t.startswith("GEOMETRY")]
                gi = geo[0] if geo else None
                extra_geo = set(geo[1:])
                attr_idx = [i for i in range(len(columns)) if i != gi]
                attr_cols = [columns[i] for i in attr_idx]
                if gi is not None and crs is None:
                    raise ProjectDbError("the result has a geometry column but no CRS is known for it: pass crs (e.g. \"EPSG:2277\")")
                stream = g.LayerStream(stem, "query", attr_cols, columns[gi] if gi is not None else None, crs)
                shown, n, hit = [], 0, False
                while True:
                    chunk = cur.fetchmany(FETCH)
                    if not chunk:
                        break
                    room = max_rows - n
                    if len(chunk) > room:
                        chunk, hit = chunk[:room], True
                    attrs = []
                    for r in chunk:
                        row = []
                        for i in attr_idx:
                            v = r[i]
                            if i in extra_geo and v is not None:
                                v = shapely.from_wkb(bytes(v)).wkt
                            row.append(v)
                        attrs.append(row)
                    shapes = [None if r[gi] is None else shapely.from_wkb(bytes(r[gi])) for r in chunk] if gi is not None else None
                    stream.write(attrs, shapes)
                    for k, r in enumerate(chunk[:max(0, return_rows - len(shown))]):
                        vals = [_cell(v) for v in attrs[k]]
                        if gi is not None:
                            vals.append(None if r[gi] is None else {"geometry": "wkb", "bytes": len(bytes(r[gi]))})
                        shown.append(vals)
                    n += len(chunk)
                    if hit:
                        break
                out = stream.close()
            except BaseException as e:
                if stream is not None:
                    stream.abort()
                    stream = None
                if sg.fired:
                    raise ProjectDbError(sg.message().replace("nothing in it was kept", "its result was not written")) from None
                if dl.fired:
                    raise ProjectDbError(f"the query ran past {timeout:g} s and was stopped") from None
                if isinstance(e, ProjectDbError):
                    raise
                duckdb = _duckdb()
                if isinstance(e, duckdb.Error):
                    raise ProjectDbError(f"the query failed: {_msg(e)}") from None
                raise
        summary = stream.summary()
        return {"columns": attr_cols + ([columns[gi]] if gi is not None else []), "types": types, "rows": shown, "rows_read": n,
                "rows_returned": len(shown), "hit_read_limit": hit, "output_path": out, "summary": summary,
                "checks": stream.checks("The result"), "statement": stmts[0], "spatial": spatial,
                "seconds": round(time.monotonic() - started, 3)}
    finally:
        con.close()


def describe(params: dict[str, Any]) -> dict[str, Any]:
    """Tables and views with their columns, keys, indexes, comments and
    exact row counts."""
    _set_details(params)
    path = _plain_path(params.get("path"))
    con, spatial = _open(path, int(params.get("memory_mb") or 2048), read_only=True)
    try:
        tables = []
        for name, comment, has_pk in con.execute(
                "SELECT table_name, comment, has_primary_key FROM duckdb_tables() "
                "WHERE database_name = current_database() AND NOT internal ORDER BY table_name").fetchall():
            n = con.execute(f"SELECT count(*) FROM {_qi(name)}").fetchone()[0]
            tables.append({"name": name, "kind": "table", "rows": n, "comment": comment, "has_primary_key": has_pk,
                           "columns": _columns(con, name)})
        for name, comment, sql in con.execute(
                "SELECT view_name, comment, sql FROM duckdb_views() WHERE database_name = current_database() AND NOT internal ORDER BY view_name").fetchall():
            tables.append({"name": name, "kind": "view", "comment": comment, "sql": sql, "columns": _columns(con, name)})
        constraints = [{"table": r[0], "type": r[1], "columns": list(r[2] or []), "references": r[3], "referenced_columns": list(r[4] or [])}
                       for r in con.execute(
                           "SELECT table_name, constraint_type, constraint_column_names, referenced_table, referenced_column_names "
                           "FROM duckdb_constraints() WHERE database_name = current_database() "
                           "AND constraint_type IN ('PRIMARY KEY', 'FOREIGN KEY', 'UNIQUE') ORDER BY table_name").fetchall()]
        indexes = [{"name": r[0], "table": r[1], "unique": r[2], "sql": r[3]} for r in con.execute(
            "SELECT index_name, table_name, is_unique, sql FROM duckdb_indexes() WHERE database_name = current_database() ORDER BY index_name").fetchall()]
    finally:
        con.close()
    return {"tables": tables, "constraints": constraints, "indexes": indexes, "spatial": spatial,
            "bytes": os.path.getsize(path), **versions()}
