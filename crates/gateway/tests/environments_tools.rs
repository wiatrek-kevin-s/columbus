//! Personal tools and workflow steps in registered environments (T-9;
//! build brief Part 5, F-5, F-6, F-8, F-9, REQ-13, REQ-18, R-13c), and the
//! session approval for scripts (REQ-17, R-15, AF-6).
//!
//! A personal tool names a governed environment (`environment: <id>` in its
//! plugin.yaml) and runs in it, confined at level A; a workflow step may
//! name one too. A missing, changed or trusted environment stops the tool
//! or step with "needs you", and the rest of a workflow run goes on where
//! it can. A shared tool carries only the environment's id, never its
//! interpreter's path. Under policy `scripts.session_approval`, the
//! run_python card offers "Allow this script for the session" for the
//! engine and governed level-A environments only, bound to the code's hash,
//! the environment and the session.
//!
//! The person's own steps (registering an environment, installing a tool,
//! approving a workflow) are the library calls the terminal and the app
//! make, as environments.rs and tools.rs do. Each test makes plain venvs
//! (`python -m venv --without-pip`, no network) from the engine's Python.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj, and a machine
//! that can confine at level A; skipped otherwise.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use columbus_shared::environments::{self as envs, Confinement, CreatedBy, Registration, Trust};
use columbus_shared::policy::EnvironmentsPolicy;
use columbus_shared::workflows as wf;
use reqwest::StatusCode;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const NOW: &str = "2026-11-08T10:00:00-06:00";
const ID: &str = "personal.layer-report";
const TOOL: &str = "personal.layer-report.run";
/// How a tool or step whose environment can't be used stops.
const NEEDS_YOU: &str = "needs you";

fn python(import: &str) -> Option<String> {
    support::python_for(import)
}

fn engine() -> Option<String> {
    let py = python("import geopandas, pyogrio, shapely, pyproj");
    if py.is_none() {
        eprintln!("skipping: the GIS engine is not installed");
    }
    py
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Whether this machine can set the sandbox up. Where CI says tests must
/// not skip, a machine that cannot fails the test here, on every system
/// (the one check in `support`).
fn can_confine() -> bool {
    let ok = support::can_confine(std::path::Path::new(env!("CARGO_BIN_EXE_columbus-confine")));
    if !ok {
        eprintln!("skipping: this machine cannot confine at level A");
    }
    ok
}

/// A-1: a governed environment that cannot start confined on this system
/// is refused with a plain message (the brief's fallback). Linux must
/// confine it; elsewhere CI reports which way it went.
fn a1_fallback(message: &str) -> bool {
    if message.contains("cannot run confined on this system") {
        if cfg!(target_os = "linux") {
            panic!("Linux must confine an environment: {message}");
        }
        eprintln!("A-1 RESULT: false on {}: {message}", std::env::consts::OS);
        return true;
    }
    false
}

/// A plain venv (no pip, no network) and its interpreter.
fn make_venv(py: &str, dir: &Path) -> PathBuf {
    let out = StdCommand::new(py)
        .args(["-m", "venv", "--without-pip"])
        .arg(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "venv: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let exe = if cfg!(windows) {
        dir.join("Scripts").join("python.exe")
    } else {
        dir.join("bin").join("python")
    };
    assert!(exe.exists(), "{}", exe.display());
    exe
}

fn site_packages(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        return venv.join("Lib").join("site-packages");
    }
    for e in std::fs::read_dir(venv.join("lib")).unwrap().flatten() {
        let p = e.path().join("site-packages");
        if p.is_dir() {
            return p;
        }
    }
    panic!("no site-packages in {}", venv.display());
}

/// A venv for a tool to run in: a stand-in `networkx` module (the engine
/// has none) whose ENV_MARK names the environment, so a run shows where it
/// ran.
fn make_env_venv(py: &str, dir: &Path, mark: &str) -> PathBuf {
    let exe = make_venv(py, dir);
    let pkg = site_packages(dir).join("networkx");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("__init__.py"), format!("ENV_MARK = {mark:?}\n")).unwrap();
    exe
}

