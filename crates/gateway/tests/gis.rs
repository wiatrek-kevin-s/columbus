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
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nhide_fields: [\"{*ssn*,owner_name}\"]\nlimits: { rows: 1000, seconds: 60 }\n",
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

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.push(p.clone());
            out.extend(walk(&p));
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
        fields.contains("route_id")
            && !fields.contains("owner_ssn")
            && !fields.contains("owner_name"),
        "{fields}"
    );
    assert!(check_ids(&d).contains(&"query.row_filter".to_string()));
    assert!(
        r["extent"].is_null(),
        "the stored extent covers filtered-out rows: {r}"
    );

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
        (json!({"fields": ["owner_name"]}), "no field"),
        // Filtering or sorting on a hidden field would reveal its values.
        (
            json!({"where": "owner_ssn LIKE '103%'"}),
            "hidden by organization policy",
        ),
        (
            json!({"where": "SUBSTR(OWNER_NAME, 1, 1) = 'S'"}),
            "hidden by organization policy",
        ),
        (json!({"order_by": "owner_ssn"}), "no field"),
        // Dataset names are plain identifiers only.
        (
            json!({"dataset": "roads WHERE 1=0 UNION SELECT owner_ssn FROM roads"}),
            "not a dataset name",
        ),
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

    // A result cut off at a limit says so wherever it is used next.
    let cut = ok(&call(
        &client,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "limit": 2}),
    )
    .await);
    assert!(check_ids(&cut).contains(&"query.read_limit".to_string()));
    let cut_h = cut["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let b2 = ok(&call(
        &client,
        "vector_op",
        json!({"op": "buffer", "inputs": {"layer": cut_h}, "params": {"distance": 1, "unit": "m"}}),
    )
    .await);
    assert_eq!(check(&b2, "input.truncated")["severity"], "warn");
    let b3 = ok(&call(
        &client,
        "verify",
        json!({"layer": b2["result"]["layer"]["handle"]}),
    )
    .await);
    assert!(
        check_ids(&b3).contains(&"input.truncated".to_string()),
        "carried on: {}",
        b3["checks"]
    );

    // Files Columbus did not write are not opened, whatever their name: here
    // a VRT disguised as a GeoPackage that would read the hidden field.
    let session = roads4
        .trim_start_matches("ws://")
        .split('/')
        .next()
        .unwrap()
        .to_string();
    let results = walk(&env.root.join("workspaces"))
        .into_iter()
        .find(|p| p.file_name().is_some_and(|n| n == "results"))
        .expect("results folder");
    let evil = results.parent().unwrap().join("external").join("evil.gpkg");
    std::fs::create_dir_all(evil.parent().unwrap()).unwrap();
    std::fs::write(
        &evil,
        format!(
            "<OGRVRTDataSource><OGRVRTLayer name='x'><SrcDataSource>{}</SrcDataSource><SrcSQL>SELECT owner_ssn, geom FROM roads</SrcSQL></OGRVRTLayer></OGRVRTDataSource>",
            env.root.join("austin.gpkg").display()
        ),
    )
    .unwrap();
    let e = err(&call(
        &client,
        "verify",
        json!({"layer": format!("ws://{session}/external/evil.gpkg")}),
    )
    .await);
    assert!(e.contains("not written by a Columbus tool"), "{e}");

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
        !text.contains("45-67") && !text.contains("Secret Owner"),
        "hidden values never reach the audit log"
    );
    for tool in ["describe_dataset", "query_features", "vector_op", "verify"] {
        assert!(
            text.contains(&format!("\"tool\":\"{tool}\"")),
            "{tool} audited"
        );
    }
}

