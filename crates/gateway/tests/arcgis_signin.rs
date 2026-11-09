//! ArcGIS sign-in (T-11; build brief Part 4, D-4, REQ-10, F-3b, R-17,
//! R-18) end to end: the real `columbus-mcp arcgis signin svc` against
//! `examples/fake_oauth_server.rs` as the portal and a signed-in service,
//! with the test playing the person's browser; then the gateway reading
//! with the stored tokens.
//!
//! The portal is `http://localhost:<port>` and the service
//! `http://127.0.0.1:<port>/arcgis/rest/services/Secure/FeatureServer`:
//! one fake, two host names, so whether the portal vouches for the
//! service's host (R-18) is the fake's `FAKE_PORTAL_SERVERS` alone.
//!
//! Fake tokens contain `zq7k`; no audit line, provenance file, workspace
//! file or tool result may.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const MARK: &str = "zq7k";

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

fn python() -> Option<String> {
    std::env::var("COLUMBUS_PYTHON")
        .ok()
        .into_iter()
        .chain(["python3".to_string(), "python".to_string()])
        .find(|p| {
            Command::new(p)
                .args([
                    "-c",
                    "import geopandas, pyogrio, pyproj, shapely; assert hasattr(shapely, 'has_m')",
                ])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

/// The fake portal, and its port.
struct Fake(Child, u16);
impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

impl Fake {
    fn start(servers: &str) -> Fake {
        let mut c = Command::new(example("fake_oauth_server"));
        c.stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("FAKE_PORTAL_SERVERS", servers);
        let mut child = c.spawn().unwrap();
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

    async fn log(&self) -> Vec<Value> {
        reqwest::get(self.url("/test/log"))
            .await
            .unwrap()
            .json::<Vec<Value>>()
            .await
            .unwrap()
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

/// The token a logged request carried, if any.
fn sent_token(e: &Value) -> Option<String> {
    ["authorization", "x_esri_authorization"]
        .iter()
        .find_map(|k| e[*k].as_str())
        .map(|v| v.trim_start_matches("Bearer ").to_string())
}

fn service_requests(log: &[Value]) -> Vec<&Value> {
    log.iter()
        .filter(|e| {
            e["target"]
                .as_str()
                .unwrap()
                .starts_with("/arcgis/rest/services/Secure/")
        })
        .collect()
}

/// Calls to the token endpoint (after sign-in, every one is a refresh).
fn token_calls(log: &[Value]) -> usize {
    log.iter()
        .filter(|e| e["target"] == "/sharing/rest/oauth2/token")
        .count()
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(port: u16) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [arcgis]\nhosts: [\"127.0.0.1\", \"localhost\"]\n\
         limits: { rows: 1000, seconds: 120 }\napprovals: { page: false }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("svc.yaml"),
        format!(
            "id: svc\ntype: arcgis\nurl: http://127.0.0.1:{port}/arcgis/rest/services/Secure/FeatureServer\n\
             auth: {{type: oauth, portal: \"http://localhost:{port}\", client_id: FakeClientId123}}\n\
             description: Secured assets (fake)\ndatasets:\n  allow: [\"*\"]\n"
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
}

fn envs(root: &Path) -> Vec<(String, String)> {
    let p = |s: &str| root.join(s).display().to_string();
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let mut v = vec![
        ("COLUMBUS_POLICY".into(), p("policy.yaml")),
        ("COLUMBUS_CONNECTIONS".into(), p("connections")),
        ("COLUMBUS_SERVERS".into(), p("servers")),
        ("COLUMBUS_PERSONAL_SERVERS".into(), p("personal-servers")),
        ("COLUMBUS_PLUGINS".into(), p("plugins")),
        ("COLUMBUS_WORKSPACES".into(), p("workspaces")),
        ("COLUMBUS_AUDIT_DIR".into(), p("audit")),
        ("COLUMBUS_DEV_SECRETS".into(), "1".into()),
        ("COLUMBUS_DEV_SECRETS_DIR".into(), p("secrets")),
        ("COLUMBUS_FETCH_TEST_LOOPBACK".into(), "1".into()),
        ("COLUMBUS_APPROVALS_BROWSER".into(), "none".into()),
        ("COLUMBUS_SIGNIN_BROWSER".into(), "print".into()),
        ("COLUMBUS_WORKER".into(), worker.display().to_string()),
        ("COLUMBUS_LOG".into(), "warn".into()),
    ];
    if let Some(py) = python() {
        v.push(("COLUMBUS_PYTHON".into(), py));
    }
    v
}

/// The stored entry (the development store keeps it as a file).
fn entry_path(root: &Path) -> PathBuf {
    root.join("secrets").join("COLUMBUS_SECRET_ARCGIS_SVC")
}

fn entry(root: &Path) -> Option<Value> {
    std::fs::read_to_string(entry_path(root))
        .ok()
        .map(|t| serde_json::from_str(&t).unwrap())
}

/// How the test plays the browser.
#[derive(Clone, Copy, PartialEq)]
enum Browser {
    /// Follow the authorize link and its redirect back to the listener.
    Follow,
    /// The out-of-band page: read the code and paste it into the terminal.
    Paste,
    /// Ignore the first link (the redirect never comes back), then paste
    /// the code from the second.
    IgnoreThenPaste,
}

/// Run `columbus-mcp arcgis signin svc` with `args`, playing the browser.
async fn signin(
    root: &Path,
    args: &[&str],
    browser: Browser,
    extra: &[(&str, &str)],
) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(["arcgis", "signin", "svc"])
        .args(args)
        .stdin(if browser == Browser::Follow {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs(root) {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut out = String::new();
    let mut links = 0;
    for line in lines.by_ref() {
        let line = line.unwrap();
        out.push_str(&line);
        out.push('\n');
        let Some(url) = line.strip_prefix("open: ") else {
            continue;
        };
        links += 1;
        if browser == Browser::IgnoreThenPaste && links == 1 {
            assert!(
                url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"),
                "{url}"
            );
            continue;
        }
        let r = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        let page = r.text().await.unwrap_or_default();
        if browser == Browser::Follow {
            // (The out-of-band page shows the code, which is the test's
            // to paste, not the terminal's to print.)
            out.push_str(&format!("[browser] {page}\n"));
        } else {
            assert!(
                url.contains("redirect_uri=urn%3Aietf%3Awg%3Aoauth%3A2.0%3Aoob"),
                "{url}"
            );
            let code = page
                .split("<code id=\"code\">")
                .nth(1)
                .and_then(|r| r.split("</code>").next())
                .unwrap_or_else(|| panic!("no code on the page: {page}"));
            let mut w = stdin.take().unwrap();
            writeln!(w, "{code}").unwrap();
        }
        break;
    }
    for line in lines {
        out.push_str(&line.unwrap());
        out.push('\n');
    }
    drop(stdin);
    let status = child.wait().unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    (status.success(), format!("{out}{err}"))
}

fn cli(root: &Path, args: &[&str], extra: &[(&str, &str)], drop_vars: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(args).stdin(Stdio::null());
    for (k, v) in envs(root) {
        if !drop_vars.contains(&k.as_str()) {
            cmd.env(k, v);
        }
    }
    for k in drop_vars {
        cmd.env_remove(k);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let o = cmd.output().unwrap();
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

async fn start(root: &Path) -> RunningService<RoleClient, ()> {
    let vars = envs(root);
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

/// Every tool result, kept for the final search for tokens.
struct Calls<'a> {
    c: &'a RunningService<RoleClient, ()>,
    seen: Vec<String>,
}

impl Calls<'_> {
    async fn call(&mut self, tool: &'static str, args: Value) -> CallToolResult {
        let mut p = CallToolRequestParams::new(tool);
        if let Value::Object(m) = args {
            p = p.with_arguments(m);
        }
        let r = self.c.call_tool(p).await.unwrap();
        self.seen.push(serde_json::to_string(&r).unwrap());
        r
    }
}

fn ok(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().unwrap()
}

fn err(r: &CallToolResult) -> String {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected an error: {:?}",
        r.structured_content
    );
    serde_json::to_string(&r.content).unwrap()
}

fn card(who: &Value) -> Value {
    who["result"]["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "svc")
        .unwrap()
        .clone()
}

/// Every file under `dir` whose text contains the fake tokens' mark.
fn files_with_mark(dir: &Path) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return hits;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            hits.extend(files_with_mark(&p));
        } else if std::fs::read(&p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(MARK)) {
            hits.push(p);
        }
    }
    hits
}

#[tokio::test]
async fn sign_in_queries_with_a_bearer_refreshes_once_and_asks_again_when_lapsed() {
    // The portal lists the service's server as federated (R-18).
    let fake = Fake::start("http://127.0.0.1:{port}/arcgis");
    let env = setup(fake.1);
    let root = env.root.as_path();

    // Before sign-in: nothing is sent to the service, and the error says
    // how to sign in.
    let c = start(root).await;
    let mut calls = Calls {
        c: &c,
        seen: vec![],
    };
    let who = ok(&calls.call("whoami", json!({})).await);
    assert_eq!(card(&who)["signed_in"], false, "{who}");
    let e = err(&calls
        .call("list_datasets", json!({"connection": "svc"}))
        .await);
    assert!(e.contains("Sign in: columbus-mcp arcgis signin svc"), "{e}");
    assert!(service_requests(&fake.log().await).is_empty());
    let mut seen = std::mem::take(&mut calls.seen);
    c.cancel().await.unwrap();

    // Sign in (loopback redirect, A-2's default): the F-3b entry is stored,
    // nothing secret is printed.
    let (signed, out) = signin(root, &[], Browser::Follow, &[]).await;
    assert!(signed, "sign-in failed:\n{out}");
    assert!(out.contains("Signed in as fake_analyst for svc"), "{out}");
    assert!(out.contains("You can close this tab"), "{out}");
    assert!(
        out.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"),
        "the loopback redirect: {out}"
    );
    assert!(out.contains("code_challenge_method=S256"), "{out}");
    assert!(!out.contains(MARK), "no token or code is printed:\n{out}");
    let stored = entry(root).expect("sign-in stores arcgis:svc");
    let mut keys: Vec<&String> = stored.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["access_token", "expires_at", "portal", "refresh_token"]
    );
    assert_eq!(stored["portal"], format!("http://localhost:{}", fake.1));
    let first = stored["access_token"].as_str().unwrap().to_string();
    assert!(first.contains(MARK));

    // Queries carry the bearer token, and only to the service and portal.
    let c = start(root).await;
    let mut calls = Calls {
        c: &c,
        seen: vec![],
    };
    let who = ok(&calls.call("whoami", json!({})).await);
    assert_eq!(card(&who)["signed_in"], true, "{who}");
    let before = fake.log().await.len();
    let list = ok(&calls
        .call("list_datasets", json!({"connection": "svc"}))
        .await);
    assert_eq!(list["result"]["datasets"][0]["name"], "Assets", "{list}");
    let d = ok(&calls
        .call(
            "describe_dataset",
            json!({"connection": "svc", "dataset": "Assets"}),
        )
        .await);
    assert_eq!(d["result"]["rows"], 2, "{d}");
    let engine = python().is_some();
    if engine {
        let q = ok(&calls
            .call(
                "query_features",
                json!({"connection": "svc", "dataset": "Assets"}),
            )
            .await);
        assert_eq!(q["result"]["rows_read"], 2, "{q}");
    } else {
        eprintln!("skipping query_features: the GIS engine is not installed");
    }
    let log = fake.log().await;
    let sent = service_requests(&log[before..]);
    assert!(!sent.is_empty());
    for e in &sent {
        assert_eq!(sent_token(e).as_deref(), Some(first.as_str()), "{e}");
        assert!(
            !e["target"].as_str().unwrap().contains(MARK),
            "never in the URL: {e}"
        );
    }
    if engine {
        assert!(sent.iter().any(|e| e["target"]
            .as_str()
            .unwrap()
            .contains("/Secure/FeatureServer/0/query")
            && !e["target"].as_str().unwrap().contains("returnCountOnly")));
    }
    assert_eq!(token_calls(&log[before..]), 0, "no refresh yet");

    // An expired access token is refreshed once, without asking.
    fake.post("/test/expire-access").await;
    let mut e = entry(root).unwrap();
    e["expires_at"] = json!("2020-01-01T00:00:00Z");
    std::fs::write(entry_path(root), e.to_string()).unwrap();
    let before = fake.log().await.len();
    let d = ok(&calls
        .call(
            "describe_dataset",
            json!({"connection": "svc", "dataset": "Assets"}),
        )
        .await);
    assert_eq!(d["result"]["rows"], 2, "{d}");
    let log = fake.log().await;
    assert_eq!(token_calls(&log[before..]), 1, "one refresh: {log:?}");
    let renewed = entry(root).unwrap();
    let second = renewed["access_token"].as_str().unwrap().to_string();
    assert_ne!(second, first);
    assert_eq!(renewed["refresh_token"], stored["refresh_token"]);
    assert!(renewed["expires_at"].as_str().unwrap() > "2026");
    for e in service_requests(&log[before..]) {
        assert_eq!(sent_token(e).as_deref(), Some(second.as_str()), "{e}");
    }
    // The next call uses it without refreshing again.
    let before = fake.log().await.len();
    ok(&calls
        .call(
            "describe_dataset",
            json!({"connection": "svc", "dataset": "Assets"}),
        )
        .await);
    assert_eq!(token_calls(&fake.log().await[before..]), 0);

    // A lapsed refresh token: the plain message, word for word.
    fake.post("/test/lapse").await;
    let mut e = entry(root).unwrap();
    e["expires_at"] = json!("2020-01-01T00:00:00Z");
    std::fs::write(entry_path(root), e.to_string()).unwrap();
    let before = fake.log().await.len();
    let e = err(&calls
        .call(
            "describe_dataset",
            json!({"connection": "svc", "dataset": "Assets"}),
        )
        .await);
    assert!(
        e.contains("Sign in again: columbus-mcp arcgis signin svc"),
        "{e}"
    );
    let log = fake.log().await;
    assert_eq!(token_calls(&log[before..]), 1, "{log:?}");
    assert!(service_requests(&log[before..]).is_empty(), "{log:?}");
    seen.extend(std::mem::take(&mut calls.seen));
    c.cancel().await.unwrap();

    // Acceptance check 2: no token in the audit log, provenance, workspace
    // files or any tool result.
    for r in &seen {
        assert!(!r.contains(MARK), "a token reached a tool result: {r}");
    }
    let leaks: Vec<PathBuf> = ["audit", "workspaces", "connections"]
        .iter()
        .flat_map(|d| files_with_mark(&root.join(d)))
        .collect();
    assert!(leaks.is_empty(), "tokens written to {leaks:?}");
    assert!(
        root.join("audit").read_dir().unwrap().next().is_some(),
        "the audit log was written"
    );

    // Sign out removes the entry; whoami says so.
    let (gone, out) = cli(root, &["arcgis", "signout", "svc"], &[], &[]);
    assert!(gone && out.contains("Signed out"), "{out}");
    assert!(entry(root).is_none());
    let c = start(root).await;
    let mut calls = Calls {
        c: &c,
        seen: vec![],
    };
    let who = ok(&calls.call("whoami", json!({})).await);
    assert_eq!(card(&who)["signed_in"], false, "{who}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn the_paste_code_fallback_signs_in_and_is_offered_when_the_browser_does_not_return() {
    let fake = Fake::start("");
    let env = setup(fake.1);
    let root = env.root.as_path();

    // A-2's fallback, asked for.
    let (signed, out) = signin(root, &["--paste-code"], Browser::Paste, &[]).await;
    assert!(signed, "{out}");
    assert!(out.contains("never into a chat"), "{out}");
    assert!(out.contains("Signed in"), "{out}");
    assert!(!out.contains(MARK), "{out}");
    let first = entry(root).unwrap();

    // Offered by itself when the loopback redirect never arrives.
    let (signed, out) = signin(
        root,
        &[],
        Browser::IgnoreThenPaste,
        &[("COLUMBUS_SIGNIN_WAIT_SECS", "2")],
    )
    .await;
    assert!(signed, "{out}");
    assert!(out.contains("did not come back"), "{out}");
    assert!(!out.contains(MARK), "{out}");
    assert_ne!(entry(root).unwrap()["access_token"], first["access_token"]);

    // A redirect with someone else's state is refused, nothing stored.
    let before = entry(root).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(["arcgis", "signin", "svc"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs(root) {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    for line in lines.by_ref() {
        let line = line.unwrap();
        if let Some(url) = line.strip_prefix("open: ") {
            let u = reqwest::Url::parse(url).unwrap();
            let redirect = u
                .query_pairs()
                .find(|(k, _)| k == "redirect_uri")
                .unwrap()
                .1
                .into_owned();
            let r = reqwest::get(format!("{redirect}/?code=forged&state=not-yours"))
                .await
                .unwrap();
            let _ = r.text().await;
            break;
        }
    }
    let status = child.wait().unwrap();
    let mut err = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert!(!status.success());
    assert!(err.contains("did not match this sign-in"), "{err}");
    assert_eq!(entry(root).unwrap(), before);

    // Deleting the connection deletes its sign-in (F-3b), as a running
    // session sees the connection gone.
    let c = start(root).await;
    let mut calls = Calls {
        c: &c,
        seen: vec![],
    };
    let who = ok(&calls.call("whoami", json!({})).await);
    assert_eq!(card(&who)["signed_in"], true, "{who}");
    std::fs::remove_file(root.join("connections").join("svc.yaml")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    ok(&calls.call("list_connections", json!({})).await);
    assert!(
        entry(root).is_none(),
        "the sign-in went with the connection"
    );
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_service_the_portal_does_not_vouch_for_never_gets_the_token() {
    // R-18: the portal lists another server, not 127.0.0.1.
    let fake = Fake::start("https://gis.example.org/server");
    let env = setup(fake.1);
    let root = env.root.as_path();
    let (signed, out) = signin(root, &[], Browser::Follow, &[]).await;
    assert!(signed, "{out}");
    let token = entry(root).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_string();

    let c = start(root).await;
    let mut calls = Calls {
        c: &c,
        seen: vec![],
    };
    let before = fake.log().await.len();
    let e = err(&calls
        .call("list_datasets", json!({"connection": "svc"}))
        .await);
    assert!(e.contains("the portal does not vouch for 127.0.0.1"), "{e}");
    let e = err(&calls
        .call(
            "describe_dataset",
            json!({"connection": "svc", "dataset": "Assets"}),
        )
        .await);
    assert!(e.contains("the portal does not vouch for 127.0.0.1"), "{e}");
    let seen = std::mem::take(&mut calls.seen);
    c.cancel().await.unwrap();

    let log = fake.log().await;
    // The portal was asked which servers it vouches for (with the token:
    // it is the portal) ...
    assert!(log[before..].iter().any(|e| e["target"]
        .as_str()
        .unwrap()
        .starts_with("/sharing/rest/portals/self/servers")
        && e["host"].as_str().unwrap().starts_with("localhost")));
    // ... and the service got the request without it.
    let sent = service_requests(&log[before..]);
    assert!(!sent.is_empty(), "{log:?}");
    for e in sent {
        assert!(sent_token(e).is_none(), "a token went to the service: {e}");
        assert!(!e.to_string().contains(&token), "{e}");
    }
    for r in &seen {
        assert!(!r.contains(MARK), "{r}");
    }
}

#[tokio::test]
async fn without_a_credential_store_sign_in_is_refused() {
    // R-17: the development mode with no folder keeps nothing, like a
    // system with no credential store. (On Linux the release default is
    // the same: no native store.)
    let fake = Fake::start("");
    let env = setup(fake.1);
    let root = env.root.as_path();
    let (refused, out) = cli(
        root,
        &["arcgis", "signin", "svc"],
        &[],
        &["COLUMBUS_DEV_SECRETS_DIR"],
    );
    assert!(!refused, "{out}");
    assert!(out.contains("no credential store on this system"), "{out}");
    assert!(!root.join("secrets").exists(), "nothing written");
    assert!(fake.log().await.is_empty(), "nothing asked of the portal");

    // An unknown connection is named plainly.
    let (refused, out) = cli(root, &["arcgis", "signin", "nope"], &[], &[]);
    assert!(!refused && out.contains("no connection"), "{out}");
}
