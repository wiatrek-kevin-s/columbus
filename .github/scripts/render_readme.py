"""Renders README.md the way github.com does (GitHub's own Markdown API and
its Markdown CSS) and takes screenshots at laptop (1280) and phone (390)
widths, light and dark, with Playwright. For checking the front page.

    GH_TOKEN=… python .github/scripts/render_readme.py <github-markdown-css folder> <out folder>
"""

import json
import os
import sys
import urllib.request
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
CSS = Path(sys.argv[1])
OUT = Path(sys.argv[2]).resolve()
OUT.mkdir(parents=True, exist_ok=True)

req = urllib.request.Request(
    "https://api.github.com/markdown",
    data=json.dumps({"text": (ROOT / "README.md").read_text(encoding="utf-8"), "mode": "gfm",
                     "context": "wiatrek-kevin-s/columbus"}).encode(),
    headers={"Authorization": f"Bearer {os.environ['GH_TOKEN']}", "Accept": "application/vnd.github+json"},
    method="POST")
body = urllib.request.urlopen(req, timeout=60).read().decode()

for scheme in ["light", "dark"]:
    css = (CSS / f"github-markdown-{scheme}.css").read_text()
    bg = "#0d1117" if scheme == "dark" else "#ffffff"
    page = f"""<!doctype html><html><head><meta charset="utf-8"><base href="{ROOT.as_uri()}/">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>{css}
body {{ background: {bg}; margin: 0; }}
.box {{ box-sizing: border-box; max-width: 900px; margin: 24px auto; border: 1px solid {'#3d444d' if scheme == 'dark' else '#d1d9e0'}; border-radius: 6px; }}
.head {{ padding: 8px 16px; border-bottom: 1px solid {'#3d444d' if scheme == 'dark' else '#d1d9e0'}; font: 600 14px -apple-system, 'Segoe UI', sans-serif; color: {'#f0f6fc' if scheme == 'dark' else '#1f2328'}; }}
.markdown-body {{ padding: 32px; }}
@media (max-width: 600px) {{ .box {{ margin: 0; border: 0; }} .markdown-body {{ padding: 16px; }} }}
</style></head><body><div class="box"><div class="head">README</div><article class="markdown-body">{body}</article></div></body></html>"""
    f = OUT / f"readme-{scheme}.html"
    f.write_text(page, encoding="utf-8")
    with sync_playwright() as p:
        b = p.chromium.launch()
        for name, w, h, dsf in [("laptop", 1280, 800, 1), ("phone", 390, 844, 2)]:
            ctx = b.new_context(viewport={"width": w, "height": h}, device_scale_factor=dsf, color_scheme=scheme)
            pg = ctx.new_page()
            pg.goto(f.as_uri(), wait_until="networkidle")
            pg.wait_for_timeout(1500)
            pg.screenshot(path=str(OUT / f"{name}-{scheme}-first-screen.png"))
            pg.screenshot(path=str(OUT / f"{name}-{scheme}-full.png"), full_page=True)
            print("wrote", name, scheme)
            ctx.close()
        b.close()
