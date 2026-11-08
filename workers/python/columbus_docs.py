"""Documents the person imports as notes: Word (.docx), Excel (.xlsx), PDF,
CSV, Markdown and plain text, turned into Markdown text.

Only text comes out: headings, paragraphs, lists and tables. Nothing in a
document is run or followed: no macros, links, embedded objects, images or
external references. Word and Excel files are ZIP archives of XML, read with
the standard library; their XML must be UTF-8 (or ASCII) and must not
declare a document type or entities (a document never needs one), and every
part read is capped, so a crafted file can't make the worker read or build
without limit. Work stops once the text reaches its cap. PDFs need the
``pypdf`` package (in the engine's requirements); without it PDF import
says so.

Invisible characters (controls, zero-width and direction marks, Unicode
"tag" characters, variation selectors, fillers) are removed from all text:
what the person previews is all there is.

``convert_document({"path": ..., "name": ...})`` returns ``{"markdown",
"title", "warnings", "format", "sha256", "bytes"}``; ``name`` is the
original file's name, for a title when the document has none.

Run as ``python -I columbus_docs.py <worker folder> <in> <out.json> <name>``
it converts one file and writes ``{"ok": true, "result": ...}`` or ``{"ok":
false, "error": ...}`` to ``out.json``: how the gateway runs it confined.
"""

from __future__ import annotations

import csv
import hashlib
import io
import os
import re
import sys
import zipfile
import zlib
from typing import Any
from xml.etree import ElementTree as ET

if __name__ == "__main__":  # run confined: the worker's folder comes first
    sys.path.insert(0, sys.argv[1])

import columbus_worker as w  # noqa: E402

# Largest document read, in bytes.
MAX_FILE = 50 * 1024 * 1024
# Largest XML part of a Word or Excel file, once unpacked.
MAX_PART = 25 * 1024 * 1024
# Most Markdown text produced (characters).
MAX_TEXT = 400_000
# Most rows and columns of one table kept.
MAX_ROWS = 500
MAX_COLS = 40
MAX_SHEETS = 20
MAX_PAGES = 300
# Longest text kept from one cell or shared string.
MAX_CELL = 500
# Most elements in a part read whole (Word's document, Excel's workbook).
MAX_ELEMENTS = 1_000_000

FORMATS = {".docx": "word", ".xlsx": "excel", ".pdf": "pdf", ".csv": "csv", ".md": "markdown", ".markdown": "markdown", ".txt": "text"}

W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
S = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
R = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}"

# The same characters the gateway treats as hidden (canon::hidden_char),
# less tab and line feed.
HIDDEN = re.compile(
    "[\x00-\x08\x0b-\x1f\x7f-\x9f\xad\u034f\u061c\u115f\u1160\u17b4\u17b5\u180b-\u180f"
    "\u200b-\u200f\u2028-\u202e\u2060-\u206f\u3164\ufe00-\ufe0f\ufeff\uffa0\ufff9-\ufffb"
    "\U000e0000-\U000e0fff]"
)

# Failures of a damaged or unusual ZIP member.
ZIP_ERRORS = (zipfile.BadZipFile, zipfile.LargeZipFile, RuntimeError, NotImplementedError, zlib.error, EOFError, OSError, ValueError)


def _err(msg: str):
    return w.WorkerError(w.INVALID_PARAMS, msg)


# How many invisible characters this conversion removed.
_removed = [0]


def _clean(s: str) -> str:
    """Text without control or invisible formatting characters (tabs and
    line breaks kept)."""
    out, n = HIDDEN.subn("", s)
    _removed[0] += n
    return out


def _cell(s: Any) -> str:
    t = _clean(("" if s is None else str(s))[: MAX_CELL * 2]).replace("\n", " ").replace("|", "\\|").strip()
    return t[:MAX_CELL]