/// The test-only arcpy stub (T-8) in a venv: arcpy imports, licence ArcInfo.
fn add_arcpy_stub(venv: &Path) {
    let src =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/arcpy_stub/arcpy/__init__.py");
    let dir = site_packages(venv).join("arcpy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(src, dir.join("__init__.py")).unwrap();
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
    py: String,
}

impl Env {
    fn environments(&self) -> PathBuf {
        self.root.join("environments")
    }
    fn tools(&self) -> PathBuf {
        self.root.join("tools")
    }
    fn workflows(&self) -> PathBuf {
        self.root.join("workflows")
    }
    fn launch_file(&self) -> PathBuf {
        self.root.join("launch.txt")
    }
    fn policy(&self, text: &str) {
        std::fs::write(
            self.root.join("policy.yaml"),
            format!(
                "connection_types: [gpkg]\nlimits: {{ rows: 1000, seconds: 60 }}\napprovals: {{ page: true, wait_seconds: 0 }}\n{text}"
            ),
        )
        .unwrap();
    }
    fn vars(&self) -> Vec<(String, PathBuf)> {
        let r = |p: &str| self.root.join(p);
        vec![
            ("COLUMBUS_POLICY".into(), r("policy.yaml")),
            ("COLUMBUS_CONNECTIONS".into(), r("connections")),
            ("COLUMBUS_ENVIRONMENTS".into(), r("environments")),
            ("COLUMBUS_ENVS".into(), r("envs")),
            ("COLUMBUS_SERVERS".into(), r("servers")),
            ("COLUMBUS_PLUGINS".into(), r("plugins")),
            ("COLUMBUS_PERSONAL_PLUGINS".into(), r("personal-plugins")),
            ("COLUMBUS_WORKSPACES".into(), r("workspaces")),
            ("COLUMBUS_TOOLS".into(), r("tools")),
            ("COLUMBUS_ORG_TOOLS".into(), r("org-tools")),
            ("COLUMBUS_WORKFLOWS".into(), r("workflows")),
            ("COLUMBUS_AUDIT_DIR".into(), r("audit")),
            ("COLUMBUS_LIVE_DIR".into(), r("live")),
            (
                "COLUMBUS_WORKER".into(),
                repo().join("workers/python/columbus_worker.py"),
            ),
            ("COLUMBUS_PYTHON".into(), PathBuf::from(&self.py)),
            ("COLUMBUS_APPROVALS_LAUNCH_FILE".into(), r("launch.txt")),
            ("COLUMBUS_COMPANION".into(), PathBuf::from("off")),
            ("COLUMBUS_LOG".into(), PathBuf::from("warn")),
        ]
    }
}

fn setup(py: &str, policy: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    // A real path: macOS's temp folder is behind a link.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    make_fixture(py, &root);
    for d in ["connections", "envs"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let env = Env {
        _dir: dir,
        root,
        py: py.to_string(),
    };
    env.policy(policy);
    let gpkg = serde_json::to_string(&env.root.join("austin.gpkg").display().to_string()).unwrap();
    std::fs::write(
        env.root.join("connections").join("austin.yaml"),
        format!(
            "id: austin\ntype: gpkg\npath: {gpkg}\ndescription: Austin roads\n\
             datasets:\n  allow: [\"roads\"]\nrow_filter: \"district = 14\"\n"
        ),
    )
    .unwrap();
    env
}

/// What the person does at the keyboard: probe, look, type the id.
fn register(
    env: &Env,
    id: &str,
    python: &Path,
    trust: Trust,
    confinement: Confinement,
) -> envs::Environment {
    let found = envs::probe(python, Duration::from_secs(120)).unwrap();
    let reg = Registration {
        id: id.into(),
        python: python.to_path_buf(),
        trust,
        confinement,
        created_by: CreatedBy::Person,
        description: Some("test venv".into()),
    };
    let all = EnvironmentsPolicy {
        allow: true,
        allow_trusted: true,
        allow_network: true,
        ..Default::default()
    };
    envs::register(
        &env.environments(),
        &env.root.join("envs"),
        &all,
        &reg,
        &found,
        id,
        NOW,
    )
    .unwrap()
}

async fn start(env: &Env) -> RunningService<RoleClient, ()> {
    let vars = env.vars();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            for (k, v) in &vars {
                cmd.env(k, v);
            }
        });
    ().serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap()
}

