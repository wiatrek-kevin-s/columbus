"""Builds the README pictures' sample data from public US government data:
a small extract of central Austin, Texas, written to austin.gpkg beside this
script. Run once, on a machine with internet (the "README pictures"
workflow does it on GitHub's runners); the result is committed.

    python app/desktop/e2e/demo/fetch_austin.py [--out app/desktop/e2e/demo/austin.gpkg]

Sources (both US government works, free of copyright, 17 U.S.C. 105; see
docs/images/DATA-CREDITS.md):
- US Census Bureau, 2024 TIGER/Line Shapefiles: roads and faces (census
  blocks) of Travis County, Texas (FIPS 48453).
- NHTSA Fatality Analysis Reporting System (FARS), 2020-2023: fatal crashes,
  the accident file, kept where they fall in the extract.

Layers written: roads (lines), census_blocks (polygons), fatal_crashes
(points), all in EPSG:4326, clipped to BBOX.
"""

import argparse
import csv
import io
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

# Central Austin: downtown, the university, East Austin, and IH 35 and
# MoPac through them (lon/lat, WGS 84).
BBOX = (-97.790, 30.220, -97.670, 30.340)
TIGER = "https://www2.census.gov/geo/tiger/TIGER2024/{layer}/tl_2024_48453_{name}.zip"
FARS = "https://static.nhtsa.gov/nhtsa/downloads/FARS/{year}/National/FARS{year}NationalCSV.zip"
YEARS = [2020, 2021, 2022, 2023]
UA = {"User-Agent": "columbus-readme-demo-data/1.0 (+https://github.com/wiatrek-kevin-s/columbus)"}


def get(url, dest):
    print("fetching", url, flush=True)
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=300) as r, open(dest, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
    return dest


def tiger(tmp, layer, name):
    import geopandas as gpd

    z = get(TIGER.format(layer=layer, name=name), tmp / f"{name}.zip")
    df = gpd.read_file(f"zip://{z}", bbox=BBOX, engine="pyogrio")
    return df.to_crs("EPSG:4326")


def fars(tmp):
    rows = []
    for year in YEARS:
        z = get(FARS.format(year=year), tmp / f"fars{year}.zip")
        with zipfile.ZipFile(z) as zf:
            name = next(n for n in zf.namelist() if n.lower().rsplit("/", 1)[-1] == "accident.csv")
            text = io.TextIOWrapper(zf.open(name), encoding="latin-1")
            for raw in csv.DictReader(text):
                # Some years start with a byte-order mark (read as latin-1).
                r = {k.replace("\ufeff", "").replace("ï»¿", "").strip().upper(): v for k, v in raw.items() if k}
                if r["STATE"] != "48" or r["COUNTY"] != "453":
                    continue
                try:
                    lat, lon = float(r["LATITUDE"]), float(r["LONGITUD"])
                except ValueError:
                    continue
                if not (BBOX[0] <= lon <= BBOX[2] and BBOX[1] <= lat <= BBOX[3]):
                    continue
                rows.append({
                    "case_id": f"{year}-{r['ST_CASE']}",
                    "crash_date": f"{int(r['YEAR']):04d}-{int(r['MONTH']):02d}-{int(r['DAY']):02d}",
                    "year": int(r["YEAR"]),
                    "fatalities": int(r["FATALS"]),
                    "persons": int(r.get("PERSONS") or r.get("PERNOTMVIT") or 0),
                    "trafficway": (r.get("TWAY_ID") or "").strip(),
                    "lon": round(lon, 6),
                    "lat": round(lat, 6),
                })
        print(year, "crashes so far:", len(rows), flush=True)
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(Path(__file__).with_name("austin.gpkg")))
    out = Path(ap.parse_args().out)
    import geopandas as gpd
    from shapely.geometry import box

    clip = box(*BBOX)
    with tempfile.TemporaryDirectory() as t:
        tmp = Path(t)
        roads = tiger(tmp, "ROADS", "roads")
        roads = roads.clip(clip)
        roads = roads[~roads.geometry.is_empty]
        roads = roads.rename(columns={"LINEARID": "linear_id", "FULLNAME": "street_name", "RTTYP": "route_type", "MTFCC": "feature_class"})
        roads = roads[["linear_id", "street_name", "route_type", "feature_class", "geometry"]]
        roads["geometry"] = roads.geometry.simplify(0.00001)
        print("roads:", len(roads))

        faces = tiger(tmp, "FACES", "faces")
        faces = faces[faces["BLOCKCE20"].notna()]
        faces["block_id"] = faces["STATEFP20"] + faces["COUNTYFP20"] + faces["TRACTCE20"] + faces["BLOCKCE20"]
        blocks = faces[["block_id", "TRACTCE20", "geometry"]].dissolve(by="block_id", as_index=False)
        blocks = blocks.rename(columns={"TRACTCE20": "tract"})
        blocks = blocks.clip(clip)
        blocks = blocks[~blocks.geometry.is_empty]
        blocks["geometry"] = blocks.geometry.simplify(0.00001).make_valid()
        blocks = blocks[blocks.geom_type.isin(["Polygon", "MultiPolygon"])]
        print("census blocks:", len(blocks))

        rows = fars(tmp)
        crashes = gpd.GeoDataFrame(rows, geometry=gpd.points_from_xy([r["lon"] for r in rows], [r["lat"] for r in rows]), crs="EPSG:4326")
        print("fatal crashes:", len(crashes))
        if len(roads) < 1000 or len(blocks) < 300 or len(crashes) < 20:
            sys.exit("the extract is smaller than expected; check the sources")

        if out.exists():
            out.unlink()
        roads.to_file(out, layer="roads", driver="GPKG", engine="pyogrio")
        blocks.to_file(out, layer="census_blocks", driver="GPKG", engine="pyogrio")
        crashes.to_file(out, layer="fatal_crashes", driver="GPKG", engine="pyogrio")
    print(f"wrote {out} ({out.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