def _table(rows: list[list[Any]], warnings: list[str], what: str, more: bool = False) -> str:
    rows = [r for r in rows if any(str(c).strip() for c in r if c is not None)]
    if not rows:
        return ""
    if more or len(rows) > MAX_ROWS:
        warnings.append(f"{what}: only the first {MAX_ROWS} rows were kept")
        rows = rows[:MAX_ROWS]
    width = min(max(len(r) for r in rows), MAX_COLS)
    if any(len(r) > MAX_COLS for r in rows):
        warnings.append(f"{what}: only the first {MAX_COLS} columns were kept")
    norm = [[_cell(r[i]) if i < len(r) else "" for i in range(width)] for r in rows]
    head, *body = norm
    lines = ["| " + " | ".join(h or " " for h in head) + " |", "|" + "---|" * width]
    lines += ["| " + " | ".join(c for c in r) + " |" for r in body]
    return "\n".join(lines)


_DECL = re.compile(rb"^\s*<\?xml[^>]*?encoding\s*=\s*[\"']([^\"']*)[\"']", re.I)


def _part(z: zipfile.ZipFile, name: str) -> bytes | None:
    """One XML part's bytes, checked: within the size cap, UTF-8 or ASCII,
    and declaring no document type or entities."""
    try:
        info = z.getinfo(name)
    except KeyError:
        return None
    if info.file_size > MAX_PART:
        raise _err(f"{name} in the file is larger than Columbus reads ({MAX_PART // 1024 // 1024} MB)")
    try:
        with z.open(info) as f:
            data = f.read(MAX_PART + 1)
    except ZIP_ERRORS as e:
        raise _err(f"{name} in the file could not be unpacked ({_why(e)}); save it again from Word or Excel") from None
    if len(data) > MAX_PART:
        raise _err(f"{name} in the file unpacks larger than it says")
    # Office writes UTF-8. Any other encoding (UTF-16 above all, where the
    # markup's bytes differ) is refused, so the check below sees the markup.
    if data.startswith(b"\xef\xbb\xbf"):
        data = data[3:]
    m = _DECL.match(data[:512])
    if b"\x00" in data or data.startswith((b"\xff\xfe", b"\xfe\xff")) or (m and m.group(1).lower() not in (b"utf-8", b"utf8", b"us-ascii", b"ascii")):
        raise _err(f"{name} in the file is not in the UTF-8 that Word and Excel write; it was not read")
    up = data.upper()
    if b"<!DOCTYPE" in up or b"<!ENTITY" in up:
        raise _err("the file's XML declares a document type or entities, which a document never needs; it was not read")
    return data


def _why(e: Exception) -> str:
    if isinstance(e, RuntimeError) and "encrypted" in str(e).lower():
        return "it is protected with a password"
    return type(e).__name__


def _xml(z: zipfile.ZipFile, name: str) -> ET.Element | None:
    """A part as a tree, of at most MAX_ELEMENTS elements."""
    root = None
    for ev, el, _ in _events(z, name, MAX_ELEMENTS):
        if root is None:
            root = el
    return root


def _events(z: zipfile.ZipFile, name: str, limit: int | None = None):
    """A part's start and end events, streamed, each with the element's
    parent: an element read can be taken out of its parent, so a large part
    is never all in memory. With `limit`, a part of more elements than that
    is refused."""
    data = _part(z, name)
    if data is None:
        return
    stack: list[ET.Element] = []
    n = 0
    try:
        for ev, el in ET.iterparse(io.BytesIO(data), events=("start", "end")):
            if ev == "start":
                n += 1
                if limit is not None and n > limit:
                    raise _err(f"{name} in the file has more parts than Columbus reads ({limit:,})")
                stack.append(el)
                yield ev, el, stack[-2] if len(stack) > 1 else None
            else:
                stack.pop()
                yield ev, el, stack[-1] if stack else None
    except ET.ParseError as e:
        raise _err(f"{name} in the file is not well-formed XML ({e})") from None


