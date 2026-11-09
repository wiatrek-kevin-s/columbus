//! Environments Columbus creates (T-7; build brief Part 5 "Creating one",
//! REQ-14, F-7 `environments.index_hosts`): the assistant proposes one with
//! `propose_environment`; only exact pins of real package names are filed;
//! the person approves it at the keyboard; Columbus's own pinned uv (in the
//! engine, from requirements-uv.txt) creates it under the envs folder,
//! wheels only, reaching only the index hosts, and registers it governed.
//!
//! The person's approval is the library call the app makes after Touch ID
//! or Windows Hello, and the terminal after they typed the id
//! (`columbus_shared::env_create::approve` takes the fingerprint they were
//! shown and what they typed), as workflows.rs approves workflows with
//! `columbus_shared::workflows::install`.
//!
//! `an_approved_proposal_creates_a_governed_environment` needs PyPI: it
//! skips without it, and fails instead with COLUMBUS_TEST_REQUIRE_NETWORK=1
//! (set in CI). `creation_reaches_only_the_index_hosts_and_never_builds`
//! uses a package index this test serves on 127.0.0.1, with no network.
//! Both need the GIS engine (skipped otherwise) with uv installed in it.

mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use columbus_shared::env_create as create;
use columbus_shared::environments::{self as envs, CreatedBy, Trust};
use columbus_shared::policy::EnvironmentsPolicy;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const NOW: &str = "2026-11-08T10:00:00-06:00";
const REQUIRE_NETWORK: &str = "COLUMBUS_TEST_REQUIRE_NETWORK";

fn engine() -> Option<String> {
    let py = support::python_for("import geopandas, pyogrio, shapely, pyproj");
    if py.is_none() {
        eprintln!("skipping: the GIS engine is not installed");
    }
    py
}

fn required() -> bool {
    std::env::var(REQUIRE_NETWORK).is_ok_and(|v| v == "1")
}

/// The engine's uv; without it the test skips, or fails where CI requires
/// these tests to run.
fn engine_with_uv() -> Option<String> {
    let py = engine()?;
    let info = create::engine_info(Path::new(&py)).unwrap();
    if create::engine_uv(&info).is_err() {
        let why = format!(
            "uv is not installed in the engine ({}); install workers/python/requirements-uv.txt",
            info.executable
        );
        assert!(!required(), "{REQUIRE_NETWORK}=1 but {why}");
        eprintln!("skipping: {why}");
        return None;
    }
    Some(py)
}

fn pypi_reachable() -> bool {
    let ok = ("pypi.org", 443).to_socket_addrs().is_ok_and(|mut a| {
        a.any(|a| TcpStream::connect_timeout(&a, Duration::from_secs(10)).is_ok())
    });
    if !ok {
        assert!(
            !required(),
            "{REQUIRE_NETWORK}=1 but pypi.org:443 cannot be reached"
        );
        eprintln!("skipping: no network to pypi.org");
    }
    ok
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn environments(&self) -> PathBuf {
        self.root.join("environments")
    }
    fn envs(&self) -> PathBuf {
        self.root.join("envs")
    }
}

