//! run_python end to end: the real gateway with a client that cannot show
//! prompts, so every run is approved by the test playing the person on the
//! local approvals page. Covers the policy switch, the full code on the
//! approval card, a run on a governed layer whose output inherits its labels,
//! approvals bound to the code, confinement (no writes or reads outside the
//! run folder, no network, no secrets), exit codes and the time limit.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj for the fixture
//! and query_features; skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
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

fn launcher() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    dir.join(format!("columbus-confine{}", std::env::consts::EXE_SUFFIX))
}

fn can_confine() -> bool {
    StdCommand::new(launcher())
        .args(["--check", "A"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Ten roads around Austin in EPSG:3081, eight in district 14.
fn make_fixture(py: &str, folder: &Path) {
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import LineString
path = os.path.join(sys.argv[1], "austin.gpkg")
lines = [LineString([(-97.80 + i * 0.01, 30.20), (-97.80 + i * 0.01, 30.30)]) for i in range(10)]
gpd.GeoDataFrame(
    {"route_id": [f"{i:04d}" for i in range(10)], "district": [14] * 8 + [12] * 2, "lanes": [2, 4] * 5},
    geometry=lines, crs="EPSG:4326").to_crs("EPSG:3081").to_file(path, layer="roads", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(folder)
        .status()
        .unwrap()
        .success());
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn launch_file(&self) -> PathBuf {
        self.root.join("launch.txt")
    }
    fn connection_file(&self) -> PathBuf {
        self.root.join("connections").join("austin.yaml")
    }
}

fn setup(py: &str, policy: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    make_fixture(py, &root);
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [gpkg]\nlimits: {{ rows: 1000, seconds: 60 }}\n{policy}"),
    )
    .unwrap();
    let gpkg = serde_json::to_string(&root.join("austin.gpkg").display().to_string()).unwrap();
    std::fs::write(
        root.join("connections").join("austin.yaml"),
        format!(
            "id: austin\ntype: gpkg\npath: {gpkg}\ndescription: Austin roads\n\
             datasets:\n  allow: [\"roads\"]\nrow_filter: \"district = 14\"\n"
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
}

async fn start(env: &Env, py: &str) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = env.root.clone();
    let py = py.to_string();
    let launch = env.launch_file();
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_APPROVALS_LAUNCH_FILE", &launch)
                .env("COLUMBUS_DEV_SECRETS", "1")
                .env("COLUMBUS_SECRET_AUSTIN", "hunter2-not-for-scripts")
                .env("COLUMBUS_LOG", "warn");
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

fn check<'a>(v: &'a Value, id: &str) -> Option<&'a Value> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
}

/// The person's browser on the approvals page.
struct Browser {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Browser {
    async fn open(env: &Env) -> Browser {
        let deadline = Instant::now() + Duration::from_secs(15);
        let launch = loop {
            if let Ok(u) = std::fs::read_to_string(env.launch_file()) {
                if !u.is_empty() {
                    break u;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the approvals page was never opened"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let (base, t) = launch.split_once("/#t=").unwrap();
        let r = http
            .post(format!("{base}/api/session"))
            .header("origin", base)
            .header("sec-fetch-site", "same-origin")
            .json(&json!({"launch": t}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let token = r.json::<Value>().await.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        Browser {
            base: base.to_string(),
            token,
            http,
        }
    }

    async fn pending(&self, id: &str) -> Value {
        let r = self
            .http
            .get(format!("{}/api/approvals", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let all = r.json::<Value>().await.unwrap();
        all["pending"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is not waiting on the page: {all}"))
    }

    async fn decide(&self, id: &str, decision: &str) {
        let r = self
            .http
            .post(format!("{}/api/approvals/{id}", self.base))
            .bearer_auth(&self.token)
            .header("origin", &self.base)
            .header("sec-fetch-site", "same-origin")
            .json(&json!({"decision": decision}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
}

/// Ask, let the person allow it on the page, and run it with the approval.
async fn approved_run(
    c: &RunningService<RoleClient, ()>,
    b: &Browser,
    args: Value,
) -> (Value, Value) {
    let first = ok(&call(c, "run_python", args.clone()).await);
    assert_eq!(first["result"]["status"], "approval_required", "{first}");
    let id = first["result"]["approval"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let card = b.pending(&id).await;
    b.decide(&id, "allow_once").await;
    let mut with = args;
    with["approval_id"] = json!(id);
    (ok(&call(c, "run_python", with).await), card)
}

#[tokio::test]
async fn scripts_need_policy_and_the_persons_approval() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj") else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    // Off by default.
    let env = setup(&py, "approvals: { page: false }\n");
    let c = start(&env, &py).await;
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["scripts_allowed"], false);
    let e = err(&call(&c, "run_python", json!({"code": "print(1)"})).await);
    assert!(e.contains("allow_scripts"), "{e}");
    c.cancel().await.unwrap();

    // Allowed, but the person must approve, and this client cannot ask.
    let env = setup(&py, "allow_scripts: true\napprovals: { page: false }\n");
    let c = start(&env, &py).await;
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["scripts_allowed"], true);
    let r = call(&c, "run_python", json!({"code": "print(1)"})).await;
    if !can_confine() {
        assert!(err(&r).contains("confined"));
        c.cancel().await.unwrap();
        return;
    }
    let r = ok(&r);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    assert!(r["result"]["approval"]["summary"]
        .as_str()
        .unwrap()
        .contains("Run a Python script (1 line, "));
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "x".repeat(100 * 1024 + 1)}),
    )
    .await);
    assert!(e.contains("at most 100 KB"), "{e}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn approved_scripts_run_confined() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj") else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(
        &py,
        "allow_scripts: true\napprovals: { page: true, wait_seconds: 0 }\n",
    );
    let c = start(&env, &py).await;
    if !can_confine() {
        let e = err(&call(&c, "run_python", json!({"code": "print(1)"})).await);
        assert!(e.contains("confined"), "{e}");
        c.cancel().await.unwrap();
        return;
    }
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "fields": ["route_id", "lanes"], "order_by": "route_id"}),
    )
    .await);
    let roads = q["result"]["layer"]["handle"].as_str().unwrap().to_string();

    // Read the layer, write a table, print.
    let code = r#"import json, os, sqlite3, csv
paths = json.load(open("inputs.json"))
db = sqlite3.connect(paths["roads"])
table = db.execute("SELECT table_name FROM gpkg_contents").fetchone()[0]
rows = db.execute(f'SELECT route_id, lanes FROM "{table}" ORDER BY route_id').fetchall()
with open(os.path.join("out", "result.csv"), "w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["route_id", "lanes_x2"])
    for r, l in rows:
        w.writerow([r, l * 2])
print(f"rows={len(rows)} run_dir_ok={os.path.samefile(os.environ['COLUMBUS_RUN_DIR'], os.getcwd())}")
"#;
    let args = json!({"code": code, "inputs": {"roads": roads}});
    let first = ok(&call(&c, "run_python", args.clone()).await);
    assert_eq!(first["result"]["status"], "approval_required", "{first}");
    let id = first["result"]["approval"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = Browser::open(&env).await;
    let card = b.pending(&id).await;
    // The person sees the full code, the inputs and where the data came from.
    assert_eq!(card["risk"], "script");
    assert_eq!(card["detail"]["code"], code);
    let summary = card["summary"].as_str().unwrap();
    assert!(
        summary.contains("on input roads")
            && summary.contains("austin")
            && summary.contains("no network"),
        "{summary}"
    );
    // "For this session" is not on offer for scripts: it counts as once.
    b.decide(&id, "always_this_session").await;
    let mut with = args.clone();
    with["approval_id"] = json!(id);
    let r = ok(&call(&c, "run_python", with.clone()).await);
    assert_eq!(r["result"]["status"], "done", "{r}");
    assert_eq!(r["result"]["exit_code"], 0);
    assert!(
        r["result"]["stdout"]
            .as_str()
            .unwrap()
            .contains("rows=8 run_dir_ok=True"),
        "{r}"
    );
    let out = r["result"]["outputs"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        out.contains("/results/script-") && out.ends_with("-result.csv"),
        "{out}"
    );
    assert_eq!(r["result"]["outputs"][0]["summary"]["rows"], 8);
    // The same code again needs a new approval, and the old one is spent.
    let again = ok(&call(&c, "run_python", args.clone()).await);
    assert_eq!(again["result"]["status"], "approval_required");
    assert!(err(&call(&c, "run_python", with).await).contains("already used"));

    // The output is a layer the other tools read, carrying the input's labels.
    let rows = ok(&call(&c, "read_output", json!({"handle": out, "limit": 3})).await);
    assert_eq!(rows["result"]["total"], 8);
    assert_eq!(rows["result"]["rows"][0][0], "0000");
    assert_eq!(rows["result"]["rows"][0][1], "4");
    let listed = ok(&call(&c, "list_outputs", json!({})).await);
    let card = listed["result"]
        .as_array()
        .or_else(|| listed["result"]["outputs"].as_array())
        .unwrap()
        .iter()
        .find(|o| o["handle"] == out)
        .cloned()
        .unwrap_or_else(|| panic!("{out} not listed: {listed}"));
    assert!(
        card["labels"].to_string().contains("connection:austin"),
        "{card}"
    );
    // Provenance records the code and the input.
    let prov = r["provenance"].as_str().unwrap();
    let who = ok(&call(&c, "whoami", json!({})).await);
    let ws = PathBuf::from(who["result"]["workspace"].as_str().unwrap());
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join("provenance").join(format!("{prov}.prov.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(rec["steps"][0]["script"]["code"], code);
    assert_eq!(rec["inputs"][0]["handle"], roads);
    assert!(rec["inputs"][0]["provenance"].is_string());
    // The run folder is gone; the output lives in results/.
    assert!(!ws.join("runs").read_dir().unwrap().any(|_| true));

    // An approval is bound to the code it was given for.
    let other = json!({"code": "print('other')"});
    let first = ok(&call(&c, "run_python", json!({"code": "print('approved')"})).await);
    let id = first["result"]["approval"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    b.decide(&id, "allow_once").await;
    let mut swapped = other.clone();
    swapped["approval_id"] = json!(id);
    let e = err(&call(&c, "run_python", swapped).await);
    assert!(e.contains("different request"), "{e}");

    // Confinement: no writes or reads outside the run folder, no network,
    // nothing of the gateway's environment, links in out/ not taken.
    let escape = ws.join("escape.txt");
    let code = format!(
        r#"import os, socket
def attempt(name, f):
    try:
        f()
        print(name + ": ok")
    except Exception as e:
        print(name + ": refused", type(e).__name__)
attempt("write_workspace", lambda: open({escape:?}, "w").write("x"))
attempt("read_connections", lambda: open({conn:?}).read())
attempt("list_workspace", lambda: os.listdir({ws:?}))
attempt("network", lambda: socket.create_connection(("127.0.0.1", 9), timeout=2))
attempt("link", lambda: os.symlink({conn:?}, os.path.join("out", "link.csv")))
open(os.path.join("out", "fine.csv"), "w").write("a\n1\n")
print("env", sorted(os.environ))
"#,
        escape = escape.display().to_string(),
        conn = env.connection_file().display().to_string(),
        ws = ws.display().to_string(),
    );
    let (r, _) = approved_run(&c, &b, json!({"code": code})).await;
    let stdout = r["result"]["stdout"].as_str().unwrap();
    for line in [
        "write_workspace: refused",
        "read_connections: refused",
        "list_workspace: refused",
        "network: refused",
        // A sandboxed process on Windows cannot make links at all.
        if cfg!(windows) {
            "link: refused"
        } else {
            "link: ok"
        },
    ] {
        assert!(stdout.contains(line), "{line} in {stdout}");
    }
    assert!(!escape.exists());
    assert!(
        !stdout.contains("hunter2")
            && !stdout.contains("COLUMBUS_SECRET")
            && !stdout.contains("COLUMBUS_POLICY")
    );
    assert!(stdout.contains("COLUMBUS_RUN_DIR"));
    let skipped = check(&r, "script.output_skipped").expect("link reported");
    assert_eq!(skipped["severity"], "warn");
    assert!(skipped["message"].as_str().unwrap().contains("link.csv"));
    let outs = r["result"]["outputs"].as_array().unwrap();
    assert_eq!(outs.len(), 1, "{r}");
    assert!(outs[0]["handle"].as_str().unwrap().ends_with("-fine.csv"));

    // A non-zero exit is a fail, with stderr and capped stdout.
    let (r, _) = approved_run(
        &c,
        &b,
        json!({"code": "import sys\nprint('x' * 100000)\nprint('it broke', file=sys.stderr)\nsys.exit(3)\n"}),
    )
    .await;
    assert_eq!(r["result"]["status"], "failed");
    assert_eq!(r["result"]["exit_code"], 3);
    assert_eq!(check(&r, "script.exit_code").unwrap()["severity"], "fail");
    assert_eq!(
        check(&r, "script.stdout_truncated").unwrap()["severity"],
        "info"
    );
    assert!(r["result"]["stdout"].as_str().unwrap().len() < 70 * 1024);
    assert!(r["result"]["stderr"].as_str().unwrap().contains("it broke"));

    // The time limit ends the script and what it started.
    let code = r#"import subprocess, sys, time
p = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"])
print("child", p.pid, flush=True)
time.sleep(120)
"#;
    let started = Instant::now();
    let (r, _) = approved_run(&c, &b, json!({"code": code, "timeout_seconds": 2})).await;
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(r["result"]["status"], "failed");
    assert_eq!(check(&r, "script.timeout").unwrap()["severity"], "fail");
    let child: u32 = r["result"]["stdout"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("{r}"));
    if cfg!(target_os = "linux") {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = std::fs::read_to_string(format!("/proc/{child}/stat")).unwrap_or_default();
            let gone = state.is_empty() || state.split_whitespace().nth(2) == Some("Z");
            if gone {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the script's child outlived it: {state}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    // Inputs must be Columbus's own and unchanged.
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "inputs": {"x": format!("{}/../policy.yaml", roads.rsplit_once('/').unwrap().0)}}),
    )
    .await);
    assert!(e.contains("input x"), "{e}");
    let roads_file = {
        let (_, rel) = roads
            .strip_prefix("ws://")
            .unwrap()
            .split_once('/')
            .unwrap();
        ws.join(rel)
    };
    let mut bytes = std::fs::read(&roads_file).unwrap();
    bytes.push(0);
    std::fs::write(&roads_file, bytes).unwrap();
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "inputs": {"roads": roads}}),
    )
    .await);
    assert!(e.contains("changed after Columbus wrote it"), "{e}");

    c.cancel().await.unwrap();
    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).unwrap();
    let text = std::fs::read_to_string(&audit).unwrap();
    assert!(text.contains("\"decision\":\"running\""));
    assert!(!text.contains("hunter2"));
}