def _zip(path: str) -> zipfile.ZipFile:
    try:
        return zipfile.ZipFile(path)
    except ZIP_ERRORS:
        raise _err("this is not a Word or Excel file (it isn't the ZIP they are made of); save it again from Word or Excel") from None


def _children(el: ET.Element, tags: set[str]):
    """Elements of these tags under `el`, in order, looking through content
    controls and custom XML wrappers (but not into the elements found)."""
    through = {f"{W}sdt", f"{W}sdtContent", f"{W}customXml", f"{W}smartTag"}
    stack = [iter(list(el))]
    while stack:
        e = next(stack[-1], None)
        if e is None:
            stack.pop()
        elif e.tag in tags:
            yield e
        elif e.tag in through:
            stack.append(iter(list(e)))


def _vanished(run: ET.Element) -> bool:
    rpr = run.find(f"{W}rPr")
    v = rpr.find(f"{W}vanish") if rpr is not None else None
    return v is not None and (v.get(f"{W}val") or "true").lower() not in ("0", "false", "off")


def _text_of(el: ET.Element, seen: dict[str, bool]) -> str:
    """All the visible text under `el`, each element visited once; paragraphs
    nested in it (text boxes, a cell's paragraphs) are separated by spaces."""
    parts: list[str] = []
    n = 0
    stack = [el]
    while stack and n <= MAX_TEXT:
        e = stack.pop()
        tag = e.tag
        if tag == f"{W}r" and _vanished(e):
            seen["hidden"] = True
            continue
        if tag == f"{W}t":
            parts.append(e.text or "")
            n += len(e.text or "")
        elif tag == f"{W}tab":
            parts.append("\t")
        elif tag in (f"{W}br", f"{W}cr"):
            parts.append("\n")
        elif tag == f"{W}p" and e is not el and parts:
            parts.append(" ")
        stack.extend(reversed(list(e)))
    return _clean("".join(parts)).strip()


def _numbering(z: zipfile.ZipFile) -> dict[str, dict[int, str]]:
    """numId -> {level: number format} from word/numbering.xml."""
    root = _xml(z, "word/numbering.xml")
    if root is None:
        return {}
    abstract: dict[str, dict[int, str]] = {}
    for a in root.findall(f"{W}abstractNum"):
        levels = {}
        for lvl in a.findall(f"{W}lvl"):
            fmt = lvl.find(f"{W}numFmt")
            try:
                levels[int(lvl.get(f"{W}ilvl") or "0")] = (fmt.get(f"{W}val") if fmt is not None else "") or "bullet"
            except ValueError:
                continue
        abstract[a.get(f"{W}abstractNumId") or ""] = levels
    out = {}
    for num in root.findall(f"{W}num"):
        a = num.find(f"{W}abstractNumId")
        if a is not None:
            out[num.get(f"{W}numId") or ""] = abstract.get(a.get(f"{W}val") or "", {})
    return out


