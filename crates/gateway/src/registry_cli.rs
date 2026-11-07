//! `columbus-mcp registry query '<json>'`: one read-only registry call for
//! the Columbus app's Registry page, made through Columbus itself (in
//! process, like a workflow run) so the page gets exactly what an assistant
//! would: the checked index, its checks and the same audit record.
//!
//! The JSON is `{"tool": "registry_search" | "registry_get", "args": {...}}`;
//! nothing else can be called this way. It prints one JSON object:
//! `{"ok": true, "result": …, "checks": […]}` or `{"ok": false, "error": …}`.

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, Implementation, InitializeRequestParams,
};
use rmcp::{ClientHandler, ServiceExt};
use serde_json::{json, Value};

use crate::server::{Gateway, State};

/// The only calls the app's Registry page makes.
const ALLOWED: &[&str] = &["registry_search", "registry_get"];
pub const CLIENT: &str = "columbus-app-registry";

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
    let paths = crate::config::Paths::from_env();
    let state = State::load(paths).map_err(|e| e.to_string())?;
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
