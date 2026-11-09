//! T-20 (AF-18, REQ-25, REQ-26, R-19, F-11): workflow steps read `files`
//! and `arcgis` datasets through the same tools, `outputs.folder` delivers
//! `gdb`, and a signed-in ArcGIS connection keeps its sign-in alive across
//! unattended runs.
//!
//! The ArcGIS side is `examples/fake_oauth_server.rs`: a portal at
//! `http://localhost:<port>` and two services on `127.0.0.1:<port>`, one
//! public (`Public/FeatureServer`) and one that needs a token
//! (`Secure/FeatureServer`). Runs go through the real `columbus-mcp
//! workflow run <id> --scheduled`, as the scheduler starts them.
//!
//! Fake tokens contain `zq7k`; no run record, report, audit line or
//! provenance file may.
//!
//! Needs the GIS engine (Python with geopandas, pyogrio, shapely 2.1+ and
//! pyproj); skipped otherwise.

mod support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use columbus_shared::workflows as wf;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const MARK: &str = "zq7k";

fn python() -> Option<String> {
    support::python_for(
        "import geopandas, pyogrio, pyproj, shapely; assert hasattr(shapely, 'has_m')",
    )
}

fn example(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let p = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.exists(),
        "build the example first: cargo build --example {name}"
    );
    p
}

/// The fake portal and services, and its port.
struct Fake(Child, u16);
impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

impl Fake {
    fn start() -> Fake {
        let mut child = Command::new(example("fake_oauth_server"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // The portal vouches for the services' host (R-18).
            .env(
                "FAKE_PORTAL_SERVERS",
                "http://127.0.0.1:{port}/arcgis/rest/services",
            )
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let base = line.trim().strip_prefix("listening ").unwrap().to_string();
        let port = base.rsplit(':').next().unwrap().parse().unwrap();
        Fake(child, port)
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.1)
    }

    async fn get(&self, path: &str) -> Vec<Value> {
        reqwest::get(self.url(path))
            .await
            .unwrap()
            .json::<Vec<Value>>()
            .await
            .unwrap()
    }

    /// The grant types of the ArcGIS token calls so far.
    async fn grants(&self) -> Vec<String> {
        self.get("/test/grants")
            .await
            .iter()
            .map(|g| g["grant_type"].as_str().unwrap_or("").to_string())
            .collect()
    }

    async fn post(&self, path: &str) {
        let r = reqwest::Client::new()
            .post(self.url(path))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
    }
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    py: String,
}

fn q(p: &Path) -> String {
    serde_json::to_string(&p.display().to_string()).unwrap()
}

impl Env {
    fn new(py: &str, port: u16) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for d in ["connections", "outbox", "parcels", "other-files"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(
            root.join("policy.yaml"),
            format!(
                "connection_types: [arcgis, files]\nhosts: [\"127.0.0.1\", \"localhost\"]\n\
                 limits: {{ rows: 1000, seconds: 120 }}\napprovals: {{ page: true, wait_seconds: 0 }}\n\
                 allow_workflows: true\nexports:\n  - {{ id: outbox, path: {}, granted: true }}\n",
                q(&root.join("outbox"))
            ),
        )
        .unwrap();
        // A folder of files: parcels as lon/lat points.
        std::fs::write(
            root.join("parcels/parcels.csv"),
            "PARCEL,lon,lat\nP1,-97.7431,30.2672\nP2,-97.7500,30.2800\n",
        )
        .unwrap();
        std::fs::write(
            root.join("other-files/other.csv"),
            "A,lon,lat\n1,-97.7,30.2\n",
        )
        .unwrap();
        let conn = |id: &str, body: String| {
            std::fs::write(
                root.join("connections").join(format!("{id}.yaml")),
                format!("id: {id}\n{body}datasets:\n  allow: [\"*\"]\n"),
            )
            .unwrap();
        };
        conn(
            "parcels",
            format!("type: files\npath: {}\n", q(&root.join("parcels"))),
        );
        conn(
            "other-files",
            format!("type: files\npath: {}\n", q(&root.join("other-files"))),
        );
        conn(
            "assets",
            format!("type: arcgis\nurl: http://127.0.0.1:{port}/arcgis/rest/services/Public/FeatureServer\n"),
        );
        conn(
            "svc",
            format!(
                "type: arcgis\nurl: http://127.0.0.1:{port}/arcgis/rest/services/Secure/FeatureServer\n\
                 auth: {{type: oauth, portal: \"http://localhost:{port}\", client_id: FakeClientId123}}\n"
            ),
        );
        Env {
            _dir: dir,
            root,
            py: py.to_string(),
        }
    }

