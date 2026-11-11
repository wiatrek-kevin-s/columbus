//! The boundary (build brief 3, D-16, REQ-36): what a script can reach.
//!
//! Real scripts run through `run_python` at level A, under policy
//! `scripts.approval: none`, so nobody is asked: the sandbox is all that
//! stands between the script and the person's computer. A script, and a
//! program it starts, try every way out, and each must fail: the
//! connections folder, the policy file, the audit log, another session's
//! workspace, this session's own results, the person's home, the gateway's
//! environment (Linux), a TCP connection to this computer and to a public
//! address, UDP, a name lookup, and writing or deleting anything outside
//! the run folder. Reading its inputs, writing its outputs and its
//! temporary files, and importing the engine's packages still work.
//!
//! The second test is the other half: where the sandbox cannot be set up,
//! no script runs, whatever the policy allows elsewhere.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj; skipped
//! otherwise (CI installs it, and fails instead of skipping). On Linux in
//! CI the first test also fails, instead of skipping, where the sandbox
//! cannot be set up; on other systems it runs where the sandbox can be.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

/// A password the gateway holds in its own environment (development
/// secrets), as a connection's would be in the credential store.
const SECRET: &str = "hunter2-not-for-scripts";

fn engine() -> Option<String> {
    let py = support::python_for("import geopandas, pyogrio, shapely, pyproj");
    if py.is_none() {
        eprintln!("skipping: the GIS engine is not installed");
    }
    py
}

