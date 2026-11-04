//! End to end for the GIS tools: describe a dataset, query it with
//! structured arguments, process the results in the workspace and verify
//! them, through the real `columbus-mcp` binary and Python worker.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj; skipped
//! otherwise. With `COLUMBUS_TEST_PG_HOST` set it also runs against PostGIS
//! (see tests/postgis.rs).

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
     "lanes": [2, 4] * 5, "owner_ssn": ["123-45-6789"] * 10},
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
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nhide_fields: [\"*ssn*\"]\nlimits: { rows: 1000, seconds: 60 }\n",
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
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
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
async fn gis_session_on_a_geopackage() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj") else {
        eprintln!(
            "skipping: the GIS engine (geopandas, pyogrio, shapely, pyproj) is not installed"
        );
        return;
    };
    let env = setup(&py);
    let client = start(&env.root, &py, &[]).await;

    // describe_dataset: fields without the hidden one, CRS facts, filtered count.
    let d = ok(&call(
        &client,
        "describe_dataset",
        json!({"connection": "austin", "dataset": "roads"}),
    )
    .await);
    let r = &d["result"];
    assert_eq!(r["rows"], 8, "counted under the row filter: {r}");
    assert_eq!(r["crs"]["code"], "EPSG:3081");
    assert_eq!(r["crs"]["geographic"], false);
    assert_eq!(r["geometry_column"], "geom");
    let fields = serde_json::to_string(&r["fields"]).unwrap();
    assert!(
        fields.contains("route_id") && !fields.contains("owner_ssn"),
        "{fields}"
    );
    assert!(check_ids(&d).contains(&"query.row_filter".to_string()));

    // sample_rows: a few rows, under the filter.
    let s = ok(&call(
        &client,
        "sample_rows",
        json!({"connection": "austin", "dataset": "roads", "n": 3}),
    )
    .await);
    assert_eq!(s["result"]["rows_returned"], 3);
    assert!(!s["result"]["columns"].to_string().contains("owner_ssn"));

    // query_features: structured arguments become checked SQL with the filter.
    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "fields": ["route_id", "lanes"], "where": "lanes = 4", "order_by": "route_id"}),
    )
    .await);
    let res = &q["result"];
    assert_eq!(res["rows_read"], 4, "{res}");
    assert!(res["executed_sql"]
        .as_str()
        .unwrap()
        .contains("district = 14"));
    assert_eq!(res["columns"], json!(["route_id", "lanes", "geom"]));
    let roads4 = res["layer"]["handle"].as_str().unwrap().to_string();
    assert!(roads4.starts_with("ws://"));
    assert_eq!(res["layer"]["summary"]["crs"]["code"], "EPSG:3081");
    assert!(q["provenance"].is_string());

    // A where clause that keeps everything, and one that keeps nothing.
    let all = ok(&call(
        &client,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "where": "lanes > 0"}),
    )
    .await);
    assert_eq!(check(&all, "filter.all_rows")["severity"], "warn");
    let none = ok(&call(
        &client,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "where": "route_id = '7'"}),
    )
    .await);
    assert_eq!(check(&none, "filter.zero_rows")["severity"], "warn");

    // bbox in lon/lat, applied before the limit.
    let b = ok(&call(
        &client,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "bbox": [-97.805, 30.19, -97.775, 30.31], "bbox_crs": "EPSG:4326", "limit": 5}),
    )
    .await);
    assert_eq!(b["result"]["rows_read"], 3, "{}", b["result"]);

    // Refusals: hidden or unknown fields, injection through names, subqueries,
    // a bad CRS string.
    for (args, expect) in [
        (json!({"fields": ["owner_ssn"]}), "no field"),
        (
            json!({"fields": ["route_id\" FROM x --"]}),
            "not a field name",
        ),
        (
            json!({"order_by": "1; DROP TABLE roads"}),
            "not a field name",
        ),
        (
            json!({"where": "fid IN (SELECT fid FROM roads)"}),
            "not allowed in a where clause",
        ),
        (
            json!({"bbox": [0, 0, 1, 1], "bbox_crs": "+proj=longlat"}),
            "EPSG code",
        ),
    ] {
        let mut a = json!({"connection": "austin", "dataset": "roads"});
        a.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        let e = err(&call(&client, "query_features", a.clone()).await);
        assert!(e.contains(expect), "{a}: {e}");
    }

    // vector_op: buffering in degrees is refused with the fix; in feet it works.
    let wgs = ok(&call(
        &client,
        "vector_op",
        json!({"op": "reproject", "inputs": {"layer": roads4}, "params": {"crs": "EPSG:4326"}}),
    )
    .await);
    let wgs_h = wgs["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let e = err(&call(&client, "vector_op", json!({"op": "buffer", "inputs": {"layer": wgs_h}, "params": {"distance": 100, "unit": "ft"}})).await);
    assert!(e.contains("crs.measure") && e.contains("EPSG:3081"), "{e}");
    let buf = ok(&call(&client, "vector_op", json!({"op": "buffer", "inputs": {"layer": roads4}, "params": {"distance": 100, "unit": "ft"}})).await);
    assert_eq!(buf["result"]["layer"]["summary"]["rows"], 4);
    assert!(buf["provenance"].is_string());
    assert!(check_ids(&buf).contains(&"flow.labels".to_string()));

    // A dataset as an input: read through the same permissions (row filter
    // applied), then clipped by the county.
    let clip = ok(&call(
        &client,
        "vector_op",
        json!({"op": "clip", "inputs": {"layer": "austin:roads", "mask": "austin-ref:county"}}),
    )
    .await);
    assert_eq!(
        clip["result"]["layer"]["summary"]["rows"], 6,
        "the county ends at -97.745: {}",
        clip["result"]
    );

    // Attribute join with unpadded keys: the check names the problem and the fix.
    let j = ok(&call(
        &client,
        "vector_op",
        json!({"op": "join", "inputs": {"left": "austin:roads", "right": {"connection": "austin-ref", "dataset": "surfaces"}}, "params": {"left_key": "route_id"}}),
    )
    .await);
    let c = check(&j, "join.unmatched");
    assert_eq!(c["severity"], "fail");
    assert!(c["fix"].as_str().unwrap().contains("pad"), "{c}");

    // Refused: handles outside this session, unknown ops, unknown datasets.
    let e = err(&call(&client, "vector_op", json!({"op": "buffer", "inputs": {"layer": "ws://someone-else/results/x.gpkg"}, "params": {"distance": 1, "unit": "m"}})).await);
    assert!(e.contains("session"), "{e}");
    let e = err(&call(
        &client,
        "vector_op",
        json!({"op": "explode", "inputs": {"layer": roads4}}),
    )
    .await);
    assert!(e.to_lowercase().contains("op"), "{e}");
    let e = err(&call(&client, "vector_op", json!({"op": "buffer", "inputs": {"layer": "austin:secret_table"}, "params": {"distance": 1, "unit": "m"}})).await);
    assert!(e.contains("allowed datasets"), "{e}");

    // verify: on a result and on a dataset.
    let v = ok(&call(
        &client,
        "verify",
        json!({"layer": buf["result"]["layer"]["handle"]}),
    )
    .await);
    assert_eq!(v["result"]["rows"], 4);
    let v = ok(&call(&client, "verify", json!({"layer": "austin:roads"})).await);
    assert!(
        check_ids(&v).contains(&"crs.info".to_string()),
        "{}",
        v["checks"]
    );

    // Every result is listed, with its labels.
    let outs = ok(&call(&client, "list_outputs", json!({})).await);
    assert!(outs.to_string().contains("vector_op buffer"), "{outs}");

    client.cancel().await.unwrap();
    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).expect("audit chain intact");
    let text = std::fs::read_to_string(&audit).unwrap();
    assert!(
        !text.contains("123-45-6789"),
        "hidden values never reach the audit log"
    );
    for tool in ["describe_dataset", "query_features", "vector_op", "verify"] {
        assert!(
            text.contains(&format!("\"tool\":\"{tool}\"")),
            "{tool} audited"
        );
    }
}