async fn call(c: &RunningService<RoleClient, ()>, tool: &str, args: Value) -> CallToolResult {
    let mut p = CallToolRequestParams::new(tool.to_string());
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
        // The next session's page writes a new link.
        let _ = std::fs::remove_file(env.launch_file());
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
        let all = self
            .http
            .get(format!("{}/api/approvals", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
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

/// A layer of this session's: the district-14 roads.
async fn roads(c: &RunningService<RoleClient, ()>, lanes: Option<u32>) -> String {
    let mut a =
        json!({"connection": "austin", "dataset": "roads", "fields": ["route_id", "lanes"]});
    if let Some(l) = lanes {
        a["where"] = json!(format!("lanes = {l}"));
    }
    let q = ok(&call(c, "query_features", a).await);
    q["result"]["layer"]["handle"].as_str().unwrap().to_string()
}

/// A personal tool that imports `networkx`, a package the engine doesn't
/// have: [`make_env_venv`] gives the environments a stand-in that says
/// which environment it is in. It reports that and the layer's size.
fn proposal(layer: &str, environment: Option<&str>) -> Value {
    let mut a = json!({
        "name": "layer_report",
        "title": "Report a layer's size",
        "description": "Writes a small JSON report with the size in bytes of the workspace layer it is given and the Python installation it ran in. Use it to check which environment a tool runs in.",
        "not_for": "Anything about the layer's rows or geometry; use verify for that.",
        "inputs": {"layer": {"kind": "layer", "description": "Any workspace layer at all.", "geometry": "any", "crs": "any"}},
        "outputs": {"report": {"kind": "report", "description": "The layer's size in bytes and the Python prefix."}},
        "examples": [{"description": "Report on the roads layer", "arguments": {"layer": "ws://<session>/results/roads.gpkg"}}],
        "code": "import json\nimport pathlib\n\nimport networkx\n\n\ndef run(ctx):\n    src = pathlib.Path(ctx.input_file(\"layer\"))\n    report = {\"bytes\": src.stat().st_size, \"mark\": networkx.ENV_MARK}\n    pathlib.Path(ctx.output_path(\"report\")).write_text(json.dumps(report), encoding=\"utf-8\")\n    return {\"bytes\": report[\"bytes\"]}\n",
        "test": {"inputs": {"layer": layer}},
    });
    if let Some(e) = environment {
        a["environment"] = json!(e);
    }
    a
}

/// Propose the tool, approve its test run on the page, and install it (as
/// Settings does). Returns the manifest, or None where A-1's fallback
/// applies on this system.
async fn propose_and_install(
    c: &RunningService<RoleClient, ()>,
    env: &Env,
    browser: &mut Option<Browser>,
    layer: &str,
    environment: Option<&str>,
) -> Option<String> {
    let r = ok(&call(c, "propose_tool", proposal(layer, environment)).await);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    let ap = r["result"]["approval"]["id"].as_str().unwrap().to_string();
    if browser.is_none() {
        *browser = Some(Browser::open(env).await);
    }
    let b = browser.as_ref().unwrap();
    let card = b.pending(&ap).await;
    if let Some(e) = environment {
        let summary = card["summary"].as_str().unwrap();
        assert!(
            summary.contains(&format!("in environment {e}")),
            "{summary}"
        );
        assert!(card["detail"]["manifest"]
            .as_str()
            .unwrap()
            .contains(&format!("environment: {e}")));
    }
    b.decide(&ap, "allow_once").await;
    let mut again = proposal(layer, environment);
    again["approval_id"] = json!(ap);
    let r = call(c, "propose_tool", again).await;
    if r.is_error == Some(true) && a1_fallback(&err(&r)) {
        return None;
    }
    let r = ok(&r);
    assert_eq!(r["result"]["status"], "waiting_for_person", "{r}");
    let hash = r["result"]["hash"].as_str().unwrap();
    columbus_shared::tools::install(&env.tools(), ID, hash, NOW).unwrap();
    Some(r["result"]["manifest"].as_str().unwrap().to_string())
}

const ENV_POLICY: &str = "allow_personal_tools: true\nallow_scripts: true\nallow_workflows: true\nenvironments: { allow: true }\n";

/// Check 1 (F-5, REQ-13, REQ-18): a tool in a governed environment runs
/// there, confined; its audit line and provenance name the environment;
/// and its shared .columbus file carries the environment's id only, never
/// its interpreter's path.
#[tokio::test]
async fn a_tool_in_a_governed_environment_runs_there_and_shares_without_its_path() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(&py, ENV_POLICY);
    let venv = env.root.join("geo-venv");
    let vpy = make_env_venv(&py, &venv, "geo");
    register(&env, "geo", &vpy, Trust::Governed, Confinement::A);
    let c = start(&env).await;
    let layer = roads(&c, None).await;
    // An id that is no environment is refused before anyone is asked.
    let e = err(&call(&c, "propose_tool", proposal(&layer, Some("nowhere"))).await);
    assert!(e.contains("nowhere") && e.contains("not proposed"), "{e}");
    let mut browser = None;
    let Some(manifest) = propose_and_install(&c, &env, &mut browser, &layer, Some("geo")).await
    else {
        c.cancel().await.unwrap();
        return;
    };
    assert!(manifest.contains("environment: geo"), "{manifest}");

    let r = ok(&call(
        &c,
        "run_tool",
        json!({"name": TOOL, "args": {"layer": layer}}),
    )
    .await);
    assert_eq!(r["result"]["status"], "done", "{r}");
    let w = ok(&call(&c, "whoami", json!({})).await);
    let ws = PathBuf::from(w["result"]["workspace"].as_str().unwrap());
    let handle = r["result"]["saved"][0]["handle"].as_str().unwrap();
    let rel = handle
        .strip_prefix("ws://")
        .unwrap()
        .split_once('/')
        .unwrap()
        .1;
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(ws.join(rel)).unwrap()).unwrap();
    assert_eq!(
        report["mark"], "geo",
        "it did not run in the environment: {report}"
    );
    assert!(report["bytes"].as_u64().unwrap() > 0);
    // F-9: the provenance step names the environment.
    let prov = r["provenance"].as_str().unwrap();
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join("provenance").join(format!("{prov}.prov.json"))).unwrap(),
    )
    .unwrap();
    let step = &rec["steps"][0];
    assert_eq!(step["environment"]["id"], "geo", "{rec}");
    assert_eq!(step["environment"]["trust"], "governed");
    assert!(step["environment"]["packages_sha256"].is_string());
    c.cancel().await.unwrap();
    // F-8: the audit line carries it.
    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect::<String>();
    assert!(
        audit
            .lines()
            .any(|l| l.contains("\"run_tool\"") && l.contains("\"environment\":\"geo\"")),
        "{audit}"
    );

    // The shared file: the id, never the interpreter's path (the recipient
    // uses their own environment of that name).
    let s = columbus_shared::share::export_tool(&env.tools(), ID, "kevin", NOW, "0.14.0").unwrap();
    let text = columbus_shared::share::to_json(&s).unwrap();
    assert!(text.contains("environment: geo"), "{text}");
    for p in [vpy.display().to_string(), venv.display().to_string()] {
        assert!(!text.contains(&p), "the shared file carries {p}");
        let escaped = serde_json::to_string(&p).unwrap();
        assert!(
            !text.contains(escaped.trim_matches('"')),
            "the shared file carries {p}"
        );
    }
    assert!(!text.contains("geo-venv"), "{text}");
}