fn can_confine() -> bool {
    StdCommand::new(env!("CARGO_BIN_EXE_columbus-confine"))
        .args(["--check", "A"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// CI says tests must not skip (the same switch as for the engine).
fn must_not_skip() -> bool {
    std::env::var("COLUMBUS_TEST_REQUIRE_ENGINE").is_ok_and(|v| v == "1")
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

fn setup(py: &str, policy: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    // A real path: macOS's temporary folder is behind a link.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    make_fixture(py, &root);
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [gpkg]\nlimits: {{ rows: 1000, seconds: 120 }}\napprovals: {{ page: false }}\n{policy}"),
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
    Env {
        _dir: dir,
        root,
        py: py.to_string(),
    }
}

async fn start(env: &Env, extra: &[(&str, PathBuf)]) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = env.root.clone();
    let py = env.py.clone();
    let extra: Vec<(String, PathBuf)> = extra
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_ENVIRONMENTS", root.join("environments"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_PERSONAL_PLUGINS", root.join("personal-plugins"))
                .env("COLUMBUS_TOOLS", root.join("tools"))
                .env("COLUMBUS_ORG_TOOLS", root.join("org-tools"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_LIVE_DIR", root.join("live"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_COMPANION", "off")
                .env("COLUMBUS_APPROVALS_BROWSER", "none")
                .env("COLUMBUS_DEV_SECRETS", "1")
                .env("COLUMBUS_SECRET_AUSTIN", SECRET)
                .env("COLUMBUS_LOG", "warn");
            for (k, v) in &extra {
                cmd.env(k, v);
            }
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

/// A layer of the session's (the district-14 roads) and where its file is.
async fn roads(c: &RunningService<RoleClient, ()>) -> (String, PathBuf, PathBuf) {
    let q = ok(&call(
        c,
        "query_features",
        json!({"connection": "austin", "dataset": "roads", "fields": ["route_id", "lanes"], "order_by": "route_id"}),
    )
    .await);
    let handle = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let who = ok(&call(c, "whoami", json!({})).await);
    let ws = PathBuf::from(who["result"]["workspace"].as_str().unwrap());
    let (_, rel) = handle
        .strip_prefix("ws://")
        .unwrap()
        .split_once('/')
        .unwrap();
    let file = rel.split('/').fold(ws.clone(), |p, part| p.join(part));
    assert!(file.is_file(), "{}", file.display());
    (handle, ws, file)
}

/// The person's home folder, as the system names it.
fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

/// A file's permissions and modified time, to see that neither changed.
fn marks(p: &Path) -> (bool, u32, std::time::SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    #[cfg(unix)]
    let mode = std::os::unix::fs::PermissionsExt::mode(&m.permissions());
    #[cfg(not(unix))]
    let mode = 0;
    (m.permissions().readonly(), mode, m.modified().unwrap())
}

/// A Python string for a path.
fn py_str(p: &Path) -> String {
    serde_json::to_string(&p.display().to_string()).unwrap()
}

/// What the script, and the program it starts, try. `P` holds the paths,
/// `PORT` a port this computer listens on, `HOME_ESCAPE` a file name.
const ATTEMPTS: &str = r#"
import json, os, socket, sqlite3, subprocess, sys

def attempt(name, f):
    try:
        f()
        print(name + ": ok", flush=True)
    except BaseException as e:
        print(name + ": refused", type(e).__name__, flush=True)

def read(path):
    with open(path, "rb") as f:
        return f.read(4096)

def write(path):
    with open(path, "w") as f:
        f.write("written by a script")

def udp():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.sendto(b"x", ("1.1.1.1", 53))

def ways_out(who, gateway):
    # What it was started with: nothing of the gateway's own may be in it.
    # (PATH is long on some systems and is the person's own: its length only.)
    env = [(k, v if k.upper() != "PATH" else "%d characters" % len(v)) for k, v in os.environ.items()]
    print(who + "environment: " + json.dumps(sorted(env)), flush=True)
    attempt(who + "list_connections", lambda: print(os.listdir(P["connections"])))
    attempt(who + "read_connection", lambda: print(read(P["connection_file"])))
    attempt(who + "read_policy", lambda: print(read(P["policy"])))
    attempt(who + "list_audit", lambda: print(os.listdir(P["audit"])))
    attempt(who + "list_other_session", lambda: print(os.listdir(P["other_ws"])))
    attempt(who + "read_other_session", lambda: print(len(read(P["other_file"]))))
    attempt(who + "list_own_workspace", lambda: print(os.listdir(P["own_ws"])))
    attempt(who + "read_own_results", lambda: print(len(read(P["own_file"]))))
    attempt(who + "list_outside", lambda: print(os.listdir(P["outside"])))
    if P["home"]:
        attempt(who + "list_home", lambda: print(len(os.listdir(P["home"]))))
        attempt(who + "write_home", lambda: write(os.path.join(P["home"], HOME_ESCAPE)))
    if P["home_file"]:
        attempt(who + "read_home_file", lambda: print(read(P["home_file"])))
    if sys.platform.startswith("linux"):
        attempt(who + "gateway_environ", lambda: print(read("/proc/%d/environ" % gateway)))
        attempt(who + "gateway_cmdline", lambda: print(read("/proc/%d/cmdline" % gateway)))
    attempt(who + "tcp_loopback", lambda: socket.create_connection(("127.0.0.1", PORT), timeout=3))
    attempt(who + "tcp_public", lambda: socket.create_connection(("1.1.1.1", 443), timeout=3))
    attempt(who + "udp", udp)
    attempt(who + "resolve", lambda: print(socket.getaddrinfo("example.com", 443)))
    attempt(who + "write_workspace", lambda: write(os.path.join(P["own_ws"], "escape.txt")))
    attempt(who + "write_results", lambda: write(os.path.join(P["own_ws"], "results", "escape.csv")))
    attempt(who + "write_other_session", lambda: write(os.path.join(P["other_ws"], "escape.txt")))
    attempt(who + "write_connections", lambda: write(os.path.join(P["connections"], "escape.yaml")))
    attempt(who + "write_policy", lambda: write(P["policy"]))
    attempt(who + "write_outside", lambda: write(os.path.join(P["outside"], "escape.txt")))
    attempt(who + "delete_connection", lambda: os.remove(P["connection_file"]))
    # Permissions and times: refused, or (Linux) reported done and not done.
    # The test looks at the files afterwards.
    for name in ("policy", "connection_file", "other_file"):
        attempt(who + "chmod_" + name, lambda: os.chmod(P[name], 0o777))
        attempt(who + "utime_" + name, lambda: os.utime(P[name], (1, 1)))

if len(sys.argv) > 1:
    # The program the script started: the same ways out, from one step on.
    ways_out("child ", int(sys.argv[1]))
    sys.exit(0)

gateway = os.getppid()
ways_out("", gateway)
child = subprocess.run([sys.executable, "-I", "-B", __file__, str(gateway)], capture_output=True, text=True)
print(child.stdout, flush=True)
print("child exit", child.returncode, child.stderr[-300:], flush=True)

# What a script is for still works: its inputs, the engine's packages, its
# temporary folder and its outputs.
def inputs():
    paths = json.load(open("inputs.json"))
    db = sqlite3.connect(paths["roads"])
    table = db.execute("SELECT table_name FROM gpkg_contents").fetchone()[0]
    print("input_rows", db.execute(f'SELECT count(*) FROM "{table}"').fetchone()[0], flush=True)
attempt("inputs", inputs)
def engine():
    import geopandas, pandas, pyproj, shapely
    paths = json.load(open("inputs.json"))
    g = geopandas.read_file(paths["roads"])
    print("engine_rows", len(g), flush=True)
    g.head(3).to_file(os.path.join("out", "three.gpkg"), layer="three", driver="GPKG")
attempt("engine", engine)
def temporary():
    folder = os.environ.get("TMPDIR") or os.environ["TEMP"]
    write(os.path.join(folder, "scratch.txt"))
attempt("temporary", temporary)
def output():
    with open(os.path.join("out", "result.csv"), "w") as f:
        f.write("a,b\n1,2\n")
attempt("output", output)
"#;

/// Every way out named in `ATTEMPTS` that must be refused everywhere.
const REFUSED: &[&str] = &[
    "list_connections",
    "read_connection",
    "read_policy",
    "list_audit",
    "list_other_session",
    "read_other_session",
    "list_own_workspace",
    "read_own_results",
    "list_outside",
    "tcp_loopback",
    "tcp_public",
    "udp",
    "write_workspace",
    "write_results",
    "write_other_session",
    "write_connections",
    "write_policy",
    "write_outside",
    "delete_connection",
];

#[tokio::test]
async fn a_script_cannot_cross_the_boundary() {
    let Some(py) = engine() else { return };
    let env = setup(&py, "allow_scripts: true\nscripts: { approval: none }\n");
    // Another session, with a result of its own in its workspace.
    let other = start(&env, &[]).await;
    if !can_confine() {
        // No sandbox here: no script runs (the second test says more).
        let e = err(&call(&other, "run_python", json!({"code": "print(1)"})).await);
        assert!(e.contains("confined"), "{e}");
        other.cancel().await.unwrap();
        // On Linux, where tests must not skip (CI), this one must run.
        assert!(
            !(cfg!(target_os = "linux") && must_not_skip()),
            "COLUMBUS_TEST_REQUIRE_ENGINE=1, so this test must not skip, but this machine cannot \
             confine at level A (Landlock and seccomp are needed)"
        );
        eprintln!("skipping: this machine cannot confine at level A");
        return;
    }
    let (_, other_ws, other_file) = roads(&other).await;
    let c = start(&env, &[]).await;
    let (handle, ws, own_file) = roads(&c).await;
    assert_ne!(ws, other_ws);

    // This computer listens; nothing confined may connect.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    // A file in the person's real home folder, and a name for one there.
    let home = home();
    let home_secret = format!("home-secret-{}", std::process::id());
    let home_file = home.as_ref().and_then(|h| {
        let mut f = tempfile::Builder::new()
            .prefix(".columbus-boundary-test-")
            .suffix(".txt")
            .tempfile_in(h)
            .ok()?;
        std::io::Write::write_all(&mut f, home_secret.as_bytes()).ok()?;
        Some(f)
    });
    if home_file.is_none() {
        eprintln!("note: no file could be made in the home folder; reading one is not checked");
    }
    let home_escape = format!(".columbus-boundary-escape-{}.txt", std::process::id());
    let policy_before = std::fs::read_to_string(env.root.join("policy.yaml")).unwrap();
    // Files outside whose permissions and times must stay as they are.
    let marked: Vec<PathBuf> = vec![
        env.root.join("policy.yaml"),
        env.root.join("connections").join("austin.yaml"),
        other_file.clone(),
    ];
    let marks_before: Vec<_> = marked.iter().map(|p| marks(p)).collect();

    let none = "\"\"".to_string();
    let paths = [
        ("connections", py_str(&env.root.join("connections"))),
        (
            "connection_file",
            py_str(&env.root.join("connections").join("austin.yaml")),
        ),
        ("policy", py_str(&env.root.join("policy.yaml"))),
        ("audit", py_str(&env.root.join("audit"))),
        ("other_ws", py_str(&other_ws)),
        ("other_file", py_str(&other_file)),
        ("own_ws", py_str(&ws)),
        ("own_file", py_str(&own_file)),
        ("outside", py_str(&env.root)),
        ("home", home.as_deref().map_or(none.clone(), py_str)),
        (
            "home_file",
            home_file.as_ref().map_or(none, |f| py_str(f.path())),
        ),
    ];
    // One path a line: a script's lines are at most 200 characters.
    let mut code = String::from("P = {\n");
    for (k, v) in &paths {
        code.push_str(&format!("    \"{k}\":\n        {v},\n"));
    }
    code.push_str(&format!(
        "}}\nPORT = {port}\nHOME_ESCAPE = \"{home_escape}\"\n{ATTEMPTS}"
    ));

    // No approval is asked: the script runs at once.
    let r = ok(&call(
        &c,
        "run_python",
        json!({"code": code, "inputs": {"roads": handle}, "timeout_seconds": 120}),
    )
    .await);
    assert_eq!(r["result"]["status"], "done", "{r}");
    let stdout = r["result"]["stdout"].as_str().unwrap().to_string();
    // Shown when the test fails (and with --nocapture): what each attempt met.
    println!("{stdout}");
    let line = |name: &str| -> String {
        stdout
            .lines()
            .find(|l| l.starts_with(&format!("{name}:")))
            .unwrap_or_else(|| panic!("no line for {name:?} in:\n{stdout}"))
            .to_string()
    };
    for who in ["", "child "] {
        let mut names: Vec<String> = REFUSED.iter().map(|n| format!("{who}{n}")).collect();
        if home.is_some() {
            names.push(format!("{who}list_home"));
            names.push(format!("{who}write_home"));
        }
        if home_file.is_some() {
            names.push(format!("{who}read_home_file"));
        }
        if cfg!(target_os = "linux") {
            names.push(format!("{who}gateway_environ"));
            names.push(format!("{who}gateway_cmdline"));
            // Checked here: a name lookup needs a socket, and there is none.
            names.push(format!("{who}resolve"));
        }
        for name in names {
            let l = line(&name);
            assert!(
                l.starts_with(&format!("{name}: refused")),
                "{l}\n\nall of it:\n{stdout}"
            );
        }
        // Its own environment holds nothing of the gateway's: no setting
        // (where the policy and the connections are) and no secret.
        let started_with: Vec<(String, String)> = serde_json::from_str(
            line(&format!("{who}environment"))
                .split_once(": ")
                .unwrap()
                .1,
        )
        .unwrap();
        let ours: Vec<&str> = started_with
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| k.to_uppercase().starts_with("COLUMBUS_"))
            .collect();
        assert_eq!(ours, ["COLUMBUS_RUN_DIR"], "{who}: {started_with:?}");
        assert!(
            !started_with.iter().any(|(_, v)| v.contains(SECRET)),
            "{who}: {started_with:?}"
        );
        if cfg!(target_os = "linux") {
            // Refused by the sandbox itself, not by a network that is down.
            for name in ["tcp_loopback", "tcp_public", "udp"] {
                let l = line(&format!("{who}{name}"));
                assert!(l.ends_with("refused PermissionError"), "{l}");
            }
        } else {
            // Not checked on this system (a lookup may be answered from a
            // cache, or fail for want of a network): said, not asserted.
            eprintln!(
                "note: {} on {}",
                line(&format!("{who}resolve")),
                std::env::consts::OS
            );
        }
    }
    assert!(stdout.contains("child exit 0"), "{stdout}");
    // What a script is for still works.
    for l in [
        "inputs: ok",
        "input_rows 8",
        "engine: ok",
        "engine_rows 8",
        "temporary: ok",
        "output: ok",
    ] {
        assert!(stdout.lines().any(|x| x == l), "{l} in:\n{stdout}");
    }
    let outs = r["result"]["outputs"].as_array().unwrap();
    let names: Vec<&str> = outs.iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["result.csv", "three.gpkg"], "{r}");
    assert_eq!(outs[1]["summary"]["rows"], 3, "{r}");

    // Nothing it was after came back in what it printed.
    let said = format!("{stdout}{}", r["result"]["stderr"].as_str().unwrap_or(""));
    for secret in [
        SECRET,
        "COLUMBUS_SECRET",
        "district = 14",
        "allow_scripts",
        home_secret.as_str(),
    ] {
        assert!(!said.contains(secret), "{secret:?} in:\n{said}");
    }
    // And nothing outside the run folder changed.
    for made in [
        ws.join("escape.txt"),
        ws.join("results").join("escape.csv"),
        other_ws.join("escape.txt"),
        env.root.join("connections").join("escape.yaml"),
        env.root.join("escape.txt"),
    ] {
        assert!(!made.exists(), "{} was written", made.display());
    }
    if let Some(h) = &home {
        let escaped = h.join(&home_escape);
        let there = escaped.exists();
        let _ = std::fs::remove_file(&escaped);
        assert!(!there, "{} was written", escaped.display());
    }
    assert_eq!(
        std::fs::read_to_string(env.root.join("policy.yaml")).unwrap(),
        policy_before
    );
    assert!(env.root.join("connections").join("austin.yaml").is_file());
    for (p, before) in marked.iter().zip(&marks_before) {
        assert_eq!(
            &marks(p),
            before,
            "the permissions or times of {} changed",
            p.display()
        );
    }
    assert!(
        listener.accept().is_err(),
        "a confined program connected to this computer"
    );
    // The run folder is gone; only the outputs were kept.
    assert!(!ws.join("runs").read_dir().unwrap().any(|_| true));

    c.cancel().await.unwrap();
    other.cancel().await.unwrap();
    // The audit log says which rule let the script run without asking.
    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).unwrap();
    let text = std::fs::read_to_string(&audit).unwrap();
    let runs: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["tool"] == "run_python")
        .collect();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0]["decision"], "running");
    assert_eq!(runs[1]["decision"], "allowed");
    for l in &runs {
        assert_eq!(l["approval"], "scripts.approval: none", "{l}");
    }
    assert!(!text.contains(SECRET));
}