#[tokio::test]
async fn gis_query_on_postgis() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_PG_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_PG_HOST not set");
        return;
    };
    let Some(py) = python("import psycopg, geopandas, pyogrio, shapely, pyproj") else {
        panic!(
            "Python with psycopg and the GIS engine is required when COLUMBUS_TEST_PG_HOST is set"
        );
    };
    let port = std::env::var("COLUMBUS_TEST_PG_PORT").unwrap_or_else(|_| "5432".into());
    let db = std::env::var("COLUMBUS_TEST_PG_DB").unwrap_or_else(|_| "gis".into());
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [postgis]\nhosts: [{host:?}]\nhide_fields: [\"owner_*\"]\nlimits: {{ rows: 1000, seconds: 30 }}\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("roads-pg.yaml"),
        format!(
            "id: roads-pg\ntype: postgis\nhost: {host}\nport: {port}\ndatabase: {db}\nuser: columbus_ro\nsslmode: disable\n\
             datasets:\n  allow: [\"roads.roadway_inventory\"]\nrow_filter: \"district_nbr = 14\"\n"
        ),
    )
    .unwrap();
    let client = start(
        root,
        &py,
        &[
            ("COLUMBUS_DEV_SECRETS", "1"),
            ("COLUMBUS_SECRET_ROADS_PG", "columbus_ro_test"),
        ],
    )
    .await;

    let d = ok(&call(
        &client,
        "describe_dataset",
        json!({"connection": "roads-pg", "dataset": "roads.roadway_inventory"}),
    )
    .await);
    assert_eq!(d["result"]["rows"], 600, "{}", d["result"]);
    assert_eq!(d["result"]["crs"]["code"], "EPSG:3081");
    assert!(!d["result"]["fields"].to_string().contains("owner_name"));

    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": "roads-pg", "dataset": "roads.roadway_inventory", "fields": ["route_id", "func_class"], "where": "func_class = 3", "order_by": "route_id"}),
    )
    .await);
    let res = &q["result"];
    assert!(res["rows_read"].as_u64().unwrap() > 0, "{res}");
    assert!(res["executed_sql"]
        .as_str()
        .unwrap()
        .contains("district_nbr = 14"));
    let h = res["layer"]["handle"].as_str().unwrap().to_string();
    assert_eq!(res["layer"]["summary"]["crs"]["code"], "EPSG:3081");

    // The read limit is enforced on whole-dataset inputs to processing.
    let buf = ok(&call(
        &client,
        "vector_op",
        json!({"op": "buffer", "inputs": {"layer": h}, "params": {"distance": 3, "unit": "m"}}),
    )
    .await);
    assert_eq!(buf["result"]["layer"]["summary"]["rows"], res["rows_read"]);

    client.cancel().await.unwrap();
}
