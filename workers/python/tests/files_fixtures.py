"""Fixtures for ``files`` connections, one per format, made at test time
(no binaries in the repository). Used by test_files.py and, through
``python files_fixtures.py <folder>``, by the gateway's tests/files.rs.

- ``inventory.xlsx`` (written with the standard library, so formulas,
  date formats and a header-only sheet are exactly as Excel saves them):
  sheet ``2025`` with ID, CSJ (text ``000123456``), Date (date-formatted),
  Serial (General serial number), Total (formula with a saved value),
  lon/lat, Name twice, Milepost (text ``12.500``); sheet ``2026`` header
  only; sheet ``Notes`` empty.
- ``roads.geojson``, ``crashes.fgb`` (EPSG:4326 points) via pyogrio.
- ``parcels.parquet`` (EPSG:2277 polygons) via geopandas.
- ``places.kml`` with two folders, ``Parks`` and ``Schools``.
- ``legacy.csv`` in Windows-1252, ``dupes.csv`` with repeated headers,
  ``empty.csv`` header only, ``settings.json`` that isn't GeoJSON.
"""

from __future__ import annotations

import os
import sys
import zipfile
from xml.sax.saxutils import escape

_MAIN = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"


def write_xlsx(path: str, sheets: list[tuple[str, list[list]]]) -> None:
    """A minimal .xlsx. Each cell is None, a str (inline string), a number,
    or a dict: {"v": number, "date": True} (date format), {"v": number,
    "f": "A2*2"} (formula with a saved value), {"f": "A2*2"} (formula never
    calculated), {"s": text} (shared string)."""
    shared: list[str] = []

    def ref(c: int, r: int) -> str:
        letters = ""
        c += 1
        while c:
            c, rem = divmod(c - 1, 26)
            letters = chr(65 + rem) + letters
        return f"{letters}{r}"

    def cell(c: int, r: int, v) -> str:
        at = ref(c, r)
        if v is None:
            return ""
        if isinstance(v, str):
            return f'<c r="{at}" t="inlineStr"><is><t>{escape(v)}</t></is></c>'
        if isinstance(v, dict):
            if "s" in v:
                shared.append(v["s"])
                return f'<c r="{at}" t="s"><v>{len(shared) - 1}</v></c>'
            style = ' s="1"' if v.get("date") else ""
            f = f"<f>{escape(v['f'])}</f>" if "f" in v else ""
            val = f"<v>{v['v']}</v>" if "v" in v else ""
            return f'<c r="{at}"{style}>{f}{val}</c>'
        return f'<c r="{at}"><v>{v}</v></c>'

    parts = {}
    for i, (_name, rows) in enumerate(sheets, 1):
        body = "".join(f'<row r="{r}">' + "".join(cell(c, r, v) for c, v in enumerate(row)) + "</row>"
                       for r, row in enumerate(rows, 1))
        parts[f"xl/worksheets/sheet{i}.xml"] = (
            f'<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="{_MAIN}"><sheetData>{body}</sheetData></worksheet>')
    n = len(sheets)
    parts["[Content_Types].xml"] = (
        '<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        + "".join(f'<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
                  for i in range(1, n + 1)) + "</Types>")
    parts["_rels/.rels"] = (
        '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
        "</Relationships>")
    parts["xl/workbook.xml"] = (
        f'<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="{_MAIN}" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>'
        + "".join(f'<sheet name="{escape(name)}" sheetId="{i}" r:id="rId{i}"/>' for i, (name, _) in enumerate(sheets, 1))
        + "</sheets></workbook>")
    rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
    parts["xl/_rels/workbook.xml.rels"] = (
        '<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        + "".join(f'<Relationship Id="rId{i}" Type="{rel}/worksheet" Target="worksheets/sheet{i}.xml"/>' for i in range(1, n + 1))
        + f'<Relationship Id="rId{n + 1}" Type="{rel}/styles" Target="styles.xml"/>'
        + f'<Relationship Id="rId{n + 2}" Type="{rel}/sharedStrings" Target="sharedStrings.xml"/>'
        + "</Relationships>")
    parts["xl/styles.xml"] = (
        f'<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="{_MAIN}">'
        '<numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy\\-mm\\-dd;@"/></numFmts>'
        '<cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="164" applyNumberFormat="1"/></cellXfs></styleSheet>')
    parts["xl/sharedStrings.xml"] = (
        f'<?xml version="1.0" encoding="UTF-8"?><sst xmlns="{_MAIN}" count="{len(shared)}">'
        + "".join(f"<si><t>{escape(s)}</t></si>" for s in shared) + "</sst>")
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, text in parts.items():
            z.writestr(name, text)


