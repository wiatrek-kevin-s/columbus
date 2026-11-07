//! `columbus-mcp app query '<json>'` (also `registry query`): one read-only
//! call for a page of the Columbus app, made through Columbus itself (in
//! process, like a workflow run) so the page gets exactly what an assistant
//! would: the same checks, policy and audit record.
//!
//! The JSON is `{"tool": …, "args": {...}, "project": "<id>"?}`, and only
//! these can be called: `registry_search`, `registry_get`, and `project_db`
//! with `{"action": "list"}` and nothing else. With `project`, the call runs
//! as in that project's chats (its connections and data groups, failing
//! closed), so a project's databases are listed as its chats see them. It
//! prints one JSON object: `{"ok": true, "result": …, "checks": […]}` or
//! `{"ok": false, "error": …}`.

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, Implementation, InitializeRequestParams,
};
use rmcp::{ClientHandler, ServiceExt};
use serde_json::{json, Value};

use crate::server::{Gateway, State};

/// The only calls the app's pages make this way.
const ALLOWED: &[&str] = &["registry_search", "registry_get", "project_db"];
pub const CLIENT: &str = "columbus-app-page";

struct PageClient;

impl ClientHandler for PageClient {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new(CLIENT, env!("CARGO_PKG_VERSION")),
        )
    }
}

pub async fn query(request: &str) -> anyhow::Result<()> {
    // As the server itself: other processes of the same user must not read
    // this one's memory or environment through /proc.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let out = match run(request).await {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": e}),
    };
    println!("{out}");
    Ok(())
}

async fn run(request: &str) -> Result<Value, String> {
    if request.len() > 16 * 1024 {
        return Err("the request is too long".into());
    }
    let req: Value = serde_json::from_str(request).map_err(|e| format!("not JSON: {e}"))?;
    let tool = req["tool"].as_str().unwrap_or("");
    if !ALLOWED.contains(&tool) {
        return Err(format!(
            "only {} can be called this way",
            ALLOWED.join(" and ")
        ));
    }
    let args = match &req["args"] {
        Value::Object(m) => m.clone(),
        Value::Null => Default::default(),
        _ => return Err("args must be an object".into()),
    };
    // project_db only lists: exactly {"action": "list"}.
    if tool == "project_db" && (args.len() != 1 || args.get("action") != Some(&json!("list"))) {
        return Err("project_db can only list databases this way".into());
    }
    let project = match &req["project"] {
        Value::Null => None,
        Value::String(p) if !p.is_empty() => Some(p.clone()),
        _ => return Err("project must be a project id".into()),
    };
    let paths = crate::config::Paths::from_env();
    let mut state = State::load(paths).map_err(|e| e.to_string())?;
    let workspace = state.sandbox.root().to_path_buf();
    let out = match &project {
        Some(id) => match crate::apply_project(&mut state, id) {
            Ok(()) => call(state, tool, args).await,
            Err(e) => Err(e),
        },
        None => call(state, tool, args).await,
    };
    // This call's own session folder, if it left it empty.
    let _ = std::fs::remove_dir(&workspace);
    out
}

async fn call(
    state: State,
    tool: &str,
    args: serde_json::Map<String, Value>,
) -> Result<Value, String> {
    let gateway = Gateway::new(state);
    let (server_io, client_io) = tokio::io::duplex(4 << 20);
    tokio::spawn(async move {
        if let Ok(s) = gateway.serve(server_io).await {
            let _ = s.waiting().await;
        }
    });
    let client = PageClient
        .serve(client_io)
        .await
        .map_err(|e| e.to_string())?;
    let r = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        client.call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(args)),
    )
    .await
    .map_err(|_| "the registry took too long to answer".to_string())?
    .map_err(|e| e.to_string())?;
    let _ = client.cancel().await;
    if r.is_error == Some(true) {
        let text = r
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(text);
    }
    let v = r.structured_content.unwrap_or(Value::Null);
    Ok(json!({"ok": true, "result": v["result"], "checks": v["checks"]}))
}
