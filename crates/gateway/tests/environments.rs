//! Registered Python environments (T-6; build brief Part 5, F-4, F-7,
//! R-13, R-13b, REQ-12, REQ-13): registration at the keyboard only, the
//! probe, `changed` after any edit or package change, governed runs
//! confined with extracts only, policy `environments.allow`, and removal
//! that never deletes a folder the person registered.
//!
//! The person's typed confirmation is the library call the terminal makes
//! once they typed the id (`columbus_shared::environments::register`,
//! `confirm`, `remove` take what was typed), as tests.rs installs tools
//! with `columbus_shared::tools::install`. Each test makes plain venvs
//! (`python -m venv --without-pip`, no network) from the engine's Python.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj for the fixture
//! and query_features; skipped otherwise.

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

/// The venv's site-packages folder.
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

/// A package "installed" without pip or the network.
fn fake_install(venv: &Path, name: &str, version: &str) -> PathBuf {
    let d = site_packages(venv).join(format!("{name}-{version}.dist-info"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("METADATA"),
        format!("Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n"),
    )
    .unwrap();
    d
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
    fn policy(&self, text: &str) {
        std::fs::write(
            self.root.join("policy.yaml"),
            format!("connection_types: [gpkg]\nlimits: {{ rows: 1000, seconds: 60 }}\n{text}"),
        )
        .unwrap();
    }
    fn yaml(&self, id: &str) -> PathBuf {
        self.environments().join(format!("{id}.yaml"))
    }
    /// Edit a registration's file by hand.
    fn edit(&self, id: &str, from: &str, to: &str) {
        let text = std::fs::read_to_string(self.yaml(id)).unwrap();
        assert!(text.contains(from), "{from} not in {text}");
        std::fs::write(self.yaml(id), text.replace(from, to)).unwrap();
    }
}

fn setup(py: Option<&str>, policy: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    // A real path: macOS's temp folder is behind a link.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    if let Some(py) = py {
        make_fixture(py, &root);
    }
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::create_dir_all(root.join("envs")).unwrap();
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

fn allowed() -> EnvironmentsPolicy {
    EnvironmentsPolicy {
        allow: true,
        ..Default::default()
    }
}

/// What the person does at the keyboard: probe, look, type the id.
fn register(env: &Env, id: &str, python: &Path, created_by: CreatedBy) -> envs::Environment {
    let found = envs::probe(python, Duration::from_secs(120)).unwrap();
    let reg = Registration {
        id: id.into(),
        python: python.to_path_buf(),
        trust: Trust::Governed,
        confinement: Confinement::A,
        created_by,
        description: Some("test venv".into()),
    };
    envs::register(
        &env.environments(),
        &env.envs(),
        &allowed(),
        &reg,
        &found,
        id,
        NOW,
    )
    .unwrap()
}

fn confirm_again(env: &Env, id: &str, policy: &EnvironmentsPolicy) -> Result<(), String> {
    let e = envs::pending(&env.environments(), id)?;
    let found = envs::probe(Path::new(&e.python), Duration::from_secs(120))?;
    envs::confirm(
        &env.environments(),
        &env.envs(),
        policy,
        id,
        &found,
        id,
        NOW,
    )
    .map(|_| ())
}

fn cli(env: &Env, args: &[&str]) -> std::process::Output {
    StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
        .args(args)
        .env("COLUMBUS_POLICY", env.root.join("policy.yaml"))
        .env("COLUMBUS_CONNECTIONS", env.root.join("connections"))
        .env("COLUMBUS_ENVIRONMENTS", env.environments())
        .env("COLUMBUS_ENVS", env.envs())
        .env("COLUMBUS_WORKSPACES", env.root.join("workspaces"))
        .env("COLUMBUS_AUDIT_DIR", env.root.join("audit"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
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

    async fn decide(&self, id: &str) {
        let r = self
            .http
            .post(format!("{}/api/approvals/{id}", self.base))
            .bearer_auth(&self.token)
            .header("origin", &self.base)
            .header("sec-fetch-site", "same-origin")
            .json(&json!({"decision": "allow_once"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
}

/// Ask, let the person allow it on the page, and run it with the approval.
/// Returns the result and the card the person saw.
async fn approved_run(
    c: &RunningService<RoleClient, ()>,
    env: &Env,
    browser: &mut Option<Browser>,
    args: Value,
) -> (Value, Value) {
    let first = ok(&call(c, "run_python", args.clone()).await);
    assert_eq!(first["result"]["status"], "approval_required", "{first}");
    let id = first["result"]["approval"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The page's link is good once: one browser per session.
    if browser.is_none() {
        *browser = Some(Browser::open(env).await);
    }
    let b = browser.as_ref().unwrap();
    let card = b.pending(&id).await;
    b.decide(&id).await;
    let mut with = args;
    with["approval_id"] = json!(id);
    (ok(&call(c, "run_python", with).await), card)
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

/// Check 1 (REQ-12, REQ-13, R-13): env add records the version and the
/// packages hash; a script in the environment reads its input and returns
/// out/x.gpkg as a checked layer; a socket call fails; whoami lists it.
#[tokio::test]
async fn a_governed_environment_runs_confined_with_extracts_only() {
    let Some(py) = engine() else { return };
    let env = setup(
        Some(&py),
        "allow_scripts: true\nenvironments: { allow: true }\napprovals: { page: true, wait_seconds: 0 }\n",
    );
    let venv = env.root.join("person-venv");
    let vpy = make_venv(&py, &venv);

    // Edge: a path that isn't a Python interpreter is refused, before
    // anything asks the person.
    let not_py = cli(
        &env,
        &[
            "env",
            "add",
            "pro",
            "--python",
            &env.root.join("policy.yaml").display().to_string(),
        ],
    );
    assert!(!not_py.status.success());
    let text = String::from_utf8_lossy(&not_py.stderr);
    assert!(text.contains("is not a Python interpreter"), "{text}");
    let fake = env.root.join("fake").join(if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    });
    std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
    std::fs::write(&fake, "#!/bin/sh\necho not python\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let e = envs::probe(&fake, Duration::from_secs(120)).unwrap_err();
    assert!(e.contains("is not a Python interpreter"), "{e}");
    let out = cli(
        &env,
        &["env", "add", "pro", "--python", &fake.display().to_string()],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not a Python interpreter"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!env.yaml("pro").exists());

    // Registration: the probe the person saw, then the id typed.
    let found = envs::probe(&vpy, Duration::from_secs(120)).unwrap();
    let reg = Registration {
        id: "pro".into(),
        python: vpy.clone(),
        trust: Trust::Governed,
        confinement: Confinement::A,
        created_by: CreatedBy::Person,
        description: None,
    };
    let typo = envs::register(
        &env.environments(),
        &env.envs(),
        &allowed(),
        &reg,
        &found,
        "por",
        NOW,
    );
    assert!(typo.unwrap_err().contains("not confirmed"));
    assert!(!env.yaml("pro").exists());
    let registered = envs::register(
        &env.environments(),
        &env.envs(),
        &allowed(),
        &reg,
        &found,
        "pro",
        NOW,
    )
    .unwrap();
    let text = std::fs::read_to_string(env.yaml("pro")).unwrap();
    assert!(
        text.contains(&format!("python_version: {}", found.python_version))
            || text.contains(&format!("python_version: '{}'", found.python_version)),
        "{text}"
    );
    assert!(text.contains(&found.packages_sha256()), "{text}");
    assert!(
        text.contains("arcpy: false") && text.contains("trust: governed"),
        "{text}"
    );
    assert_eq!(registered.probe.packages_sha256, found.packages_sha256());
    // Trusted only while policy allows it (T-8 completes trusted runs).
    let mut trusted = reg.clone();
    trusted.id = "pro-trusted".into();
    trusted.trust = Trust::Trusted;
    let e = envs::register(
        &env.environments(),
        &env.envs(),
        &allowed(),
        &trusted,
        &found,
        "pro-trusted",
        NOW,
    )
    .unwrap_err();
    assert!(e.contains("allow_trusted"), "{e}");
    let out = cli(
        &env,
        &[
            "env",
            "add",
            "pro-trusted",
            "--python",
            &vpy.display().to_string(),
            "--trust",
            "trusted",
        ],
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("allow_trusted"));
    let listed = cli(&env, &["env", "list"]);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("ready"),
        "{listed:?}"
    );

    let c = start(&env, &py).await;
    let mut browser: Option<Browser> = None;
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["environments_allowed"], true);
    let card = environment(&c, "pro").await;
    assert_eq!(card["status"], "ready", "{card}");
    assert_eq!(card["trust"], "governed");
    assert_eq!(card["python_version"], found.python_version);
    assert_eq!(card["arcpy"], "arcpy: no");

    if !can_confine() {
        let e = err(&call(
            &c,
            "run_python",
            json!({"code": "print(1)", "environment": "pro"}),
        )
        .await);
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

    // A listener the script would reach if it had any network.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let engine_prefix = String::from_utf8(
        StdCommand::new(&py)
            .args(["-c", "import sys; print(sys.prefix)"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let env_prefixes: Vec<String> = envs::read_confirmed(&env.environments())["pro"]
        .prefixes
        .clone();
    let check_engine = !env_prefixes.iter().any(|p| {
        Path::new(&engine_prefix).starts_with(p) || Path::new(p).starts_with(&engine_prefix)
    });
    let code = format!(
        r#"import json, os, shutil, socket, sqlite3, sys
def attempt(name, f):
    try:
        f()
        print(name + ": ok")
    except Exception as e:
        print(name + ": refused", type(e).__name__)
paths = json.load(open("inputs.json"))
shutil.copyfile(paths["roads"], os.path.join("out", "x.gpkg"))
db = sqlite3.connect(os.path.join("out", "x.gpkg"))
table = db.execute("SELECT table_name FROM gpkg_contents").fetchone()[0]
db.execute(f'DELETE FROM "{{table}}" WHERE lanes = 2')
db.commit()
db.close()
print("prefix", os.path.normcase(os.path.realpath(sys.prefix)) == os.path.normcase(os.path.realpath({venv:?})))
attempt("network", lambda: socket.create_connection(("127.0.0.1", {port}), timeout=3))
attempt("engine", lambda: os.listdir({engine:?}))
attempt("write_env", lambda: open(os.path.join({site:?}, "planted.pth"), "w").write("import os"))
"#,
        venv = venv.display().to_string(),
        engine = engine_prefix,
        site = site_packages(&venv).display().to_string(),
    );
    let args = json!({"code": code, "inputs": {"roads": roads}, "environment": "pro"});
    let first = call(&c, "run_python", args.clone()).await;
    if first.is_error == Some(true) && a1_fallback(&err(&first)) {
        c.cancel().await.unwrap();
        return;
    }
    let (r, card) = approved_run(&c, &env, &mut browser, args).await;
    let summary = card["summary"].as_str().unwrap();
    assert!(
        summary.contains("in environment pro") && summary.contains("no network"),
        "{summary}"
    );
    assert_eq!(r["result"]["status"], "done", "{r}");
    let stdout = r["result"]["stdout"].as_str().unwrap();
    assert!(stdout.contains("prefix True"), "{stdout}");
    assert!(stdout.contains("network: refused"), "{stdout}");
    assert!(stdout.contains("write_env: refused"), "{stdout}");
    if check_engine {
        assert!(stdout.contains("engine: refused"), "{stdout}");
    }
    assert!(!site_packages(&venv).join("planted.pth").exists());
    drop(listener);
    // out/x.gpkg came back as a layer, checked by the engine.
    let out = &r["result"]["outputs"][0];
    assert!(out["handle"].as_str().unwrap().ends_with("-x.gpkg"), "{r}");
    assert_eq!(out["summary"]["rows"], 4, "{r}");
    let rows = ok(&call(
        &c,
        "read_output",
        json!({"handle": out["handle"], "limit": 10}),
    )
    .await);
    assert_eq!(rows["result"]["total"], 4);
    // Provenance (F-9) and the audit line (F-8) name the environment.
    let ws = PathBuf::from(w["result"]["workspace"].as_str().unwrap());
    let prov = r["provenance"].as_str().unwrap();
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join("provenance").join(format!("{prov}.prov.json"))).unwrap(),
    )
    .unwrap();
    let step = &rec["steps"][0];
    assert_eq!(step["environment"]["id"], "pro");
    assert_eq!(step["environment"]["trust"], "governed");
    assert_eq!(step["environment"]["python_version"], found.python_version);
    assert_eq!(
        step["environment"]["packages_sha256"],
        found.packages_sha256()
    );
    assert_eq!(step["confinement"]["level"], "A");
    // The engine's Python still runs scripts too.
    let (r, _) = approved_run(
        &c,
        &env,
        &mut browser,
        json!({"code": "print('engine run')"}),
    )
    .await;
    assert_eq!(r["result"]["status"], "done", "{r}");
    c.cancel().await.unwrap();
    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).unwrap();
    let text = std::fs::read_to_string(&audit).unwrap();
    assert!(text.contains("\"environment\":\"pro\""), "{text}");
    assert!(text.contains("\"environment\":null"), "{text}");
}

/// Check 2 (R-13b): a package installed after registration marks the
/// environment changed, and runs are refused until it is confirmed again.
#[tokio::test]
async fn a_package_change_marks_the_environment_changed() {
    let Some(py) = engine() else { return };
    let env = setup(
        None,
        "allow_scripts: true\nenvironments: { allow: true }\napprovals: { page: true, wait_seconds: 0 }\n",
    );
    let venv = env.root.join("venv");
    let vpy = make_venv(&py, &venv);
    register(&env, "pro", &vpy, CreatedBy::Person);
    let c = start(&env, &py).await;
    assert_eq!(environment(&c, "pro").await["status"], "ready");

    let dist = fake_install(&venv, "fakepkg", "1.0");
    let card = environment(&c, "pro").await;
    assert_eq!(card["status"], "changed", "{card}");
    assert!(card["reason"]
        .as_str()
        .unwrap()
        .contains("confirm it again in Settings"));
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "pro"}),
    )
    .await);
    assert!(e.contains("confirm it again in Settings"), "{e}");

    // Confirmed again at the keyboard: it runs again (as far as this
    // system can confine it).
    confirm_again(&env, "pro", &allowed()).unwrap();
    assert_eq!(environment(&c, "pro").await["status"], "ready");
    assert!(std::fs::read_to_string(env.yaml("pro")).unwrap().contains(
        &envs::pending(&env.environments(), "pro")
            .unwrap()
            .probe
            .packages_sha256
    ));

    if can_confine() {
        // A change the cheap signal can't see (a version edited in place)
        // is found by the probe at use, which marks it changed.
        let first = call(
            &c,
            "run_python",
            json!({"code": "print(1)", "environment": "pro"}),
        )
        .await;
        if !(first.is_error == Some(true) && a1_fallback(&err(&first))) {
            assert_eq!(ok(&first)["result"]["status"], "approval_required");
            std::fs::write(
                dist.join("METADATA"),
                "Metadata-Version: 2.1\nName: fakepkg\nVersion: 2.0\n",
            )
            .unwrap();
            assert_eq!(environment(&c, "pro").await["status"], "ready");
            let e = err(&call(
                &c,
                "run_python",
                json!({"code": "print(1)", "environment": "pro"}),
            )
            .await);
            assert!(e.contains("confirm it again in Settings"), "{e}");
            assert_eq!(environment(&c, "pro").await["status"], "changed");
        }
    }
    c.cancel().await.unwrap();
}

/// Check 3 (Security baseline 9): no MCP tool registers, changes or
/// confirms an environment, and scripts can't write the registrations.
#[tokio::test]
async fn no_tool_can_register_or_change_an_environment() {
    let Some(py) = engine() else { return };
    let env = setup(
        None,
        "allow_scripts: true\nallow_personal_tools: true\nallow_workflows: true\nenvironments: { allow: true, allow_trusted: true, allow_network: true }\napprovals: { page: true, wait_seconds: 0 }\n",
    );
    let venv = env.root.join("venv");
    let vpy = make_venv(&py, &venv);
    register(&env, "pro", &vpy, CreatedBy::Person);
    let before = std::fs::read(env.yaml("pro")).unwrap();
    let c = start(&env, &py).await;
    let mut browser: Option<Browser> = None;
    let tools = c.list_all_tools().await.unwrap();
    assert!(!tools.is_empty());
    // propose_environment (T-7) only asks the person; nothing else touches
    // environments.
    let asks_only = ["propose_environment"];
    for t in &tools {
        let name = t.name.to_lowercase();
        let about_env =
            name.contains("environment") || name.starts_with("env") || name.contains("_env");
        assert!(
            !about_env || asks_only.contains(&name.as_str()),
            "a tool touches environments: {name}"
        );
        // Only the tools that run code in one take an environment's id
        // (scripts; personal tools since T-9: F-5, F-6).
        let schema = serde_json::to_string(&t.input_schema).unwrap();
        if schema.contains("\"environment\"") {
            assert!(
                ["run_python", "propose_tool", "run_tool"].contains(&name.as_str()),
                "{name}: {schema}"
            );
        }
    }
    for name in [
        "register_environment",
        "env_add",
        "add_environment",
        "confirm_environment",
        "set_environment",
        "remove_environment",
    ] {
        let mut p = CallToolRequestParams::new(name.to_string());
        p = p.with_arguments(
            json!({"id": "evil", "python": vpy, "trust": "trusted"})
                .as_object()
                .unwrap()
                .clone(),
        );
        let r = c.call_tool(p).await;
        assert!(
            r.is_err() || r.as_ref().is_ok_and(|r| r.is_error == Some(true)),
            "{name}: {r:?}"
        );
    }
    assert!(!env.yaml("evil").exists());

    if can_confine() {
        let code = format!(
            r#"import os
def attempt(name, f):
    try:
        f()
        print(name + ": ok")
    except Exception as e:
        print(name + ": refused", type(e).__name__)
attempt("write_registration", lambda: open({yaml:?}, "w").write("id: pro\n"))
attempt("new_registration", lambda: open({new:?}, "w").write("id: evil\n"))
attempt("write_confirmed", lambda: open({conf:?}, "w").write("{{}}"))
"#,
            yaml = env.yaml("pro").display().to_string(),
            new = env.yaml("evil").display().to_string(),
            conf = env
                .environments()
                .join(envs::CONFIRMED)
                .display()
                .to_string(),
        );
        for environment in [None, Some("pro")] {
            let mut args = json!({"code": code});
            if let Some(e) = environment {
                args["environment"] = json!(e);
                let first = call(&c, "run_python", args.clone()).await;
                if first.is_error == Some(true) && a1_fallback(&err(&first)) {
                    continue;
                }
            }
            let (r, _) = approved_run(&c, &env, &mut browser, args).await;
            let stdout = r["result"]["stdout"].as_str().unwrap();
            for line in [
                "write_registration: refused",
                "new_registration: refused",
                "write_confirmed: refused",
            ] {
                assert!(stdout.contains(line), "{line} in {stdout}");
            }
        }
    }
    assert_eq!(std::fs::read(env.yaml("pro")).unwrap(), before);
    assert!(!env.yaml("evil").exists());
    assert_eq!(environment(&c, "pro").await["status"], "ready");
    c.cancel().await.unwrap();
}

/// Check 4 (F-7): with environments.allow false, env add and the
/// environment argument are refused, and scripts still run in the engine.
#[tokio::test]
async fn with_environments_off_scripts_run_only_in_the_engine() {
    let Some(py) = engine() else { return };
    let env = setup(
        None,
        "allow_scripts: true\napprovals: { page: true, wait_seconds: 0 }\n",
    );
    let venv = env.root.join("venv");
    let vpy = make_venv(&py, &venv);
    let out = cli(
        &env,
        &["env", "add", "pro", "--python", &vpy.display().to_string()],
    );
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("environments.allow"), "{text}");
    assert!(!env.yaml("pro").exists());
    // The terminal's own call refuses too.
    let found = envs::probe(&vpy, Duration::from_secs(120)).unwrap();
    let reg = Registration {
        id: "pro".into(),
        python: vpy.clone(),
        trust: Trust::Governed,
        confinement: Confinement::A,
        created_by: CreatedBy::Person,
        description: None,
    };
    let e = envs::register(
        &env.environments(),
        &env.envs(),
        &EnvironmentsPolicy::default(),
        &reg,
        &found,
        "pro",
        NOW,
    )
    .unwrap_err();
    assert!(e.contains("environments.allow"), "{e}");
    // One registered while it was on stays unused while it is off.
    register(&env, "pro", &vpy, CreatedBy::Person);

    let c = start(&env, &py).await;
    let mut browser: Option<Browser> = None;
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["environments_allowed"], false);
    assert_eq!(w["result"]["environments"], json!([]));
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "pro"}),
    )
    .await);
    assert!(e.contains("environments.allow"), "{e}");
    if can_confine() {
        let (r, card) = approved_run(
            &c,
            &env,
            &mut browser,
            json!({"code": "import sys, os\nprint('prefix', os.path.realpath(sys.prefix))\n"}),
        )
        .await;
        assert_eq!(r["result"]["status"], "done", "{r}");
        assert!(!card["summary"].as_str().unwrap().contains("environment"));
        let stdout = r["result"]["stdout"].as_str().unwrap();
        let venv_real = std::fs::canonicalize(&venv).unwrap().display().to_string();
        assert!(!stdout.contains(&venv_real), "{stdout}");
    }
    c.cancel().await.unwrap();
}

