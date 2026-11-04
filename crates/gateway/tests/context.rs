//! The context pack through the real server (design section 5): always-on
//! rules in whoami, search, entries, data groups, and the dataset entry
//! attached to describe_dataset. Uses the fictional Contoso County pack.

use std::path::{Path, PathBuf};
use std::process::Command;

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
            Command::new(p)
                .args(["-c", import])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/context/contoso-county")
        .canonicalize()
        .unwrap()
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
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_CONTEXT", sample())
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_LOG", "warn")
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
    r.structured_content.clone().unwrap()
}

#[tokio::test]
async fn context_pack_in_a_session() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj") else {
        eprintln!("skipping: GIS engine not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = root.join("roads.gpkg");
    let script = r#"
import sys, geopandas as gpd
from shapely.geometry import LineString
gpd.GeoDataFrame({"ROUTE_ID": ["CR0001"], "FROM_MEAS": [0.0], "TO_MEAS": [1.0], "FUNC_CLASS": [5], "STATUS": ["A"]},
    geometry=[LineString([(0, 0), (1609, 0)])], crs="EPSG:3081").to_file(sys.argv[1], layer="roadway_inventory", driver="GPKG", engine="pyogrio")
"#;
    assert!(Command::new(&py)
        .args(["-c", script])
        .arg(&gpkg)
        .status()
        .unwrap()
        .success());
    std::fs::write(root.join("policy.yaml"), "connection_types: [gpkg]\n").unwrap();
    std::fs::write(
        root.join("connections").join("roads-prod.yaml"),
        format!(
            "id: roads-prod\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"roadway_inventory\"]\n",
            serde_json::to_string(&gpkg.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    let c = start(root, &py).await;

    let who = ok(&call(&c, "whoami", json!({})).await);
    let rules = who["result"]["always_on_rules"].to_string();
    assert!(
        rules.contains("EPSG:3081") && rules.contains("miles"),
        "{rules}"
    );

    let s = ok(&call(&c, "context_search", json!({"query": "lrs quality check"})).await);
    assert_eq!(
        s["result"]["cards"][0]["id"], "monthly-lrs-qc",
        "{}",
        s["result"]
    );
    // The sign inventory was last confirmed more than 12 months ago.
    let s = ok(&call(
        &c,
        "context_search",
        json!({"query": "signs", "kind": "dataset"}),
    )
    .await);
    assert_eq!(s["result"]["cards"][0]["stale"], true, "{}", s["result"]);
    assert!(s["checks"].to_string().contains("context.stale"));

    let g = ok(&call(&c, "context_get", json!({"id": "monthly-lrs-qc"})).await);
    assert!(g["result"]["body"].as_str().unwrap().contains("0.001 mile"));

    let dg = ok(&call(
        &c,
        "describe_data_group",
        json!({"id": "live-grid-linework"}),
    )
    .await);
    let ds = dg["result"]["datasets"].as_array().unwrap();
    assert_eq!(ds.len(), 3);
    assert_eq!(ds[1]["filter"], "STATUS <> 'R'");
    assert!(
        dg["checks"]
            .to_string()
            .contains("context.datasets_unavailable"),
        "two of its datasets are on connections this session lacks"
    );

    // describe_dataset carries the pack's entry, gotchas included.
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "roads-prod", "dataset": "roadway_inventory"}),
    )
    .await);
    let entry = &d["result"]["context"];
    assert_eq!(entry["card"]["id"], "roadway-inventory", "{}", d["result"]);
    assert!(entry["body"]
        .as_str()
        .unwrap()
        .contains("Measures are in miles"));

    let e = call(&c, "context_get", json!({"id": "nope"})).await;
    assert_eq!(e.is_error, Some(true));
    c.cancel().await.unwrap();
}

#[test]
fn pack_lint_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"))
        .args(["pack", "lint"])
        .arg(sample())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("Contoso County GIS") && text.contains("No problems found"),
        "{text}"
    );
}
