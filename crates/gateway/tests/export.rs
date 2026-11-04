//! read_output and export_output (design sections 3 and 4): paging through
//! a result, exporting to a granted folder with a delivery spec applied,
//! approvals for other folders, and a fail in a layer's history blocking
//! its export.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj; skipped
//! otherwise.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python(import: &str) -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", import])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

/// Ten roads around Austin in EPSG:3081 (eight in district 14), a county
/// polygon, a field the policy hides, and a table of surfaces whose route
/// codes lost their zero padding.
fn make_fixture(py: &str, folder: &Path) {
    let script = r#"
import sys, os
import geopandas as gpd, pandas as pd
from shapely.geometry import LineString, Polygon
folder = sys.argv[1]
path = os.path.join(folder, "austin.gpkg")
lines = [LineString([(-97.80 + i * 0.01, 30.20), (-97.80 + i * 0.01, 30.30)]) for i in range(10)]
gpd.GeoDataFrame(
    {"route_id": [f"{i:04d}" for i in range(10)], "district": [14] * 8 + [12] * 2,
     "lanes": [2, 4] * 5, "owner_ssn": [f"10{i}-45-67{i:02d}" for i in range(10)],
     "owner_name": ["Secret Owner"] * 10},
    geometry=lines, crs="EPSG:4326").to_crs("EPSG:3081").to_file(path, layer="roads", driver="GPKG", engine="pyogrio")
gpd.GeoDataFrame({"name": ["Travis"]},
    geometry=[Polygon([(-97.85, 30.15), (-97.745, 30.15), (-97.745, 30.35), (-97.85, 30.35)])],
    crs="EPSG:4326").to_crs("EPSG:3081").to_file(path, layer="county", driver="GPKG", engine="pyogrio")
gpd.GeoDataFrame({"route_id": [str(i) for i in range(10)], "surface": ["asphalt"] * 10},
    geometry=[None] * 10).to_file(path, layer="surfaces", driver="GPKG", engine="pyogrio")
"#;
    let ok = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(folder)
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
    std::fs::create_dir_all(root.join("connections")).unwrap();
    for d in ["share", "outbox", "pack/delivery"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(
        root.join("pack/pack.yaml"),
        "name: Test pack\nformat_version: 1\n",
    )
    .unwrap();
    std::fs::write(
        root.join("pack/delivery/district.yaml"),
        "id: district\nkind: delivery\ntitle: District deliverable\nsummary: What districts expect.\n\
         crs: EPSG:2277\nformats: [GeoPackage]\nfields:\n  rename: { lanes: LANE_COUNT }\n\
         metadata: [title, contact]\nowner: GIS\nlast_confirmed: 2026-01-01\nstatus: reviewed\n",
    )
    .unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [gpkg]\nhide_fields: [\"{{*ssn*,owner_name}}\"]\nlimits: {{ rows: 1000, seconds: 60 }}\n\
             approvals: {{ page: false }}\n\
             exports:\n  - {{ id: share, path: {}, granted: true }}\n  - {{ id: outbox, path: {} }}\n",
            serde_json::to_string(&root.join("share").display().to_string()).unwrap(),
            serde_json::to_string(&root.join("outbox").display().to_string()).unwrap()
        ),
    )
    .unwrap();
    let gpkg = serde_json::to_string(&root.join("austin.gpkg").display().to_string()).unwrap();
    // The roads, filtered to one district, and the reference layers from the
    // same file, unfiltered (a connection's row filter applies to every
    // dataset it allows).
    std::fs::write(
        root.join("connections").join("austin.yaml"),
        format!(
            "id: austin\ntype: gpkg\npath: {gpkg}\ndescription: Austin roads\n\
             datasets:\n  allow: [\"roads\"]\nrow_filter: \"district = 14\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("austin-ref.yaml"),
        format!(
            "id: austin-ref\ntype: gpkg\npath: {gpkg}\ndescription: Austin reference layers\n\
             datasets:\n  allow: [\"county\", \"surfaces\"]\n"
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
}

async fn start(root: &Path, py: &str, extra: &[(&str, &str)]) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = root.to_path_buf();
    let py = py.to_string();
    let extra: Vec<(String, String)> = extra
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
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
            for (k, v) in &extra {
                cmd.env(k, v);
            }
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

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn check_ids(v: &Value) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

fn check<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check in {}", v["checks"]))
}

#[tokio::test]
async fn read_and_export_outputs() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj") else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(&py);
    let client = start(&env.root, &py, &[]).await;

    let w = ok(&call(&client, "whoami", json!({})).await);
    assert_eq!(w["result"]["export_folders"][0]["id"], "share");
    assert_eq!(w["result"]["export_folders"][0]["granted"], true);

    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "fields": ["route_id", "lanes"], "order_by": "route_id"}),
    )
    .await);
    let roads = q["result"]["layer"]["handle"].as_str().unwrap().to_string();

    // read_output pages through the layer; geometry is a type and a box.
    let r = ok(&call(
        &client,
        "read_output",
        json!({"handle": roads, "offset": 2, "limit": 3}),
    )
    .await);
    assert_eq!(r["result"]["total"], 8);
    assert_eq!(r["result"]["rows"].as_array().unwrap().len(), 3);
    assert_eq!(r["result"]["rows"][0][0], "0002");
    assert_eq!(r["result"]["rows"][0][2]["type"], "LineString");
    let e = err(&call(
        &client,
        "read_output",
        json!({"handle": "ws://nope/results/x.gpkg"}),
    )
    .await);
    assert!(e.contains("not"), "{e}");

    // To a granted folder, with the delivery spec applied.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": roads, "folder": "share", "name": "district roads", "delivery": "district",
               "metadata": {"title": "Roads", "contact": "gis@example.org"}}),
    )
    .await);
    assert_eq!(x["result"]["status"], "exported", "{x}");
    let out = env.root.join("share").join("district roads.gpkg");
    assert!(out.is_file());
    assert!(env
        .root
        .join("share")
        .join("district roads.prov.json")
        .is_file());
    let ids = check_ids(&x);
    assert!(ids.contains(&"export.reprojected".to_string()), "{ids:?}");
    assert!(ids.contains(&"export.renamed".to_string()), "{ids:?}");
    let probe = StdCommand::new(&py)
        .args(["-c", "import sys, geopandas as g; d = g.read_file(sys.argv[1]); print(d.crs.to_epsg(), sorted(d.columns))"])
        .arg(&out)
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&probe.stdout);
    assert!(
        printed.contains("2277") && printed.contains("LANE_COUNT"),
        "{printed}"
    );
    let side: Value = serde_json::from_str(
        &std::fs::read_to_string(env.root.join("share").join("district roads.prov.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(side["export"]["delivery"], "district");
    assert!(!side["lineage"].as_array().unwrap().is_empty());

    // The same name again: refused unless overwrite (which needs approval).
    let e = err(&call(
        &client,
        "export_output",
        json!({"handle": roads, "folder": "share", "name": "district roads"}),
    )
    .await);
    assert!(e.contains("already exist"), "{e}");

    // A spec's missing metadata is a fail: blocked even in a granted folder.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": roads, "folder": "share", "name": "no metadata", "delivery": "district"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "approval_required", "{x}");
    assert_eq!(check(&x, "delivery.metadata")["severity"], "fail");
    assert!(!env.root.join("share").join("no metadata.gpkg").exists());

    // A folder that is not granted needs a person.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": roads, "folder": "outbox", "name": "roads"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "approval_required", "{x}");
    assert!(x["result"]["approval"]["summary"]
        .as_str()
        .unwrap()
        .contains("not a granted export folder"));
    assert!(!env.root.join("outbox").join("roads.gpkg").exists());

    // A fail earlier in the layer's history (a join that matched nothing)
    // carries into the export.
    let j = ok(&call(
        &client,
        "vector_op",
        json!({"op": "join", "inputs": {"left": roads, "right": "austin-ref:surfaces"}, "params": {"left_key": "route_id"}}),
    )
    .await);
    assert_eq!(check(&j, "join.unmatched")["severity"], "fail");
    let joined = j["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let b = ok(&call(
        &client,
        "vector_op",
        json!({"op": "buffer", "inputs": {"layer": joined}, "params": {"distance": 10, "unit": "m"}}),
    )
    .await);
    let buffered = b["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": buffered, "folder": "share", "name": "buffered"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "approval_required", "{x}");
    let c = check(&x, "join.unmatched");
    assert_eq!(c["severity"], "fail");
    assert!(c["message"].as_str().unwrap().starts_with("Earlier step"));

    // The fail survives a long chain and an erased provenance record: it is
    // carried in memory, not read back from files the workspace can change.
    let prov_dir = walk_files(&env.root.join("workspaces"))
        .into_iter()
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json") && p.to_string_lossy().contains("provenance")
        })
        .collect::<Vec<_>>();
    for p in &prov_dir {
        std::fs::write(p, "{}").unwrap();
    }
    let mut h = buffered.clone();
    for _ in 0..55 {
        let b = ok(&call(
            &client,
            "vector_op",
            json!({"op": "simplify", "inputs": {"layer": h}, "params": {"tolerance": 0.01, "unit": "m"}}),
        )
        .await);
        h = b["result"]["layer"]["handle"].as_str().unwrap().to_string();
    }
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": h, "folder": "share", "name": "long chain"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "approval_required", "{x}");
    assert_eq!(check(&x, "join.unmatched")["severity"], "fail");

    // Something else already named like the export (a GeoPackage's -wal
    // file) counts as in the way.
    std::fs::write(env.root.join("share").join("wal.gpkg-wal"), b"x").unwrap();
    let e = err(&call(
        &client,
        "export_output",
        json!({"handle": roads, "folder": "share", "name": "wal"}),
    )
    .await);
    assert!(e.contains("wal.gpkg-wal"), "{e}");

    // Required metadata must have something in it.
    let x = ok(&call(
        &client,
        "export_output",
        json!({"handle": roads, "folder": "share", "name": "falsy", "delivery": "district",
               "metadata": {"title": false, "contact": ""}}),
    )
    .await);
    assert_eq!(check(&x, "delivery.metadata")["severity"], "fail");

    // Nothing is left half-written in the folder.
    for f in std::fs::read_dir(env.root.join("share")).unwrap().flatten() {
        assert!(
            !f.file_name().to_string_lossy().starts_with(".columbus-"),
            "{:?}",
            f.path()
        );
    }

    // Names cannot leave the folder; unknown folders are named.
    for bad in ["../escape", "a/b", "CON"] {
        let e = err(&call(
            &client,
            "export_output",
            json!({"handle": roads, "folder": "share", "name": bad}),
        )
        .await);
        assert!(e.contains("name may use"), "{bad}: {e}");
    }
    let e = err(&call(
        &client,
        "export_output",
        json!({"handle": roads, "folder": "desktop", "name": "x"}),
    )
    .await);
    assert!(e.contains("share, outbox"), "{e}");
    client.cancel().await.unwrap();
}