/// Check 1 (R-13c, REQ-18): a workflow step whose environment was removed
/// stops with "needs you"; a later step that doesn't use it still runs, one
/// that does is skipped, and nothing is exported. A step naming another
/// environment runs in it (F-6).
#[tokio::test]
async fn a_workflow_step_whose_environment_was_removed_stops_with_needs_you() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(&py, ENV_POLICY);
    let pro = make_env_venv(&py, &env.root.join("pro-venv"), "pro");
    let other = env.root.join("other-venv");
    let opy = make_env_venv(&py, &other, "other");
    register(&env, "pro", &pro, Trust::Governed, Confinement::A);
    register(&env, "other", &opy, Trust::Governed, Confinement::A);
    let c = start(&env).await;
    let layer = roads(&c, None).await;
    let mut browser = None;
    if propose_and_install(&c, &env, &mut browser, &layer, Some("pro"))
        .await
        .is_none()
    {
        c.cancel().await.unwrap();
        return;
    }
    let text = "id: env-check\nname: Environment check\nscope: { connections: [austin], run_minutes: 5 }\nsteps:\n  - id: roads\n    tool: query_features\n    args: { connection: austin, dataset: roads }\n  - id: report\n    tool: personal.layer-report.run\n    args: { layer: \"${roads}\" }\n  - id: elsewhere\n    tool: personal.layer-report.run\n    environment: other\n    args: { layer: \"${roads}\" }\n  - id: after\n    tool: verify\n    args: { layer: \"${report}\" }\n  - id: checked\n    tool: verify\n    args: { layer: \"${roads}\" }\n";
    let r = ok(&call(&c, "propose_workflow", json!({"workflow": text})).await);
    assert_eq!(r["result"]["status"], "waiting_for_person", "{r}");
    c.cancel().await.unwrap();
    let p = wf::read_proposal(&env.workflows(), &env.tools(), "env-check").unwrap();
    assert!(p
        .plain
        .iter()
        .any(|l| l.contains("personal.layer-report.run in environment other")));
    wf::install(
        &env.workflows(),
        &env.tools(),
        "env-check",
        &p.fingerprint,
        NOW,
    )
    .unwrap();

    // The person removes the environment the tool runs in.
    let removal = envs::remove(&env.environments(), &env.root.join("envs"), "pro", "pro").unwrap();
    let _ = removal;
    assert!(!env.environments().join("pro.yaml").exists());
    let out = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
        .args(["workflow", "run", "env-check"])
        .envs(env.vars())
        .output()
        .unwrap();
    let runs = wf::list_runs(&env.workflows(), "env-check", 1);
    let latest = runs
        .first()
        .unwrap_or_else(|| panic!("no run recorded: {}", String::from_utf8_lossy(&out.stderr)));
    let (rec, report) = wf::read_run(
        &env.workflows(),
        "env-check",
        latest["run"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(rec["status"], "needs_you", "{rec}");
    let step = |id: &str| {
        rec["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no step {id}: {rec}"))
    };
    let s = step("report");
    assert_eq!(s["status"], "needs_you", "{s}");
    let why = s["error"].as_str().unwrap();
    assert!(why.contains(NEEDS_YOU) && why.contains("pro"), "{why}");
    // The rest of the run goes on where it can.
    let e = step("elsewhere");
    if !(e["status"] == "failed" && a1_fallback(e["error"].as_str().unwrap_or(""))) {
        assert_eq!(e["status"], "done", "{e}");
        // F-6: it ran in the step's environment, not the tool's.
        let ws = PathBuf::from(rec["workspace"].as_str().unwrap());
        let prov: Value = serde_json::from_str(
            &std::fs::read_to_string(
                ws.join("provenance")
                    .join(format!("{}.prov.json", e["provenance"].as_str().unwrap())),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(prov["steps"][0]["environment"]["id"], "other", "{prov}");
    }
    assert_eq!(step("after")["status"], "skipped", "{rec}");
    assert_eq!(step("checked")["status"], "done", "{rec}");
    assert!(report.to_lowercase().contains("needs you"), "{report}");
}

/// Check 1 (REQ-18 edge, D-3): a tool and a workflow step that name a
/// trusted environment are both refused, at proposal and at run, whatever
/// the policy says about trusted environments; one at level B too.
#[tokio::test]
async fn a_trusted_environment_is_refused_for_tools_and_steps() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(
        &py,
        "allow_personal_tools: true\nallow_scripts: true\nallow_workflows: true\nenvironments: { allow: true, allow_trusted: true, allow_network: true }\n",
    );
    let tpy = make_venv(&py, &env.root.join("trusted-venv"));
    let gpy = make_env_venv(&py, &env.root.join("geo-venv"), "geo");
    let npy = make_venv(&py, &env.root.join("net-venv"));
    register(&env, "pro", &tpy, Trust::Trusted, Confinement::A);
    register(&env, "geo", &gpy, Trust::Governed, Confinement::A);
    register(&env, "net", &npy, Trust::Governed, Confinement::B);
    let c = start(&env).await;
    let layer = roads(&c, None).await;

    // A tool naming it is refused before anyone is asked.
    for (id, why) in [("pro", "trusted"), ("net", "level A only")] {
        let e = err(&call(&c, "propose_tool", proposal(&layer, Some(id))).await);
        assert!(e.contains(why) && e.contains("not proposed"), "{e}");
    }
    // An installed tool asked to run in it is refused too.
    let mut browser = None;
    if propose_and_install(&c, &env, &mut browser, &layer, Some("geo"))
        .await
        .is_none()
    {
        c.cancel().await.unwrap();
        return;
    }
    for (id, why) in [("pro", "trusted"), ("net", "level A only")] {
        let e = err(&call(
            &c,
            "run_tool",
            json!({"name": TOOL, "args": {"layer": layer}, "environment": id}),
        )
        .await);
        assert!(e.contains(NEEDS_YOU) && e.contains(why), "{e}");
    }
    // A workflow step naming it is refused when proposed.
    let step = |e: &str| {
        format!("id: trusted-step\nname: Trusted step\nscope: {{ connections: [austin] }}\nsteps:\n  - id: roads\n    tool: query_features\n    args: {{ connection: austin, dataset: roads }}\n  - id: report\n    tool: personal.layer-report.run\n    environment: {e}\n    args: {{ layer: \"${{roads}}\" }}\n")
    };
    let e = err(&call(&c, "propose_workflow", json!({"workflow": step("pro")})).await);
    assert!(e.contains("trusted") && e.contains("report"), "{e}");
    // Only on a personal tool step.
    let e = err(&call(
        &c,
        "propose_workflow",
        json!({"workflow": "id: on-core\nname: On core\nscope: { connections: [austin] }\nsteps:\n  - id: roads\n    tool: query_features\n    environment: geo\n    args: { connection: austin, dataset: roads }\n"}),
    )
    .await);
    assert!(e.contains("only to a personal tool step"), "{e}");
    c.cancel().await.unwrap();

    // One approved while the environment was governed, then re-registered
    // as trusted: the step is refused when it runs.
    let ok_text = step("geo");
    let p = wf::check(&ok_text).unwrap();
    assert_eq!(p.steps[1].environment.as_deref(), Some("geo"));
    wf::write_proposal(&env.workflows(), &ok_text, &json!({"id": "trusted-step"})).unwrap();
    let v = wf::read_proposal(&env.workflows(), &env.tools(), "trusted-step").unwrap();
    wf::install(
        &env.workflows(),
        &env.tools(),
        "trusted-step",
        &v.fingerprint,
        NOW,
    )
    .unwrap();
    envs::remove(&env.environments(), &env.root.join("envs"), "geo", "geo").unwrap();
    register(&env, "geo", &gpy, Trust::Trusted, Confinement::A);
    let out = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
        .args(["workflow", "run", "trusted-step"])
        .envs(env.vars())
        .output()
        .unwrap();
    let runs = wf::list_runs(&env.workflows(), "trusted-step", 1);
    let latest = runs
        .first()
        .unwrap_or_else(|| panic!("no run recorded: {}", String::from_utf8_lossy(&out.stderr)));
    let (rec, _) = wf::read_run(
        &env.workflows(),
        "trusted-step",
        latest["run"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(rec["status"], "needs_you", "{rec}");
    let s = &rec["steps"][1];
    assert_eq!(s["status"], "needs_you", "{s}");
    assert!(s["error"].as_str().unwrap().contains("trusted"), "{s}");
}

/// REQ-18 edge: approving a workflow whose step runs in a governed
/// environment with arcpy shows the arcpy licence line as a warning.
#[tokio::test]
async fn approving_a_workflow_in_an_arcpy_environment_warns_of_the_licence() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(&py, ENV_POLICY);
    let gpy = make_env_venv(&py, &env.root.join("geo-venv"), "geo");
    register(&env, "geo", &gpy, Trust::Governed, Confinement::A);
    let apro = env.root.join("arcpy-venv");
    let apy = make_venv(&py, &apro);
    add_arcpy_stub(&apro);
    let arc = register(&env, "arcgov", &apy, Trust::Governed, Confinement::A);
    assert!(arc.probe.arcpy, "the stub imports");
    let c = start(&env).await;
    let layer = roads(&c, None).await;
    let mut browser = None;
    if propose_and_install(&c, &env, &mut browser, &layer, Some("geo"))
        .await
        .is_none()
    {
        c.cancel().await.unwrap();
        return;
    }
    let text = "id: arc-check\nname: Arc check\nscope: { connections: [austin] }\nsteps:\n  - id: roads\n    tool: query_features\n    args: { connection: austin, dataset: roads }\n  - id: report\n    tool: personal.layer-report.run\n    environment: arcgov\n    args: { layer: \"${roads}\" }\n";
    let r = ok(&call(&c, "propose_workflow", json!({"workflow": text})).await);
    let warnings = r["result"]["warnings"].to_string();
    assert!(
        warnings.contains("ArcGIS Pro licence: Advanced") && warnings.contains("arcgov"),
        "{r}"
    );
    c.cancel().await.unwrap();
    // What the app shows the person when they approve it (its view adds
    // these, marked "Warning:", from the person's environments folder).
    let w = wf::check(text).unwrap();
    let warnings = wf::environment_warnings(&w, &env.tools(), &env.environments());
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("ArcGIS Pro licence: Advanced") && w.contains("experimental")),
        "{warnings:?}"
    );
    // A governed one without arcpy has nothing to warn of.
    let trusted = wf::check(&text.replace("arcgov", "geo")).unwrap();
    assert!(wf::environment_warnings(&trusted, &env.tools(), &env.environments()).is_empty());
    // A workflow with no environment has none.
    let plain = "id: no-env\nname: No env\nscope: { connections: [austin] }\nsteps:\n  - id: roads\n    tool: query_features\n    args: { connection: austin, dataset: roads }\n";
    let w = wf::check(plain).unwrap();
    assert!(wf::environment_warnings(&w, &env.tools(), &env.environments()).is_empty());
}

