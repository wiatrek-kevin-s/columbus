"""Documents the person imports as notes: Word (.docx), Excel (.xlsx), PDF,
CSV, Markdown and plain text, turned into Markdown text.

Only text comes out: headings, paragraphs, lists and tables. Nothing in a
document is run or followed: no macros, links, embedded objects, images or
external references. Word and Excel files are ZIP archives of XML, read with
the standard library; their XML must not declare entities (a document never
needs one), and every part read is capped, so a crafted file can't make the
worker read or build without limit. PDFs need the ``pypdf`` package (in the
engine's requirements); without it PDF import says so.

``convert_document({"path": ...})`` returns ``{"markdown", "title",
"warnings", "format", "sha256", "bytes"}``.
"""

from __future__ import annotations

import csv
import hashlib
import io
import os
import re
import zipfile
from typing import Any
from xml.etree import ElementTree as ET

import columbus_worker as w

# Largest document read, in bytes.
MAX_FILE = 50 * 1024 * 1024
# Largest XML part of a Word or Excel file, once unpacked.
MAX_PART = 40 * 1024 * 1024
# Most Markdown text produced (characters).
MAX_TEXT = 400_000
# Most rows and columns of one table kept.
MAX_ROWS = 500
MAX_COLS = 40
MAX_SHEETS = 20
MAX_PAGES = 300

FORMATS = {".docx": "word", ".xlsx": "excel", ".pdf": "pdf", ".csv": "csv", ".md": "markdown", ".markdown": "markdown", ".txt": "text"}

W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
S = "{http://schemas.openxmlformats.org/spreadsheetml/2006/main}"
R = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}"


def _err(msg: str):
    return w.WorkerError(w.INVALID_PARAMS, msg)


def _clean(s: str) -> str:
    """Text without control or invisible formatting characters (tabs and
    line breaks kept)."""
    out = []
    for ch in s:
        o = ord(ch)
        if ch in "\t\n":
            out.append(ch)
        elif o < 32 or o == 127 or 0x80 <= o < 0xA0:
            continue
        elif 0x200B <= o <= 0x200F or 0x202A <= o <= 0x202E or 0x2066 <= o <= 0x2069 or o in (0xFEFF, 0x061C):
            continue
        else:
            out.append(ch)
    return "".join(out)


def _cell(s: Any) -> str:
    t = _clean("" if s is None else str(s)).replace("\n", " ").replace("|", "\\|").strip()
    return t[:500]


def _table(rows: list[list[Any]], warnings: list[str], what: str) -> str:
    rows = [r for r in rows if any(str(c).strip() for c in r if c is not None)]
    if not rows:
        return ""
    if len(rows) > MAX_ROWS:
        warnings.append(f"{what}: only the first {MAX_ROWS} of {len(rows)} rows were kept")
        rows = rows[:MAX_ROWS]
    width = min(max(len(r) for r in rows), MAX_COLS)
    if any(len(r) > MAX_COLS for r in rows):
        warnings.append(f"{what}: only the first {MAX_COLS} columns were kept")
    norm = [[_cell(r[i]) if i < len(r) else "" for i in range(width)] for r in rows]
    head, *body = norm
    lines = ["| " + " | ".join(h or " " for h in head) + " |", "|" + "---|" * width]
    lines += ["| " + " | ".join(c for c in r) + " |" for r in body]
    return "\n".join(lines)


def _xml(z: zipfile.ZipFile, name: str) -> ET.Element | None:
    try:
        info = z.getinfo(name)
    except KeyError:
        return None
    if info.file_size > MAX_PART:
        raise _err(f"{name} in the file is larger than Columbus reads ({MAX_PART // 1024 // 1024} MB)")
    with z.open(info) as f:
        data = f.read(MAX_PART + 1)
    if len(data) > MAX_PART:
        raise _err(f"{name} in the file unpacks larger than it says")
    head = data[:4096].upper()
    if b"<!DOCTYPE" in head or b"<!ENTITY" in data.upper():
        raise _err("the file's XML declares entities, which a document never needs; it was not read")
    try:
        return ET.fromstring(data)
    except ET.ParseError as e:
        raise _err(f"{name} in the file is not well-formed XML ({e})") from None


def _zip(path: str) -> zipfile.ZipFile:
    try:
        return zipfile.ZipFile(path)
    except zipfile.BadZipFile:
        raise _err("this is not a Word or Excel file (it isn't the ZIP they are made of); save it again from Word or Excel") from None


def _word(path: str, warnings: list[str]) -> tuple[str, str | None]:
    with _zip(path) as z:
        doc = _xml(z, "word/document.xml")
        if doc is None:
            raise _err("this is not a Word document (no word/document.xml)")
        if any(n.lower().startswith("word/vbaproject") for n in z.namelist()):
            warnings.append("it holds macros; they were not read or run")
        title = None
        core = _xml(z, "docProps/core.xml")
        if core is not None:
            t = core.find("{http://purl.org/dc/elements/1.1/}title")
            if t is not None and (t.text or "").strip():
                title = _clean(t.text.strip())[:200]
    body = doc.find(f"{W}body")
    out: list[str] = []
    if body is None:
        return "", title

    def text_of(p: ET.Element) -> str:
        parts = []
        for el in p.iter():
            if el.tag == f"{W}t":
                parts.append(el.text or "")
            elif el.tag == f"{W}tab":
                parts.append("\t")
            elif el.tag in (f"{W}br", f"{W}cr"):
                parts.append("\n")
        return _clean("".join(parts)).strip()

    for el in body:
        if el.tag == f"{W}p":
            t = text_of(el)
            if not t:
                continue
            ppr = el.find(f"{W}pPr")
            style = ""
            listed = False
            if ppr is not None:
                ps = ppr.find(f"{W}pStyle")
                if ps is not None:
                    style = (ps.get(f"{W}val") or "").lower()
                listed = ppr.find(f"{W}numPr") is not None
            m = re.match(r"heading\s*([1-6])$", style)
            if style == "title":
                out.append(f"# {t}")
                title = title or t[:200]
            elif m:
                out.append("#" * (int(m.group(1)) + 1) + " " + t)
            elif listed or style.startswith("list"):
                out.append(f"- {t}")
            else:
                out.append(t)
        elif el.tag == f"{W}tbl":
            rows = []
            for tr in el.iter(f"{W}tr"):
                rows.append([" ".join(text_of(p) for p in tc.iter(f"{W}p")).strip() for tc in tr.findall(f"{W}tc")])
            t = _table(rows, warnings, "a table")
            if t:
                out.append(t)
    return "\n\n".join(out), title


