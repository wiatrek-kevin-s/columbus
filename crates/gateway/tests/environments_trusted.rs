//! Trusted environments (T-8; build brief Part 5, D-3, REQ-15) and arcpy
//! detection and licence reporting (REQ-16, R-13, D-13), through the real
//! `columbus-mcp` binary.
//!
//! The person's typed confirmation is the library call the terminal makes
//! once they typed the id (`columbus_shared::environments::register`,
//! `confirm`), as environments.rs does. arcpy is a stub package
//! (tests/fixtures/arcpy_stub, test-only) copied into a throwaway venv's
//! site-packages; what its `ProductInfo()` returns, or that importing it
//! hangs, is chosen by a file beside it. No real ArcGIS is used.
//!
//! The trusted-run test needs Python with geopandas, pyogrio, shapely and
//! pyproj (the fixture, query_features and export_output); skipped
//! otherwise. The arcpy test needs only a Python with venv.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use columbus_shared::environments::{self as envs, Confinement, CreatedBy, Registration, Trust};
use columbus_shared::policy::EnvironmentsPolicy;
use reqwest::StatusCode;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const NOW: &str = "2026-11-08T10:00:00-06:00";
/// The approval card's sentence for every trusted run (REQ-15), as the
/// brief words it.
const CARD: &str = "Columbus cannot enforce your connection rules for this run: the script can read, write and send data anywhere you can.";
/// The start of the not-initialized licence sentence (REQ-16).
const NOT_INIT: &str = "not initialized: open ArcGIS Pro and sign in";

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

fn engine() -> Option<String> {
    let py = python("import geopandas, pyogrio, shapely, pyproj");
    if py.is_none() {
        eprintln!("skipping: the GIS engine is not installed");
    }
    py
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
    let lib = venv.join("lib");
    for e in std::fs::read_dir(&lib).unwrap().flatten() {
        let p = e.path().join("site-packages");
        if p.is_dir() {
            return p;
        }
    }
    panic!("no site-packages in {}", lib.display());
}