/// The card for a run_python request that waits for the person (`r`), as
/// the page shows it, and the approval's id. The page opens with the first.
async fn card_of(env: &Env, browser: &mut Option<Browser>, r: &Value) -> (String, Value) {
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    let id = r["result"]["approval"]["id"].as_str().unwrap().to_string();
    if browser.is_none() {
        *browser = Some(Browser::open(env).await);
    }
    let card = browser.as_ref().unwrap().pending(&id).await;
    (id, card)
}

fn script(code: &str, inputs: Value, environment: Option<&str>) -> Value {
    let mut a = json!({"code": code, "inputs": inputs});
    if let Some(e) = environment {
        a["environment"] = json!(e);
    }
    a
}

const CODE: &str = "import json, os\npaths = json.load(open(\"inputs.json\"))\nprint(\"size\", os.path.getsize(paths[\"roads\"]))\n";

/// Check 2 (REQ-17, R-15): under scripts.session_approval, "Allow this
/// script for the session" is offered for the engine and a governed level-A
/// environment; the same code with new inputs then runs without asking,
/// one changed character asks again, and a new session asks again.
#[tokio::test]
async fn a_script_allowed_for_the_session_runs_again_with_new_inputs() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(
        &py,
        "allow_scripts: true\nenvironments: { allow: true }\nscripts: { session_approval: true }\n",
    );
    let gpy = make_venv(&py, &env.root.join("geo-venv"));
    register(&env, "geo", &gpy, Trust::Governed, Confinement::A);
    for environment in [None, Some("geo")] {
        let c = start(&env).await;
        let mut browser = None;
        let two = roads(&c, Some(2)).await;
        let four = roads(&c, Some(4)).await;
        let first = script(CODE, json!({"roads": two}), environment);
        let r = call(&c, "run_python", first.clone()).await;
        if r.is_error == Some(true) && a1_fallback(&err(&r)) {
            c.cancel().await.unwrap();
            continue;
        }
        let (id, card) = card_of(&env, &mut browser, &ok(&r)).await;
        let b = browser.as_ref().unwrap();
        assert_eq!(card["offer_always"], true, "{card}");
        assert!(
            card["always_covers"]
                .as_str()
                .unwrap()
                .contains("Allow this script for the session"),
            "{card}"
        );
        b.decide(&id, "always_this_session").await;
        let mut with = first.clone();
        with["approval_id"] = json!(id);
        let r = ok(&call(&c, "run_python", with).await);
        assert_eq!(r["result"]["status"], "done", "{r}");
        // The same code with new inputs: no approval asked.
        let r = ok(&call(
            &c,
            "run_python",
            script(CODE, json!({"roads": four}), environment),
        )
        .await);
        assert_eq!(
            r["result"]["status"], "done",
            "the same code asked again: {r}"
        );
        // One changed character: asked again.
        let changed = CODE.replace("size", "Size");
        let r = ok(&call(
            &c,
            "run_python",
            script(&changed, json!({"roads": four}), environment),
        )
        .await);
        assert_eq!(r["result"]["status"], "approval_required", "{r}");
        // The same code in another environment (or the engine) is another
        // script: asked again.
        let elsewhere = if environment.is_some() {
            None
        } else {
            Some("geo")
        };
        let r = call(
            &c,
            "run_python",
            script(CODE, json!({"roads": four}), elsewhere),
        )
        .await;
        if !(r.is_error == Some(true) && a1_fallback(&err(&r))) {
            assert_eq!(ok(&r)["result"]["status"], "approval_required");
        }
        c.cancel().await.unwrap();
        // A new session asks again.
        let c = start(&env).await;
        let layer = roads(&c, Some(2)).await;
        let r = ok(&call(
            &c,
            "run_python",
            script(CODE, json!({"roads": layer}), environment),
        )
        .await);
        assert_eq!(r["result"]["status"], "approval_required", "{r}");
        c.cancel().await.unwrap();
    }
}

