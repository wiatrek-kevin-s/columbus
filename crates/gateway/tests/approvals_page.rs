//! The local approvals page, end to end. The gateway runs as a real binary
//! with a client that cannot show prompts; the test plays the person's
//! browser over HTTP, and also tries what a hostile page or agent would.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::{Duration, Instant};

use columbus_broker::registry::Broker;
use columbus_broker::review;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::StatusCode;
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

fn fake_server() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let p = dir.join(format!("fake_mcp_server{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.exists(),
        "build the example first: cargo build --example fake_mcp_server"
    );
    p
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn launch_file(&self) -> PathBuf {
        self.root.join("launch.txt")
    }
}

async fn setup(wait_seconds: u64) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in ["connections", "servers", "personal"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [gpkg]\nservers: {{ allow_unconfined_local: true }}\napprovals: {{ page: true, wait_seconds: {wait_seconds} }}\n"
        ),
    )
    .unwrap();
    let manifest = root.join("servers").join("vault.yaml");
    std::fs::write(
        &manifest,
        format!(
            "id: vault\nname: Test vault\ntransport: {{ type: stdio, command: {}, args: [plain] }}\ntrust: standard\ntools:\n  export_report: {{ risk: read, output: to_workspace }}\n  upload: {{ risk: write, egress: true, args: {{ content: file_handle }}, approval: always }}\n  attach: {{ risk: write, egress: true, args: {{ path: path }}, approval: always }}\n  default: {{ allow: false }}\n",
            serde_json::to_string(&fake_server().display().to_string()).unwrap()
        ),
    )
    .unwrap();
    // Approve the server's tool definitions, as `servers approve` would.
    let policy = columbus_shared::policy::OrgPolicy::load(&root.join("policy.yaml")).unwrap();
    let ws = root.join("review");
    std::fs::create_dir_all(&ws).unwrap();
    let b = Broker::load(
        &policy,
        &root.join("servers"),
        &root.join("personal"),
        &ws,
        Arc::new(|_: &str| Ok(None)),
        &[],
    )
    .unwrap();
    let p = b.get("vault").unwrap().clone();
    let c = b.connected(&p).await.map_err(|s| format!("{s:?}")).unwrap();
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str(
        &serde_yaml::to_string(&json!({"pinned": review::pin(&c.review, None)})).unwrap(),
    );
    std::fs::write(&manifest, text).unwrap();
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
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PERSONAL_SERVERS", root.join("personal"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_APPROVALS_LAUNCH_FILE", &launch)
                .env("COLUMBUS_LOG", "warn");
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

