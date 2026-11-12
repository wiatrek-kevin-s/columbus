#!/usr/bin/env python3
"""The app's icon, made from the mark the app draws in its own window.

The mark is LOGO in app/desktop/ui/js/core.js, coloured by the `svg.mark`
rules of app/desktop/ui/app.css in the light theme. From it this script
writes three drawings and, from them, every icon file Tauri bundles:

  icon.svg         macOS, on Apple's grid: a tile of 824 in 1024 with corners
                   rounded the way Apple's are (a superellipse); drawn into
                   icon.icns. The same drawing is the README's logo,
                   docs/images/logo.svg.
  icon-square.svg  Windows and Linux, 48 px and up: the tile fills the square.
  icon-small.svg   Windows and Linux, 16 to 32 px: the same, with the mark a
                   little larger and its ring bolder, so that it still reads.

  python3 make_icons.py          write the drawings and draw every size
                                 (needs Node with Playwright and its Chromium)
  python3 make_icons.py --check  check that the drawings follow the app's mark
                                 and that the icon files were drawn from them
                                 (standard library only; CI runs it)

When the mark or its colours change, run it without --check and commit what
it writes. The icon files are not to be replaced by hand: the check fails.
"""

import argparse
import hashlib
import json
import math
import re
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
CORE = ROOT / "app/desktop/ui/js/core.js"
CSS = ROOT / "app/desktop/ui/app.css"
README_LOGO = ROOT / "docs/images/logo.svg"
SUMS = HERE / "made-from.sha256"

# The tile: the light theme's panel at the top, its paper at the bottom.
TILE_TOP, TILE_BOTTOM, EDGE = "--panel", "--paper", "--ink"

# kind: (tile half-size in 1024, mark's outer diameter as a share of the tile,
#        ring stroke as a multiple of the app's)
KINDS = {
    "icon.svg": (412, 0.62, 1.0),
    "icon-square.svg": (512, 0.62, 1.0),
    "icon-small.svg": (512, 0.74, 1.375),
}

# What each icon file holds: (drawing, pixel size).
PNGS = {
    "32x32.png": ("icon-small.svg", 32),
    "64x64.png": ("icon-square.svg", 64),
    "128x128.png": ("icon-square.svg", 128),
    "128x128@2x.png": ("icon-square.svg", 256),
    "icon.png": ("icon-square.svg", 512),
}
ICO = [("icon-small.svg", 16), ("icon-small.svg", 24), ("icon-small.svg", 32),
       ("icon-square.svg", 48), ("icon-square.svg", 64), ("icon-square.svg", 256)]
# The entries of a macOS icon file, in the order Tauri's own tool writes them.
ICNS = [("ic07", 128), ("ic08", 256), ("ic09", 512), ("ic10", 1024),
        ("ic11", 32), ("ic12", 64), ("ic13", 256), ("ic14", 512)]
OUTPUTS = [*PNGS, "icon.ico", "icon.icns"]

ATTRIBUTES = {"fill", "stroke", "stroke-width", "stroke-linecap", "stroke-linejoin",
              "opacity", "fill-opacity", "stroke-opacity"}


def fail(message):
    sys.exit(f"make_icons: {message}")


def tokens(css):
    """The first value of each custom property: the light theme's, which comes first."""
    light, dark = css.find(":root"), css.find("prefers-color-scheme: dark")
    if light < 0 or (0 <= dark < light):
        fail(f"{CSS}: the light theme's :root no longer comes first")
    found = {}
    for name, value in re.findall(r"(--[a-z0-9-]+)\s*:\s*([^;{}]+);", css):
        found.setdefault(name, value.strip())
    return found


def resolve(value, colours):
    m = re.fullmatch(r"var\((--[a-z0-9-]+)\)", value)
    if not m:
        return value
    if m.group(1) not in colours:
        fail(f"{CSS}: {m.group(1)} is not set")
    return colours[m.group(1)]