fn setup(policy: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    // A real path: macOS's temp folder is behind a link.
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [gpkg]\n{policy}"),
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
                .env("COLUMBUS_APPROVALS_LAUNCH_FILE", root.join("launch.txt"))
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

async fn propose(
    c: &RunningService<RoleClient, ()>,
    id: &str,
    packages: &[&str],
) -> CallToolResult {
    call(
        c,
        "propose_environment",
        json!({"id": id, "packages": packages, "reason": "Test environment for the T-7 checks"}),
    )
    .await
}

async fn whoami_envs(c: &RunningService<RoleClient, ()>) -> Vec<Value> {
    let w = ok(&call(c, "whoami", json!({})).await);
    w["result"]["environments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// What the person does at the keyboard: look at the proposal, approve it.
async fn approve(
    env: &Env,
    py: &str,
    policy: EnvironmentsPolicy,
    id: &str,
) -> Result<envs::Environment, String> {
    let (dir, root, py, id) = (
        env.environments(),
        env.envs(),
        PathBuf::from(py),
        id.to_string(),
    );
    tokio::task::spawn_blocking(move || {
        let (_, fp) = create::read_proposal(&dir, &id)?;
        create::approve(&dir, &root, &policy, &py, &id, &fp, &id, NOW)
    })
    .await
    .unwrap()
}

fn policy_with(hosts: &[&str]) -> EnvironmentsPolicy {
    EnvironmentsPolicy {
        allow: true,
        index_hosts: hosts.iter().map(|h| h.to_string()).collect(),
        ..Default::default()
    }
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

/// Check 1: a proposal with one exact-pinned, wheel-only package
/// (six==1.17.0), approved, is created under the envs folder and
/// registered governed, created by Columbus; whoami lists it. With the
/// files host left out of index_hosts, creation fails naming it.
#[tokio::test]
async fn an_approved_proposal_creates_a_governed_environment() {
    let Some(py) = engine_with_uv() else { return };
    if !pypi_reachable() {
        return;
    }
    let env = setup("allow_scripts: true\nenvironments: { allow: true }\n");
    let c = start(&env, &py).await;

    let r = ok(&propose(&c, "six-env", &["six==1.17.0"]).await);
    assert_eq!(r["result"]["status"], "waiting_for_person", "{r}");
    let fp = r["result"]["fingerprint"].as_str().unwrap().to_string();
    // Nothing exists before the person approves, and no tool approves.
    assert!(!env.envs().join("six-env").exists());
    assert!(whoami_envs(&c).await.iter().all(|e| e["id"] != "six-env"));
    for name in ["approve_environment", "create_environment"] {
        let mut p = CallToolRequestParams::new(name.to_string());
        p = p.with_arguments(json!({"id": "six-env"}).as_object().unwrap().clone());
        let r = c.call_tool(p).await;
        assert!(
            r.is_err() || r.is_ok_and(|r| r.is_error == Some(true)),
            "{name}"
        );
    }
    let policy = policy_with(&["pypi.org", "files.pythonhosted.org"]);
    // What the person approves is what they were shown, and they confirm.
    let e = create::approve(
        &env.environments(),
        &env.envs(),
        &policy,
        Path::new(&py),
        "six-env",
        "sha256:00",
        "six-env",
        NOW,
    )
    .unwrap_err();
    assert!(e.contains("changed after you looked"), "{e}");
    let e = create::approve(
        &env.environments(),
        &env.envs(),
        &policy,
        Path::new(&py),
        "six-env",
        &fp,
        "",
        NOW,
    )
    .unwrap_err();
    assert!(e.contains("not confirmed"), "{e}");
    assert!(!env.envs().join("six-env").exists());

    let made = approve(&env, &py, policy.clone(), "six-env").await.unwrap();
    assert_eq!(made.trust, Trust::Governed);
    assert_eq!(made.created_by, CreatedBy::Columbus);
    let folder = std::fs::canonicalize(env.envs().join("six-env")).unwrap();
    assert!(
        Path::new(&made.python).starts_with(&folder),
        "{} not under {}",
        made.python,
        folder.display()
    );
    assert!(folder.join("pyvenv.cfg").is_file());
    assert!(site_packages(&folder).join("six.py").is_file());
    assert!(create::read_proposal(&env.environments(), "six-env").is_err());
    let listed = whoami_envs(&c).await;
    let e = listed
        .iter()
        .find(|e| e["id"] == "six-env")
        .unwrap_or_else(|| panic!("six-env not listed: {listed:?}"));
    assert_eq!(e["status"], "ready", "{e}");
    assert_eq!(e["trust"], "governed", "{e}");
    assert_eq!(e["created_by"], "columbus", "{e}");

    // pypi.org's files are on files.pythonhosted.org: without it in
    // index_hosts, creation fails naming it, and nothing is left behind.
    ok(&propose(&c, "six-b", &["six==1.17.0"]).await);
    let e = approve(&env, &py, policy_with(&["pypi.org"]), "six-b")
        .await
        .unwrap_err();
    assert!(e.contains("files.pythonhosted.org"), "{e}");
    assert!(e.contains("not in environments.index_hosts"), "{e}");
    assert!(!env.envs().join("six-b").exists());
    assert!(!env.environments().join("six-b.yaml").exists());
    c.cancel().await.unwrap();
}

/// A package index on 127.0.0.1, as a mirror would be: `nowheel` has only
/// a source archive (whose setup.py would leave a marker if it ever ran),
/// and `elsewhere` links its wheel on another host (localhost).
struct Index {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

fn serve_index(files: PathBuf) -> Index {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && s.read(&mut b).is_ok_and(|n| n == 1) {
                head.push(b[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let path = head.split(' ').nth(1).unwrap_or("").to_string();
            log.lock().unwrap().push(path.clone());
            let page = |links: &str| {
                format!("<!DOCTYPE html><html><body>{links}</body></html>").into_bytes()
            };
            let (status, ctype, body) = match path.as_str() {
                "/simple/nowheel/" => (
                    "200 OK",
                    "text/html",
                    page("<a href=\"/files/nowheel-1.0.tar.gz\">nowheel-1.0.tar.gz</a>"),
                ),
                "/simple/elsewhere/" => (
                    "200 OK",
                    "text/html",
                    page(&format!("<a href=\"http://localhost:{port}/files/elsewhere-1.0-py3-none-any.whl\">elsewhere-1.0-py3-none-any.whl</a>")),
                ),
                p if p.starts_with("/files/") => {
                    match std::fs::read(files.join(&p["/files/".len()..])) {
                        Ok(b) => ("200 OK", "application/octet-stream", b),
                        Err(_) => ("404 Not Found", "text/plain", b"no".to_vec()),
                    }
                }
                _ => ("404 Not Found", "text/plain", b"no".to_vec()),
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
        }
    });
    Index { port, seen }
}

/// The index's two files, made with the engine's Python.
fn make_index_files(py: &str, files: &Path, marker: &Path) {
    let script = r#"
import io, os, sys, tarfile, zipfile
files, marker = sys.argv[1], sys.argv[2]
setup = "open(%r, 'w').write('built')\nfrom setuptools import setup\nsetup(name='nowheel', version='1.0')\n" % marker
with tarfile.open(os.path.join(files, "nowheel-1.0.tar.gz"), "w:gz") as t:
    for name, text in [("nowheel-1.0/setup.py", setup),
                       ("nowheel-1.0/PKG-INFO", "Metadata-Version: 2.1\nName: nowheel\nVersion: 1.0\n")]:
        data = text.encode()
        info = tarfile.TarInfo(name)
        info.size = len(data)
        t.addfile(info, io.BytesIO(data))
with zipfile.ZipFile(os.path.join(files, "elsewhere-1.0-py3-none-any.whl"), "w") as z:
    z.writestr("elsewhere/__init__.py", "")
    z.writestr("elsewhere-1.0.dist-info/METADATA", "Metadata-Version: 2.1\nName: elsewhere\nVersion: 1.0\n")
    z.writestr("elsewhere-1.0.dist-info/WHEEL", "Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n")
    z.writestr("elsewhere-1.0.dist-info/RECORD", "")
"#;
    let out = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(files)
        .arg(marker)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Check 2: an unpinned package is refused at proposal; a package whose
/// file is on a host outside index_hosts fails the creation naming the
/// host; a package with no wheel is refused and nothing is built from
/// source. Deterministic: the index is served here, on 127.0.0.1, and the
/// other host is localhost, refused by Columbus's proxy before any lookup.
#[tokio::test]
async fn creation_reaches_only_the_index_hosts_and_never_builds() {
    let Some(py) = engine_with_uv() else { return };
    let env = setup("allow_scripts: true\nenvironments: { allow: true }\n");
    let files = env.root.join("index-files");
    std::fs::create_dir_all(&files).unwrap();
    let marker = env.root.join("built-from-source.txt");
    make_index_files(&py, &files, &marker);
    let index = serve_index(files);
    let host = format!("127.0.0.1:{}", index.port);
    std::fs::write(
        env.root.join("policy.yaml"),
        format!("connection_types: [gpkg]\nallow_scripts: true\nenvironments: {{ allow: true, index_hosts: [\"{host}\"] }}\n"),
    )
    .unwrap();
    let c = start(&env, &py).await;

    // Unpinned, ranged, URL and VCS requirements are refused at proposal.
    for (spec, says) in [
        ("nowheel", "not an exact pin"),
        ("nowheel>=1.0", "not an exact pin"),
        ("nowheel==1.*", "not an exact pin"),
        (
            "nowheel @ https://evil.example/nowheel-1.0.tar.gz",
            "evil.example",
        ),
        ("git+https://github.com/x/nowheel.git", "github.com"),
        ("../nowheel", "not an exact pin"),
    ] {
        let e = err(&propose(&c, "bad", &[spec]).await);
        assert!(e.contains(says), "{spec}: {e}");
    }
    let e = err(&propose(&c, "bad", &["no wheel==1.0"]).await);
    assert!(e.contains("not a package name"), "{e}");
    assert!(create::read_proposal(&env.environments(), "bad").is_err());

    let policy = policy_with(&[&host]);
    // A file on another host: the creation fails naming it.
    ok(&propose(&c, "elsewhere", &["elsewhere==1.0"]).await);
    let e = approve(&env, &py, policy.clone(), "elsewhere")
        .await
        .unwrap_err();
    assert!(e.contains(&format!("localhost:{}", index.port)), "{e}");
    assert!(e.contains("not in environments.index_hosts"), "{e}");
    assert!(!env.envs().join("elsewhere").exists());
    assert!(!env.environments().join("elsewhere.yaml").exists());
    // The proposal stays for the person to decline.
    assert!(create::read_proposal(&env.environments(), "elsewhere").is_ok());
    create::decline(&env.environments(), "elsewhere").unwrap();

    // Only a source archive: refused, and its build code never runs.
    ok(&propose(&c, "nowheel", &["nowheel==1.0"]).await);
    let e = approve(&env, &py, policy.clone(), "nowheel")
        .await
        .unwrap_err();
    assert!(e.contains("no wheel for this system"), "{e}");
    assert!(!marker.exists(), "setup.py ran");
    assert!(!env.envs().join("nowheel").exists());
    assert!(!env.environments().join("nowheel.yaml").exists());
    let seen = index.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|p| p == "/simple/nowheel/"), "{seen:?}");
    assert!(whoami_envs(&c).await.is_empty());
    c.cancel().await.unwrap();
}

/// With environments.allow off, nothing can be proposed.
#[tokio::test]
async fn with_environments_off_nothing_is_proposed() {
    let Some(py) = engine() else { return };
    let env = setup("allow_scripts: true\n");
    let c = start(&env, &py).await;
    let e = err(&propose(&c, "six-env", &["six==1.17.0"]).await);
    assert!(e.contains("environments.allow"), "{e}");
    assert!(create::list_proposals(&env.environments()).is_empty());
    c.cancel().await.unwrap();
}
