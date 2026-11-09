"""Checks the links in the repository's Markdown files.

- Every relative link and image (Markdown, and HTML src, href and srcset)
  must name a file or folder that exists, and a #section must be a heading
  (or an explicit id) in the file it points to, by GitHub's own rules.
- Links to this repository's files on github.com (…/blob/main/<path>,
  …/tree/main/<path>) are checked the same way, against the working tree.
- With --external, other http(s) links in the front-page files (README,
  SECURITY, CONTRIBUTING, the issue forms, the data credits) are fetched;
  any answer below 400 passes (a sign-in page counts).

    python .github/scripts/check_links.py [--external]
"""

import argparse
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REPO_URL = "https://github.com/wiatrek-kevin-s/columbus"
SKIP_DIRS = {"target", "node_modules", ".git", "out", "dist", "gen"}
FRONT = ["README.md", "SECURITY.md", "CONTRIBUTING.md", "docs/images/DATA-CREDITS.md",
         ".github/ISSUE_TEMPLATE/bug_report.yml", ".github/ISSUE_TEMPLATE/feature_request.yml", ".github/ISSUE_TEMPLATE/config.yml"]

MD_LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
HTML_ATTR = re.compile(r"""\b(?:src|href)\s*=\s*["']([^"']+)["']""", re.I)
SRCSET = re.compile(r"""\bsrcset\s*=\s*["']([^"']+)["']""", re.I)
BARE_URL = re.compile(r"https?://[^\s)\"'<>`]+")
FENCE = re.compile(r"^\s*(```|~~~)")


def md_files():
    for p in sorted(ROOT.rglob("*.md")):
        if not any(part in SKIP_DIRS for part in p.relative_to(ROOT).parts):
            yield p


def strip_code(text, inline=True):
    """Text without fenced code blocks (and inline code spans)."""
    out, fenced = [], False
    for line in text.splitlines():
        if FENCE.match(line):
            fenced = not fenced
            out.append("")
            continue
        out.append("" if fenced else re.sub(r"`[^`]*`", "", line) if inline else line)
    return "\n".join(out)


def slug(heading):
    """GitHub's anchor for a heading."""
    h = re.sub(r"<[^>]+>", "", heading)
    h = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", h)   # links keep their text
    h = h.replace("`", "").strip().lower()
    h = re.sub(r"[^\w\- ]", "", h, flags=re.UNICODE)
    return h.replace(" ", "-")


_anchors = {}


def anchors(path):
    if path not in _anchors:
        found, seen = set(), {}
        text = strip_code(path.read_text(encoding="utf-8"), inline=False)
        for line in text.splitlines():
            m = re.match(r"^\s{0,3}#{1,6}\s+(.*?)\s*#*\s*$", line)
            if m:
                s = slug(m.group(1))
                n = seen.get(s, 0)
                seen[s] = n + 1
                found.add(s if n == 0 else f"{s}-{n}")
        for m in re.finditer(r"""<a\s+(?:id|name)=["']([^"']+)["']""", text):
            found.add(m.group(1))
        _anchors[path] = found
    return _anchors[path]


def targets(text):
    for m in MD_LINK.finditer(text):
        yield m.group(1)
    for m in HTML_ATTR.finditer(text):
        yield m.group(1)
    for m in SRCSET.finditer(text):
        for part in m.group(1).split(","):
            if part.strip():
                yield part.strip().split()[0]


def check_local(src, link, errors):
    raw = link
    if link.startswith(REPO_URL + "/blob/") or link.startswith(REPO_URL + "/tree/"):
        rest = link[len(REPO_URL) + 1:].split("/", 2)
        if len(rest) < 3:
            return
        link = "/" + rest[2]
    elif re.match(r"^[a-z][a-z0-9+.-]*:", link, re.I) or "{{" in link:
        return  # another scheme (http, mailto…), or a template's placeholder
    path, _, frag = link.partition("#")
    path = urllib.parse.unquote(path.split("?")[0])
    if path.startswith("/"):
        target = ROOT / path.lstrip("/")
    elif path:
        target = (src.parent / path)
    else:
        target = src
    target = target.resolve()
    try:
        target.relative_to(ROOT)
    except ValueError:
        errors.append(f"{src.relative_to(ROOT)}: {raw}: points outside the repository")
        return
    if not target.exists():
        errors.append(f"{src.relative_to(ROOT)}: {raw}: no such file")
        return
    if frag and target.suffix == ".md" and frag not in anchors(target):
        errors.append(f"{src.relative_to(ROOT)}: {raw}: no section #{frag} in {target.relative_to(ROOT)}")


def check_external(urls):
    errors = []
    for url in sorted(urls):
        if url.startswith(REPO_URL + "/blob/") or url.startswith(REPO_URL + "/tree/"):
            continue  # checked against the working tree
        last = None
        for attempt in range(3):
            try:
                req = urllib.request.Request(url, method="GET", headers={"User-Agent": "columbus-link-check"})
                with urllib.request.urlopen(req, timeout=30) as r:
                    last = r.status
                break
            except urllib.error.HTTPError as e:
                last = e.code
                if e.code < 400 or e.code in (401, 403) and "github.com" in url:
                    break  # a sign-in wall on github.com: the page exists
                if e.code not in (429, 500, 502, 503, 504):
                    break
            except (urllib.error.URLError, TimeoutError, OSError) as e:
                last = str(e)
            time.sleep(3 * (attempt + 1))
        ok = isinstance(last, int) and (last < 400 or (last in (401, 403) and "github.com" in url))
        print(f"  {'ok ' if ok else 'BAD'} {last} {url}")
        if not ok:
            errors.append(f"{url}: {last}")
    return errors


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--external", action="store_true")
    a = ap.parse_args()
    errors, n = [], 0
    for f in md_files():
        text = strip_code(f.read_text(encoding="utf-8"))
        for link in targets(text):
            n += 1
            check_local(f, link, errors)
    print(f"checked {n} links in Markdown files")
    if a.external:
        urls = set()
        for name in FRONT:
            text = (ROOT / name).read_text(encoding="utf-8")
            text = strip_code(text) if name.endswith(".md") else text
            urls.update(u.rstrip(".,;:") for u in BARE_URL.findall(text))
        print(f"fetching {len(urls)} external links in the front-page files")
        errors += check_external(urls)
    for e in errors:
        print("BROKEN", e)
    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