def the_mark():
    """The mark's viewBox, its elements with their colours, and its ring."""
    js = CORE.read_text(encoding="utf-8")
    css = CSS.read_text(encoding="utf-8")
    colours = tokens(css)
    logo = re.search(r"const LOGO = '([^']*)';", js)
    call = re.search(r'svgFrom\("([^"]+)",\s*LOGO,\s*\{([^}]*)\}\)', js)
    if not logo or not call:
        fail(f"{CORE}: no LOGO, or logo() no longer draws it with svgFrom")
    root_fill = re.search(r'fill:\s*"([^"]+)"', call.group(2))

    def attributes(match):
        cls = match.group(1)
        rule = re.search(r"svg\.mark \." + re.escape(cls) + r"\s*\{([^}]*)\}", css)
        if not rule:
            fail(f"{CSS}: no rule for svg.mark .{cls}")
        out = []
        for declaration in filter(None, (d.strip() for d in rule.group(1).split(";"))):
            prop, value = (part.strip() for part in declaration.split(":", 1))
            if prop not in ATTRIBUTES:
                fail(f"{CSS}: svg.mark .{cls} sets {prop}, which this script does not draw")
            out.append(f'{prop}="{resolve(value, colours)}"')
        return " ".join(out)

    elements = re.sub(r'class="([a-z0-9-]+)"', attributes, logo.group(1))
    circle = re.search(r"<circle\b([^>]*)>", elements)
    ring = dict(re.findall(r'([a-z-]+)="([^"]*)"', circle.group(1))) if circle else {}
    if not {"cx", "cy", "r", "stroke-width"} <= ring.keys():
        fail(f"{CORE}: the mark has no ring (a circle with a stroke)")
    cx, cy, r, width = (float(ring[k]) for k in ("cx", "cy", "r", "stroke-width"))
    return {
        "viewbox": call.group(1),
        "fill": root_fill.group(1) if root_fill else None,
        "elements": elements,
        "ring": (cx, cy, r, width),
        "tile": (resolve(f"var({TILE_TOP})", colours), resolve(f"var({TILE_BOTTOM})", colours)),
        "edge": resolve(f"var({EDGE})", colours),
    }


def superellipse(half, n=5.0, steps=360):
    """Apple's tile: |x|^5 + |y|^5 = half^5, centred in 1024."""
    points = []
    for i in range(steps):
        phi = 2 * math.pi * i / steps
        c, s = math.cos(phi), math.sin(phi)
        radius = half / (abs(c) ** n + abs(s) ** n) ** (1 / n)
        points.append(f"{512 + radius * c:.1f} {512 + radius * s:.1f}")
    return "M" + " L".join(points) + "Z"


def drawing(name, mark):
    half, share, bold = KINDS[name]
    cx, cy, r, width = mark["ring"]
    elements = mark["elements"]
    if bold != 1.0:
        elements = re.sub(r'stroke-width="([\d.]+)"',
                          lambda m: f'stroke-width="{float(m.group(1)) * bold:g}"', elements)
    scale = share * 2 * half / (2 * (r + width * bold / 2))
    top, bottom = mark["tile"]
    fill = f' fill="{mark["fill"]}"' if mark["fill"] else ""
    return "\n".join([
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">',
        "  <!-- Made by app/desktop/src-tauri/icons/make_icons.py from the mark the app draws",
        "       (LOGO in app/desktop/ui/js/core.js, coloured as app.css colours it). Do not edit. -->",
        "  <defs>",
        '    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">',
        f'      <stop offset="0" stop-color="{top}"/>',
        f'      <stop offset="1" stop-color="{bottom}"/>',
        "    </linearGradient>",
        "  </defs>",
        f'  <path d="{superellipse(half)}" fill="url(#tile)"/>',
        f'  <path d="{superellipse(half - 1)}" fill="none" stroke="{mark["edge"]}" stroke-opacity="0.12" stroke-width="2"/>',
        f'  <g transform="translate({512 - cx * scale:.3f} {512 - cy * scale:.3f}) scale({scale:.4f})"{fill}>',
        f"    {elements}",
        "  </g>",
        "</svg>",
        "",
    ])


def drawings():
    mark = the_mark()
    return {name: drawing(name, mark) for name in KINDS}


def digest(path):
    data = path.read_bytes()
    if path.suffix == ".svg":
        data = data.replace(b"\r\n", b"\n")
    return hashlib.sha256(data).hexdigest()


