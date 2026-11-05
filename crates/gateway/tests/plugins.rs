//! Engine plugins end to end (design change 1, section 7): the LRS checker
//! from the change document, run by the real gateway on governed data.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Routes and events built so each LRS check fires exactly once.
fn make_gpkg(py: &str, path: &Path) {
    let script = r#"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.executescript('''
CREATE TABLE routes (route_id TEXT, end_meas REAL, district INTEGER);
INSERT INTO routes VALUES ('IH0035', 10, 14), ('US0183', 5, 14);
CREATE TABLE events (fid INTEGER PRIMARY KEY, route_id TEXT, from_meas REAL, to_meas REAL, district INTEGER, inspector_ssn TEXT);
INSERT INTO events (route_id, from_meas, to_meas, district, inspector_ssn) VALUES
  ('IH0035', 0, 2, 14, '111-22-3333'),
  ('IH0035', 2, 4, 14, '111-22-3333'),
  ('IH0035', 3.5, 6, 14, '111-22-3333'),
  ('IH0035', 7, 9, 14, '111-22-3333'),
  ('IH0035', 9, 11, 14, '111-22-3333'),
  ('US0183', 3, 1, 14, '111-22-3333'),
  ('SH0071', 0, 1, 14, '111-22-3333'),
  ('IH0035', 5, 6, 12, '111-22-3333');
CREATE TABLE payroll (id INTEGER, amount REAL, district INTEGER);
CREATE TABLE segments (fid INTEGER PRIMARY KEY, district INTEGER, geom BLOB);
''')
import struct
def line(*pts):
    wkb = struct.pack("<BII", 1, 2, len(pts)) + b"".join(struct.pack("<dd", x, y) for x, y in pts)
    return b"GP" + bytes([0, 0x01]) + struct.pack("<i", 3081) + wkb
db.executemany("INSERT INTO segments (district, geom) VALUES (?, ?)", [
    (14, line((-98.0, 30.0), (-97.5, 30.5))),
    (14, line((-97.5, 30.5), (-97.0, 31.0))),
    (14, None),
    (12, line((-120.0, 40.0), (-119.0, 41.0))),
])
db.commit()
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(path)
        .status()
        .unwrap()
        .success());
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn plugin_dir(&self) -> PathBuf {
        self.root.join("plugins").join("lrs-check")
    }

    fn approve(&self) {
        let mut approved = String::new();
        for (id, dir, entry) in [
            (
                "contoso.lrs-check",
                self.plugin_dir(),
                "worker.py".to_string(),
            ),
            (
                "columbus.example-rust",
                self.root.join("plugins").join("geom"),
                rust_entry(),
            ),
        ] {
            let Ok(yaml) = std::fs::read_to_string(dir.join("plugin.yaml")) else {
                continue;
            };
            let mut all = yaml.into_bytes();
            all.push(0);
            all.extend_from_slice(&std::fs::read(dir.join(&entry)).unwrap());
            approved.push_str(&format!(
                "{id}: {:?}\n",
                format!("sha256:{}", columbus_shared::canon::sha256_hex(&all))
            ));
        }
        std::fs::write(self.root.join("plugins").join("approved.yaml"), approved).unwrap();
    }

    /// Install the Rust SDK's example plugin, building it if needed.
    fn install_rust_example(&self) {
        let target = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let built = target.join("examples").join(rust_entry());
        if !built.exists() {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let ok = StdCommand::new(cargo)
                .args([
                    "build",
                    "-p",
                    "columbus-plugin",
                    "--example",
                    "geometry_stats",
                ])
                .current_dir(repo())
                .status()
                .unwrap();
            assert!(ok.success(), "building the Rust example plugin failed");
        }
        let dir = self.root.join("plugins").join("geom");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(
            repo().join("workers/sdk-rust/examples/geometry-stats.plugin.yaml"),
            dir.join("plugin.yaml"),
        )
        .unwrap();
        std::fs::copy(&built, dir.join(rust_entry())).unwrap();
    }
}

fn setup(py: &str) -> Env {
    setup_with(py, 30)
}

fn setup_with(py: &str, seconds: u64) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in ["connections", "servers", "plugins/lrs-check"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    for f in ["plugin.yaml", "worker.py"] {
        std::fs::copy(
            repo().join("examples/plugins/lrs-check").join(f),
            root.join("plugins/lrs-check").join(f),
        )
        .unwrap();
    }
    let gpkg = root.join("lrs.gpkg");
    make_gpkg(py, &gpkg);
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [gpkg]\nhide_fields: [\"*_ssn\"]\nlimits: {{ rows: 1000, seconds: {seconds} }}\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("lrs.yaml"),
        format!(
            "id: lrs\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"routes\", \"events\", \"segments\"]\nrow_filter: \"district = 14\"\n",
            serde_json::to_string(&gpkg.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
}

async fn start(env: &Env, py: &str) -> RunningService<RoleClient, ()> {
    let worker = repo().join("workers/python/columbus_worker.py");
    let root = env.root.clone();
    let py = py.to_string();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_PERSONAL_PLUGINS", root.join("personal-plugins"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
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

fn provider<'a>(lp: &'a Value, id: &str) -> &'a Value {
    lp["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap()
}

const RUN: &str = "contoso.lrs-check.check_events";

fn rust_entry() -> String {
    format!("geometry_stats{}", std::env::consts::EXE_SUFFIX)
}

fn run_args() -> Value {
    json!({"name": RUN, "args": {"events": "lrs:events", "routes": {"connection": "lrs", "dataset": "routes"}}})
}

#[tokio::test]
async fn unapproved_and_changed_plugins_stay_off() {
    let Some(py) = python() else { return };
    let env = setup(&py);
    let c = start(&env, &py).await;
    let lp = ok(&call(&c, "list_providers", json!({})).await);
    assert_eq!(
        provider(&lp, "contoso.lrs-check")["status"]["state"],
        "unapproved"
    );
    let e = err(&call(&c, "run_tool", run_args()).await);
    assert!(e.contains("has not been approved"), "{e}");
    c.cancel().await.unwrap();

    env.approve();
    std::fs::write(env.plugin_dir().join("worker.py"), "# tampered\n").unwrap();
    let c = start(&env, &py).await;
    let lp = ok(&call(&c, "list_providers", json!({})).await);
    assert_eq!(
        provider(&lp, "contoso.lrs-check")["status"]["state"],
        "changed"
    );
    let e = err(&call(&c, "run_tool", run_args()).await);
    assert!(e.contains("changed since it was approved"), "{e}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_worker_that_does_not_match_its_manifest_is_refused() {
    let Some(py) = python() else { return };
    let env = setup(&py);
    // The manifest declares a parameter the worker does not.
    let yaml = std::fs::read_to_string(env.plugin_dir().join("plugin.yaml")).unwrap();
    let yaml = yaml.replace(
        "      tolerance:",
        "      upload_to: { type: string, default: x }\n      tolerance:",
    );
    std::fs::write(env.plugin_dir().join("plugin.yaml"), yaml).unwrap();
    env.approve();
    let c = start(&env, &py).await;
    let e = err(&call(&c, "run_tool", run_args()).await);
    assert!(e.contains("do not match plugin.yaml"), "{e}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn the_lrs_checker_runs_on_governed_data() {
    let Some(py) = python() else { return };
    let env = setup(&py);
    env.approve();
    let c = start(&env, &py).await;

    let lp = ok(&call(&c, "list_providers", json!({})).await);
    let p = provider(&lp, "contoso.lrs-check");
    assert_eq!(
        (p["kind"].as_str(), p["status"]["state"].as_str()),
        (Some("native"), Some("ready"))
    );
    let found = ok(&call(
        &c,
        "search_tools",
        json!({"query": "route events gaps overlaps"}),
    )
    .await);
    assert_eq!(found["result"][0]["name"], RUN);
    let d = ok(&call(&c, "describe_tool", json!({"name": RUN})).await);
    assert_eq!(
        d["result"]["input_schema"]["required"],
        json!(["events", "routes"])
    );
    assert!(d["result"]["reviewed_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    // Arguments are checked against plugin.yaml before anything runs.
    let mut bad = run_args();
    bad["args"]["upload_to"] = json!("https://x.example");
    assert!(err(&call(&c, "run_tool", bad).await).contains("no parameter \\\"upload_to\\\""));
    let mut bad = run_args();
    bad["args"]["tolerance"] = json!(-1);
    assert!(err(&call(&c, "run_tool", bad).await).contains("out of range"));
    let mut bad = run_args();
    bad["args"]["events"] = json!("lrs:payroll");
    assert!(
        err(&call(&c, "run_tool", bad).await).contains("not in this connection's allowed datasets")
    );
    let mut bad = run_args();
    bad["args"]["events"] = json!({"connection": "lrs", "dataset": "events", "where": "1=1 OR route_id IN (SELECT id FROM payroll)"});
    assert!(err(&call(&c, "run_tool", bad).await).contains("not allowed in a where clause"));

    let r = ok(&call(&c, "run_tool", run_args()).await);
    assert_eq!(r["result"]["status"], "done");
    // Seven events in district 14; the district 12 one is filtered out by the server.
    assert_eq!(r["result"]["summary"]["events_checked"], 7);
    let checks = r["checks"].as_array().unwrap();
    for id in [
        "lrs.overlap",
        "lrs.gap",
        "lrs.out_of_range",
        "lrs.reversed",
        "lrs.unknown_route",
    ] {
        let c = checks
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("missing {id}: {checks:?}"));
        assert_eq!(c["detail"]["count"], 1, "{id}");
    }
    assert!(checks.iter().any(|c| c["id"] == "flow.labels"));
    assert!(checks.iter().any(|c| c["id"] == "verify.pending"));
    let saved = r["result"]["saved"].as_array().unwrap();
    assert_eq!(saved.len(), 2);
    let report = saved.iter().find(|f| f["kind"] == "table").unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_string();

    // Outputs are workspace files that carry the input's data label.
    let lo = ok(&call(&c, "list_outputs", json!({})).await);
    let card = lo["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["handle"] == report.as_str())
        .unwrap();
    assert_eq!(card["labels"], json!(["connection:lrs"]));
    assert_eq!(card["rows"], 5);
    assert_eq!(card["made_by"], RUN);

    let who = ok(&call(&c, "whoami", json!({})).await);
    let ws = PathBuf::from(who["result"]["workspace"].as_str().unwrap());
    let csv = std::fs::read_to_string(
        ws.join(
            report
                .split_once("/results/")
                .map(|(_, f)| format!("results/{f}"))
                .unwrap(),
        ),
    )
    .unwrap();
    assert!(csv.contains("lrs.gap") && csv.contains("SH0071"));

    // The plugin's extracts never held the hidden field.
    for e in std::fs::read_dir(ws.join("inputs")).unwrap() {
        let bytes = std::fs::read(e.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("inspector_ssn") && !text.contains("111-22-3333"));
    }
    // Provenance names the governed inputs and the plugin's exact version.
    let prov = r["provenance"].as_str().unwrap();
    let p: Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join("provenance").join(format!("{prov}.prov.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(p["inputs"][0]["connection"], "lrs");
    assert!(p["inputs"][0]["executed_sql"]
        .as_str()
        .unwrap()
        .contains("district = 14"));
    assert!(p["steps"][0]["plugin_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    c.cancel().await.unwrap();

    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).unwrap();
    assert!(std::fs::read_to_string(&audit)
        .unwrap()
        .contains("\"provider\":\"contoso.lrs-check\""));
}

#[tokio::test]
async fn a_rust_plugin_reads_governed_geometry_and_stops_on_time() {
    let Some(py) = python() else { return };
    let env = setup_with(&py, 3);
    env.install_rust_example();
    env.approve();
    let c = start(&env, &py).await;

    let lp = ok(&call(&c, "list_providers", json!({})).await);
    assert_eq!(
        provider(&lp, "columbus.example-rust")["status"]["state"],
        "ready"
    );

    let r = ok(&call(
        &c,
        "run_tool",
        json!({"name": "columbus.example-rust.bbox", "args": {"layer": "lrs:segments"}}),
    )
    .await);
    assert_eq!(r["result"]["status"], "done");
    // Three segments in district 14 (one without geometry); the district 12 one never reaches the plugin.
    assert_eq!(
        r["result"]["summary"]["rows"], 3,
        "{}",
        r["result"]["summary"]
    );
    let got: Vec<f64> = r["result"]["summary"]["bbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(got, vec![-98.0, 30.0, -97.0, 31.0]);
    assert_eq!(r["result"]["saved"].as_array().unwrap().len(), 1);

    // A job that runs past the policy's time limit is stopped, not left running.
    let started = std::time::Instant::now();
    let e = err(&call(
        &c,
        "run_tool",
        json!({"name": "columbus.example-rust.wait", "args": {"seconds": 20}}),
    )
    .await);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(12),
        "stopped in {:?}",
        started.elapsed()
    );
    // Either timer may stop it first: the server's (which kills the job) or
    // the plugin's own deadline from the SDK. Both are a timely stop.
    assert!(
        e.contains("longer than 3 s")
            || e.contains("longer than its time limit")
            || e.to_lowercase().contains("timed out"),
        "{e}"
    );
    // The plugin still works afterwards.
    ok(&call(
        &c,
        "run_tool",
        json!({"name": "columbus.example-rust.bbox", "args": {"layer": "lrs:segments"}}),
    )
    .await);
    c.cancel().await.unwrap();
}

/// Design change 1, milestone 1 gate: a level A plugin cannot read outside
/// the workspace (nor the network). The plugin here tries to: it reads a
/// file outside the workspace and the connection's own GeoPackage (which
/// plugins must only see through extracts), connects to a local listener,
/// and writes inside the workspace, which is allowed.
#[tokio::test]
async fn a_level_a_plugin_cannot_read_outside_the_workspace() {
    let Some(py) = python() else { return };
    if columbus_confine_ready().is_none() {
        eprintln!("skipping: this machine cannot confine");
        return;
    }
    let env = setup(&py);
    let secret = env.root.join("secret.txt");
    std::fs::write(&secret, "the secret").unwrap();
    let dir = env.root.join("plugins").join("snoop");
    std::fs::create_dir_all(&dir).unwrap();
    let yaml = "id: test.snoop\nversion: 1.0.0\nruntime: python\nentry: worker.py\ntools:\n  snoop:\n    summary: Try to read and connect where it should not.\n    params:\n      path: { type: string }\n      gpkg: { type: string }\n      results: { type: string }\n      port: { type: number, min: 1, max: 65535 }\n    risk: read\n";
    let worker = r#"
import os, socket
from columbus_plugin import Plugin

plugin = Plugin("test.snoop", "1.0.0")

def attempt(fn):
    try:
        return "ok: " + str(fn())[:40]
    except Exception as e:
        return "refused: " + type(e).__name__

@plugin.tool("snoop", params=["path", "gpkg", "results", "port"])
def snoop(ctx):
    def connect():
        s = socket.create_connection(("127.0.0.1", int(ctx.param("port"))), timeout=3)
        s.close()
        return "connected"
    def write_here():
        with open("written-by-plugin.txt", "w") as f:
            f.write("x")
        return os.path.abspath("written-by-plugin.txt")
    return {
        "read_outside": attempt(lambda: open(ctx.param("path")).read()),
        "read_gpkg": attempt(lambda: open(ctx.param("gpkg"), "rb").read(16)),
        "read_results": attempt(lambda: open(ctx.param("results")).read()),
        "network": attempt(connect),
        "write_workspace": attempt(write_here),
    }

if __name__ == "__main__":
    plugin.serve()
"#;
    std::fs::write(dir.join("plugin.yaml"), yaml).unwrap();
    std::fs::write(dir.join("worker.py"), worker).unwrap();
    let mut all = yaml.as_bytes().to_vec();
    all.push(0);
    all.extend_from_slice(worker.as_bytes());
    std::fs::write(
        env.root.join("plugins").join("approved.yaml"),
        format!(
            "test.snoop: {:?}\n",
            format!("sha256:{}", columbus_shared::canon::sha256_hex(&all))
        ),
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let c = start(&env, &py).await;
    let lp = ok(&call(&c, "list_providers", json!({})).await);
    let p = provider(&lp, "test.snoop");
    assert_eq!(p["confinement"], "A", "{p}");
    assert_eq!(p["status"]["state"], "ready", "{p}");
    // A file in the session's results, where other tools' outputs live.
    let who = ok(&call(&c, "whoami", json!({})).await);
    let results =
        std::path::Path::new(who["result"]["workspace"].as_str().unwrap()).join("results");
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("governed.txt"), "governed rows").unwrap();
    let r = ok(&call(
        &c,
        "run_tool",
        json!({"name": "test.snoop.snoop", "args": {
            "results": results.join("governed.txt").display().to_string(),
            "path": secret.display().to_string(),
            "gpkg": env.root.join("lrs.gpkg").display().to_string(),
            "port": port,
        }}),
    )
    .await);
    let res = &r["result"]["summary"];
    let text = res.to_string();
    assert!(
        !text.contains("the secret"),
        "read outside the workspace: {text}"
    );
    assert!(
        !text.contains("governed rows"),
        "read the workspace results: {text}"
    );
    for k in ["read_outside", "read_gpkg", "read_results", "network"] {
        assert!(
            res[k].as_str().unwrap_or("").starts_with("refused"),
            "{k} should be refused at level A: {text}"
        );
    }
    assert!(
        res["write_workspace"]
            .as_str()
            .unwrap_or("")
            .starts_with("ok"),
        "writing in its own folder is allowed: {text}"
    );
    c.cancel().await.unwrap();
}

/// The launcher, if this machine can confine at level A.
fn columbus_confine_ready() -> Option<()> {
    let launcher = PathBuf::from(env!("CARGO_BIN_EXE_columbus-confine"));
    StdCommand::new(launcher)
        .args(["--check", "A"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| ())
}