/// Layer and field names with spaces, as Census TIGER GeoPackages have
/// them ("Tribal Census Tract"): every GIS tool takes the name list_datasets
/// shows, and names that could change the statement are still refused.
#[tokio::test]
async fn names_with_spaces_work_in_every_gis_tool() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj") else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box
path = os.path.join(sys.argv[1], "tiger.gpkg")
gpd.GeoDataFrame({"NAME": ["A", "B", "C"], "Land Area": [10.0, 20.0, 30.0], "owner_ssn": ["1", "2", "3"]},
    geometry=[box(-97.8 + i * 0.1, 30.2, -97.75 + i * 0.1, 30.25) for i in range(3)],
    crs="EPSG:4269").to_file(path, layer="Tribal Census Tract", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(&py)
        .arg("-c")
        .arg(script)
        .arg(&root)
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nhide_fields: [\"*ssn*\"]\n",
    )
    .unwrap();
    let gpkg = serde_json::to_string(&root.join("tiger.gpkg").display().to_string()).unwrap();
    std::fs::write(
        root.join("connections").join("tiger.yaml"),
        format!("id: tiger\ntype: gpkg\npath: {gpkg}\ndatasets:\n  allow: [\"*\"]\n"),
    )
    .unwrap();
    // The same layer with a row filter: the table is then read through a
    // filtered derived table, whose alias must keep its quotes.
    std::fs::write(
        root.join("connections").join("tiger-gov.yaml"),
        format!(
            "id: tiger-gov\ntype: gpkg\npath: {gpkg}\ndatasets:\n  allow: [\"*\"]\n\
             row_filter: '\"Land Area\" > 15'\n"
        ),
    )
    .unwrap();
    let client = start(&root, &py, &[]).await;
    let ds = "Tribal Census Tract";

    let l = ok(&call(&client, "list_datasets", json!({"connection": "tiger"})).await);
    assert_eq!(l["result"]["datasets"][0]["name"], ds);
    let d = ok(&call(
        &client,
        "describe_dataset",
        json!({"connection": "tiger", "dataset": ds}),
    )
    .await);
    assert_eq!(d["result"]["rows"], 3, "{d}");
    let s = ok(&call(
        &client,
        "sample_rows",
        json!({"connection": "tiger", "dataset": ds, "n": 2}),
    )
    .await);
    assert_eq!(s["result"]["rows_returned"], 2, "{s}");

    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": "tiger", "dataset": ds, "fields": ["NAME", "Land Area"],
               "where": "\"Land Area\" >= 20", "order_by": "Land Area"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], 2, "{q}");
    let sql = q["result"]["executed_sql"].as_str().unwrap();
    assert!(
        sql.contains("\"Tribal Census Tract\"") && sql.contains("\"Land Area\""),
        "{sql}"
    );
    let handle = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    assert!(!handle.contains(' '), "output names stay plain: {handle}");

    let v = ok(&call(&client, "verify", json!({"layer": format!("tiger:{ds}")})).await);
    assert!(v["checks"].is_array(), "{v}");
    let r = ok(&call(
        &client,
        "vector_op",
        json!({"op": "reproject", "inputs": {"layer": format!("tiger:{ds}")}, "params": {"crs": "EPSG:3081"}}),
    )
    .await);
    assert_eq!(
        r["result"]["layer"]["summary"]["crs"]["code"], "EPSG:3081",
        "{r}"
    );

    // Under a row filter and hidden fields too.
    let g = ok(&call(
        &client,
        "query_features",
        json!({"connection": "tiger-gov", "dataset": ds}),
    )
    .await);
    assert_eq!(g["result"]["rows_read"], 2, "{g}");
    assert!(
        !g["result"]["columns"].to_string().contains("owner_ssn"),
        "{g}"
    );
    let r = ok(&call(
        &client,
        "vector_op",
        json!({"op": "reproject", "inputs": {"layer": format!("tiger-gov:{ds}")}, "params": {"crs": "EPSG:3081"}}),
    )
    .await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 2, "{r}");
    let v = ok(&call(
        &client,
        "verify",
        json!({"layer": format!("tiger-gov:{ds}")}),
    )
    .await);
    assert!(v["checks"].is_array(), "{v}");

    // Still refused: anything that could close the quoting or add SQL.
    for bad in [
        "Tribal\" Census",
        "Tract]; DROP",
        "x\u{202e}y",
        " Tribal",
        "a.b.c.d",
        "roads; SELECT 1",
    ] {
        let e = err(&call(
            &client,
            "query_features",
            json!({"connection": "tiger", "dataset": bad}),
        )
        .await);
        assert!(e.contains("not a dataset name"), "{bad:?}: {e}");
    }
    let e = err(&call(
        &client,
        "query_features",
        json!({"connection": "tiger", "dataset": ds, "fields": ["NAME\" FROM x --"]}),
    )
    .await);
    assert!(e.contains("not a field name"), "{e}");
    client.cancel().await.unwrap();
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