def _word(path: str, warnings: list[str]) -> tuple[str, str | None]:
    with _zip(path) as z:
        doc = _xml(z, "word/document.xml")
        if doc is None:
            raise _err("this is not a Word document (no word/document.xml)")
        if any(n.lower().startswith("word/vbaproject") for n in z.namelist()):
            warnings.append("it holds macros; they were not read or run")
        numbering = _numbering(z)
        title = None
        core = _xml(z, "docProps/core.xml")
        if core is not None:
            t = core.find("{http://purl.org/dc/elements/1.1/}title")
            if t is not None and (t.text or "").strip():
                title = _clean(t.text.strip()[:400])[:200]
    body = doc.find(f"{W}body")
    out: list[str] = []
    if body is None:
        return "", title
    seen: dict[str, bool] = {}
    counters: dict[str, list[int]] = {}
    total = 0

    for el in _children(body, {f"{W}p", f"{W}tbl"}):
        if total > MAX_TEXT:
            break
        if el.tag == f"{W}p":
            t = _text_of(el, seen)
            if not t:
                continue
            ppr = el.find(f"{W}pPr")
            style = ""
            num_id, level = None, 0
            if ppr is not None:
                ps = ppr.find(f"{W}pStyle")
                if ps is not None:
                    style = (ps.get(f"{W}val") or "").lower()
                np_ = ppr.find(f"{W}numPr")
                if np_ is not None:
                    i = np_.find(f"{W}numId")
                    lv = np_.find(f"{W}ilvl")
                    num_id = (i.get(f"{W}val") if i is not None else None) or ""
                    try:
                        level = min(max(int(lv.get(f"{W}val") or "0") if lv is not None else 0, 0), 8)
                    except ValueError:
                        level = 0
            m = re.match(r"heading\s*([1-6])$", style)
            if style == "title":
                line = f"# {t}"
                title = title or t[:200]
            elif m:
                line = "#" * (int(m.group(1)) + 1) + " " + t
            elif (num_id is not None and num_id != "0") or style.startswith("list"):
                fmt = numbering.get(num_id or "", {}).get(level, "bullet") if num_id else ("decimal" if style.startswith("listnumber") else "bullet")
                key = num_id or f"style:{style}"
                if fmt in ("bullet", "none", ""):
                    line = "   " * level + f"- {t}"
                else:
                    c = counters.setdefault(key, [0] * 9)
                    c[level] += 1
                    for j in range(level + 1, 9):
                        c[j] = 0
                    line = "   " * level + f"{c[level]}. {t}"
            else:
                line = t
        else:
            rows = []
            for tr in _children(el, {f"{W}tr"}):
                if len(rows) > MAX_ROWS:
                    break
                rows.append([_text_of(tc, seen)[: MAX_CELL * 2] for tc in _children(tr, {f"{W}tc"})][: MAX_COLS + 1])
            line = _table(rows, warnings, "a table")
            if not line:
                continue
        out.append(line)
        total += len(line) + 2
    if seen.get("hidden"):
        warnings.append("text formatted as hidden in Word was left out")
    return "\n\n".join(out), title


def _col_index(ref: str) -> int:
    letters = re.match(r"([A-Z]+)", ref or "")
    n = 0
    for ch in (letters.group(1) if letters else "")[:4]:
        n = n * 26 + (ord(ch) - 64)
    return max(n - 1, 0)


def _sheet_rows(z: zipfile.ZipFile, part: str, shared: list[str]) -> tuple[list[list[str]], bool]:
    """A sheet's rows (at most MAX_ROWS kept, MAX_COLS + 1 columns), streamed;
    and whether there were more."""
    rows: list[list[str]] = []
    cur: dict[int, str] | None = None
    nxt = 0
    for ev, el, parent in _events(z, part, MAX_ELEMENTS * 4):
        tag = el.tag
        if ev == "start":
            if tag == f"{S}row":
                cur, nxt = {}, 0
            continue
        if tag == f"{S}c" and cur is not None:
            ref = el.get("r")
            i = _col_index(ref) if ref else nxt
            nxt = i + 1
            if i <= MAX_COLS and i not in cur:
                t = el.get("t")
                v = el.find(f"{S}v")
                vt = (v.text or "") if v is not None else ""
                if t == "s":
                    try:
                        k = int(vt)
                        val = shared[k] if 0 <= k < len(shared) else ""
                    except ValueError:
                        val = ""
                elif t == "inlineStr":
                    val = "".join(x.text or "" for x in el.iter(f"{S}t"))[: MAX_CELL * 2]
                elif t == "b":
                    val = "TRUE" if vt == "1" else "FALSE"
                else:
                    val = vt[: MAX_CELL * 2]
                cur[i] = val
            if parent is not None:
                parent.remove(el)
        elif tag == f"{S}row":
            if cur and any(v.strip() for v in cur.values()):
                if len(rows) >= MAX_ROWS:
                    return rows, True
                rows.append([cur.get(i, "") for i in range(max(cur) + 1)])
            cur = None
            if parent is not None:
                parent.remove(el)
    return rows, False


