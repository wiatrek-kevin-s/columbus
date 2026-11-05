//! A running session picks up changed settings files without a restart:
//! connections added or removed, the policy's switches, and broken or
//! restart-only changes, which stop every tool but whoami until fixed. A
//! change set proposed, or an approval given, under older settings is not
//! used under newer ones.
//!
//! The Settings window's Test button asks a running session to try a
//! connection before it is saved, with a key only this account can read.
//!
//! The edit and test cases need Python with geopandas, pyogrio and shapely, and are
//! skipped without it; the rest run anywhere.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ElicitRequestParams,
    ElicitResult, ElicitationAction, ElicitationCapability, FormElicitationCapability,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", "import geopandas, pyogrio, shapely"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

type During = Box<dyn Fn() + Send>;

/// Says yes to every prompt; first runs `during`, as if the person changed
/// the settings while deciding.
#[derive(Clone, Default)]
struct Person {
    asked: Arc<AtomicUsize>,
    during: Arc<std::sync::Mutex<Option<During>>>,
}

impl ClientHandler for Person {
    fn get_info(&self) -> ClientConfig {
        let mut c = ClientConfig::default();
        let mut caps = ClientCapabilities::default();
        let mut el = ElicitationCapability::default();
        el.form = Some(FormElicitationCapability::default());
        caps.elicitation = Some(el);
        c.capabilities = caps;
        c
    }

    async fn create_elicitation(
        &self,
        _request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if let Some(f) = self.during.lock().unwrap().take() {
            f();
        }
        Ok(ElicitResult::new(ElicitationAction::Accept)
            .with_content(json!({"decision": "allow_once"})))
    }
}

async fn start(root: &Path, py: &str, person: Person) -> RunningService<RoleClient, Person> {
    start_with(root, py, person, false).await
}

async fn start_with(
    root: &Path,
    py: &str,
    person: Person,
    companion: bool,
) -> RunningService<RoleClient, Person> {
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
                .env("COLUMBUS_COMPANION", if companion { "on" } else { "off" })
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
        });
    person
        .serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap()
}

async fn call(
    c: &RunningService<RoleClient, Person>,
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

fn conn_yaml(id: &str, path: &Path, extra: &str) -> String {
    let p = serde_json::to_string(&path.display().to_string()).unwrap();
    format!("id: {id}\ntype: gpkg\npath: {p}\ndatasets:\n  allow: [\"*\"]\n{extra}")
}

/// Write a file whole and move it into place, as the Columbus app does,
/// then wait out the moment a session lets a changed file settle.
fn put(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp-write");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
    settle();
}

fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(1200));
}

fn ids(r: &CallToolResult) -> Vec<String> {
    let v = ok(r);
    let list = v["result"]
        .as_array()
        .cloned()
        .or_else(|| v["result"]["connections"].as_array().cloned());
    list.unwrap_or_default()
        .iter()
        .filter_map(|c| c["id"].as_str().map(String::from))
        .collect()
}

fn audit_text(root: &Path) -> String {
    let mut out = String::new();
    for e in std::fs::read_dir(root.join("audit")).unwrap().flatten() {
        out.push_str(&std::fs::read_to_string(e.path()).unwrap_or_default());
    }
    out
}

