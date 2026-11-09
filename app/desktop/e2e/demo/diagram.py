"""Writes docs/images/how-it-works-light.svg and -dark.svg: the README's
"How it works" diagram (AI app -> Columbus -> your data and your Python).
Hand-laid SVG; run again after editing the words or colours."""

from pathlib import Path

OUT = Path(__file__).resolve().parents[4] / "docs" / "images"

THEMES = {
    "light": dict(text="#1f2328", muted="#59636e", box="#f6f8fa", line="#d1d9e0", accent="#0969da", accent_bg="#ddf4ff",
                  ok="#1a7f37", ok_bg="#dafbe1", arrow="#59636e"),
    "dark": dict(text="#f0f6fc", muted="#9198a1", box="#151b23", line="#3d444d", accent="#4493f8", accent_bg="#0c2d6b",
                 ok="#3fb950", ok_bg="#0f2d1a", arrow="#9198a1"),
}


def svg(c):
    font = "-apple-system,BlinkMacSystemFont,'Segoe UI','Noto Sans',Helvetica,Arial,sans-serif"

    def box(x, y, w, h, fill, stroke, r=10):
        return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}" stroke="{stroke}" stroke-width="1.5"/>'

    def text(x, y, s, size=14, weight=400, fill=None, anchor="start"):
        return f'<text x="{x}" y="{y}" font-size="{size}" font-weight="{weight}" fill="{fill or c["text"]}" text-anchor="{anchor}">{s}</text>'

    def lines(x, y, items, size=13, gap=19, fill=None):
        return "".join(text(x, y + i * gap, s, size, fill=fill or c["muted"]) for i, s in enumerate(items))

    def arrow(x1, y1, x2, y2, label, ly):
        return (f'<line x1="{x1}" y1="{y1}" x2="{x2 - 8}" y2="{y2}" stroke="{c["arrow"]}" stroke-width="2"/>'
                f'<path d="M{x2} {y2} l-10 -6 v12 z" fill="{c["arrow"]}"/>'
                + text((x1 + x2) / 2, ly, label, 12, fill=c["muted"], anchor="middle"))

    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 920 330" width="920" height="330" font-family="{font}" role="img" '
             f'aria-labelledby="t d"><title id="t">How Columbus works</title><desc id="d">Your AI app asks Columbus. Columbus checks every request '
             f'against your organization\'s rules, asks you before anything is exported or changed, and logs every step. Only then does it read '
             f'your data or run your Python, on your computer.</desc>']
    # 1. The AI app.
    parts.append(box(10, 95, 200, 140, c["box"], c["line"]))
    parts.append(text(110, 128, "Your AI app", 16, 600, anchor="middle"))
    parts.append(lines(30, 158, ["Columbus's own chat,", "Claude, Copilot, or any", "app that speaks MCP"]))
    parts.append(arrow(210, 165, 290, 165, "asks", 155))
    # 2. Columbus.
    parts.append(box(290, 20, 310, 290, c["accent_bg"], c["accent"], 14))
    parts.append(text(445, 52, "Columbus", 18, 700, c["accent"], "middle"))
    parts.append(text(445, 72, "on your computer", 12, fill=c["muted"], anchor="middle"))
    rows = [("Rules", ["Read-only by default; only the rows", "and fields your organization allows"]),
            ("Approvals", ["Exports, edits and scripts", "wait for your yes"]),
            ("Checks and log", ["Every result checked,", "every step written down"])]
    for i, (title, desc) in enumerate(rows):
        y = 86 + i * 74
        parts.append(box(308, y, 274, 66, c["box"], c["line"], 8))
        parts.append(f'<circle cx="328" cy="{y + 20}" r="7" fill="{c["ok_bg"]}" stroke="{c["ok"]}" stroke-width="1.5"/>'
                     f'<path d="M324.5 {y + 20} l2.5 2.5 l4.5 -5" stroke="{c["ok"]}" stroke-width="1.8" fill="none"/>')
        parts.append(text(344, y + 25, title, 14, 600))
        parts.append(lines(324, y + 44, desc[:1], 12, fill=c["muted"]))
        parts.append(text(324 + 0, y + 58, desc[1], 12, fill=c["muted"]) if len(desc) > 1 else "")
    parts.append(arrow(600, 105, 680, 105, "reads", 95))
    parts.append(arrow(600, 240, 680, 240, "runs", 230))
    # 3. Data and Python.
    parts.append(box(680, 20, 230, 165, c["box"], c["line"]))
    parts.append(text(700, 48, "Your data", 16, 600))
    parts.append(lines(700, 74, ["Oracle and SQL Server", "File geodatabases, shapefiles", "ArcGIS Online and Portal", "CSV, Excel, GeoPackage", "Rasters, PostGIS"], 12, 19))
    parts.append(box(680, 195, 230, 115, c["box"], c["line"]))
    parts.append(text(700, 223, "Your Python", 16, 600))
    parts.append(lines(700, 249, ["The GIS engine (GeoPandas,", "GDAL), your environments,", "ArcGIS Pro's arcpy (experimental)"], 12, 19))
    parts.append("</svg>")
    return "".join(parts)


for name, c in THEMES.items():
    p = OUT / f"how-it-works-{name}.svg"
    p.write_text(svg(c) + "\n")
    print(p, p.stat().st_size, "bytes")