/// Check 2 (REQ-17 failure, D-3): a trusted environment never offers the
/// session approval; nor does a level-B one, nor anything with the switch
/// off. Choosing it anyway counts as once.
#[tokio::test]
async fn a_trusted_or_network_environment_never_offers_the_session_approval() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(
        &py,
        "allow_scripts: true\nenvironments: { allow: true, allow_trusted: true, allow_network: true }\nscripts: { session_approval: true }\n",
    );
    let tpy = make_venv(&py, &env.root.join("trusted-venv"));
    let npy = make_venv(&py, &env.root.join("net-venv"));
    register(&env, "pro", &tpy, Trust::Trusted, Confinement::A);
    register(&env, "net", &npy, Trust::Governed, Confinement::B);
    let can_b = StdCommand::new(env!("CARGO_BIN_EXE_columbus-confine"))
        .args(["--check", "B"])
        .output()
        .is_ok_and(|o| o.status.success());
    let c = start(&env).await;
    let mut browser = None;
    let layer = roads(&c, Some(2)).await;
    let mut cases = vec!["pro"];
    if can_b {
        cases.push("net");
    }
    for id in cases {
        let args = script(CODE, json!({"roads": layer}), Some(id));
        let r = call(&c, "run_python", args.clone()).await;
        if r.is_error == Some(true) && a1_fallback(&err(&r)) {
            continue;
        }
        let (ap, card) = card_of(&env, &mut browser, &ok(&r)).await;
        let b = browser.as_ref().unwrap();
        assert_eq!(card["offer_always"], false, "{id}: {card}");
        assert!(card.get("always_covers").is_none(), "{id}: {card}");
        assert!(
            card["summary"]
                .as_str()
                .unwrap()
                .contains("Scripts are approved one run at a time"),
            "{card}"
        );
        // Choosing "for the session" anyway counts as once.
        b.decide(&ap, "always_this_session").await;
        let mut with = args.clone();
        with["approval_id"] = json!(ap);
        let r = ok(&call(&c, "run_python", with).await);
        assert_eq!(r["result"]["status"], "done", "{r}");
        let again = ok(&call(&c, "run_python", args).await);
        assert_eq!(
            again["result"]["status"], "approval_required",
            "{id}: {again}"
        );
    }
    c.cancel().await.unwrap();

    // With the switch off, never: not even for the engine.
    env.policy("allow_scripts: true\nenvironments: { allow: true }\n");
    let c = start(&env).await;
    let mut browser = None;
    let layer = roads(&c, Some(2)).await;
    let args = script(CODE, json!({"roads": layer}), None);
    let r = ok(&call(&c, "run_python", args.clone()).await);
    let (ap, card) = card_of(&env, &mut browser, &r).await;
    let b = browser.as_ref().unwrap();
    assert_eq!(card["offer_always"], false, "{card}");
    b.decide(&ap, "always_this_session").await;
    let mut with = args.clone();
    with["approval_id"] = json!(ap);
    assert_eq!(
        ok(&call(&c, "run_python", with).await)["result"]["status"],
        "done"
    );
    assert_eq!(
        ok(&call(&c, "run_python", args).await)["result"]["status"],
        "approval_required"
    );
    c.cancel().await.unwrap();
}