#[tokio::test]
async fn settings_apply_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let file = root.join("a.gpkg");
    std::fs::write(&file, b"not opened by this test").unwrap();
    let policy = root.join("policy.yaml");
    put(
        &policy,
        "connection_types: [gpkg]\nallow_edits: false   # edits\n",
    );
    put(&root.join("connections/a.yaml"), &conn_yaml("a", &file, ""));
    // Python is not needed here: the worker is never asked.
    let c = start(&root, "python3", Person::default()).await;

    assert_eq!(ids(&call(&c, "list_connections", json!({})).await), ["a"]);

    // A connection added in the Columbus app is there on the next call.
    put(&root.join("connections/b.yaml"), &conn_yaml("b", &file, ""));
    assert_eq!(
        ids(&call(&c, "list_connections", json!({})).await),
        ["a", "b"]
    );

    // One removed is gone at once, for every tool.
    std::fs::remove_file(root.join("connections/a.yaml")).unwrap();
    settle();
    assert_eq!(ids(&call(&c, "list_connections", json!({})).await), ["b"]);
    let e = err(&call(&c, "describe_connection", json!({"connection": "a"})).await);
    assert!(
        e.contains("unknown connection") || e.contains("\\\"a\\\""),
        "{e}"
    );

    // The switches: edits turned on shows at once.
    put(
        &policy,
        "connection_types: [gpkg]\nallow_edits: true   # edits\n",
    );
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["edits_allowed"], json!(true), "{w}");

    // A policy that does not load stops everything but whoami, which says why.
    put(&policy, "connection_types: [gpkg\n");
    let e = err(&call(&c, "list_connections", json!({})).await);
    assert!(e.contains("could not be read"), "{e}");
    let w = ok(&call(&c, "whoami", json!({})).await);
    let notes = w["result"]["notes"].to_string();
    assert!(notes.contains("could not be read"), "{notes}");
    // The last good settings are not used meanwhile: still refused.
    let e = err(&call(&c, "describe_connection", json!({"connection": "b"})).await);
    assert!(e.contains("could not be read"), "{e}");

    // Fixed: back to work, with the fixed file's settings.
    put(&policy, "connection_types: [gpkg]\nallow_edits: false\n");
    assert_eq!(ids(&call(&c, "list_connections", json!({})).await), ["b"]);
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["edits_allowed"], json!(false));

    // A bad connection file stops it too (the server never runs on part of
    // the folder), and so does a duplicate id.
    put(&root.join("connections/c.yaml"), "id: c\ntype: gpkg\n");
    let e = err(&call(&c, "list_connections", json!({})).await);
    assert!(e.contains("could not be read"), "{e}");
    put(&root.join("connections/c.yaml"), &conn_yaml("b", &file, ""));
    let e = err(&call(&c, "list_connections", json!({})).await);
    assert!(e.contains("duplicate"), "{e}");
    std::fs::remove_file(root.join("connections/c.yaml")).unwrap();
    settle();
    assert_eq!(ids(&call(&c, "list_connections", json!({})).await), ["b"]);

    // What only a start applies (connected servers here): refused until
    // undone, never applied halfway.
    put(
        &policy,
        "connection_types: [gpkg]\nservers:\n  allow_personal: true\n",
    );
    let e = err(&call(&c, "list_connections", json!({})).await);
    assert!(e.contains("restarted"), "{e}");
    put(&policy, "connection_types: [gpkg]\n");
    assert_eq!(ids(&call(&c, "list_connections", json!({})).await), ["b"]);

    // A policy file that goes missing is a problem, never the defaults
    // (which hide nothing).
    std::fs::remove_file(&policy).unwrap();
    settle();
    let e = err(&call(&c, "list_connections", json!({})).await);
    assert!(e.contains("is gone"), "{e}");
    put(&policy, "connection_types: [gpkg]\n");
    assert_eq!(ids(&call(&c, "list_connections", json!({})).await), ["b"]);

    // An emptied policy file is refused too.
    put(&policy, "# nothing here any more\n");
    let e = err(&call(&c, "list_connections", json!({})).await);
    assert!(e.contains("is empty"), "{e}");
    put(&policy, "connection_types: [gpkg]\n");
    assert_eq!(ids(&call(&c, "list_connections", json!({})).await), ["b"]);

    // A file dated in the future is used only once the same contents have
    // been seen again a moment later.
    std::fs::write(&policy, "connection_types: [gpkg]\nallow_edits: true\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&policy)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
        .unwrap();
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(
        w["result"]["policy"]["edits_allowed"],
        json!(false),
        "used at first sight"
    );
    settle();
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["edits_allowed"], json!(true));
    put(&policy, "connection_types: [gpkg]\n");
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["edits_allowed"], json!(false));

    // A change is not used until the file has been still for a moment.
    std::fs::write(&policy, "connection_types: [gpkg]\nallow_edits: true\n").unwrap();
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(
        w["result"]["policy"]["edits_allowed"],
        json!(false),
        "used before it settled"
    );
    settle();
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["edits_allowed"], json!(true));

    // Every reload is in the audit log.
    let audit = audit_text(&root);
    assert!(audit.matches("\"settings.reload\"").count() >= 8, "{audit}");
    c.cancel().await.unwrap();
}

/// Five tracts in a GeoPackage, for the edit cases.
fn tracts(py: &str, root: &Path) -> PathBuf {
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["A", "B", "C", "D", "E"]},
    geometry=[box(i * 1000, 0, i * 1000 + 900, 900) for i in range(5)], crs="EPSG:3081"
).to_file(os.path.join(sys.argv[1], "t.gpkg"), layer="tracts", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(root)
        .status()
        .unwrap()
        .success());
    root.join("t.gpkg")
}