def sums_text():
    return "".join(f"{digest(HERE / name)}  {name}\n" for name in [*KINDS, *OUTPUTS])


def render(jobs):
    """Draw each (drawing, size) with Chromium, through Node's Playwright."""
    script = r"""
const { chromium } = require('playwright');
const fs = require('fs');
(async () => {
  const jobs = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1024, height: 1024 }, deviceScaleFactor: 1 });
  for (const j of jobs) {
    const svg = fs.readFileSync(j.svg).toString('base64');
    await page.setContent('<!doctype html><body style="margin:0;background:transparent">' +
      `<img id="i" width="${j.size}" height="${j.size}" style="display:block" src="data:image/svg+xml;base64,${svg}">`);
    await page.waitForFunction(() => { const i = document.getElementById('i'); return i.complete && i.naturalWidth > 0; });
    await page.screenshot({ path: j.out, omitBackground: true, clip: { x: 0, y: 0, width: j.size, height: j.size } });
  }
  await browser.close();
})().catch((e) => { console.error(e); process.exit(1); });
"""
    if not shutil.which("node"):
        fail("drawing the icons needs Node with Playwright (npm install playwright)")
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        (tmp / "render.js").write_text(script, encoding="utf-8")
        listed = []
        for svg, size in sorted(set(jobs)):
            listed.append({"svg": str(HERE / svg), "size": size, "out": str(tmp / f"{svg}-{size}.png")})
        (tmp / "jobs.json").write_text(json.dumps(listed), encoding="utf-8")
        subprocess.run(["node", str(tmp / "render.js"), str(tmp / "jobs.json")], check=True)
        return {(Path(j["svg"]).name, j["size"]): Path(j["out"]).read_bytes() for j in listed}


def pack_ico(images):
    head = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, data = b"", b""
    for size, png in images:
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), offset + len(data))
        data += png
    return head + entries + data


def pack_icns(images):
    toc = b"TOC " + struct.pack(">I", 8 + 8 * len(images))
    toc += b"".join(kind.encode() + struct.pack(">I", 8 + len(png)) for kind, png in images)
    body = toc + b"".join(kind.encode() + struct.pack(">I", 8 + len(png)) + png for kind, png in images)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def make():
    for name, text in drawings().items():
        (HERE / name).write_text(text, encoding="utf-8", newline="\n")
    README_LOGO.write_text((HERE / "icon.svg").read_text(encoding="utf-8"), encoding="utf-8", newline="\n")
    jobs = [*PNGS.values(), *ICO, *(("icon.svg", size) for _, size in ICNS)]
    drawn = render(jobs)
    for name, job in PNGS.items():
        (HERE / name).write_bytes(drawn[job])
    (HERE / "icon.ico").write_bytes(pack_ico([(size, drawn[(svg, size)]) for svg, size in ICO]))
    (HERE / "icon.icns").write_bytes(pack_icns([(kind, drawn[("icon.svg", size)]) for kind, size in ICNS]))
    SUMS.write_text(sums_text(), encoding="utf-8", newline="\n")
    print(f"wrote {', '.join([*KINDS, *OUTPUTS])}, {SUMS.name} and {README_LOGO.relative_to(ROOT)}")


def check():
    problems = []
    expected = drawings()
    for name, text in expected.items():
        path = HERE / name
        if not path.exists() or path.read_text(encoding="utf-8").replace("\r\n", "\n") != text:
            problems.append(f"{name} does not follow the app's mark as it is now")
    if README_LOGO.read_text(encoding="utf-8").replace("\r\n", "\n") != expected["icon.svg"]:
        problems.append(f"{README_LOGO.relative_to(ROOT)} is not icon.svg")
    if not problems and (not SUMS.exists() or SUMS.read_text(encoding="utf-8").replace("\r\n", "\n") != sums_text()):
        problems.append("the icon files were not drawn from these drawings (or were changed by hand)")
    if problems:
        fail("; ".join(problems) + ". Run python3 app/desktop/src-tauri/icons/make_icons.py and commit what it writes.")
    print("the app's icons are drawn from the mark the app draws")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true", help="check only; change nothing")
    if parser.parse_args().check:
        check()
    else:
        make()