/// The audit lines of the session's script runs, oldest first.
fn script_lines(env: &Env) -> Vec<Value> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|f| {
            std::fs::read_to_string(f)
                .unwrap()
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .collect::<Vec<_>>()
        })
        .filter(|l| l["tool"] == "run_python")
        .collect()
}

/// One boundary in place of approving each script (T-27; build brief 3,
/// D-16, F-15, REQ-36). Under `scripts.approval: none` a script in the
/// engine's Python or a governed level-A environment runs with no approval,
/// and its audit line names the rule that allowed it. A level-B (network)
/// run and a trusted run still ask every time. Under `each` everything
/// asks, as before. whoami, search_tools and describe_tool say what is in
/// force.
#[tokio::test]
async fn under_scripts_approval_none_only_a_run_that_leaves_the_sandbox_asks() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(
        &py,
        "allow_scripts: true\nallow_personal_tools: true\nenvironments: { allow: true, allow_trusted: true, allow_network: true }\nscripts: { approval: none }\n",
    );
    let gpy = make_venv(&py, &env.root.join("geo-venv"));
    let tpy = make_venv(&py, &env.root.join("trusted-venv"));
    let npy = make_venv(&py, &env.root.join("net-venv"));
    register(&env, "geo", &gpy, Trust::Governed, Confinement::A);
    register(&env, "pro", &tpy, Trust::Trusted, Confinement::A);
    register(&env, "net", &npy, Trust::Governed, Confinement::B);
    let can_b = StdCommand::new(env!("CARGO_BIN_EXE_columbus-confine"))
        .args(["--check", "B"])
        .output()
        .is_ok_and(|o| o.status.success());
    let c = start(&env).await;

    // What the assistant is told is what is in force.
    let w = ok(&call(&c, "whoami", json!({})).await);
    let p = &w["result"]["policy"];
    assert_eq!(p["script_approval"], "none", "{p}");
    assert!(
        p["script_approval_means"]
            .as_str()
            .unwrap()
            .contains("without asking"),
        "{p}"
    );
    let found = ok(&call(&c, "search_tools", json!({"query": "run a python script"})).await);
    let card = found["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "run_python")
        .unwrap_or_else(|| panic!("{found}"));
    assert_eq!(card["approval"], "if_outside_sandbox", "{card}");
    let d = ok(&call(&c, "describe_tool", json!({"name": "run_python"})).await);
    let text = d["result"]["description"].as_str().unwrap();
    assert!(
        text.contains("In force now: A script that runs in the sandbox with no network")
            && text.contains("scripts.approval: none")
            && !text.contains("approves every run (scripts.approval: each)"),
        "{text}"
    );

    let two = roads(&c, Some(2)).await;
    let four = roads(&c, Some(4)).await;
    // Inside the boundary: no approval, whatever the code and its inputs.
    let mut free = 0;
    for environment in [None, Some("geo")] {
        let r = call(
            &c,
            "run_python",
            script(CODE, json!({"roads": two}), environment),
        )
        .await;
        if r.is_error == Some(true) && a1_fallback(&err(&r)) {
            continue;
        }
        let r = ok(&r);
        assert_eq!(r["result"]["status"], "done", "{environment:?}: {r}");
        assert!(
            r["result"]["stdout"].as_str().unwrap().contains("size "),
            "{r}"
        );
        // Provenance says how it came to run.
        let prov = r["provenance"].as_str().unwrap();
        let ws = PathBuf::from(
            ok(&call(&c, "whoami", json!({})).await)["result"]["workspace"]
                .as_str()
                .unwrap(),
        );
        let rec: Value = serde_json::from_str(
            &std::fs::read_to_string(ws.join("provenance").join(format!("{prov}.prov.json")))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(rec["steps"][0]["approval"], "scripts.approval: none");
        assert_eq!(rec["steps"][0]["confinement"]["level"], "A");
        assert_eq!(rec["steps"][0]["confinement"]["network"], "none");
        let changed = CODE.replace("\"size\"", "\"bytes\"");
        assert_ne!(changed, CODE);
        let r = ok(&call(
            &c,
            "run_python",
            script(&changed, json!({"roads": four}), environment),
        )
        .await);
        assert_eq!(r["result"]["status"], "done", "{environment:?}: {r}");
        free += 2;
    }

    // Across it: a trusted run and a network run ask every time.
    let mut browser = None;
    let mut asked = 0;
    let mut cases = vec![("pro", "outside the sandbox")];
    if can_b {
        cases.push(("net", "can reach the internet"));
    }
    for (id, why) in cases {
        let args = script(CODE, json!({"roads": two}), Some(id));
        let r = call(&c, "run_python", args.clone()).await;
        if r.is_error == Some(true) && a1_fallback(&err(&r)) {
            continue;
        }
        let (ap, card) = card_of(&env, &mut browser, &ok(&r)).await;
        let b = browser.as_ref().unwrap();
        assert_eq!(card["offer_always"], false, "{id}: {card}");
        let summary = card["summary"].as_str().unwrap();
        assert!(
            summary.contains(why) && summary.contains("is approved every run"),
            "{id}: {summary}"
        );
        // Choosing "for the session" anyway counts as once.
        b.decide(&ap, "always_this_session").await;
        let mut with = args.clone();
        with["approval_id"] = json!(ap);
        let r = ok(&call(&c, "run_python", with).await);
        assert_eq!(r["result"]["status"], "done", "{id}: {r}");
        let again = ok(&call(&c, "run_python", args).await);
        assert_eq!(
            again["result"]["status"], "approval_required",
            "{id}: {again}"
        );
        asked += 1;
    }
    // A proposed tool's test run is asked about too, whatever the rule for
    // scripts: a new tool is something being installed.
    let r = ok(&call(&c, "propose_tool", proposal(&two, Some("geo"))).await);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    c.cancel().await.unwrap();

    // The audit log: the rule, for every run that was not asked about; an
    // approval's id for every run that was.
    let lines = script_lines(&env);
    let by_rule: Vec<&Value> = lines
        .iter()
        .filter(|l| l["approval"] == "scripts.approval: none")
        .collect();
    // Each run writes two lines: when it starts and when it ends.
    assert_eq!(by_rule.len(), free * 2, "{lines:?}");
    for l in &by_rule {
        assert!(
            l["environment"].is_null() || l["environment"] == "geo",
            "{l}"
        );
        assert!(
            l["decision"] == "running" || l["decision"] == "allowed",
            "{l}"
        );
    }
    let waited: Vec<&Value> = lines
        .iter()
        .filter(|l| l["decision"] == "approval_required")
        .collect();
    assert_eq!(waited.len(), asked * 2, "{lines:?}");
    for l in &waited {
        assert!(
            l["environment"] == "pro" || l["environment"] == "net",
            "{l}"
        );
    }
    for l in lines
        .iter()
        .filter(|l| l["environment"] == "pro" || l["environment"] == "net")
    {
        assert_ne!(l["approval"], "scripts.approval: none", "{l}");
    }

    // The same scripts under `each`: every run asks, as before.
    env.policy("allow_scripts: true\nenvironments: { allow: true }\nscripts: { approval: each }\n");
    let c = start(&env).await;
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["script_approval"], "each");
    let found = ok(&call(&c, "search_tools", json!({"query": "run a python script"})).await);
    let card = found["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "run_python")
        .unwrap();
    assert_eq!(card["approval"], "yes");
    let d = ok(&call(&c, "describe_tool", json!({"name": "run_python"})).await);
    assert!(
        d["result"]["description"]
            .as_str()
            .unwrap()
            .contains("approves every run (scripts.approval: each)"),
        "{d}"
    );
    let layer = roads(&c, Some(2)).await;
    let mut browser = None;
    for environment in [None, Some("geo")] {
        let args = script(CODE, json!({"roads": layer}), environment);
        let r = call(&c, "run_python", args.clone()).await;
        if r.is_error == Some(true) && a1_fallback(&err(&r)) {
            continue;
        }
        let (ap, card) = card_of(&env, &mut browser, &ok(&r)).await;
        let b = browser.as_ref().unwrap();
        assert_eq!(card["offer_always"], false, "{card}");
        assert!(
            card["summary"]
                .as_str()
                .unwrap()
                .contains("Scripts are approved one run at a time"),
            "{card}"
        );
        b.decide(&ap, "allow_once").await;
        let mut with = args.clone();
        with["approval_id"] = json!(ap);
        assert_eq!(
            ok(&call(&c, "run_python", with).await)["result"]["status"],
            "done"
        );
        assert_eq!(
            ok(&call(&c, "run_python", args).await)["result"]["status"],
            "approval_required"
        );
    }
    c.cancel().await.unwrap();
}
