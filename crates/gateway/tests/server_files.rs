//! Files to and from a connected server whose tools take nested parameters
//! (a geoprocessing runner such as RustGIS's `run_tool`): with `"*": path`
//! in the manifest, workspace handles anywhere in the arguments are handed
//! over as the server's own copies, and `out://<name>` asks it to write a
//! file, taken back into the workspace after the call.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use columbus_broker::registry::Broker;
use columbus_broker::review;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

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

async fn setup(variant: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in ["connections", "servers", "personal"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nservers: { allow_unconfined_local: true }\n",
    )
    .unwrap();
    let manifest = root.join("servers").join("geo.yaml");
    std::fs::write(
        &manifest,
        format!(
            "id: geo\nname: Test geoprocessing\ntransport: {{ type: stdio, command: {}, args: [{variant}] }}\ntrust: standard\ntools:\n  export_report: {{ risk: read, output: to_workspace }}\n  run_tool: {{ risk: read, args: {{ \"*\": path }} }}\n  default: {{ allow: false }}\n",
            serde_json::to_string(&fake_server().display().to_string()).unwrap()
        ),
    )
    .unwrap();
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
    let p = b.get("geo").unwrap().clone();
    let c = b.connected(&p).await.map_err(|s| format!("{s:?}")).unwrap();
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str(
        &serde_yaml::to_string(&json!({"pinned": review::pin(&c.review, None)})).unwrap(),
    );
    std::fs::write(&manifest, text).unwrap();
    Env { _dir: dir, root }
}

async fn start(env: &Env) -> RunningService<RoleClient, ()> {
    let root = env.root.clone();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PERSONAL_SERVERS", root.join("personal"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_PYTHON", "python3")
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

fn checks(v: &Value) -> String {
    v["checks"].to_string()
}

fn handle_path(workspace: &Path, handle: &str) -> PathBuf {
    let (_session, rel) = handle
        .strip_prefix("ws://")
        .unwrap()
        .split_once('/')
        .unwrap();
    workspace.join(rel)
}

#[tokio::test]
async fn nested_handles_go_in_and_asked_for_files_come_back() {
    let env = setup("plain").await;
    let c = start(&env).await;
    let report = ok(&call(&c, "run_tool", json!({"name": "geo.export_report"})).await);
    let input = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let who = ok(&call(&c, "whoami", json!({})).await);
    let ws = PathBuf::from(who["result"]["workspace"].as_str().unwrap());

    let r = ok(&call(
        &c,
        "run_tool",
        json!({"name": "geo.run_tool", "args": {"tool": "slope", "params": {"input": input, "output": "out://slope.txt"}}}),
    )
    .await);
    let saved = &r["result"]["saved"];
    assert_eq!(saved.as_array().map(Vec::len), Some(1), "{r}");
    let h = saved[0]["handle"].as_str().unwrap();
    assert!(h.ends_with("-slope.txt"), "{h}");
    let made = std::fs::read_to_string(handle_path(&ws, h)).unwrap();
    assert!(
        made.starts_with("slope of "),
        "the server's file, taken: {made}"
    );
    assert!(
        checks(&r).contains("provider.outputs_taken"),
        "{}",
        checks(&r)
    );
    // The server wrote in its own folder, never in the workspace's results.
    let external = r["result"]["external"].to_string();
    assert!(!external.contains("results"), "{external}");

    // A name that is not a plain file name is refused before anything runs.
    let bad = call(
        &c,
        "run_tool",
        json!({"name": "geo.run_tool", "args": {"tool": "slope", "params": {"input": input, "output": "out://../x.txt"}}}),
    )
    .await;
    assert_eq!(bad.is_error, Some(true));

    // A handle that is not this session's: refused, nothing runs.
    let bad = call(
        &c,
        "run_tool",
        json!({"name": "geo.run_tool", "args": {"tool": "slope", "params": {"input": "ws://nope/x", "output": "out://a.txt"}}}),
    )
    .await;
    assert_eq!(bad.is_error, Some(true));
    c.cancel().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_in_place_of_an_asked_for_file_is_not_followed() {
    let env = setup("linky").await;
    let c = start(&env).await;
    let report = ok(&call(&c, "run_tool", json!({"name": "geo.export_report"})).await);
    let input = report["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let r = ok(&call(
        &c,
        "run_tool",
        json!({"name": "geo.run_tool", "args": {"tool": "slope", "params": {"input": input, "output": "out://slope.txt"}}}),
    )
    .await);
    assert_eq!(
        r["result"]["saved"].as_array().map_or(0, Vec::len),
        0,
        "{r}"
    );
    assert!(
        checks(&r).contains("provider.outputs_skipped"),
        "{}",
        checks(&r)
    );
    c.cancel().await.unwrap();
}
