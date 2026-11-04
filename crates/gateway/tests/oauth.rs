//! OAuth sign-in for remote servers (design change 1, section 8), end to
//! end: the real `columbus-mcp servers signin` against
//! `examples/fake_oauth_server.rs`, then the gateway connecting with the
//! stored tokens.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

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

/// The fake server, and the base URL it printed.
struct Fake(Child, String);
impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn fake(env: &[(&str, &str)]) -> Fake {
    let mut c = Command::new(example("fake_oauth_server"));
    c.stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c.spawn().unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let base = line.trim().strip_prefix("listening ").unwrap().to_string();
    Fake(child, base)
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(base: &str, scopes: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("servers")).unwrap();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nhosts: [\"127.0.0.1\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("servers").join("remote.yaml"),
        format!(
            "id: remote\nname: Remote files\ntransport: {{ type: http, url: \"{base}/mcp\" }}\n\
             auth: {{ type: oauth, scopes: [{scopes}] }}\ntrust: standard\ntools:\n  hello: {{ risk: read }}\n"
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
}

fn envs(root: &Path) -> Vec<(String, String)> {
    let p = |s: &str| root.join(s).display().to_string();
    vec![
        ("COLUMBUS_POLICY".into(), p("policy.yaml")),
        ("COLUMBUS_CONNECTIONS".into(), p("connections")),
        ("COLUMBUS_SERVERS".into(), p("servers")),
        ("COLUMBUS_PERSONAL_SERVERS".into(), p("personal-servers")),
        ("COLUMBUS_PLUGINS".into(), p("plugins")),
        ("COLUMBUS_WORKSPACES".into(), p("workspaces")),
        ("COLUMBUS_AUDIT_DIR".into(), p("audit")),
        ("COLUMBUS_DEV_SECRETS".into(), "1".into()),
        ("COLUMBUS_DEV_SECRETS_DIR".into(), p("secrets")),
        ("COLUMBUS_APPROVALS_BROWSER".into(), "none".into()),
        ("COLUMBUS_LOG".into(), "warn".into()),
    ]
}

/// Run `servers signin remote`, playing the person's browser: follow the
/// printed link (the fake server approves at once and redirects back).
async fn signin(env: &Env) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(["servers", "signin", "remote"])
        .env("COLUMBUS_SIGNIN_BROWSER", "print")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs(&env.root) {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let mut out = String::new();
    for line in lines.by_ref() {
        let line = line.unwrap();
        out.push_str(&line);
        out.push('\n');
        if let Some(url) = line.strip_prefix("open: ") {
            let url = url.to_string();
            // The browser: follows the redirect to the CLI's listener.
            let r = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .unwrap()
                .get(&url)
                .send()
                .await
                .unwrap();
            let page = r.text().await.unwrap_or_default();
            out.push_str(&format!("[browser] {page}\n"));
            break;
        }
    }
    for line in lines {
        out.push_str(&line.unwrap());
        out.push('\n');
    }
    let status = child.wait().unwrap();
    let mut err = String::new();
    use std::io::Read;
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    (status.success(), format!("{out}{err}"))
}

async fn start(env: &Env) -> RunningService<RoleClient, ()> {
    let vars = envs(&env.root);
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

fn remote(lp: &CallToolResult) -> Value {
    lp.structured_content.clone().unwrap()["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "remote")
        .unwrap()
        .clone()
}

#[tokio::test]
async fn sign_in_stores_tokens_and_the_gateway_uses_them() {
    let server = fake(&[]);
    let env = setup(&server.1, "files.read");

    // Before sign-in: the server waits for it, and nothing is sent.
    let c = start(&env).await;
    let p = remote(&call(&c, "list_providers", json!({})).await);
    assert_eq!(p["status"]["state"], "sign_in_required", "{p}");
    assert!(p["status"]["reason"]
        .as_str()
        .unwrap()
        .contains("servers signin remote"));
    c.cancel().await.unwrap();

    let (ok, out) = signin(&env).await;
    assert!(ok, "sign-in failed:\n{out}");
    assert!(out.contains("Signed in"), "{out}");
    assert!(
        out.contains("[browser] ") && out.contains("You can close this tab"),
        "{out}"
    );
    assert!(!out.contains("access-"), "tokens are never printed:\n{out}");

    // The gateway connects with the stored token: the server is reachable
    // (trial mode until a person approves its tools).
    let c = start(&env).await;
    let p = remote(&call(&c, "list_providers", json!({})).await);
    assert_eq!(p["status"]["state"], "trial", "{p}");
    let who =
        serde_json::to_string(&call(&c, "whoami", json!({})).await.structured_content).unwrap();
    assert!(
        !who.contains("access-") && !who.contains("refresh-"),
        "tokens never reach the agent"
    );
    c.cancel().await.unwrap();
    let audit = std::fs::read_dir(env.root.join("audit")).unwrap();
    for f in audit.flatten() {
        let text = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            !text.contains("access-") && !text.contains("refresh-"),
            "tokens never reach the audit log"
        );
    }

    // Sign out forgets the tokens.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(["servers", "signout", "remote"]);
    for (k, v) in envs(&env.root) {
        cmd.env(k, v);
    }
    assert!(cmd.status().unwrap().success());
    let c = start(&env).await;
    let p = remote(&call(&c, "list_providers", json!({})).await);
    assert_eq!(p["status"]["state"], "sign_in_required", "{p}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_grant_wider_than_the_manifest_is_discarded() {
    let server = fake(&[("FAKE_OAUTH_GRANT", "files.read files.write")]);
    let env = setup(&server.1, "files.read");
    let (ok, out) = signin(&env).await;
    assert!(!ok, "should fail:\n{out}");
    assert!(
        out.contains("files.write") && out.contains("discarded"),
        "{out}"
    );
    let c = start(&env).await;
    let p = remote(&call(&c, "list_providers", json!({})).await);
    assert_eq!(
        p["status"]["state"], "sign_in_required",
        "nothing kept: {p}"
    );
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn expired_tokens_are_refreshed_by_the_gateway() {
    // Tokens that expire at once: connecting must refresh, not fail.
    let server = fake(&[("FAKE_OAUTH_TOKEN_TTL", "1")]);
    let env = setup(&server.1, "files.read");
    let (ok, out) = signin(&env).await;
    assert!(ok, "{out}");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let c = start(&env).await;
    let p = remote(&call(&c, "list_providers", json!({})).await);
    assert_eq!(p["status"]["state"], "trial", "{p}");
    c.cancel().await.unwrap();
}