#[tokio::test]
async fn edits_are_checked_against_the_settings_in_force() {
    let Some(py) = python() else {
        eprintln!("skipped: needs Python with geopandas, pyogrio and shapely");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = tracts(&py, &root);
    let policy = root.join("policy.yaml");
    put(&policy, "connection_types: [gpkg]\nallow_edits: false\n");
    put(
        &root.join("connections/t.yaml"),
        &conn_yaml("t", &gpkg, "mode: read-write\n"),
    );
    let person = Person::default();
    let c = start(&root, &py, person.clone()).await;

    let propose = || {
        call(
            &c,
            "propose_edits",
            json!({"connection": "t", "dataset": "tracts",
        "changes": [{"op": "update", "key": 1, "values": {"NAME": "One"}}]}),
        )
    };

    // Proposed while edits are off; the person turns them on in Settings.
    let r = ok(&propose().await);
    let old = r["result"]["change"].as_str().unwrap().to_string();
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": old, "target": "source"}),
    )
    .await);
    assert!(e.contains("allow_edits"), "{e}");
    put(&policy, "connection_types: [gpkg]\nallow_edits: true\n");
    // The change set was checked against the old settings: proposed again.
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": old, "target": "source"}),
    )
    .await);
    assert!(e.contains("propose the edits again"), "{e}");

    // The settings change while the person is deciding (scripts turned on,
    // which narrows nothing): the yes is not used.
    let r = ok(&propose().await);
    let change = r["result"]["change"].as_str().unwrap().to_string();
    let p2 = policy.clone();
    *person.during.lock().unwrap() = Some(Box::new(move || {
        put(
            &p2,
            "connection_types: [gpkg]\nallow_edits: true\nallow_scripts: true\n",
        );
    }));
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert!(
        e.contains("settings changed while the person was deciding"),
        "{e}"
    );
    assert_eq!(person.asked.load(Ordering::SeqCst), 1);

    // The person narrows the connection while deciding: the call's answer
    // is withheld, and nothing is written.
    let r = ok(&propose().await);
    let change = r["result"]["change"].as_str().unwrap().to_string();
    let t = root.join("connections/t.yaml");
    let narrowed = conn_yaml(
        "t",
        &gpkg,
        "mode: read-write\nrow_filter: \"NAME <> 'Z'\"\n",
    );
    *person.during.lock().unwrap() = Some(Box::new(move || put(&t, &narrowed)));
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert!(e.contains("narrowed"), "{e}");
    assert!(audit_text(&root).contains("narrowed while the call ran"));

    // Under unchanged settings it goes through.
    let r = ok(&propose().await);
    let change = r["result"]["change"].as_str().unwrap().to_string();
    let r = ok(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert_eq!(r["result"]["status"], json!("written"), "{r}");

    // Edits turned off: refused at once, no restart.
    put(&policy, "connection_types: [gpkg]\nallow_edits: false\n");
    let r = ok(&propose().await);
    let change = r["result"]["change"].as_str().unwrap().to_string();
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert!(e.contains("allow_edits"), "{e}");
    c.cancel().await.unwrap();
}

/// The running session's port and test key, once it has announced itself.
async fn announced(root: &Path) -> (u16, String) {
    let live = root.join("live");
    for _ in 0..200 {
        if let Ok(entries) = std::fs::read_dir(&live) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let (Ok(text), Ok(key)) = (
                    std::fs::read_to_string(&p),
                    std::fs::read_to_string(p.with_extension("key")),
                ) else {
                    continue;
                };
                let v: Value = serde_json::from_str(&text).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(p.with_extension("key"))
                        .unwrap()
                        .permissions()
                        .mode();
                    assert_eq!(
                        mode & 0o077,
                        0,
                        "the key file is readable by others: {mode:o}"
                    );
                }
                return (v["port"].as_u64().unwrap() as u16, key);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the session did not announce itself");
}

