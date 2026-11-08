//! The M-values chain (T-19, REQ-24) through the real gateway, shared by
//! `measures.rs` (a GeoPackage fixture) and `oracle.rs` (CI's Oracle job):
//! query_features, vector_op reproject, join, spatial_join and dissolve,
//! and export_output to gpkg, gdb and shp. Every output's first vertex
//! carries the source's M value (route `R<i>` starts at measure
//! 1000 + 10 i); buffer drops M with a `geometry.m_dropped` info check.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::RoleClient;
use serde_json::{json, Value};

/// The source's names, all in one connection: the route layer, a dataset
/// to join to it (`join_fields` of it, or all; `join_left` = `join_right`),
/// and a layer to join spatially.
pub struct Names<'a> {
    pub conn: &'a str,
    pub routes: &'a str,
    pub route_id: &'a str,
    pub district: &'a str,
    pub join: &'a str,
    pub join_fields: Option<&'a [&'a str]>,
    pub join_left: &'a str,
    pub join_right: &'a str,
    pub other: &'a str,
}

/// First measure of route `R<i>`, as the fixtures write it.
pub fn route_m(i: u64) -> f64 {
    1000.0 + 10.0 * i as f64
}

async fn call(c: &RunningService<RoleClient, ()>, tool: &'static str, args: Value) -> Value {
    let mut p = CallToolRequestParams::new(tool);
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    let r: CallToolResult = c.call_tool(p).await.unwrap();
    assert_ne!(r.is_error, Some(true), "{tool}: {:?}", r.content);
    r.structured_content.clone().expect("structured output")
}

fn handle(v: &Value) -> String {
    v["result"]["layer"]["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("no layer handle in {v}"))
        .to_string()
}

fn check_ids(v: &Value) -> Vec<(String, String)> {
    v["checks"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    (
                        c["id"].as_str().unwrap_or("").to_string(),
                        c["severity"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The file behind a `ws://<session>/<path>` handle, under `workspaces`.
fn ws_path(workspaces: &Path, h: &str) -> PathBuf {
    let rest = h.trim_start_matches("ws://");
    let (session, rel) = rest.split_once('/').expect("ws://session/path");
    fn find(dir: &Path, name: &str) -> Option<PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == name) {
                    return Some(p);
                }
                if let Some(f) = find(&p, name) {
                    return Some(f);
                }
            }
        }
        None
    }
    let dir = find(workspaces, session).unwrap_or_else(|| panic!("no session folder for {h}"));
    dir.join(rel)
}

/// Each row's (route id, first vertex's M), read with M through Arrow;
/// "none" where the geometry has no M.
const PROBE: &str = r#"
import sys, warnings
warnings.simplefilter("ignore")
import pyogrio, shapely
meta, t = pyogrio.read_arrow(sys.argv[1])
g = shapely.from_wkb(t.column(meta["geometry_name"] or "wkb_geometry").to_pylist())
key = sys.argv[2]
ids = t.column(key).to_pylist() if key in t.column_names else [""] * len(g)
for i, s in zip(ids, g):
    m = shapely.get_coordinates(s, include_m=True)[0][-1] if shapely.has_m(s) else "none"
    print(f"{i}\t{m}")
"#;

fn first_ms(py: &str, path: &Path, key: &str) -> Vec<(String, String)> {
    let o = StdCommand::new(py)
        .args(["-c", PROBE])
        .arg(path)
        .arg(key)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "probe of {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(|l| {
            let (a, b) = l.split_once('\t').unwrap();
            (a.to_string(), b.to_string())
        })
        .collect()
}

/// Every row's first vertex has its route's first measure.
fn assert_route_ms(py: &str, path: &Path, key: &str, what: &str) {
    let rows = first_ms(py, path, key);
    assert!(!rows.is_empty(), "{what}: no rows in {}", path.display());
    for (id, m) in rows {
        let i: u64 = id
            .trim_start_matches('R')
            .parse()
            .unwrap_or_else(|_| panic!("{what}: route id {id:?}"));
        let got: f64 = m
            .parse()
            .unwrap_or_else(|_| panic!("{what}: {id} has no M ({m})"));
        assert!(
            (got - route_m(i)).abs() < 1e-6,
            "{what}: {id} starts at M {got}, not {}",
            route_m(i)
        );
    }
}

