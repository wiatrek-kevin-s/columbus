//! Benchmark (REQ-20, AF-15): how long the engine takes to start, and a
//! warm `query_features` on a generated 100,000-row GeoPackage, through the
//! real `columbus-mcp` binary and Python engine, as an MCP client sees them.
//!
//! What is timed:
//! - `engine_start_s`: from launching `columbus-mcp` until the engine has
//!   loaded its GIS libraries and answered: the MCP handshake, `whoami`
//!   (which starts the Python engine and pings it) and `describe_dataset`
//!   on a one-row layer (which waits for GDAL, PROJ and geopandas).
//! - `first_query_s` (shown, not judged): the first `query_features` reading
//!   all 100,000 rows into a workspace layer.
//! - `warm_query_s`: the same call again, once the first one has primed the
//!   engine and the file caches. Every row is read and written again; only
//!   `describe_dataset`'s answer is reused, as in any session.
//!
//! Targets (success measure 5): engine start under 10 s, warm query under
//! 2 s. The test fails only at twice those (20 s, 4 s), so runner noise does
//! not flake it; a time between target and limit passes with an
//! "over target" line for the final review.
//!
//! The targets are for the CI Linux runner, so the test is `#[ignore]`d and
//! run there by its own step in the "Rust (ubuntu-latest)" job:
//! `cargo test -p columbus-mcp --test benchmark --locked -- --ignored --nocapture`.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const ROWS: u64 = 100_000;
const ENGINE_START_TARGET: f64 = 10.0;
const WARM_QUERY_TARGET: f64 = 2.0;

fn python() -> Option<String> {
    let mut candidates: Vec<String> = std::env::var("COLUMBUS_PYTHON").into_iter().collect();
    candidates.extend(["python3".to_string(), "python".to_string()]);
    candidates.into_iter().find(|p| {
        StdCommand::new(p)
            .args(["-c", "import geopandas, pyogrio, shapely, pyproj"])
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// 100,000 points in EPSG:3081 with a few typical attributes, and a
/// one-row layer for the start-up check, written once with the engine's
/// own Python.
fn make_fixture(py: &str, path: &Path) {
    let script = r#"
import sys
import numpy as np, geopandas as gpd, shapely
path, n = sys.argv[1], int(sys.argv[2])
i = np.arange(n)
pts = shapely.points(1_000_000 + (i % 1000) * 10.0, 7_000_000 + (i // 1000) * 10.0)
gpd.GeoDataFrame(
    {"k": i, "route_id": [f"{v % 5000:05d}" for v in i], "district": (i % 25) + 1,
     "lanes": (i % 4) + 1, "aadt": (i * 37) % 90_000, "name": [f"segment {v}" for v in i]},
    geometry=pts, crs="EPSG:3081").to_file(path, layer="pts", driver="GPKG", engine="pyogrio")
gpd.GeoDataFrame({"name": ["one"]}, geometry=shapely.points([(1_000_000.0, 7_000_000.0)]),
    crs="EPSG:3081").to_file(path, layer="tiny", driver="GPKG", engine="pyogrio")
"#;
    let started = Instant::now();
    let out = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(path)
        .arg(ROWS.to_string())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fixture script failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!(
        "BENCH fixture: {ROWS} rows written in {:.2} s",
        started.elapsed().as_secs_f64()
    );
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

/// Run the full query once and time it; check that every row was read.
async fn timed_query(c: &RunningService<RoleClient, ()>) -> Duration {
    let started = Instant::now();
    let q = ok(&call(
        c,
        "query_features",
        json!({"connection": "bench", "dataset": "pts"}),
    )
    .await);
    let took = started.elapsed();
    assert_eq!(q["result"]["rows_read"], ROWS, "{q}");
    assert!(q["result"]["layer"]["handle"].is_string(), "{q}");
    took
}

/// A plain line for the log; and one more when a time is over its target.
fn judge(name: &str, secs: f64, target: f64) {
    let limit = target * 2.0;
    if secs >= target && secs < limit {
        eprintln!(
            "BENCH over target: {name}={secs:.2} is over the {target:.0} s target \
             (passes; the limit is {limit:.0} s). For the final review."
        );
    }
}

#[tokio::test]
#[ignore = "timing benchmark; CI runs it on the Linux runner with --ignored"]
async fn engine_start_and_a_warm_query_on_100k_rows_are_fast() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = root.join("bench.gpkg");
    make_fixture(&py, &gpkg);
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nlimits: { rows: 200000, seconds: 120 }\n",
    )
    .unwrap();
    let p = serde_json::to_string(&gpkg.display().to_string()).unwrap();
    std::fs::write(
        root.join("connections/bench.yaml"),
        format!("id: bench\ntype: gpkg\npath: {p}\ndatasets:\n  allow: [\"*\"]\n"),
    )
    .unwrap();

    // Engine start: launch, handshake, whoami, and the first GIS answer.
    let started = Instant::now();
    let c = start(&root, &py).await;
    let who = ok(&call(&c, "whoami", json!({})).await);
    assert!(
        who["result"]["engines"]["error"].is_null(),
        "engine did not answer: {who}"
    );
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "bench", "dataset": "tiny"}),
    )
    .await);
    let engine_start = started.elapsed().as_secs_f64();
    assert!(d["result"].is_object(), "{d}");

    let first = timed_query(&c).await.as_secs_f64();
    let warm = timed_query(&c).await.as_secs_f64();
    c.cancel().await.unwrap();

    eprintln!(
        "BENCH engine_start_s={engine_start:.2} first_query_s={first:.2} warm_query_s={warm:.2} rows={ROWS}"
    );
    judge("engine_start_s", engine_start, ENGINE_START_TARGET);
    judge("warm_query_s", warm, WARM_QUERY_TARGET);
    assert!(
        engine_start < ENGINE_START_TARGET * 2.0,
        "engine start took {engine_start:.2} s; the limit is {} s (REQ-20)",
        ENGINE_START_TARGET * 2.0
    );
    assert!(
        warm < WARM_QUERY_TARGET * 2.0,
        "warm query_features on {ROWS} rows took {warm:.2} s; the limit is {} s (REQ-20)",
        WARM_QUERY_TARGET * 2.0
    );
}
