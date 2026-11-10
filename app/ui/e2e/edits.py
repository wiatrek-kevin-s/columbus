"""Edits in the browser, end to end: the agent proposes a change set, the
preview shows in the companion window, and applying it to the source puts
an edit card on the approvals page listing every change, with no "for this
session" button. The person allows it and the GeoPackage is written. Then
the same on the server's built-in page (no UI folder beside it): the card
lists every change there too, and declining writes nothing.

    python app/ui/e2e/edits.py target/debug/columbus-mcp
"""

import json
import os
import sys
import tempfile
import threading
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

REPO = Path(__file__).resolve().parents[3]
OUT = Path(__file__).resolve().parent / "out"
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mcp_client import Mcp, wait_file  # noqa: E402
import a11y  # noqa: E402


def main():
    exe = Path(sys.argv[1] if len(sys.argv) > 1 else REPO / "target/debug/columbus-mcp")
    ui = REPO / "app/ui/dist"
    assert (ui / "index.html").exists(), "build the UI first: npm run build"
    OUT.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp())
    import geopandas as gpd
    from shapely.geometry import box

    gpd.GeoDataFrame(
        {"NAME": ["A", "B", "C"], "district": [14, 14, 14]},
        geometry=[box(-97.9 + i * 0.05, 30.2, -97.86 + i * 0.05, 30.24) for i in range(3)],
        crs="EPSG:4326",
    ).to_crs("EPSG:3081").to_file(root / "t.gpkg", layer="tracts", driver="GPKG", engine="pyogrio")
    (root / "connections").mkdir()
    (root / "policy.yaml").write_text(
        "connection_types: [gpkg]\nallow_edits: true\napprovals: { page: true, wait_seconds: 60 }\n"
    )
    (root / "connections/tiger.yaml").write_text(
        f"id: tiger\ntype: gpkg\npath: {json.dumps(str(root / 't.gpkg'))}\nmode: read-write\n"
        "datasets:\n  allow: [\"*\"]\n"
    )
    env = dict(os.environ,
               COLUMBUS_POLICY=str(root / "policy.yaml"), COLUMBUS_CONNECTIONS=str(root / "connections"),
               COLUMBUS_WORKSPACES=str(root / "ws"), COLUMBUS_AUDIT_DIR=str(root / "audit"),
               COLUMBUS_LIVE_DIR=str(root / "live"), COLUMBUS_UI_DIR=str(ui),
               COLUMBUS_WORKER=str(REPO / "workers/python/columbus_worker.py"), COLUMBUS_PYTHON=sys.executable,
               COLUMBUS_APPROVALS_LAUNCH_FILE=str(root / "launch.txt"), COLUMBUS_LOG="warn")
    mcp = Mcp([str(exe), "serve"], env)
    try:
        mcp.tool("whoami", {})
        p = mcp.tool("propose_edits", {"connection": "tiger", "dataset": "tracts", "changes": [
            {"op": "update", "key": 1, "values": {"NAME": "Riverside"}},
            {"op": "delete", "key": 3},
            {"op": "insert", "values": {"NAME": "New tract", "district": 14},
             "geometry": {"type": "Polygon", "coordinates": [[[-97.7, 30.3], [-97.66, 30.3], [-97.66, 30.34], [-97.7, 30.34], [-97.7, 30.3]]]}},
        ]})
        change = p["result"]["change"]
        assert p["result"]["counts"] == {"update": 1, "insert": 1, "delete": 1}, p

        result = {}
        t = threading.Thread(target=lambda: result.update(
            r=mcp.tool("apply_edits", {"change": change, "target": "source"}, timeout=90)))
        t.start()
        link = wait_file(root / "launch.txt")
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1100, "height": 900})
            problems = []
            page.on("pageerror", lambda e: problems.append(str(e)))
            page.goto(link)
            card = page.locator("article.approval")
            expect(card).to_be_visible(timeout=20000)
            text = card.inner_text()
            assert "Write 1 update, 1 insertion and 1 deletion to tracts on tiger" in text, text
            assert 'NAME "A" → "Riverside"' in text, text
            assert 'Delete fid 3: NAME "C", district 14' in text, text
            assert 'New row: NAME "New tract", district 14' in text, text
            expect(card.get_by_role("button", name="Allow for this session")).to_have_count(0)
            expect(card).to_contain_text("one change set at a time")
            allow = card.get_by_role("button", name="Allow once")
            expect(allow).to_be_enabled(timeout=3000)
            assert not a11y.unnamed(page), a11y.unnamed(page)  # every control has a name, and Tab reaches it
            page.screenshot(path=str(OUT / "edit-approval.png"), full_page=True)
            allow.click()
            t.join(60)
            assert result.get("r", {}).get("result", {}).get("status") == "written", result
            assert not problems, problems
            browser.close()
        rows = gpd.read_file(root / "t.gpkg", layer="tracts", engine="pyogrio")
        assert sorted(rows["NAME"]) == ["B", "New tract", "Riverside"], list(rows["NAME"])
    finally:
        mcp.close()

    # The server's own page, when no UI folder is installed beside it.
    assert not (exe.parent / "ui").exists(), "this part needs a server with no ui/ folder beside it"
    plain = {k: v for k, v in env.items() if k != "COLUMBUS_UI_DIR"}
    plain.update(COLUMBUS_LIVE_DIR=str(root / "live2"), COLUMBUS_APPROVALS_LAUNCH_FILE=str(root / "launch2.txt"))
    mcp = Mcp([str(exe), "serve"], plain)
    try:
        mcp.tool("whoami", {})
        p = mcp.tool("propose_edits", {"connection": "tiger", "dataset": "tracts", "changes": [
            {"op": "update", "key": 2, "values": {"NAME": "Lakeside"}},
        ]})
        result = {}
        t = threading.Thread(target=lambda: result.update(r=mcp.request(
            "tools/call", {"name": "apply_edits", "arguments": {"change": p["result"]["change"], "target": "source"}}, 90)))
        t.start()
        link = wait_file(root / "launch2.txt")
        with sync_playwright() as pw:
            browser = pw.chromium.launch()
            page = browser.new_page(viewport={"width": 1100, "height": 900})
            problems = []
            page.on("pageerror", lambda e: problems.append(str(e)))
            page.goto(link)
            card = page.locator(".card.waiting")
            expect(card).to_be_visible(timeout=20000)
            # Every change, as a line of text in the card's own box.
            expect(card.locator("pre.code .ln")).to_have_count(1)
            box_text = card.locator("pre.code").inner_text()
            assert 'NAME "B" → "Lakeside"' in box_text, box_text
            assert "undefined" not in card.inner_text(), card.inner_text()
            expect(card.get_by_role("button", name="Allow for this session")).to_have_count(0)
            decline = card.get_by_role("button", name="Decline")
            expect(decline).to_be_enabled(timeout=3000)
            assert not a11y.unnamed(page), a11y.unnamed(page)  # every control has a name, and Tab reaches it
            page.screenshot(path=str(OUT / "edit-approval-plain.png"), full_page=True)
            decline.click()
            t.join(60)
            assert "the person declined" in json.dumps(result.get("r", {})), result
            assert not problems, problems
            browser.close()
        rows = gpd.read_file(root / "t.gpkg", layer="tracts", engine="pyogrio")
        assert sorted(rows["NAME"]) == ["B", "New tract", "Riverside"], "a declined change set writes nothing: " + str(list(rows["NAME"]))
        print("edits e2e: ok")
    finally:
        mcp.close()


if __name__ == "__main__":
    main()