/// Ask for a challenge, check the session's proof, then send `body` with
/// the app's proof, as the Columbus app does. `key` None sends no proof.
async fn send(port: u16, body: &Value, key: Option<&str>, origin: Option<&str>) -> (u16, Value) {
    use columbus_shared::proof;
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let mine = "ab".repeat(32);
    // Only a key holder can ask; without one, sign with a wrong key.
    let signer = key.unwrap_or("not-the-key");
    let hello_sig = proof::prove(signer, proof::HELLO, &[mine.as_bytes()]);
    let mut hello = http
        .post(format!("{base}/api/test-hello"))
        .header("X-Columbus-Test-Hello", format!("{mine}.{hello_sig}"));
    if let Some(o) = origin {
        hello = hello.header("Origin", o);
    }
    let r = hello.send().await.unwrap();
    if r.status() != 200 {
        return (r.status().as_u16(), Value::Null);
    }
    let h: Value = r.json().await.unwrap();
    let challenge = h["challenge"].as_str().unwrap().to_string();
    let bytes = serde_json::to_vec(body).unwrap();
    let mut req = http
        .post(format!("{base}/api/test-connection"))
        .header("Content-Type", "application/json")
        .body(bytes.clone());
    if let Some(k) = key {
        // The session proves it holds the key before anything is sent.
        let want = proof::prove(k, proof::SERVER, &[mine.as_bytes()]);
        assert!(proof::same(h["proof"].as_str().unwrap(), &want));
        let sig = proof::prove(
            k,
            proof::APP,
            &[challenge.as_bytes(), proof::body_hash(&bytes).as_bytes()],
        );
        req = req.header("X-Columbus-Test", format!("{challenge}.{sig}"));
    }
    if let Some(o) = origin {
        req = req.header("Origin", o);
    }
    let r = req.send().await.unwrap();
    let code = r.status().as_u16();
    (code, r.json::<Value>().await.unwrap_or(Value::Null))
}

fn spec(id: &str, path: &Path, allow: &str) -> Value {
    json!({
        "id": id, "type": "gpkg", "path": path.display().to_string(), "mode": "read",
        "datasets": {"allow": [allow], "deny": []},
    })
}

