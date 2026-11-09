"""Writes docs/images/social-card.png (1280x640): the repository's social
preview (Settings > General > Social preview; uploaded by hand). Logo, name,
pitch and a slice of the hero picture, drawn by headless Chrome.

    python app/desktop/e2e/demo/social_card.py /usr/bin/google-chrome
"""

import base64
import subprocess
import sys
import tempfile
from pathlib import Path

IMAGES = Path(__file__).resolve().parents[4] / "docs" / "images"
CHROME = sys.argv[1] if len(sys.argv) > 1 else "google-chrome"


def data_url(p, kind):
    return f"data:{kind};base64," + base64.b64encode(p.read_bytes()).decode()


HTML = f"""<!doctype html><html><head><meta charset="utf-8"><style>
html, body {{ margin: 0; width: 1280px; height: 640px; overflow: hidden; }}
body {{ background: linear-gradient(135deg, #1f4266 0%, #17324d 60%, #10263b 100%); color: #fff;
  font-family: Inter, -apple-system, 'Segoe UI', 'Noto Sans', Helvetica, Arial, sans-serif; position: relative; }}
.text {{ position: absolute; left: 72px; top: 92px; width: 520px; }}
.brand {{ display: flex; align-items: center; gap: 22px; }}
.brand img {{ width: 104px; height: 104px; margin-left: -12px; }}
h1 {{ font-size: 76px; font-weight: 750; letter-spacing: -1.5px; margin: 0; }}
p.pitch {{ font-size: 37px; line-height: 1.28; font-weight: 500; margin: 44px 0 0; color: #f3f6f7; }}
p.foot {{ font-size: 22px; color: #9fc4ea; margin: 40px 0 0; }}
.shot {{ position: absolute; left: 650px; top: 64px; width: 700px; height: 520px; border-radius: 16px; overflow: hidden;
  box-shadow: 0 24px 60px rgba(0,0,0,.45); border: 1px solid rgba(255,255,255,.18); }}
.shot img {{ display: block; width: 1040px; margin-left: -180px; }}
</style></head><body>
<div class="text">
  <div class="brand"><img src="{data_url(IMAGES / 'logo.svg', 'image/svg+xml')}" alt=""><h1>Columbus</h1></div>
  <p class="pitch">Let AI work on your organization's GIS data, under your rules, on your computer.</p>
  <p class="foot">Oracle · SQL Server · file geodatabases · ArcGIS · Excel</p>
</div>
<div class="shot"><img src="{data_url(IMAGES / 'hero-light.png', 'image/png')}" alt=""></div>
</body></html>"""

with tempfile.TemporaryDirectory() as t:
    page = Path(t) / "card.html"
    page.write_text(HTML)
    out = IMAGES / "social-card.png"
    subprocess.run([CHROME, "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
                    f"--screenshot={out}", "--window-size=1280,640", page.as_uri()], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print(out, out.stat().st_size // 1024, "KB")