def _col_index(ref: str) -> int:
    letters = re.match(r"([A-Z]+)", ref or "")
    n = 0
    for ch in letters.group(1) if letters else "":
        n = n * 26 + (ord(ch) - 64)
    return max(n - 1, 0)


def _excel(path: str, warnings: list[str]) -> tuple[str, str | None]:
    with _zip(path) as z:
        if any(n.lower().startswith("xl/vbaproject") for n in z.namelist()):
            warnings.append("it holds macros; they were not read or run")
        shared: list[str] = []
        ss = _xml(z, "xl/sharedStrings.xml")
        if ss is not None:
            for si in ss.findall(f"{S}si"):
                shared.append("".join(t.text or "" for t in si.iter(f"{S}t")))
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
        listed = list(sheets) if sheets is not None else []
        if len(listed) > MAX_SHEETS:
            warnings.append(f"only the first {MAX_SHEETS} of {len(listed)} sheets were kept")
        for sh in listed[:MAX_SHEETS]:
            name = _clean(sh.get("name") or "Sheet")
            target = targets.get(sh.get(f"{R}id"), "")
            # Only parts inside the workbook's own folder.
            part = os.path.normpath("xl/" + target.lstrip("/").removeprefix("xl/")).replace("\\", "/")
            if not part.startswith("xl/worksheets/"):
                continue
            ws = _xml(z, part)
            if ws is None:
                continue
            rows = []
            for row in ws.iter(f"{S}row"):
                if len(rows) > MAX_ROWS:
                    break
                cells: list[str] = []
                for c in row.findall(f"{S}c"):
                    i = _col_index(c.get("r") or "")
                    if i >= MAX_COLS * 2:
                        continue
                    while len(cells) < i:
                        cells.append("")
                    t = c.get("t")
                    v = c.find(f"{S}v")
                    if t == "s" and v is not None:
                        try:
                            val = shared[int(v.text or "0")]
                        except (ValueError, IndexError):
                            val = ""
                    elif t == "inlineStr":
                        val = "".join(x.text or "" for x in c.iter(f"{S}t"))
                    else:
                        val = v.text if v is not None else ""
                    cells.append(val or "")
                rows.append(cells)
            t = _table(rows, warnings, f"sheet {name}")
            if t:
                out.append(f"## {name}\n\n{t}")
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
        for i, page in enumerate(pages):
            if i >= MAX_PAGES:
                break
            t = _clean(page.extract_text() or "").strip()
            if t:
                out.append(t)
        title = None
        meta = reader.metadata
        if meta and meta.title:
            title = _clean(str(meta.title)).strip()[:200] or None
    except w.WorkerError:
        raise
    except Exception as e:  # noqa: BLE001 - a broken PDF
        raise _err(f"this PDF could not be read ({type(e).__name__})") from None
    if not out:
        warnings.append("no text was found: it may be a scan (images of pages), which Columbus can't read yet")
    return "\n\n".join(out), title


def _text(path: str) -> str:
    with open(path, "rb") as f:
        data = f.read(MAX_FILE + 1)
    for enc in ("utf-8-sig", "cp1252", "latin-1"):
        try:
            return data.decode(enc)
        except UnicodeDecodeError:
            continue
    return data.decode("utf-8", "replace")


def _csv(path: str, warnings: list[str]) -> tuple[str, str | None]:
    text = _text(path)
    try:
        dialect = csv.Sniffer().sniff(text[:20000], delimiters=",;\t|")
    except csv.Error:
        dialect = csv.excel
    rows = []
    for i, r in enumerate(csv.reader(io.StringIO(text), dialect)):
        if i > MAX_ROWS:
            warnings.append(f"only the first {MAX_ROWS} rows were kept")
            break
        rows.append(r)
    return _table(rows, warnings, "the table"), None


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
    if fmt == "word":
        md, title = _word(path, warnings)
    elif fmt == "excel":
        md, title = _excel(path, warnings)
    elif fmt == "pdf":
        md, title = _pdf(path, warnings)
    elif fmt == "csv":
        md, title = _csv(path, warnings)
    else:
        md = _clean(_text(path).replace("\r\n", "\n"))
    md = md.strip()
    if len(md) > MAX_TEXT:
        warnings.append(f"the text was cut to its first {MAX_TEXT:,} characters")
        md = md[:MAX_TEXT].rstrip() + "\n\n…"
    if not title:
        m = re.search(r"^#{1,3}\s+(.+)$", md, re.M)
        title = m.group(1).strip()[:200] if m else os.path.splitext(os.path.basename(path))[0][:200]
    return {"markdown": md, "title": _clean(title).strip(), "warnings": warnings, "format": fmt,
            "sha256": h.hexdigest(), "bytes": size}