/// Check 5 (R-13b, D-3): editing the YAML (trust to trusted, then python
/// to another interpreter) marks it changed each time; runs are refused
/// until it is confirmed again.
#[tokio::test]
async fn editing_the_registration_marks_it_changed() {
    let Some(py) = engine() else { return };
    let env = setup(
        None,
        "allow_scripts: true\nenvironments: { allow: true }\napprovals: { page: true, wait_seconds: 0 }\n",
    );
    let vpy = make_venv(&py, &env.root.join("venv"));
    let other = make_venv(&py, &env.root.join("other-venv"));
    register(&env, "pro", &vpy, CreatedBy::Person);
    let c = start(&env, &py).await;
    assert_eq!(environment(&c, "pro").await["status"], "ready");
    let refused = |e: String| assert!(e.contains("confirm it again in Settings"), "{e}");

    // Trust edited to trusted.
    env.edit("pro", "trust: governed", "trust: trusted");
    let card = environment(&c, "pro").await;
    assert_eq!(card["status"], "changed", "{card}");
    refused(err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "pro"}),
    )
    .await));
    // Confirming it as trusted is refused while policy doesn't allow it.
    let e = confirm_again(&env, "pro", &allowed()).unwrap_err();
    assert!(e.contains("allow_trusted"), "{e}");
    assert_eq!(environment(&c, "pro").await["status"], "changed");
    env.edit("pro", "trust: trusted", "trust: governed");
    confirm_again(&env, "pro", &allowed()).unwrap();
    assert_eq!(environment(&c, "pro").await["status"], "ready");

    // Python edited to another interpreter.
    let text = std::fs::read_to_string(env.yaml("pro")).unwrap();
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
    doc["python"] = serde_yaml::Value::from(other.display().to_string());
    std::fs::write(env.yaml("pro"), serde_yaml::to_string(&doc).unwrap()).unwrap();
    assert_eq!(environment(&c, "pro").await["status"], "changed");
    refused(err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "pro"}),
    )
    .await));
    // And created_by edited to columbus.
    confirm_again(&env, "pro", &allowed()).unwrap();
    assert_eq!(environment(&c, "pro").await["status"], "ready");
    env.edit("pro", "created_by: person", "created_by: columbus");
    assert_eq!(environment(&c, "pro").await["status"], "changed");
    refused(err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "pro"}),
    )
    .await));
    // ...which can't even be confirmed: its interpreter isn't Columbus's.
    assert!(confirm_again(&env, "pro", &allowed())
        .unwrap_err()
        .contains("Columbus created"));
    env.edit("pro", "created_by: columbus", "created_by: person");
    assert_eq!(environment(&c, "pro").await["status"], "ready");
    // An unknown key is refused (the formats rule).
    env.edit(
        "pro",
        "trust: governed",
        "trust: governed\nallow_everything: true",
    );
    assert_eq!(environment(&c, "pro").await["status"], "invalid");
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "pro"}),
    )
    .await);
    assert!(e.contains("cannot be used"), "{e}");
    env.edit(
        "pro",
        "trust: governed\nallow_everything: true",
        "trust: governed",
    );
    assert_eq!(environment(&c, "pro").await["status"], "ready");
    if can_confine() {
        let first = call(
            &c,
            "run_python",
            json!({"code": "print(1)", "environment": "pro"}),
        )
        .await;
        if !(first.is_error == Some(true) && a1_fallback(&err(&first))) {
            assert_eq!(ok(&first)["result"]["status"], "approval_required");
        }
    }
    c.cancel().await.unwrap();
}