def _excel(path: str, warnings: list[str]) -> tuple[str, str | None]:
    with _zip(path) as z:
        if any(n.lower().startswith("xl/vbaproject") for n in z.namelist()):
            warnings.append("it holds macros; they were not read or run")
        shared: list[str] = []
        for ev, el, parent in _events(z, "xl/sharedStrings.xml", MAX_ELEMENTS * 4):
            if ev == "end" and el.tag == f"{S}si":
                # The string's own text: direct and rich-text runs, not
                # phonetic guides.
                parts = [t.text or "" for t in el.findall(f"{S}t")]
                parts += [t.text or "" for r in el.findall(f"{S}r") for t in r.findall(f"{S}t")]
                shared.append("".join(parts)[: MAX_CELL * 2])
                if parent is not None:
                    parent.remove(el)
        wb = _xml(z, "xl/workbook.xml")
        rels = _xml(z, "xl/_rels/workbook.xml.rels")
        if wb is None:
            raise _err("this is not an Excel workbook (no xl/workbook.xml)")
        targets = {}
        if rels is not None:
            for r in rels:
                targets[r.get("Id")] = r.get("Target") or ""
        sheets = wb.find(f"{S}sheets")
        out = []
        total = 0
        listed = list(sheets) if sheets is not None else []
        hidden = [sh for sh in listed if (sh.get("state") or "visible") != "visible"]
        for sh in hidden:
            warnings.append(f"the hidden sheet {_cell(sh.get('name') or 'Sheet')} was left out")
        visible = [sh for sh in listed if sh not in hidden]
        if len(visible) > MAX_SHEETS:
            warnings.append(f"only the first {MAX_SHEETS} of {len(visible)} sheets were kept")
        for sh in visible[:MAX_SHEETS]:
            if total > MAX_TEXT:
                break
            name = _cell(sh.get("name") or "Sheet")
            target = targets.get(sh.get(f"{R}id"), "")
            # Only parts inside the workbook's own folder.
            part = os.path.normpath("xl/" + target.lstrip("/").removeprefix("xl/")).replace("\\", "/")
            if not part.startswith("xl/worksheets/"):
                continue
            rows, more = _sheet_rows(z, part, shared)
            t = _table(rows, warnings, f"sheet {name}", more)
            if t:
                out.append(f"## {name}\n\n{t}")
                total += len(t)
    return "\n\n".join(out), None


def _pdf(path: str, warnings: list[str]) -> tuple[str, str | None]:
    try:
        from pypdf import PdfReader
    except ImportError:
        raise w.WorkerError(w.UNSUPPORTED, "reading PDFs needs the pypdf package in Columbus's engine; run the engine setup again to add it") from None
    try:
        reader = PdfReader(path)
        if reader.is_encrypted:
            raise _err("this PDF is protected with a password; save an unprotected copy to import it")
        pages = reader.pages
        n = len(pages)
        if n > MAX_PAGES:
            warnings.append(f"only the first {MAX_PAGES} of {n} pages were read")
        out = []
        total = 0
        for i, page in enumerate(pages):
            if i >= MAX_PAGES or total > MAX_TEXT:
                break
            t = _clean(page.extract_text() or "").strip()
            if t:
                out.append(t)
                total += len(t)
        title = None
        meta = reader.metadata
        if meta and meta.title:
            title = _clean(str(meta.title)[:400]).strip()[:200] or None
    except w.WorkerError:
        raise
    except Exception as e:  # noqa: BLE001 - a broken PDF
        raise _err(f"this PDF could not be read ({type(e).__name__})") from None
    if not out:
        warnings.append("no text was found: it may be a scan (images of pages), which Columbus can't read yet")
    return "\n\n".join(out), title