    fn workflows(&self) -> PathBuf {
        self.root.join("workflows")
    }

    fn tools(&self) -> PathBuf {
        self.root.join("tools")
    }

    fn state_file(&self) -> PathBuf {
        self.root
            .join("connections")
            .join(columbus_shared::arcgis_state::FILE)
    }

    fn entry(&self) -> Value {
        let t = std::fs::read_to_string(self.root.join("secrets/COLUMBUS_SECRET_ARCGIS_SVC"))
            .expect("svc is signed in");
        serde_json::from_str(&t).unwrap()
    }

    fn vars(&self) -> Vec<(String, String)> {
        let r = |p: &str| self.root.join(p).display().to_string();
        let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../workers/python/columbus_worker.py")
            .canonicalize()
            .unwrap();
        vec![
            ("COLUMBUS_POLICY".into(), r("policy.yaml")),
            ("COLUMBUS_CONNECTIONS".into(), r("connections")),
            ("COLUMBUS_SERVERS".into(), r("servers")),
            ("COLUMBUS_PERSONAL_SERVERS".into(), r("personal-servers")),
            ("COLUMBUS_PLUGINS".into(), r("plugins")),
            ("COLUMBUS_PERSONAL_PLUGINS".into(), r("personal-plugins")),
            ("COLUMBUS_WORKSPACES".into(), r("workspaces")),
            ("COLUMBUS_TOOLS".into(), r("tools")),
            ("COLUMBUS_WORKFLOWS".into(), r("workflows")),
            ("COLUMBUS_AUDIT_DIR".into(), r("audit")),
            ("COLUMBUS_LIVE_DIR".into(), r("live")),
            ("COLUMBUS_DEV_SECRETS".into(), "1".into()),
            ("COLUMBUS_DEV_SECRETS_DIR".into(), r("secrets")),
            ("COLUMBUS_FETCH_TEST_LOOPBACK".into(), "1".into()),
            ("COLUMBUS_APPROVALS_BROWSER".into(), "none".into()),
            ("COLUMBUS_APPROVALS_LAUNCH_FILE".into(), r("launch.txt")),
            ("COLUMBUS_SIGNIN_BROWSER".into(), "print".into()),
            ("COLUMBUS_COMPANION".into(), "off".into()),
            ("COLUMBUS_WORKER".into(), worker.display().to_string()),
            ("COLUMBUS_PYTHON".into(), self.py.clone()),
            ("COLUMBUS_LOG".into(), "warn".into()),
        ]
    }

    async fn start(&self) -> RunningService<RoleClient, ()> {
        let vars = self.vars();
        let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(
            move |cmd| {
                for (k, v) in &vars {
                    cmd.env(k, v);
                }
            },
        );
        ().serve(TokioChildProcess::new(cmd).unwrap())
            .await
            .unwrap()
    }

    /// Propose a workflow, as the assistant would, then approve it, as the
    /// app does after Touch ID.
    async fn propose_and_approve(&self, text: &str) {
        let c = self.start().await;
        let r = call(&c, "propose_workflow", json!({"workflow": text})).await;
        assert_ne!(r.is_error, Some(true), "{:?}", r.content);
        c.cancel().await.unwrap();
        let id = wf::check(text).unwrap().id;
        let p = wf::read_proposal(&self.workflows(), &self.tools(), &id).unwrap();
        wf::install(&self.workflows(), &self.tools(), &id, &p.fingerprint, "now").unwrap();
    }