/// Put the test-only stub arcpy into the venv; returns the file that says
/// what it does.
fn install_stub_arcpy(venv: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("arcpy_stub")
        .join("arcpy")
        .join("__init__.py");
    let dir = site_packages(venv).join("arcpy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&src, dir.join("__init__.py")).unwrap();
    dir.join("stub_product.txt")
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
    fn environments(&self) -> PathBuf {
        self.root.join("environments")
    }
    fn envs(&self) -> PathBuf {
        self.root.join("envs")
    }
    fn share(&self) -> PathBuf {
        self.root.join("share")
    }
    fn policy(&self, text: &str) {
        let share = serde_json::to_string(&self.share().display().to_string()).unwrap();
        std::fs::write(
            self.root.join("policy.yaml"),
            format!(
                "connection_types: [gpkg]\nlimits: {{ rows: 1000, seconds: 60 }}\n\
                 exports:\n  - {{ id: share, path: {share}, granted: true }}\n{text}"
            ),
        )
        .unwrap();
    }
    fn yaml(&self, id: &str) -> PathBuf {
        self.environments().join(format!("{id}.yaml"))
    }
}

fn setup(py: Option<&str>, policy: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    // A real path: macOS's temp folder is behind a link. On Windows,
    // without the `\\?\` prefix canonicalize adds (a person's path never
    // has it, and Python reports sys.prefix without it).
    let root = plain_path(std::fs::canonicalize(dir.path()).unwrap());
    if let Some(py) = py {
        make_fixture(py, &root);
    }
    for d in ["connections", "envs", "share"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let env = Env { _dir: dir, root };
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

fn plain_path(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

fn trusted_allowed() -> EnvironmentsPolicy {
    EnvironmentsPolicy {
        allow: true,
        allow_trusted: true,
        ..Default::default()
    }
}

/// What the person does at the keyboard: probe, look, type the id.
fn register(
    env: &Env,
    id: &str,
    python: &Path,
    trust: Trust,
    policy: &EnvironmentsPolicy,
) -> Result<envs::Environment, String> {
    let found = envs::probe(python, Duration::from_secs(120))?;
    let reg = Registration {
        id: id.into(),
        python: python.to_path_buf(),
        trust,
        confinement: Confinement::A,
        created_by: CreatedBy::Person,
        description: None,
    };
    envs::register(
        &env.environments(),
        &env.envs(),
        policy,
        &reg,
        &found,
        id,
        NOW,
    )
}

fn cli(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(args)
        .env("COLUMBUS_POLICY", env.root.join("policy.yaml"))
        .env("COLUMBUS_CONNECTIONS", env.root.join("connections"))
        .env("COLUMBUS_ENVIRONMENTS", env.environments())
        .env("COLUMBUS_ENVS", env.envs())
        .env("COLUMBUS_WORKSPACES", env.root.join("workspaces"))
        .env("COLUMBUS_AUDIT_DIR", env.root.join("audit"))
        .stdin(std::process::Stdio::null());
    // Nobody is at the keyboard. The id is read from the terminal itself,
    // not stdin: on Unix a test has no terminal, so the prompt fails at
    // once; on Windows the test's console would wait for typing forever,
    // so the command gets no console (DETACHED_PROCESS).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
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
                .env("COLUMBUS_ENVIRONMENTS", root.join("environments"))
                .env("COLUMBUS_ENVS", root.join("envs"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_APPROVALS_LAUNCH_FILE", &launch)
                .env("COLUMBUS_LOG", "warn");
        }),
    )
    .unwrap();
    ().serve(transport).await.unwrap()
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

async fn environment(c: &RunningService<RoleClient, ()>, id: &str) -> Value {
    let w = ok(&call(c, "whoami", json!({})).await);
    w["result"]["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} not listed: {}", w["result"]["environments"]))
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

fn audit_text(env: &Env) -> String {
    let path = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&path).unwrap();
    std::fs::read_to_string(&path).unwrap()
}

/// Run a test's body on a thread of its own and fail, saying so, if it
/// has not finished within `limit`: a hang (a process that was not
/// ended, a pipe that never closes) becomes a failure with a message
/// instead of a test that never ends. The body's `step:` lines show how
/// far it got.
fn within<F, Fut>(limit: Duration, name: &'static str, body: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rt.block_on(body())));
            let _ = tx.send(r);
        })
        .unwrap();
    match rx.recv_timeout(limit) {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!(
            "{name} did not finish within {} s: something it started was not ended (see the last step: line above)",
            limit.as_secs()
        ),
    }
}

fn at_step(what: &str) {
    eprintln!("step: {what}");
}

/// Check 1 (REQ-15, D-3): without environments.allow_trusted a trusted
/// environment is refused. With it, the script runs unconfined as the
/// person (it reads a file outside the workspace); the card carries the
/// full "cannot enforce" sentence and offers no session allow (choosing
/// "for this session" counts once; the same call asks again); outputs
/// carry the session's labels plus environment:pro; and exporting that
/// output to a folder under a flow rule asks for approval, while the same
/// session's other output goes without asking.
#[test]
fn trusted_runs_need_the_switch_are_approved_each_time_and_labelled() {
    within(
        Duration::from_secs(600),
        "trusted_runs_need_the_switch_are_approved_each_time_and_labelled",
        trusted_runs,
    );
}

async fn trusted_runs() {
    let Some(py) = engine() else { return };
    let base = "allow_scripts: true\napprovals: { page: true, wait_seconds: 0 }\n\
                flows:\n  - { from: [\"environment:*\"], to: [\"folder:*\"], action: approve }\n";
    let env = setup(
        Some(&py),
        &format!("{base}environments: {{ allow: true }}\n"),
    );
    let venv = env.root.join("pro-venv");
    let vpy = make_venv(&py, &venv);
    // Registering trusted needs the switch, at the keyboard too.
    let off = EnvironmentsPolicy {
        allow: true,
        ..Default::default()
    };
    let e = register(&env, "pro", &vpy, Trust::Trusted, &off).unwrap_err();
    assert!(e.contains("allow_trusted"), "{e}");
    assert!(!env.yaml("pro").exists());
    // Registered while an earlier policy allowed it; the switch is now off.
    at_step("registered trusted pro");
    let reg = register(&env, "pro", &vpy, Trust::Trusted, &trusted_allowed()).unwrap();
    assert_eq!(reg.trust, Trust::Trusted);

    at_step("gateway without the switch");
    // Without the switch: refused, with the reason.
    let c = start(&env, &py).await;
    let card = environment(&c, "pro").await;
    assert_eq!(card["trust"], "trusted");
    assert_eq!(card["confinement"], "none");
    assert!(
        card["trust_line"]
            .as_str()
            .unwrap()
            .starts_with("trusted (experimental)"),
        "{card}"
    );
    assert_eq!(card["experimental"], true);
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "pro"}),
    )
    .await);
    assert!(
        e.contains("allow_trusted") && e.contains("does not allow trusted environments"),
        "{e}"
    );
    c.cancel().await.unwrap();

    at_step("gateway with the switch");
    // With the switch.
    env.policy(&format!(
        "{base}environments: {{ allow: true, allow_trusted: true }}\n"
    ));
    let c = start(&env, &py).await;
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["trusted_environments_allowed"], true);
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "fields": ["route_id", "lanes"], "order_by": "route_id"}),
    )
    .await);
    let roads = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    // A file of the person's outside the workspace: only an unconfined
    // script can read it.
    let outside = env.root.join("outside.txt");
    std::fs::write(&outside, "read as the person").unwrap();
    let code = format!(
        r#"import json, os, shutil, sqlite3, sys
paths = json.load(open("inputs.json"))
shutil.copyfile(paths["roads"], os.path.join("out", "x.gpkg"))
db = sqlite3.connect(os.path.join("out", "x.gpkg"))
table = db.execute("SELECT table_name FROM gpkg_contents").fetchone()[0]
db.execute(f'DELETE FROM "{{table}}" WHERE lanes = 2')
db.commit()
db.close()
print("prefix", os.path.normcase(os.path.realpath(sys.prefix)) == os.path.normcase(os.path.realpath({venv:?})), sys.prefix)
print("outside:", open({outside:?}).read())
"#,
        venv = venv.display().to_string(),
        outside = outside.display().to_string(),
    );
    at_step("first trusted run_python");
    let args = json!({"code": code, "inputs": {"roads": roads}, "environment": "pro"});
    let first = ok(&call(&c, "run_python", args.clone()).await);
    assert_eq!(first["result"]["status"], "approval_required", "{first}");
    let summary = first["result"]["approval"]["summary"].as_str().unwrap();
    assert!(summary.contains(CARD), "{summary}");
    let id = first["result"]["approval"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let browser = Browser::open(&env).await;
    let shown = browser.pending(&id).await;
    let shown_summary = shown["summary"].as_str().unwrap();
    assert!(shown_summary.contains(CARD), "{shown}");
    assert!(
        shown_summary.contains("trusted environment pro (experimental)"),
        "{shown_summary}"
    );
    assert_eq!(shown["offer_always"], false, "{shown}");
    assert!(shown.get("always_covers").is_none(), "{shown}");
    assert_eq!(shown["detail"]["warning"], CARD, "{shown}");
    at_step("approved; running");
    // "For this session" is not on offer: it counts as once.
    browser.decide(&id, "always_this_session").await;
    let mut with = args.clone();
    with["approval_id"] = json!(id);
    let r = ok(&call(&c, "run_python", with.clone()).await);
    assert_eq!(r["result"]["status"], "done", "{r}");
    let stdout = r["result"]["stdout"].as_str().unwrap();
    assert!(stdout.contains("prefix True"), "{stdout}");
    assert!(stdout.contains("outside: read as the person"), "{stdout}");
    // The approval was spent; the same run asks again.
    let again = ok(&call(&c, "run_python", args.clone()).await);
    assert_eq!(again["result"]["status"], "approval_required", "{again}");
    assert!(call(&c, "run_python", with).await.is_error == Some(true));

    at_step("checking labels");
    // Outputs carry the session's labels plus environment:pro.
    let out = &r["result"]["outputs"][0];
    let handle = out["handle"].as_str().unwrap().to_string();
    assert_eq!(out["summary"]["rows"], 4, "{r}");
    let labels_check = r["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "flow.labels")
        .unwrap_or_else(|| panic!("no flow.labels check: {r}"));
    let msg = labels_check["message"].as_str().unwrap();
    assert!(
        msg.contains("environment:pro") && msg.contains("connection:austin"),
        "{msg}"
    );
    let ws = PathBuf::from(w["result"]["workspace"].as_str().unwrap());
    let prov = r["provenance"].as_str().unwrap();
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join("provenance").join(format!("{prov}.prov.json"))).unwrap(),
    )
    .unwrap();
    let labels: Vec<&str> = rec["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(
        labels.contains(&"environment:pro") && labels.contains(&"connection:austin"),
        "{labels:?}"
    );
    let step = &rec["steps"][0];
    assert_eq!(step["environment"]["id"], "pro");
    assert_eq!(step["environment"]["trust"], "trusted");
    assert_eq!(step["confinement"]["level"], "none");
    assert_eq!(step["confinement"]["unconfined"], true);

    at_step("exports");
    // REQ-15 failure case: the flow rule on environment:* asks before the
    // trusted output leaves for the folder; the session's own query
    // result, without that label, goes without asking.
    let plain = ok(&call(
        &c,
        "export_output",
        json!({"handle": roads, "folder": "share", "name": "roads_plain"}),
    )
    .await);
    assert_eq!(plain["result"]["status"], "exported", "{plain}");
    let x = ok(&call(
        &c,
        "export_output",
        json!({"handle": handle, "folder": "share", "name": "roads_trusted"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "approval_required", "{x}");
    let xs = x["result"]["approval"]["summary"].as_str().unwrap();
    assert!(xs.contains("flow rule asks for approval"), "{xs}");
    assert!(xs.contains("environment:pro"), "{xs}");
    assert!(!env.share().join("roads_trusted.gpkg").exists());
    let xid = x["result"]["approval"]["id"].as_str().unwrap().to_string();
    browser.decide(&xid, "allow_once").await;
    let x = ok(&call(
        &c,
        "export_output",
        json!({"handle": handle, "folder": "share", "name": "roads_trusted", "approval_id": xid}),
    )
    .await);
    assert_eq!(x["result"]["status"], "exported", "{x}");
    assert!(env.share().join("roads_trusted.gpkg").is_file());
    c.cancel().await.unwrap();

    let text = audit_text(&env);
    assert!(text.contains("\"environment\":\"pro\""), "{text}");
    assert!(text.contains("environment:pro"), "{text}");
}

/// Check 2 (REQ-16, R-13, D-13): a stub arcpy whose ProductInfo() says
/// NotInitialized shows the not-initialized sentence in whoami; confirmed
/// again with ArcInfo it shows "Advanced"; a probe that hangs (200 s) is
/// stopped at the limit with everything it started, and reported; nothing
/// is recorded. whoami marks arcpy and trusted environments experimental.
/// The real limit is 120 s; the test shortens it (test builds only).
#[test]
fn arcpy_licence_is_reported_in_plain_words_and_a_hung_probe_is_stopped() {
    within(
        Duration::from_secs(600),
        "arcpy_licence_is_reported_in_plain_words_and_a_hung_probe_is_stopped",
        arcpy_licence,
    );
}

async fn arcpy_licence() {
    let Some(py) = python("import venv") else {
        eprintln!("skipping: no Python");
        return;
    };
    // The default limit is R-13's 120 s; this process never shortens it.
    assert_eq!(envs::PROBE_SECONDS, 120);
    assert!(std::env::var_os("COLUMBUS_TEST_PROBE_SECONDS").is_none());
    assert_eq!(envs::probe_limit(), Duration::from_secs(120));

    let env = setup(None, "environments: { allow: true, allow_trusted: true }\n");
    let venv = env.root.join("pro-clone");
    let vpy = make_venv(&py, &venv);
    let product = install_stub_arcpy(&venv);

    at_step("probe NotInitialized");
    // NotInitialized: arcpy imports, no licence is set.
    std::fs::write(&product, "NotInitialized\n").unwrap();
    let reg = register(&env, "pro", &vpy, Trust::Trusted, &trusted_allowed()).unwrap();
    assert!(reg.probe.arcpy);
    assert_eq!(reg.probe.licence.as_deref(), Some("NotInitialized"));
    let text = std::fs::read_to_string(env.yaml("pro")).unwrap();
    assert!(
        text.contains("arcpy: true") && text.contains("licence: NotInitialized"),
        "{text}"
    );
    // A governed one beside it, without arcpy: not experimental.
    let plain = make_venv(&py, &env.root.join("plain"));
    register(&env, "plain", &plain, Trust::Governed, &trusted_allowed()).unwrap();

    let c = start(&env, &py).await;
    let card = environment(&c, "pro").await;
    assert_eq!(card["status"], "ready", "{card}");
    let licence = card["licence"].as_str().unwrap();
    assert_eq!(
        licence,
        "ArcGIS Pro licence: not initialized: open ArcGIS Pro and sign in; Named User licences lapse after about 14 days without Pro being opened"
    );
    assert!(licence.contains(NOT_INIT));
    let arcpy = card["arcpy"].as_str().unwrap();
    assert!(arcpy.starts_with("arcpy: yes (experimental)"), "{arcpy}");
    assert!(arcpy.contains(NOT_INIT), "{arcpy}");
    assert!(
        card["trust_line"]
            .as_str()
            .unwrap()
            .starts_with("trusted (experimental)"),
        "{card}"
    );
    assert_eq!(card["experimental"], true);
    let other = environment(&c, "plain").await;
    assert_eq!(other["arcpy"], "arcpy: no");
    assert!(other["licence"].is_null(), "{other}");
    assert_eq!(other["experimental"], false);
    assert!(!other["trust_line"]
        .as_str()
        .unwrap()
        .contains("experimental"));

    at_step("probe ArcInfo");
    // Pro opened and signed in: confirmed again with a fresh probe.
    std::fs::write(&product, "ArcInfo\n").unwrap();
    let found = envs::probe(&vpy, Duration::from_secs(120)).unwrap();
    assert_eq!(found.licence.as_deref(), Some("ArcInfo"));
    envs::confirm(
        &env.environments(),
        &env.envs(),
        &trusted_allowed(),
        "pro",
        &found,
        "pro",
        NOW,
    )
    .unwrap();
    let card = environment(&c, "pro").await;
    assert_eq!(card["status"], "ready", "{card}");
    assert_eq!(card["licence"], "ArcGIS Pro licence: Advanced");
    assert_eq!(
        card["arcpy"],
        "arcpy: yes (experimental); ArcGIS Pro licence: Advanced"
    );

    at_step("hung probe");
    // A probe that hangs for 200 s, with a child process of its own.
    let beat = env.root.join("heartbeat.txt");
    std::fs::write(&product, format!("sleep:200\n{}\n", beat.display())).unwrap();
    let limit = Duration::from_secs(5);
    let started = Instant::now();
    let e = envs::probe(&vpy, limit).unwrap_err();
    let took = started.elapsed();
    assert!(
        e.contains("ran longer than 5 s and was stopped, with everything it started"),
        "{e}"
    );
    assert!(e.contains("nothing was recorded"), "{e}");
    assert!(took >= limit && took < Duration::from_secs(60), "{took:?}");
    // Its child is gone too: the heartbeat stops.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !beat.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(beat.exists(), "the hung probe's child never started");
    std::thread::sleep(Duration::from_millis(1000));
    let before = std::fs::metadata(&beat).unwrap().len();
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        std::fs::metadata(&beat).unwrap().len(),
        before,
        "the probe's child kept running"
    );
    at_step("after the hung probe");
    // Confirmation with a hung probe changes nothing.
    let card = environment(&c, "pro").await;
    assert_eq!(card["licence"], "ArcGIS Pro licence: Advanced");

    at_step("terminal, hung probe");
    // At the keyboard: the terminal says so plainly and registers nothing
    // (test builds may shorten the 120 s limit; release builds never do).
    let out = cli(
        &env,
        &["env", "add", "hung", "--python", &vpy.display().to_string()],
        &[("COLUMBUS_TEST_PROBE_SECONDS", "5")],
    );
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("(at most 5 s)"), "{stdout}");
    assert!(
        stderr.contains("ran longer than 5 s and was stopped"),
        "{stderr}"
    );
    assert!(!env.yaml("hung").exists());
    at_step("terminal, quick probe");
    // Without the override the terminal uses the full 120 s; it is never
    // lengthened. A quick probe shows the limit and the licence words.
    std::fs::write(&product, "ArcView\n").unwrap();
    let out = cli(
        &env,
        &[
            "env",
            "add",
            "basic",
            "--python",
            &vpy.display().to_string(),
            "--trust",
            "trusted",
        ],
        &[("COLUMBUS_TEST_PROBE_SECONDS", "999")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("(at most 120 s)"), "{stdout}");
    assert!(stdout.contains("arcpy: yes (experimental)"), "{stdout}");
    assert!(stdout.contains("ArcGIS Pro licence: Basic"), "{stdout}");
    assert!(stdout.contains(CARD), "{stdout}");
    // Nobody typed the id, so nothing was registered.
    assert!(!out.status.success());
    assert!(!env.yaml("basic").exists());
    c.cancel().await.unwrap();
}