async fn wait_for_launch(env: &Env) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(u) = std::fs::read_to_string(env.launch_file()) {
            if !u.is_empty() {
                std::fs::remove_file(env.launch_file()).unwrap();
                return u;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the approvals page was never opened"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn no_cookie(r: &reqwest::Response) {
    assert!(
        r.headers().get("set-cookie").is_none(),
        "the approvals page never sets cookies"
    );
}

/// The person's browser tab: loads the page shell, then (as its script
/// does) trades the launch token from the URL fragment for a session token.
struct Browser {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Browser {
    async fn open(launch: &str) -> Browser {
        let http = http();
        let (base, launch_token) = launch
            .split_once("/#t=")
            .expect("launch link is /#t=<token>");
        assert!(base.starts_with("http://127.0.0.1:"), "{launch}");
        let base = base.to_string();

        // The shell holds no secrets and needs none.
        let r = http.get(format!("{base}/")).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        no_cookie(&r);
        let csp = r
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            csp.contains("script-src 'nonce-") && csp.contains("frame-ancestors 'none'"),
            "{csp}"
        );
        assert_eq!(r.headers().get("x-frame-options").unwrap(), "DENY");
        let html = r.text().await.unwrap();
        assert!(!html.contains(launch_token));

        let r = exchange(&http, &base, launch_token, &base).await;
        assert_eq!(r.status(), StatusCode::OK);
        no_cookie(&r);
        let token = r.json::<Value>().await.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(token, launch_token);
        Browser { base, token, http }
    }

    async fn pending(&self) -> Vec<Value> {
        let r = self
            .http
            .get(format!("{}/api/approvals", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        no_cookie(&r);
        r.json::<Value>().await.unwrap()["pending"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn wait_pending(&self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(a) = self.pending().await.into_iter().next() {
                return a;
            }
            assert!(Instant::now() < deadline, "nothing appeared on the page");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", self.token)).unwrap(),
        );
        h.insert("origin", HeaderValue::from_str(&self.base).unwrap());
        h.insert("sec-fetch-site", HeaderValue::from_static("same-origin"));
        h
    }

    async fn decide_with(&self, id: &str, decision: &str, headers: HeaderMap) -> StatusCode {
        let r = self
            .http
            .post(format!("{}/api/approvals/{id}", self.base))
            .headers(headers)
            .json(&json!({"decision": decision}))
            .send()
            .await
            .unwrap();
        no_cookie(&r);
        r.status()
    }

    async fn decide(&self, id: &str, decision: &str) {
        assert_eq!(
            self.decide_with(id, decision, self.headers()).await,
            StatusCode::OK
        );
    }
}

async fn exchange(
    http: &reqwest::Client,
    base: &str,
    launch: &str,
    origin: &str,
) -> reqwest::Response {
    http.post(format!("{base}/api/session"))
        .header("origin", origin)
        .header("sec-fetch-site", "same-origin")
        .json(&json!({"launch": launch}))
        .send()
        .await
        .unwrap()
}

/// Path of a workspace handle on disk.
fn handle_path(workspace: &Path, handle: &str) -> PathBuf {
    let (_session, rel) = handle
        .strip_prefix("ws://")
        .unwrap()
        .split_once('/')
        .unwrap();
    workspace.join(rel)
}

#[tokio::test]
async fn the_person_approves_and_declines_on_the_page() {
    let Some(py) = python() else { return };
    let env = setup(20).await;
    let c = Arc::new(start(&env, &py).await);

    let who = ok(&call(&c, "whoami", json!({})).await);
    assert!(who["result"]["approvals"]
        .as_str()
        .unwrap()
        .contains("approvals page"));
    let report = ok(&call(&c, "run_tool", json!({"name": "vault.export_report"})).await);
    let handle = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let upload = json!({"name": "vault.upload", "args": {"name": "r.json", "content": handle}});

    // The agent's call waits while the person decides on the page.
    let c2 = c.clone();
    let args = upload.clone();
    let running = tokio::spawn(async move { call(&c2, "run_tool", args).await });
    let launch = wait_for_launch(&env).await;
    let browser = Browser::open(&launch).await;

    // The launch token works once, only through the exchange, and only from the page's own origin.
    let launch_token = launch.split_once("/#t=").unwrap().1;
    assert_eq!(
        exchange(&http(), &browser.base, launch_token, &browser.base)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // Without a session token there is nothing to see.
    let api = format!("{}/api/approvals", browser.base);
    assert_eq!(
        http().get(&api).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http()
            .get(&api)
            .bearer_auth(launch_token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED,
        "a launch token is not a session"
    );
    assert_eq!(
        http()
            .get(&api)
            .bearer_auth("0".repeat(64))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let a = browser.wait_pending().await;
    assert!(a["summary"].as_str().unwrap().contains("Test vault"), "{a}");
    assert_eq!(a["detail"]["destination"], "Test vault");
    assert_eq!(a["request"]["name"], "vault.upload");
    let id = a["id"].as_str().unwrap().to_string();

    // Requests a hostile page or another program could make are refused.
    let mut h = browser.headers();
    h.remove("authorization");
    assert_eq!(
        browser.decide_with(&id, "allow_once", h).await,
        StatusCode::UNAUTHORIZED,
        "no session token"
    );
    let mut h = browser.headers();
    h.insert("authorization", HeaderValue::from_static("Bearer guess"));
    assert_eq!(
        browser.decide_with(&id, "allow_once", h).await,
        StatusCode::UNAUTHORIZED,
        "guessed token"
    );
    let mut h = browser.headers();
    h.insert("cookie", HeaderValue::from_static("columbus_1=anything"));
    h.remove("authorization");
    assert_eq!(
        browser.decide_with(&id, "allow_once", h).await,
        StatusCode::UNAUTHORIZED,
        "cookies are not accepted at all"
    );
    let mut h = browser.headers();
    h.insert("origin", HeaderValue::from_static("https://evil.example"));
    assert_eq!(
        browser.decide_with(&id, "allow_once", h).await,
        StatusCode::FORBIDDEN,
        "cross-origin"
    );
    let mut h = browser.headers();
    h.remove("origin");
    assert_eq!(
        browser.decide_with(&id, "allow_once", h).await,
        StatusCode::FORBIDDEN,
        "no origin"
    );
    let mut h = browser.headers();
    h.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
    assert_eq!(
        browser.decide_with(&id, "allow_once", h).await,
        StatusCode::FORBIDDEN,
        "cross-site"
    );
    let mut h = browser.headers();
    h.insert(
        "host",
        HeaderValue::from_static("columbus.attacker.example"),
    );
    assert_eq!(
        browser.decide_with(&id, "allow_once", h).await,
        StatusCode::FORBIDDEN,
        "DNS rebinding"
    );
    assert!(!running.is_finished(), "nothing above approved it");

    browser.decide(&id, "allow_once").await;
    let r = running.await.unwrap();
    let r = ok(&r);
    assert_eq!(r["result"]["status"], "done");
    assert!(
        r["result"]["external"]["structured"]["bytes"]
            .as_u64()
            .unwrap()
            > 0
    );

    // Decline: the page is already open, so no new tab; the request appears there.
    let c2 = c.clone();
    let args = upload.clone();
    let running = tokio::spawn(async move { call(&c2, "run_tool", args).await });
    let a = browser.wait_pending().await;
    assert!(
        !env.launch_file().exists(),
        "an open page is reused, not a new tab"
    );
    browser.decide(a["id"].as_str().unwrap(), "decline").await;
    let e = err(&running.await.unwrap());
    assert!(e.contains("declined"), "{e}");
    // A decided approval cannot be decided again.
    assert_eq!(
        browser
            .decide_with(a["id"].as_str().unwrap(), "allow_once", browser.headers())
            .await,
        StatusCode::CONFLICT
    );

    Arc::try_unwrap(c).ok().unwrap().cancel().await.unwrap();
    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).unwrap();
    let text = std::fs::read_to_string(&audit).unwrap();
    assert_eq!(
        text.matches("\"client\":\"approvals-page\"").count(),
        2,
        "both decisions audited"
    );
    let token = launch.rsplit('/').next().unwrap();
    assert!(
        !text.contains(token) && !text.contains("/open/"),
        "the launch link is never logged"
    );
}

#[tokio::test]
async fn an_approval_given_after_the_call_returned_can_be_used() {
    let Some(py) = python() else { return };
    let env = setup(1).await;
    let c = start(&env, &py).await;
    let report = ok(&call(&c, "run_tool", json!({"name": "vault.export_report"})).await);
    let handle = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let upload = json!({"name": "vault.upload", "args": {"name": "r.json", "content": handle}});

    let started = Instant::now();
    let r = ok(&call(&c, "run_tool", upload.clone()).await);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(r["result"]["status"], "approval_required");
    let text = r.to_string();
    assert!(
        !text.contains("127.0.0.1") && !text.contains("/open/"),
        "the agent never gets the page's address: {text}"
    );
    assert!(r["result"]["approval"]["next"]
        .as_str()
        .unwrap()
        .contains("approvals page"));
    let id = r["result"]["approval"]["id"].as_str().unwrap().to_string();

    let browser = Browser::open(&wait_for_launch(&env).await).await;
    // The agent cannot spend an approval nobody gave.
    let mut early = upload.clone();
    early["approval_id"] = json!(id);
    assert!(err(&call(&c, "run_tool", early.clone()).await).contains("still waiting for a person"));
    browser.decide(&id, "allow_once").await;
    let st = ok(&call(&c, "approval_status", json!({"id": id})).await);
    assert_eq!(st["result"]["state"], "approved");

    // The approval covers the file as it was, not whatever the handle points at later.
    let who = ok(&call(&c, "whoami", json!({})).await);
    let file = handle_path(
        Path::new(who["result"]["workspace"].as_str().unwrap()),
        &handle,
    );
    let original = std::fs::read(&file).unwrap();
    std::fs::write(&file, b"{\"swapped\": true}").unwrap();
    assert!(
        err(&call(&c, "run_tool", early.clone()).await).contains("granted for a different request")
    );
    std::fs::write(&file, &original).unwrap();
    let r = ok(&call(&c, "run_tool", early.clone()).await);
    assert_eq!(r["result"]["status"], "done");
    // Single use.
    assert!(err(&call(&c, "run_tool", early).await).contains("already used"));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn policy_can_turn_the_page_off() {
    let Some(py) = python() else { return };
    let env = setup(5).await;
    let p = env.root.join("policy.yaml");
    let text = std::fs::read_to_string(&p)
        .unwrap()
        .replace("page: true", "page: false");
    std::fs::write(&p, text).unwrap();
    let c = start(&env, &py).await;
    let report = ok(&call(&c, "run_tool", json!({"name": "vault.export_report"})).await);
    let handle = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let r = ok(&call(
        &c,
        "run_tool",
        json!({"name": "vault.upload", "args": {"name": "x", "content": handle}}),
    )
    .await);
    assert_eq!(r["result"]["status"], "approval_required");
    assert!(r["result"]["approval"]["next"]
        .as_str()
        .unwrap()
        .contains("turns the Columbus approvals page off"));
    assert!(!env.launch_file().exists());
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn allow_for_this_session_covers_only_the_same_contents() {
    let Some(py) = python() else { return };
    let env = setup(20).await;
    let c = Arc::new(start(&env, &py).await);
    let report = ok(&call(&c, "run_tool", json!({"name": "vault.export_report"})).await);
    let handle = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let upload = json!({"name": "vault.upload", "args": {"name": "r.json", "content": handle}});

    let c2 = c.clone();
    let args = upload.clone();
    let running = tokio::spawn(async move { call(&c2, "run_tool", args).await });
    let browser = Browser::open(&wait_for_launch(&env).await).await;
    let a = browser.wait_pending().await;
    browser
        .decide(a["id"].as_str().unwrap(), "always_this_session")
        .await;
    assert_eq!(ok(&running.await.unwrap())["result"]["status"], "done");

    // The same request again: no question.
    let r = ok(&call(&c, "run_tool", upload.clone()).await);
    assert_eq!(r["result"]["status"], "done");
    assert!(browser.pending().await.is_empty());

    // Same handle, different contents: asked again.
    let who = ok(&call(&c, "whoami", json!({})).await);
    let file = handle_path(
        Path::new(who["result"]["workspace"].as_str().unwrap()),
        &handle,
    );
    std::fs::write(&file, b"{\"rows\": \"different data\"}").unwrap();
    let c2 = c.clone();
    let args = upload.clone();
    let running = tokio::spawn(async move { call(&c2, "run_tool", args).await });
    let a = browser.wait_pending().await;
    let file_row = &a["detail"]["files"][0];
    assert!(
        file_row["name"].as_str().unwrap().contains("external"),
        "{a}"
    );
    assert!(
        a["request"]["files"][0]["sha256"].is_string(),
        "the card's exact request names the contents: {a}"
    );
    browser.decide(a["id"].as_str().unwrap(), "decline").await;
    assert!(err(&running.await.unwrap()).contains("declined"));
    Arc::try_unwrap(c).ok().unwrap().cancel().await.unwrap();
}

#[tokio::test]
async fn identical_requests_merge_and_a_flood_is_refused() {
    let Some(py) = python() else { return };
    let env = setup(0).await;
    let c = start(&env, &py).await;
    let report = ok(&call(&c, "run_tool", json!({"name": "vault.export_report"})).await);
    let handle = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let upload = |n: usize| json!({"name": "vault.upload", "args": {"name": format!("r{n}.json"), "content": handle}});

    let first = ok(&call(&c, "run_tool", upload(0)).await);
    let again = ok(&call(&c, "run_tool", upload(0)).await);
    assert_eq!(
        first["result"]["approval"]["id"], again["result"]["approval"]["id"],
        "one request, one approval"
    );
    for n in 1..20 {
        assert_eq!(
            ok(&call(&c, "run_tool", upload(n)).await)["result"]["status"],
            "approval_required"
        );
    }
    let e = err(&call(&c, "run_tool", upload(20)).await);
    assert!(e.contains("already waiting for the person"), "{e}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_file_changed_while_waiting_for_approval_is_not_sent() {
    let Some(py) = python() else { return };
    let env = setup(20).await;
    let c = Arc::new(start(&env, &py).await);
    let report = ok(&call(&c, "run_tool", json!({"name": "vault.export_report"})).await);
    let handle = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let who = ok(&call(&c, "whoami", json!({})).await);
    let file = handle_path(
        Path::new(who["result"]["workspace"].as_str().unwrap()),
        &handle,
    );
    let attach = json!({"name": "vault.attach", "args": {"path": handle}});

    // A tool that reads the path itself: the file is swapped while the person decides.
    let c2 = c.clone();
    let args = attach.clone();
    let running = tokio::spawn(async move { call(&c2, "run_tool", args).await });
    let browser = Browser::open(&wait_for_launch(&env).await).await;
    let a = browser.wait_pending().await;
    std::fs::write(&file, b"{\"swapped\": \"while waiting\"}").unwrap();
    browser
        .decide(a["id"].as_str().unwrap(), "allow_once")
        .await;
    let e = err(&running.await.unwrap());
    assert!(e.contains("changed after it was checked"), "{e}");

    // Workspace files go to a server only as handles (copied into its own
    // folder once allowed), never as raw paths into the workspace.
    // As a person would type it (no \\?\ prefix on Windows).
    let ws = who["result"]["workspace"]
        .as_str()
        .unwrap()
        .trim_start_matches(r"\\?\")
        .to_string();
    let e = err(&call(
        &c,
        "run_tool",
        json!({"name": "vault.attach", "args": {"path": Path::new(&ws).join("external").display().to_string()}}),
    )
    .await);
    assert!(e.contains("ws:// handle"), "{e}");

    // Unchanged: approved and sent exactly.
    let c2 = c.clone();
    let args = attach.clone();
    let running = tokio::spawn(async move { call(&c2, "run_tool", args).await });
    let a = browser.wait_pending().await;
    browser
        .decide(a["id"].as_str().unwrap(), "allow_once")
        .await;
    let r = ok(&running.await.unwrap());
    let sent = std::fs::read(&file).unwrap();
    assert_eq!(r["result"]["external"]["structured"]["bytes"], sent.len());

    // A second browser tab given a used link gets nothing.
    let launch_again = format!("{}/#t={}", browser.base, "0".repeat(64));
    let (_, t) = launch_again.split_once("/#t=").unwrap();
    assert_eq!(
        exchange(&http(), &browser.base, t, &browser.base)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    Arc::try_unwrap(c).ok().unwrap().cancel().await.unwrap();
}