/// Run the chain. `share` is the export folder granted as `share`.
pub async fn run(
    client: &RunningService<RoleClient, ()>,
    py: &str,
    workspaces: &Path,
    share: &Path,
    n: &Names<'_>,
) {
    let q = call(
        client,
        "query_features",
        json!({"connection": n.conn, "dataset": n.routes, "order_by": n.route_id}),
    )
    .await;
    assert!(q["result"]["rows_read"].as_u64().unwrap() > 0, "{q}");
    let routes = handle(&q);
    assert_route_ms(
        py,
        &ws_path(workspaces, &routes),
        n.route_id,
        "query_features",
    );

    let rep = call(
        client,
        "vector_op",
        json!({"op": "reproject", "inputs": {"layer": routes}, "params": {"crs": "EPSG:3081"}}),
    )
    .await;
    let rep_h = handle(&rep);
    assert_eq!(
        rep["result"]["layer"]["summary"]["crs"]["code"],
        "EPSG:3081"
    );
    assert_route_ms(py, &ws_path(workspaces, &rep_h), n.route_id, "reproject");

    let mut args = json!({"connection": n.conn, "dataset": n.join});
    if let Some(f) = n.join_fields {
        args["fields"] = json!(f);
    }
    let d = call(client, "query_features", args).await;
    let join = call(
        client,
        "vector_op",
        json!({"op": "join", "inputs": {"left": routes, "right": handle(&d)},
               "params": {"left_key": n.join_left, "right_key": n.join_right, "key_as_text": true}}),
    )
    .await;
    assert_route_ms(py, &ws_path(workspaces, &handle(&join)), n.route_id, "join");

    let other = call(
        client,
        "query_features",
        json!({"connection": n.conn, "dataset": n.other}),
    )
    .await;
    let sj = call(
        client,
        "vector_op",
        json!({"op": "spatial_join", "inputs": {"left": routes, "right": handle(&other)}}),
    )
    .await;
    assert_route_ms(
        py,
        &ws_path(workspaces, &handle(&sj)),
        n.route_id,
        "spatial_join",
    );

    // Dissolve merges routes, so its first vertex is some route's first
    // (one group per district the connection lets through).
    let dis = call(
        client,
        "vector_op",
        json!({"op": "dissolve", "inputs": {"layer": routes}, "params": {"by": n.district}}),
    )
    .await;
    let ms = first_ms(py, &ws_path(workspaces, &handle(&dis)), n.district);
    assert!(!ms.is_empty(), "{dis}");
    for (_, m) in ms {
        let m: f64 = m
            .parse()
            .unwrap_or_else(|_| panic!("dissolve lost M: {dis}"));
        assert!(
            m >= route_m(0) && (m - 1000.0) % 10.0 == 0.0,
            "dissolve: first M {m}"
        );
    }
    assert!(!check_ids(&dis)
        .iter()
        .any(|(id, _)| id == "geometry.m_dropped"));

    for (fmt, ext) in [("gpkg", "gpkg"), ("gdb", "gdb"), ("shp", "shp")] {
        let name = format!("routes_{fmt}");
        let x = call(
            client,
            "export_output",
            json!({"handle": routes, "folder": "share", "name": name, "format": fmt}),
        )
        .await;
        assert_eq!(x["result"]["status"], "exported", "{x}");
        assert_route_ms(
            py,
            &share.join(format!("{name}.{ext}")),
            n.route_id,
            &format!("export {fmt}"),
        );
    }

    let buf = call(
        client,
        "vector_op",
        json!({"op": "buffer", "inputs": {"layer": rep_h}, "params": {"distance": 10, "unit": "m"}}),
    )
    .await;
    assert!(
        check_ids(&buf).contains(&("geometry.m_dropped".to_string(), "info".to_string())),
        "{buf}"
    );
    let ms = first_ms(py, &ws_path(workspaces, &handle(&buf)), n.route_id);
    assert!(
        !ms.is_empty() && ms.iter().all(|(_, m)| m == "none"),
        "{ms:?}"
    );
}