def _text(path: str) -> str:
    """A text file's text, at most a little over MAX_TEXT characters."""
    with open(path, "rb") as f:
        data = f.read(MAX_FILE + 1)
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        encs = ("utf-16",)
    else:
        encs = ("utf-8-sig", "cp1252", "latin-1")
    for enc in encs:
        try:
            text = data.decode(enc)
            break
        except UnicodeDecodeError:
            continue
    else:
        text = data.decode("utf-8", "replace")
    # Cut before cleaning: the end is cut anyway.
    return text[: MAX_TEXT + 10_000]


def _csv(path: str, warnings: list[str]) -> tuple[str, str | None]:
    text = _text(path)
    try:
        dialect = csv.Sniffer().sniff(text[:20000], delimiters=",;\t|")
    except csv.Error:
        dialect = csv.excel
    rows = []
    more = False
    csv.field_size_limit(MAX_TEXT + 10_000)
    try:
        for r in csv.reader(io.StringIO(text), dialect):
            if len(rows) >= MAX_ROWS:
                more = True
                break
            rows.append(r[: MAX_COLS + 1])
    except csv.Error as e:
        raise _err(f"this CSV could not be read ({e})") from None
    return _table(rows, warnings, "the table", more), None


def convert_document(params: dict[str, Any]) -> dict[str, Any]:
    path = params.get("path")
    if not isinstance(path, str) or not path:
        raise _err("no document given")
    if not os.path.isfile(path):
        raise _err(f"{path} is not a file")
    size = os.path.getsize(path)
    if size > MAX_FILE:
        raise _err(f"the document is larger than Columbus imports ({MAX_FILE // 1024 // 1024} MB)")
    ext = os.path.splitext(path)[1].lower()
    fmt = FORMATS.get(ext)
    if fmt is None:
        raise _err("Columbus imports Word (.docx), Excel (.xlsx), PDF, CSV, Markdown and text files")
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    warnings: list[str] = []
    title = None
    _removed[0] = 0
    if fmt == "word":
        md, title = _word(path, warnings)
    elif fmt == "excel":
        md, title = _excel(path, warnings)
    elif fmt == "pdf":
        md, title = _pdf(path, warnings)
    elif fmt == "csv":
        md, title = _csv(path, warnings)
    else:
        md = _clean(_text(path).replace("\r\n", "\n").replace("\r", "\n"))
    md = md.strip()
    if len(md) > MAX_TEXT:
        warnings.append(f"the text was cut to its first {MAX_TEXT:,} characters")
        md = md[:MAX_TEXT].rstrip() + "\n\n…"
    if _removed[0]:
        n = _removed[0]
        warnings.append(f"{n} invisible character{'' if n == 1 else 's'} (control, zero-width or hidden-tag characters) {'was' if n == 1 else 'were'} removed")
    if not title:
        m = re.search(r"^#{1,3}\s+(.+)$", md[:20_000], re.M)
        name = params.get("name") if isinstance(params.get("name"), str) else ""
        title = m.group(1).strip()[:200] if m else os.path.splitext(os.path.basename(name or path))[0][:200]
    return {"markdown": md, "title": _clean(title).strip(), "warnings": warnings, "format": fmt,
            "sha256": h.hexdigest(), "bytes": size}


def _main(argv: list[str]) -> int:
    import json

    _, _, src, out, name = argv[:5]
    try:
        r = {"ok": True, "result": convert_document({"path": src, "name": name})}
    except w.WorkerError as e:
        r = {"ok": False, "error": str(e.args[0]) if e.args else "the document could not be read"}
    except MemoryError:
        r = {"ok": False, "error": "the document needed more memory than Columbus allows for reading one"}
    except RecursionError:
        r = {"ok": False, "error": "the document is nested too deeply to read"}
    with open(out, "w", encoding="utf-8") as f:
        json.dump(r, f)
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv))