/// Check 6: removing a person-registered environment leaves their folder;
/// a Columbus-created one under <Columbus data>/envs/ loses its folder,
/// and only after the confirmation.
#[tokio::test]
async fn removing_never_deletes_the_persons_folder() {
    let Some(py) = python("import venv") else {
        eprintln!("skipping: no Python");
        return;
    };
    let env = setup(None, "environments: { allow: true }\n");
    let person = env.root.join("person-venv");
    let ppy = make_venv(&py, &person);
    register(&env, "mine", &ppy, CreatedBy::Person);
    // A Columbus-created environment: what T-7's creation leaves, made here
    // with venv under <Columbus data>/envs/<id>/.
    let made = env.envs().join("made");
    let mpy = make_venv(&py, &made);
    register(&env, "made", &mpy, CreatedBy::Columbus);
    // created_by: columbus is refused for an interpreter outside envs/.
    let found = envs::probe(&ppy, Duration::from_secs(120)).unwrap();
    let outside = Registration {
        id: "outside".into(),
        python: ppy.clone(),
        trust: Trust::Governed,
        confinement: Confinement::A,
        created_by: CreatedBy::Columbus,
        description: None,
    };
    assert!(envs::register(
        &env.environments(),
        &env.envs(),
        &allowed(),
        &outside,
        &found,
        "outside",
        NOW
    )
    .is_err());
    // A person's environment under envs/ edited to say columbus.
    let sneaky = env.envs().join("sneaky");
    let spy = make_venv(&py, &sneaky);
    register(&env, "sneaky", &spy, CreatedBy::Person);
    env.edit("sneaky", "created_by: person", "created_by: columbus");

    // The person's: the YAML only.
    let plan = envs::removal(&env.environments(), &env.envs(), "mine").unwrap();
    assert_eq!(plan.folder, None);
    assert!(envs::remove(&env.environments(), &env.envs(), "mine", "nope").is_err());
    assert!(env.yaml("mine").exists());
    let done = envs::remove(&env.environments(), &env.envs(), "mine", "mine").unwrap();
    assert_eq!(done.folder, None);
    assert!(!env.yaml("mine").exists());
    assert!(person.is_dir() && ppy.exists());
    assert!(!envs::read_confirmed(&env.environments()).contains_key("mine"));

    // Columbus's: its folder too, only once confirmed.
    let plan = envs::removal(&env.environments(), &env.envs(), "made").unwrap();
    assert_eq!(plan.folder, Some(std::fs::canonicalize(&made).unwrap()));
    assert!(
        envs::remove(&env.environments(), &env.envs(), "made", "mad")
            .unwrap_err()
            .contains("not confirmed")
    );
    assert!(made.is_dir() && env.yaml("made").exists());
    envs::remove(&env.environments(), &env.envs(), "made", "made").unwrap();
    assert!(!made.exists());
    assert!(!env.yaml("made").exists());

    // Edited to say columbus: not in what was confirmed, so the folder stays.
    assert_eq!(
        envs::removal(&env.environments(), &env.envs(), "sneaky")
            .unwrap()
            .folder,
        None
    );
    envs::remove(&env.environments(), &env.envs(), "sneaky", "sneaky").unwrap();
    assert!(sneaky.is_dir());
    assert!(env.envs().is_dir());

    // The terminal command: refused before it asks, for an unknown id.
    let out = cli(&env, &["env", "remove", "ghost"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no environment"));
}

/// Level B (network) needs environments.allow_network, at registration and
/// at every run, and the approval card says the script can reach the
/// internet.
#[tokio::test]
async fn network_level_needs_policy_and_is_on_the_card() {
    let Some(py) = engine() else { return };
    let env = setup(
        None,
        "allow_scripts: true\nenvironments: { allow: true, allow_network: true }\napprovals: { page: true, wait_seconds: 0 }\n",
    );
    let vpy = make_venv(&py, &env.root.join("venv"));
    let found = envs::probe(&vpy, Duration::from_secs(120)).unwrap();
    let reg = Registration {
        id: "net".into(),
        python: vpy.clone(),
        trust: Trust::Governed,
        confinement: Confinement::B,
        created_by: CreatedBy::Person,
        description: None,
    };
    let e = envs::register(
        &env.environments(),
        &env.envs(),
        &allowed(),
        &reg,
        &found,
        "net",
        NOW,
    )
    .unwrap_err();
    assert!(e.contains("allow_network"), "{e}");
    let net = EnvironmentsPolicy {
        allow: true,
        allow_network: true,
        ..Default::default()
    };
    envs::register(
        &env.environments(),
        &env.envs(),
        &net,
        &reg,
        &found,
        "net",
        NOW,
    )
    .unwrap();
    let c = start(&env, &py).await;
    assert_eq!(environment(&c, "net").await["confinement"], "B");
    if can_confine()
        && StdCommand::new(launcher())
            .args(["--check", "B"])
            .output()
            .is_ok_and(|o| o.status.success())
    {
        let first = call(
            &c,
            "run_python",
            json!({"code": "print(1)", "environment": "net"}),
        )
        .await;
        if !(first.is_error == Some(true) && a1_fallback(&err(&first))) {
            let r = ok(&first);
            assert_eq!(r["result"]["status"], "approval_required", "{r}");
            let summary = r["result"]["approval"]["summary"].as_str().unwrap();
            assert!(
                summary.contains("the script can reach the internet"),
                "{summary}"
            );
        }
    }
    c.cancel().await.unwrap();
    env.policy("allow_scripts: true\nenvironments: { allow: true }\napprovals: { page: false }\n");
    let c = start(&env, &py).await;
    let e = err(&call(
        &c,
        "run_python",
        json!({"code": "print(1)", "environment": "net"}),
    )
    .await);
    assert!(e.contains("allow_network"), "{e}");
    c.cancel().await.unwrap();
}