    /// Run a workflow as the scheduler would: (exit code, run.json, report).
    fn run(&self, id: &str) -> (i32, Value, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"))
            .args(["workflow", "run", id, "--scheduled"])
            .envs(self.vars())
            .output()
            .unwrap();
        let code = out.status.code().unwrap_or(-1);
        let runs = wf::list_runs(&self.workflows(), id, 1);
        let latest = runs.first().unwrap_or_else(|| {
            panic!(
                "no run recorded (exit {code}): {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let (rec, report) =
            wf::read_run(&self.workflows(), id, latest["run"].as_str().unwrap()).unwrap();
        (code, rec, report)
    }

    /// `columbus-mcp arcgis signin svc`, with the test as the browser.
    async fn signin(&self) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"));
        cmd.args(["arcgis", "signin", "svc"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(self.vars());
        let mut child = cmd.spawn().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut out = String::new();
        for line in lines.by_ref() {
            let line = line.unwrap();
            out.push_str(&line);
            out.push('\n');
            if let Some(url) = line.strip_prefix("open: ") {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::limited(5))
                    .build()
                    .unwrap()
                    .get(url)
                    .send()
                    .await
                    .unwrap();
                break;
            }
        }
        for line in lines {
            out.push_str(&line.unwrap());
            out.push('\n');
        }
        assert!(child.wait().unwrap().success(), "{out}");
        assert!(out.contains("Signed in"), "{out}");
    }

    /// Set the record of when svc's refresh token ends: `days` from now.
    fn refresh_ends_in(&self, days: f64) {
        let mut all: Value =
            serde_json::from_slice(&std::fs::read(self.state_file()).unwrap()).unwrap();
        let end = chrono::Utc::now() + chrono::Duration::seconds((days * 86_400.0) as i64);
        all["svc"]["refresh_expires_at"] =
            json!(end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        std::fs::write(self.state_file(), serde_json::to_vec(&all).unwrap()).unwrap();
    }

    /// Days of life left on svc's refresh token, by the record.
    fn days_left(&self) -> f64 {
        let all: Value =
            serde_json::from_slice(&std::fs::read(self.state_file()).unwrap()).unwrap();
        let end = chrono::DateTime::parse_from_rfc3339(
            all["svc"]["refresh_expires_at"].as_str().unwrap(),
        )
        .unwrap();
        (end.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds() as f64 / 86_400.0
    }

    /// Everything a run, the audit log and provenance wrote, as text.
    fn written(&self) -> String {
        let mut out = String::new();
        for d in ["workflows", "audit", "workspaces", "outbox"] {
            for e in walk(&self.root.join(d)) {
                if let Ok(t) = std::fs::read(&e) {
                    out.push_str(&String::from_utf8_lossy(&t));
                }
            }
        }
        out
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
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

fn err(r: &CallToolResult) -> String {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected an error, got {:?}",
        r.structured_content
    );
    serde_json::to_string(&r.content).unwrap()
}

const REACH: &str = "id: weekly-reach\nname: Weekly reach\npurpose: Deliver the assets and parcels as file geodatabases.\nschedule: { days: [mon], at: \"06:00\" }\nscope: { connections: [assets, parcels], run_minutes: 5 }\nsteps:\n  - id: assets\n    tool: query_features\n    args: { connection: assets, dataset: Assets }\n  - id: parcels\n    tool: query_features\n    args: { connection: parcels, dataset: parcels }\n  - id: checked\n    tool: verify\n    args: { layer: \"${parcels}\" }\noutputs:\n  folder: { export: outbox, steps: [assets, parcels], format: gdb }\n";

/// Check 1 (REQ-25, F-11): a step on a fake arcgis layer and one on a files
/// dataset run, the layers arrive as file geodatabases, and a step on a
/// connection outside the workflow's scope is still refused.
#[tokio::test]
async fn a_workflow_reads_arcgis_and_files_and_delivers_gdb() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let fake = Fake::start();
    let env = Env::new(&py, fake.1);

    // F-11's formats are accepted; others still are not.
    assert!(wf::check(&REACH.replace("format: gdb", "format: tif")).is_ok());
    let e = wf::check(&REACH.replace("format: gdb", "format: kml")).unwrap_err();
    assert!(e.contains("gpkg, gdb, geojson, fgb, shp, csv, tif"), "{e}");

    // Outside the scope: refused when proposed, whatever the connection type.
    let c = env.start().await;
    for (from, to) in [
        (
            "connection: parcels, dataset: parcels",
            "connection: other-files, dataset: other",
        ),
        (
            "connection: assets, dataset: Assets",
            "connection: svc, dataset: Assets",
        ),
    ] {
        let bad = REACH.replace(from, to);
        let e = err(&call(&c, "propose_workflow", json!({"workflow": bad})).await);
        assert!(e.contains("not listed in scope.connections"), "{e}");
    }
    c.cancel().await.unwrap();

    env.propose_and_approve(REACH).await;
    let (code, rec, report) = env.run("weekly-reach");
    assert_eq!(code, 0, "{rec:#}");
    assert!(
        rec["status"].as_str().unwrap().starts_with("succeeded"),
        "{rec:#}"
    );
    let steps = rec["steps"].as_array().unwrap();
    assert!(steps.iter().all(|s| s["status"] == "done"), "{rec:#}");
    assert!(
        steps[0]["summary"].as_str().unwrap().contains("2 rows"),
        "{rec:#}"
    );
    assert!(
        steps[1]["summary"].as_str().unwrap().contains("2 rows"),
        "{rec:#}"
    );
    // Both layers came as new file geodatabases, named for a feature class.
    let mut gdbs: Vec<String> = std::fs::read_dir(env.root.join("outbox"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    gdbs.sort();
    assert_eq!(gdbs.len(), 2, "{gdbs:?} {rec:#}");
    assert!(
        gdbs[0].starts_with("weekly_reach_assets_") && gdbs[0].ends_with(".gdb"),
        "{gdbs:?}"
    );
    assert!(
        gdbs[1].starts_with("weekly_reach_parcels_") && gdbs[1].ends_with(".gdb"),
        "{gdbs:?}"
    );
    for g in &gdbs {
        let gdb = env.root.join("outbox").join(g);
        assert!(std::fs::read_dir(&gdb).unwrap().count() > 0, "{g} is empty");
        let stem = g.trim_end_matches(".gdb");
        assert!(env
            .root
            .join("outbox")
            .join(format!("{stem}.prov.json"))
            .is_file());
        // GDAL reads it back: two features, in a feature class of that name.
        let out = Command::new(&env.py)
            .args([
                "-c",
                "import sys, pyogrio; print(pyogrio.read_info(sys.argv[1], layer=sys.argv[2])['features'])",
            ])
            .arg(&gdb)
            .arg(stem)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "2",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(report.contains("| assets | query_features |"), "{report}");
    assert!(report.contains("exported to the outbox folder"), "{report}");
}

const WEEKLY: &str = "id: svc-weekly\nname: Weekly assets\npurpose: Read the secured assets every week.\nschedule: { days: [mon], at: \"06:00\" }\nscope: { connections: [svc], run_minutes: 5 }\nsteps:\n  - id: assets\n    tool: query_features\n    args: { connection: svc, dataset: Assets }\n";

const NEEDS: &str = "needs you: sign in again (columbus-mcp arcgis signin svc)";

/// Check 2 (REQ-26, R-19): with 6 days left the refresh token is exchanged
/// before the step; with 8 it is not; an expired one stops the step with
/// "needs you" naming the sign-in command, and the report shows it.
#[tokio::test]
async fn a_weekly_run_keeps_the_arcgis_sign_in_alive() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let fake = Fake::start();
    let env = Env::new(&py, fake.1);
    env.signin().await;
    // Sign-in records when the refresh token ends (the fake says 14 days),
    // beside the connections, and never a token.
    let left = env.days_left();
    assert!((13.9..=14.0).contains(&left), "{left}");
    let state = std::fs::read_to_string(env.state_file()).unwrap();
    assert!(!state.contains(MARK), "{state}");
    // F-3b's entry keeps exactly its four keys.
    let mut keys: Vec<String> = env.entry().as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["access_token", "expires_at", "portal", "refresh_token"]
    );
    assert_eq!(fake.grants().await, ["authorization_code"]);
    env.propose_and_approve(WEEKLY).await;

    // 8 days left: not exchanged.
    env.refresh_ends_in(8.0);
    let before = env.entry()["refresh_token"].clone();
    let (code, rec, _) = env.run("svc-weekly");
    assert_eq!(code, 0, "{rec:#}");
    assert_eq!(rec["steps"][0]["status"], "done", "{rec:#}");
    assert_eq!(fake.grants().await, ["authorization_code"], "no token call");
    assert_eq!(env.entry()["refresh_token"], before);

    // 6 days left: exchanged before the step, the new refresh token stored,
    // and its 14 days recorded.
    env.refresh_ends_in(6.0);
    let (code, rec, _) = env.run("svc-weekly");
    assert_eq!(code, 0, "{rec:#}");
    assert_eq!(rec["steps"][0]["status"], "done", "{rec:#}");
    assert_eq!(
        fake.grants().await,
        ["authorization_code", "exchange_refresh_token"]
    );
    let g = fake.get("/test/grants").await;
    assert_eq!(g[1]["redirect_uri"], "urn:ietf:wg:oauth:2.0:oob", "{g:?}");
    assert_ne!(
        env.entry()["refresh_token"],
        before,
        "a new refresh token is stored"
    );
    let left = env.days_left();
    assert!((13.9..=14.0).contains(&left), "{left}");
    let renewed = rec["steps"][0]["checks"].as_array().unwrap();
    assert!(
        renewed.iter().any(|c| c["id"] == "arcgis.signin_renewed"),
        "{rec:#}"
    );

    // Expired: the step stops with "needs you", nothing is sent to the portal,
    // and the report says what to run.
    env.refresh_ends_in(-1.0);
    let (code, rec, report) = env.run("svc-weekly");
    assert_eq!(
        (code, rec["status"].as_str()),
        (2, Some("needs_you")),
        "{rec:#}"
    );
    assert_eq!(rec["steps"][0]["status"], "needs_you", "{rec:#}");
    assert_eq!(rec["steps"][0]["error"], NEEDS, "{rec:#}");
    assert!(report.contains(NEEDS), "{report}");
    assert!(report.contains("needs you: sign in again |"), "{report}");
    assert_eq!(
        fake.grants().await.len(),
        2,
        "no token call for a lapsed sign-in"
    );

    // Not known (no record) and lapsed at the portal: the exchange is tried
    // (the safe side), refused, and the step needs you the same way.
    std::fs::remove_file(env.state_file()).unwrap();
    fake.post("/test/lapse").await;
    let (code, rec, report) = env.run("svc-weekly");
    assert_eq!(
        (code, rec["status"].as_str()),
        (2, Some("needs_you")),
        "{rec:#}"
    );
    assert_eq!(rec["steps"][0]["error"], NEEDS, "{rec:#}");
    assert!(report.contains(NEEDS), "{report}");
    assert_eq!(
        fake.grants().await.last().unwrap(),
        "exchange_refresh_token"
    );

    // No token in any run record, report, audit line or provenance file.
    let written = env.written();
    assert!(!written.is_empty());
    assert!(!written.contains(MARK), "a token was written");

    // Signing out forgets the record too.
    env.signin().await;
    assert!(env.state_file().is_file());
    let out = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"))
        .args(["arcgis", "signout", "svc"])
        .envs(env.vars())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!env.state_file().exists(), "the only record was removed");
}

/// Deleting the connection forgets its record (F-3b's rule, for the record
/// beside it), and a confined script can't write the record.
#[tokio::test]
async fn the_sign_in_record_goes_with_the_connection_and_scripts_cant_write_it() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let fake = Fake::start();
    let env = Env::new(&py, fake.1);
    env.signin().await;
    assert!(env.state_file().is_file());

    // A confined script (level A, its run folder its only writable place)
    // can't write the record.
    let launcher = PathBuf::from(env!("CARGO_BIN_EXE_columbus-confine"));
    if Command::new(&launcher)
        .args(["--check", "A"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let ws = env.root.join("workspaces/run");
        std::fs::create_dir_all(&ws).unwrap();
        let ws = ws.canonicalize().unwrap();
        let dirs = Command::new(&py)
            .args([
                "-I",
                "-c",
                "import sys; print(sys.base_prefix); print(sys.prefix)",
            ])
            .output()
            .unwrap();
        let mut args: Vec<String> = vec![
            "--level".into(),
            "A".into(),
            "--name".into(),
            "test.confine".into(),
            "--rw".into(),
            ws.display().to_string(),
        ];
        for d in String::from_utf8_lossy(&dirs.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
        {
            args.push("--ro".into());
            args.push(d.trim().to_string());
        }
        let target = env.state_file().canonicalize().unwrap();
        let script = format!(
            "try:\n    open({:?}, 'w').write('{{}}')\n    print('write_state: ok')\nexcept Exception as e:\n    print('write_state: refused', type(e).__name__)\n",
            target.display().to_string()
        );
        args.extend(["--".into(), py.clone(), "-I".into(), "-c".into(), script]);
        let out = Command::new(&launcher)
            .args(&args)
            .current_dir(&ws)
            .env("TEMP", &ws)
            .env("TMP", &ws)
            .env("TMPDIR", &ws)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("write_state: refused"),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(std::fs::read_to_string(env.state_file())
            .unwrap()
            .contains("refresh_expires_at"));
    } else {
        eprintln!("confined-write sub-case skipped: this machine cannot confine");
    }

    // A running server sees the connection deleted: sign-in and record go.
    let c = env.start().await;
    let r = call(&c, "list_connections", json!({})).await;
    assert_ne!(r.is_error, Some(true));
    std::fs::remove_file(env.root.join("connections/svc.yaml")).unwrap();
    let mut gone = false;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let _ = call(&c, "list_connections", json!({})).await;
        if !env.state_file().exists()
            && !env.root.join("secrets/COLUMBUS_SECRET_ARCGIS_SVC").exists()
        {
            gone = true;
            break;
        }
    }
    c.cancel().await.unwrap();
    assert!(gone, "the record and the sign-in were removed");
}