def inventory_xlsx(path: str) -> None:
    # 46334 is 2026-11-08.
    write_xlsx(path, [
        ("2025", [
            ["ID", "CSJ", "Date", "Serial", "Total", "lon", "lat", "Name", "Name", "Milepost"],
            [1, {"s": "000123456"}, {"v": 46334, "date": True}, 46334, {"v": 12.5, "f": "A2*12.5"},
             -97.74, 30.27, "a", "b", "12.500"],
            [2, "000123457", {"v": 46335, "date": True}, 46335, {"v": 25, "f": "A3*12.5"},
             -97.75, 30.28, "c", "d", "1.50"],
        ]),
        ("2026", [["ID", "CSJ"]]),
        ("Notes", []),
    ])


KML = """<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2"><Document><name>places</name>
<Folder><name>Parks</name>
<Placemark><name>Zilker</name><Point><coordinates>-97.77,30.27,0</coordinates></Point></Placemark>
<Placemark><name>Pease</name><Point><coordinates>-97.75,30.28,0</coordinates></Point></Placemark>
</Folder>
<Folder><name>Schools</name>
<Placemark><name>Austin High</name><Point><coordinates>-97.76,30.27,0</coordinates></Point></Placemark>
</Folder>
</Document></kml>
"""


def write_all(folder: str) -> None:
    import geopandas as gpd
    from shapely.geometry import LineString, Point, Polygon

    os.makedirs(folder, exist_ok=True)
    inventory_xlsx(os.path.join(folder, "inventory.xlsx"))
    roads = gpd.GeoDataFrame({"ROAD": ["R1", "R2"], "LANES": [2, 4]},
                             geometry=[LineString([(-97.74, 30.26), (-97.73, 30.27)]),
                                       LineString([(-97.72, 30.28), (-97.71, 30.29)])], crs="EPSG:4326")
    roads.to_file(os.path.join(folder, "roads.geojson"), driver="GeoJSON", engine="pyogrio")
    crashes = gpd.GeoDataFrame({"CRASH_ID": [1, 2, 3], "SEVERITY": ["A", "B", "A"]},
                               geometry=[Point(-97.74, 30.27), Point(-97.75, 30.28), Point(-97.76, 30.29)],
                               crs="EPSG:4326")
    crashes.to_file(os.path.join(folder, "crashes.fgb"), driver="FlatGeobuf", engine="pyogrio")
    parcels = gpd.GeoDataFrame({"PARCEL": ["P1"], "ACRES": [1.25]},
                               geometry=[Polygon([(3110000, 10070000), (3110100, 10070000), (3110100, 10070100)])],
                               crs="EPSG:2277")
    parcels.to_parquet(os.path.join(folder, "parcels.parquet"))
    with open(os.path.join(folder, "places.kml"), "w", encoding="utf-8") as f:
        f.write(KML)
    with open(os.path.join(folder, "legacy.csv"), "wb") as f:
        f.write("ID,Name\n1,Café\n".encode("cp1252"))
    with open(os.path.join(folder, "dupes.csv"), "w", encoding="utf-8", newline="") as f:
        f.write("ID,Name,Name,\n1,a,b,c\n")
    with open(os.path.join(folder, "empty.csv"), "w", encoding="utf-8", newline="") as f:
        f.write("ID,Name\n")
    with open(os.path.join(folder, "settings.json"), "w", encoding="utf-8") as f:
        f.write('{"theme": "dark", "zoom": 3}\n')


if __name__ == "__main__":
    write_all(sys.argv[1])
