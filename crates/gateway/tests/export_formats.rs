//! export_output's file geodatabase (R-8, REQ-5) and GeoTIFF (R-9, REQ-6)
//! formats, through the real `columbus-mcp` binary and Python worker: a
//! points layer to a new `.gdb`, a 64-bit value kept exact, an existing
//! `.gdb` never touched, a slope result to a Cloud-Optimized GeoTIFF, a
//! delivery CRS that needs reprojecting first, and a vector layer refused
//! as `tif`.
//!
//! Needs Python with geopandas, pyogrio, shapely, pyproj and rasterio;
//! skipped otherwise.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const BIG: i64 = 3_000_000_000_123;

fn python(import: &str) -> Option<String> {
    support::python_for(import)
}

/// Five crash points in EPSG:3081, one with a parcel id beyond 32 bits, and
/// a DEM in EPSG:3081 (50 m cells, nodata -9999) rising toward the east.
fn make_fixture(py: &str, folder: &Path) {
    let script = r#"
import sys, os
import numpy as np, rasterio, geopandas as gpd
from rasterio.transform import from_origin
from shapely.geometry import Point
folder = sys.argv[1]
big = int(sys.argv[2])
gpd.GeoDataFrame(
    {"crash_id": [f"C{i}" for i in range(5)], "injuries": np.array([0, 1, 2, 0, 3], dtype="int64"),
     "parcel_id": np.array([1, 2, big, 4, 5], dtype="int64")},
    geometry=[Point(1_480_000 + 100 * i, 7_250_000) for i in range(5)], crs="EPSG:3081",
).to_file(os.path.join(folder, "crashes.gpkg"), layer="crashes", driver="GPKG", engine="pyogrio")
os.makedirs(os.path.join(folder, "rasters"), exist_ok=True)
z = (150.0 + 2.5 * np.arange(60, dtype="float64"))[np.newaxis, :].repeat(40, axis=0).astype("float32")
z[10:12, 10:12] = -9999
with rasterio.open(os.path.join(folder, "rasters", "dem.tif"), "w", driver="GTiff", height=40, width=60,
                   count=1, dtype="float32", crs="EPSG:3081", transform=from_origin(1_480_000, 7_252_000, 50, 50),
                   nodata=-9999) as d:
    d.write(z, 1)
"#;
    let ok = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(folder)
        .arg(BIG.to_string())
        .status()
        .unwrap();
    assert!(ok.success(), "fixture script failed");
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(py: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    make_fixture(py, &root);
    for d in ["connections", "share", "pack/delivery"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let q = |p: PathBuf| serde_json::to_string(&p.display().to_string()).unwrap();
    std::fs::write(
        root.join("pack/pack.yaml"),
        "name: Test pack\nformat_version: 1\n",
    )
    .unwrap();
    for (id, crs) in [("state", "EPSG:2277"), ("native", "EPSG:3081")] {
        std::fs::write(
            root.join(format!("pack/delivery/{id}.yaml")),
            format!(
                "id: {id}\nkind: delivery\ntitle: Raster deliverable {id}\nsummary: Rasters for the state.\n\
                 crs: {crs}\nformats: [GeoTIFF]\nowner: GIS\nlast_confirmed: 2026-01-01\nstatus: reviewed\n"
            ),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [gpkg, rasters]\nlimits: {{ rows: 1000, seconds: 120 }}\n\
             approvals: {{ page: false }}\n\
             exports:\n  - {{ id: share, path: {}, granted: true }}\n",
            q(root.join("share"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/crashes.yaml"),
        format!(
            "id: crashes\ntype: gpkg\npath: {}\ndescription: Crashes\ndatasets:\n  allow: [\"crashes\"]\n",
            q(root.join("crashes.gpkg"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/dem.yaml"),
        format!(
            "id: dem\ntype: rasters\npath: {}\ndescription: Elevation\ndatasets:\n  allow: [\"*\"]\n",
            q(root.join("rasters"))
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
}

async fn start(root: &Path, py: &str) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = root.to_path_buf();
    let py = py.to_string();
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_APPROVALS_BROWSER", "none")
                .env("COLUMBUS_CONTEXT", root.join("pack"));
        }),
    )
    .unwrap();
    ().serve(transport).await.unwrap()
}

async fn call(
    c: &RunningService<RoleClient, ()>,
    tool: &'static str,
    args: Value,
) -> CallToolResult {
    let mut p = CallToolRequestParams::new(tool);
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    c.call_tool(p).await.unwrap()
}

fn ok(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().expect("structured output")
}

fn err(r: &CallToolResult) -> String {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected an error, got {:?}",
        r.structured_content
    );
    serde_json::to_string(&r.content).unwrap()
}

fn check<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check in {}", v["checks"]))
}

/// Every file below `dir` (relative path) with its SHA-256, and every
/// folder (hash "dir").
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            if p.is_dir() {
                out.insert(rel, "dir".into());
                walk(base, &p, out);
            } else {
                let (h, _) = columbus_shared::canon::sha256_file(&p).unwrap();
                out.insert(rel, h);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn probe(py: &str, code: &str, arg: &Path) -> String {
    let o = StdCommand::new(py)
        .args(["-c", code])
        .arg(arg)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

#[tokio::test]
async fn gdb_export() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj, rasterio") else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(&py);
    let client = start(&env.root, &py).await;
    let share = env.root.join("share");

    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": "crashes", "dataset": "crashes", "order_by": "crash_id"}),
    )
    .await);
    let pts = q["result"]["layer"]["handle"].as_str().unwrap().to_string();

    // A points layer to a new file geodatabase, one feature class named
    // after the export, with the same rows and CRS.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": pts, "folder": "share", "name": "crash_hotspots", "format": "gdb"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "exported", "{x}");
    let gdb = share.join("crash_hotspots.gdb");
    assert!(gdb.is_dir());
    assert!(share.join("crash_hotspots.prov.json").is_file());
    let files = x["result"]["files"].as_array().unwrap();
    assert!(files.len() > 1, "{x}");
    // Every format reports paths in the export folder's canonical form
    // (macOS /private/var, Windows \\?\ and long names), so compare with
    // the .gdb folder's canonical path.
    let gdb_real = gdb.canonicalize().unwrap();
    assert!(
        files.iter().all(
            |f| Path::new(f["path"].as_str().unwrap()).starts_with(&gdb_real)
                && f["sha256"].as_str().unwrap().len() == 64
        ),
        "{x}"
    );
    for f in files {
        assert!(Path::new(f["path"].as_str().unwrap()).is_file(), "{f}");
    }
    let info = probe(
        &py,
        "import sys, pyogrio\nl = pyogrio.list_layers(sys.argv[1])\ni = pyogrio.read_info(sys.argv[1])\nprint(len(l), l[0][0], i['features'], i['crs'])",
        &gdb,
    );
    assert_eq!(info, "1 crash_hotspots 5 EPSG:3081");

    // The Int64 value beyond 32 bits reads back exact, with the info check.
    let c = check(&x, "export.needs_pro_3_2");
    assert_eq!(c["severity"], "info");
    assert!(c["message"].as_str().unwrap().contains("parcel_id"), "{c}");
    let back = probe(
        &py,
        "import sys, geopandas as g\nd = g.read_file(sys.argv[1], engine='pyogrio')\nprint(str(d['parcel_id'].dtype), ','.join(map(str, d.sort_values('crash_id')['parcel_id'])))",
        &gdb,
    );
    assert_eq!(back, format!("int64 1,2,{BIG},4,5"));
    let side: Value = serde_json::from_str(
        &std::fs::read_to_string(share.join("crash_hotspots.prov.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(side["export"]["format"], "gdb");
    assert!(!side["lineage"].as_array().unwrap().is_empty());

    // An existing .gdb is refused and left byte for byte as it was: again,
    // with overwrite, in another case, and in another format.
    let before = snapshot(&share);
    for args in [
        json!({"handle": pts, "folder": "share", "name": "crash_hotspots", "format": "gdb"}),
        json!({"handle": pts, "folder": "share", "name": "crash_hotspots", "format": "gdb", "overwrite": true}),
        json!({"handle": pts, "folder": "share", "name": "CRASH_HOTSPOTS", "format": "gdb", "overwrite": true}),
        json!({"handle": pts, "folder": "share", "name": "crash_hotspots", "format": "gpkg", "overwrite": true}),
    ] {
        let e = err(&call(&client, "export_output", args.clone()).await);
        assert!(
            e.contains("never replaced or changed") && e.contains("choose another name"),
            "{args}: {e}"
        );
    }
    assert_eq!(snapshot(&share), before);

    // Another name works, and nothing is left half-written.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": pts, "folder": "share", "name": "crash_hotspots_v2", "format": "File Geodatabase"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "exported", "{x}");
    for f in std::fs::read_dir(&share).unwrap().flatten() {
        assert!(
            !f.file_name().to_string_lossy().starts_with(".columbus-"),
            "{:?}",
            f.path()
        );
    }

    // A name that is not a feature class name is refused, not changed.
    let e = err(&call(
        &client,
        "export_output",
        json!({"handle": pts, "folder": "share", "name": "crash hotspots", "format": "gdb"}),
    )
    .await);
    assert!(e.contains("feature class name"), "{e}");
    assert!(!share.join("crash hotspots.gdb").exists());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn tif_export() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj, rasterio") else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(&py);
    let client = start(&env.root, &py).await;
    let share = env.root.join("share");

    let s = ok(&call(
        &client,
        "raster_op",
        json!({"op": "slope", "inputs": {"raster": "dem:dem.tif"}, "params": {"z_unit": "m"}}),
    )
    .await);
    let slope = s["result"]["layer"]["handle"].as_str().unwrap().to_string();

    // The slope as a COG, with the same CRS and nodata as the workspace's.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": slope, "folder": "share", "name": "slope", "format": "tif"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "exported", "{x}");
    let tif = share.join("slope.tif");
    assert!(tif.is_file());
    assert!(share.join("slope.prov.json").is_file());
    let code = "import sys, rasterio\nwith rasterio.open(sys.argv[1]) as d:\n print(d.tags(ns='IMAGE_STRUCTURE').get('LAYOUT'), d.crs.to_epsg(), d.nodata)";
    let out = probe(&py, code, &tif);
    let ws = std::fs::read_dir(env.root.join("workspaces"))
        .unwrap()
        .flatten()
        .flat_map(|s| walk(&s.path()))
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("slope-"))
        })
        .expect("the slope in the workspace");
    assert_eq!(out, probe(&py, code, &ws));
    assert!(out.starts_with("COG 3081 "), "{out}");
    assert_eq!(std::fs::read(&tif).unwrap(), std::fs::read(&ws).unwrap());

    // A delivery spec in the raster's own CRS takes its format from the
    // spec; one in another CRS fails: rasters are never reprojected here.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": slope, "folder": "share", "name": "slope native", "delivery": "native"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "exported", "{x}");
    assert!(share.join("slope native.tif").is_file());
    let e = err(&call(
        &client,
        "export_output",
        json!({"handle": slope, "folder": "share", "name": "slope state", "delivery": "state"}),
    )
    .await);
    assert!(e.contains("reproject first (raster_op reproject)"), "{e}");
    assert!(!share.join("slope state.tif").exists());

    // A vector layer is refused as tif, and a raster as a vector format.
    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": "crashes", "dataset": "crashes"}),
    )
    .await);
    let pts = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let e = err(&call(
        &client,
        "export_output",
        json!({"handle": pts, "folder": "share", "name": "points", "format": "tif"}),
    )
    .await);
    assert!(e.contains("raster results"), "{e}");
    let e = err(&call(
        &client,
        "export_output",
        json!({"handle": slope, "folder": "share", "name": "slope_gdb", "format": "gdb"}),
    )
    .await);
    assert!(e.contains("tif only"), "{e}");
    assert!(!share.join("points.tif").exists() && !share.join("slope_gdb.gdb").exists());
    client.cancel().await.unwrap();
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return out;
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        out.extend(walk(&e.path()));
    }
    out
}