#[tokio::test]
async fn the_settings_window_can_test_a_connection_before_saving_it() {
    let Some(py) = python() else {
        eprintln!("skipped: needs Python with geopandas, pyogrio and shapely");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = tracts(&py, &root);
    put(&root.join("policy.yaml"), "connection_types: [gpkg]\n");
    let c = start_with(&root, &py, Person::default(), true).await;
    let _ = call(&c, "list_connections", json!({})).await;
    let (port, key) = announced(&root).await;
    let k = Some(key.as_str());

    // A GeoPackage that is not saved anywhere yet.
    let (code, v) = send(
        port,
        &json!({"connection": spec("new", &gpkg, "*")}),
        k,
        None,
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(v["message"].as_str().unwrap().contains("tracts"), "{v}");
    assert!(!root.join("connections/new.yaml").exists());

    // Its dataset lists are applied as the session would.
    let (_, v) = send(
        port,
        &json!({"connection": spec("new", &gpkg, "roads*")}),
        k,
        None,
    )
    .await;
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(v["message"].as_str().unwrap().contains("allow none"), "{v}");

    // What the organization's policy refuses is refused here too.
    let pg = json!({"connection": {"id": "pg", "type": "postgis", "host": "db.example.org",
        "database": "gis", "mode": "read", "datasets": {"allow": ["*"]}}});
    let (_, v) = send(port, &pg, k, None).await;
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("would refuse"),
        "{v}"
    );

    // A file that does not open says so.
    let missing = json!({"connection": spec("bad", &root.join("missing.gpkg"), "*")});
    let (_, v) = send(port, &missing, k, None).await;
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("could not open"),
        "{v}"
    );

    // Without the key's proof, or from any web page: refused.
    let good = json!({"connection": spec("new", &gpkg, "*")});
    assert_eq!(send(port, &good, None, None).await.0, 403);
    let own = format!("http://127.0.0.1:{port}");
    assert_eq!(send(port, &good, k, Some(&own)).await.0, 403);
    assert_eq!(
        send(port, &good, k, Some("https://example.org")).await.0,
        403
    );
    // Asking for a challenge without the key is refused.
    let r = reqwest::Client::new()
        .post(format!("{own}/api/test-hello"))
        .header("X-Columbus-Test-Hello", "ef".repeat(32))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // A proof for one body does not cover another, and a challenge is
    // good once.
    {
        use columbus_shared::proof;
        let http = reqwest::Client::new();
        let h: Value = http
            .post(format!("{own}/api/test-hello"))
            .header(
                "X-Columbus-Test-Hello",
                format!(
                    "{}.{}",
                    "cd".repeat(32),
                    proof::prove(&key, proof::HELLO, &["cd".repeat(32).as_bytes()])
                ),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let ch = h["challenge"].as_str().unwrap();
        let signed = serde_json::to_vec(&good).unwrap();
        let sig = proof::prove(
            &key,
            proof::APP,
            &[ch.as_bytes(), proof::body_hash(&signed).as_bytes()],
        );
        let other =
            serde_json::to_vec(&json!({"connection": spec("new", &gpkg, "roads*")})).unwrap();
        let post = |body: Vec<u8>| {
            http.post(format!("{own}/api/test-connection"))
                .header("X-Columbus-Test", format!("{ch}.{sig}"))
                .body(body)
                .send()
        };
        assert_eq!(post(other).await.unwrap().status(), 403);
        assert_eq!(
            post(signed).await.unwrap().status(),
            403,
            "a challenge is spent by its first use"
        );
    }
    // The live files are this account's alone.
    #[cfg(unix)]
    for e in std::fs::read_dir(root.join("live")).unwrap().flatten() {
        use std::os::unix::fs::PermissionsExt;
        let mode = e.metadata().unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "{} is readable by others: {mode:o}",
            e.path().display()
        );
    }

    // Every test is in the audit log, from the app, never as the AI's call.
    let audit = audit_text(&root);
    assert!(
        audit.matches("\"settings.test_connection\"").count() >= 4,
        "{audit}"
    );
    assert!(audit.contains("columbus-app (settings)"), "{audit}");

    // The key goes when the session does.
    c.cancel().await.unwrap();
    let mut gone = false;
    for _ in 0..100 {
        let left = std::fs::read_dir(root.join("live"))
            .map(|d| {
                d.flatten()
                    .any(|e| e.path().extension().and_then(|x| x.to_str()) == Some("key"))
            })
            .unwrap_or(false);
        if !left {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(gone, "the key file outlived its session");
}

#[tokio::test]
async fn narrowing_a_connection_removes_what_was_read_from_it() {
    let Some(py) = python() else {
        eprintln!("skipped: needs Python with geopandas, pyogrio and shapely");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = tracts(&py, &root);
    let policy = root.join("policy.yaml");
    put(&policy, "connection_types: [gpkg]\n");
    put(&root.join("connections/t.yaml"), &conn_yaml("t", &gpkg, ""));
    put(&root.join("connections/u.yaml"), &conn_yaml("u", &gpkg, ""));
    let c = start(&root, &py, Person::default()).await;
    let query = |conn: &'static str| {
        call(
            &c,
            "query_features",
            json!({"connection": conn, "dataset": "tracts"}),
        )
    };
    let layer = |r: CallToolResult| {
        ok(&r)["result"]["layer"]["handle"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let read = |h: String| call(&c, "read_output", json!({"handle": h}));
    // Read from u first: a file carries every label the session had when it
    // was made, so one made after reading t counts as t's data too.
    let from_u = layer(query("u").await);
    let from_t = layer(query("t").await);
    ok(&read(from_t.clone()).await);

    // The person narrows t (a dataset refused) in Settings: what was read
    // from t is gone; what came from u is not.
    let narrowed = conn_yaml("t", &gpkg, "").replace(
        "datasets:\n  allow: [\"*\"]\n",
        "datasets:\n  allow: [\"*\"]\n  deny: [tracts]\n",
    );
    put(&root.join("connections/t.yaml"), &narrowed);
    err(&read(from_t).await);
    ok(&read(from_u.clone()).await);
    assert!(audit_text(&root).contains("no longer allow their data"));

    // A newly hidden field takes away everything read from any connection.
    put(
        &policy,
        "connection_types: [gpkg]\nhide_fields: [\"*secret*\"]\n",
    );
    err(&read(from_u).await);

    // A description change or a switch turned takes nothing away.
    let fresh = layer(query("u").await);
    put(
        &root.join("connections/u.yaml"),
        &conn_yaml("u", &gpkg, "description: the same data\n"),
    );
    put(
        &policy,
        "connection_types: [gpkg]\nallow_scripts: true\nhide_fields: [\"*secret*\"]\n",
    );
    ok(&read(fresh).await);
    c.cancel().await.unwrap();
}
