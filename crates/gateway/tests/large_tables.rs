//! Large tables end to end: a 300,000-row table is cut off at the default
//! read limit of 200,000 rows (and says so) until organization policy
//! raises the limit with `allow_raise`; then query_features streams every
//! row into the workspace layer in batches, and sql_select streams them to
//! its CSV. A raise without the opt-in is ignored and said so in whoami.
//!
//! Needs Python with geopandas, pyogrio and shapely; skipped otherwise.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const ROWS: u64 = 300_000;

fn python() -> Option<String> {
    support::python_for("import geopandas, pyogrio, shapely")
}

/// 300,000 points in EPSG:3081 with a code and a hidden field.
fn big_table(py: &str, root: &Path) -> PathBuf {
    let path = root.join("big.gpkg");
    let script = r#"
import sys
import geopandas as gpd, shapely
n = int(sys.argv[2])
pts = shapely.points([(1_000_000 + (i % 1000) * 10, 7_000_000 + (i // 1000) * 10) for i in range(n)])
gpd.GeoDataFrame({"k": range(n), "code": [f"{i % 50:03d}" for i in range(n)], "owner_ssn": ["123-45-6789"] * n},
    geometry=pts, crs="EPSG:3081").to_file(sys.argv[1], layer="pts", driver="GPKG", engine="pyogrio")
"#;
    let out = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(&path)
        .arg(ROWS.to_string())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

async fn start(root: &Path, py: &str) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = root.to_path_buf();
    let py = py.to_string();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_LIVE_DIR", root.join("live"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_COMPANION", "off")
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
        });
    ().serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap()
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

fn ids(v: &Value) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

/// Write a settings file whole and wait out the moment a session lets a
/// changed file settle.
fn put(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp-write");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
}

/// Features and fields as GDAL reads the layer back.
fn read_back(py: &str, gpkg: &Path) -> Value {
    let script = r#"
import sys, json, pyogrio
info = pyogrio.read_info(sys.argv[1])
print(json.dumps({"features": int(info["features"]), "fields": [str(f) for f in info["fields"]], "crs": info["crs"]}))
"#;
    let out = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(gpkg)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn workspace_file(handle: &str, root: &Path) -> PathBuf {
    // ws://<session>/<path>
    let rest = handle.strip_prefix("ws://").unwrap();
    let (session, rel) = rest.split_once('/').unwrap();
    root.join("workspaces").join(session).join(rel)
}

#[tokio::test]
async fn a_raised_limit_reads_a_table_past_the_default_and_streams_it() {
    let Some(py) = python() else {
        eprintln!("skipped: needs Python with geopandas, pyogrio and shapely");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = big_table(&py, &root);
    let policy = root.join("policy.yaml");
    // Asks for more than the default, without the opt-in: ignored.
    put(
        &policy,
        "connection_types: [gpkg]\nhide_fields: [\"*_ssn\"]\nlimits: { rows: 2000000, seconds: 600 }\n",
    );
    let p = serde_json::to_string(&gpkg.display().to_string()).unwrap();
    put(
        &root.join("connections/big.yaml"),
        &format!("id: big\ntype: gpkg\npath: {p}\ndatasets:\n  allow: [\"*\"]\n"),
    );
    let c = start(&root, &py).await;

    let who = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(who["result"]["policy"]["rows_read_per_query"], 200_000);
    assert_eq!(who["result"]["policy"]["limits_raised"], false);
    assert_eq!(
        who["result"]["policy"]["limits_ceiling"]["rows"],
        20_000_000
    );
    let notes = who["result"]["notes"].to_string();
    assert!(notes.contains("allow_raise"), "{notes}");

    // At the default limit: cut off and said so.
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "big", "dataset": "pts"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], 200_000);
    assert!(
        ids(&q).contains(&"query.read_limit".to_string()),
        "{:?}",
        ids(&q)
    );
    assert!(ids(&q).contains(&"query.streamed".to_string()));
    let cut = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let back = read_back(&py, &workspace_file(&cut, &root));
    assert_eq!(back["features"], 200_000);
    // The limit is the same one everywhere: describe_connection says it too.
    let d = ok(&call(&c, "describe_connection", json!({"connection": "big"})).await);
    assert_eq!(d["result"]["rows_read_per_query"], 200_000);

    // The organization opts in: the raised limit applies from the next call.
    put(
        &policy,
        "connection_types: [gpkg]\nhide_fields: [\"*_ssn\"]\nlimits: { rows: 2000000, seconds: 600, allow_raise: true }\n",
    );
    let who = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(who["result"]["policy"]["rows_read_per_query"], 2_000_000);
    assert_eq!(who["result"]["policy"]["query_seconds"], 600);
    assert_eq!(who["result"]["policy"]["limits_raised"], true);
    let d = ok(&call(&c, "describe_connection", json!({"connection": "big"})).await);
    assert_eq!(d["result"]["rows_read_per_query"], 2_000_000);

    // owner_ssn is hidden: refused by name, however many rows are allowed.
    let r = call(
        &c,
        "query_features",
        json!({"connection": "big", "dataset": "pts", "fields": ["k", "owner_ssn"]}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "big", "dataset": "pts"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], ROWS);
    assert_eq!(q["result"]["rows_returned"], 500);
    let checks = ids(&q);
    assert!(
        !checks.contains(&"query.read_limit".to_string()),
        "{checks:?}"
    );
    assert!(checks.contains(&"query.streamed".to_string()), "{checks:?}");
    let handle = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let back = read_back(&py, &workspace_file(&handle, &root));
    assert_eq!(back["features"], ROWS);
    let fields = back["fields"].to_string();
    assert!(
        fields.contains("code") && !fields.contains("owner_ssn"),
        "{fields}"
    );
    assert_eq!(q["result"]["layer"]["summary"]["rows"], ROWS);

    // The workspace record agrees, and the layer is complete (not truncated).
    let outs = ok(&call(&c, "list_outputs", json!({})).await);
    let card = outs["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["handle"] == handle.as_str())
        .unwrap()
        .clone();
    assert_eq!(card["rows"], ROWS);
    // Paging a large layer reads a page, not the whole file.
    let page = ok(&call(
        &c,
        "read_output",
        json!({"handle": handle, "offset": 299_990, "limit": 5}),
    )
    .await);
    assert_eq!(page["result"]["rows"].as_array().unwrap().len(), 5);
    assert_eq!(page["result"]["total"], ROWS);

    // sql_select streams to its CSV, and still stops at a limit asked of it
    // by the connection.
    let s = ok(&call(
        &c,
        "sql_select",
        json!({"connection": "big", "sql": "SELECT k, code FROM pts"}),
    )
    .await);
    assert_eq!(s["result"]["rows_read"], ROWS);
    let csv = s["result"]["workspace_file"].as_str().unwrap();
    let lines = std::fs::read_to_string(csv).unwrap().lines().count() as u64;
    assert_eq!(lines, ROWS + 1);

    // A connection can still only narrow a raised limit.
    put(
        &root.join("connections/big.yaml"),
        &format!("id: big\ntype: gpkg\npath: {p}\ndatasets:\n  allow: [\"*\"]\nlimits: {{ rows: 250000 }}\n"),
    );
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "big", "dataset": "pts"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], 250_000);
    assert!(ids(&q).contains(&"query.read_limit".to_string()));
    c.cancel().await.unwrap();
}