/// Confinement fails closed: with no launcher, or a launcher that cannot
/// set the sandbox up, no script runs, under any approval rule and even
/// where policy lets local servers run unconfined.
#[tokio::test]
async fn where_the_sandbox_cannot_be_set_up_no_script_runs() {
    let Some(py) = engine() else { return };
    let env = setup(
        &py,
        "allow_scripts: true\nscripts: { approval: none }\nservers: { allow_unconfined_local: true }\n",
    );
    let marker = env.root.join("ran-unconfined.txt");
    let code = format!(
        "open({}, \"w\").write(\"ran\")\nprint(\"ran\")\n",
        py_str(&marker)
    );

    // No launcher at all.
    let c = start(
        &env,
        &[("COLUMBUS_CONFINE", env.root.join("no-such-launcher"))],
    )
    .await;
    let e = err(&call(&c, "run_python", json!({"code": code})).await);
    assert!(e.contains("Columbus never runs a script unconfined"), "{e}");
    assert!(!marker.exists());
    c.cancel().await.unwrap();

    // A launcher that answers as the real one does when the system refuses
    // the sandbox: it exits with the launcher's own code and says why.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !can_confine() {
            eprintln!("skipping the failing launcher: this machine cannot confine");
            return;
        }
        let fake = env.root.join("columbus-confine");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'columbus-confine: could not set up confinement: refused (test)' >&2\nexit 97\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let c = start(&env, &[("COLUMBUS_CONFINE", fake)]).await;
        let e = err(&call(&c, "run_python", json!({"code": code})).await);
        assert!(
            e.contains("could not be started confined, so it did not run"),
            "{e}"
        );
        assert!(!marker.exists());
        c.cancel().await.unwrap();
        let audit = std::fs::read_dir(env.root.join("audit"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = std::fs::read_to_string(&audit).unwrap();
        assert!(text.contains("\"decision\":\"denied\""), "{text}");
    }
}
