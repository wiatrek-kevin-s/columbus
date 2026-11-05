//! The Columbus app's chat (design section 8, "Connecting an AI", route 3:
//! an API key).
//!
//! The app starts its own `columbus-mcp` session and gives the model that
//! session's tools and nothing else: no shell, no files, no web. Every call
//! goes through the server's rules like any other client's. The app is the
//! MCP client, so the server asks the person for approvals through it
//! (elicitation), and the app shows those prompts in its own chat window,
//! written by the app's code, never by the model.
//!
//! The API key is held by the app's Rust side only: it is read from the
//! operating system's credential store when a chat starts and is never
//! given to a web page or written to a file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationCapability, FormElicitationCapability, Implementation,
    InitializeRequestParams,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Notify};

/// Steps (model calls) one message may take before the chat stops.
pub const MAX_STEPS: usize = 60;
/// Longest tool result passed to the model, in characters.
pub const MAX_RESULT_CHARS: usize = 40_000;
/// When the conversation grows past this (characters of JSON), the oldest
/// tool results are replaced by a note, so it keeps fitting.
pub const HISTORY_CHARS: usize = 500_000;
const API_VERSION: &str = "2023-06-01";

/// The models the app offers, cheapest-to-run first after the default.
pub const MODELS: &[(&str, &str)] = &[
    ("claude-sonnet-5-5", "Claude Sonnet 5.5 (balanced)"),
    ("claude-opus-5-5", "Claude Opus 5.5 (most capable)"),
    ("claude-haiku-5-5", "Claude Haiku 5.5 (fastest)"),
];

#[derive(Debug, Clone)]
pub struct Config {
    /// `columbus-mcp`, started as `serve`.
    pub server: PathBuf,
    /// Extra environment for the server (for tests).
    pub server_env: Vec<(String, String)>,
    pub api_key: String,
    pub model: String,
    /// `https://api.anthropic.com`, or a stand-in in tests.
    pub api_base: String,
}

/// What the chat window shows as the conversation goes.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// The model's words.
    Text {
        text: String,
    },
    /// A tool call starting, in plain words.
    ToolStarted {
        id: String,
        name: String,
        input: Value,
    },
    /// A tool call finished.
    ToolFinished {
        id: String,
        name: String,
        ok: bool,
        summary: String,
        fails: u32,
        warns: u32,
    },
    /// The session's server asks the person; answer with [`Answer`].
    Approval {
        id: u64,
        title: String,
        message: String,
        offer_always: bool,
    },
    /// Something the person should know (the model is thinking, a limit).
    Status {
        text: String,
    },
    /// The reply is complete.
    Done,
    Error {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    AllowOnce,
    AlwaysThisSession,
    Decline,
}

/// The chat window, as the chat sees it.
pub trait Ui: Send + Sync + 'static {
    fn event(&self, e: Event);
    /// Show an approval prompt; the answer comes back on the channel.
    fn approve(
        &self,
        id: u64,
        title: String,
        message: String,
        offer_always: bool,
    ) -> oneshot::Receiver<Answer>;
}

/// Stops a reply in progress.
#[derive(Default, Clone)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

/// The MCP client side: declares form elicitation and answers it through
/// the chat window.
struct Handler {
    ui: Arc<dyn Ui>,
    next: std::sync::atomic::AtomicU64,
}

impl ClientHandler for Handler {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::builder()
                .enable_elicitation_with(
                    ElicitationCapability::new().with_form(FormElicitationCapability::new()),
                )
                .build(),
            Implementation::new("Columbus app", env!("CARGO_PKG_VERSION")),
        )
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        let ElicitRequestParams::FormElicitationParams {
            message,
            requested_schema,
            ..
        } = request
        else {
            return Ok(decline());
        };
        let schema = serde_json::to_value(&requested_schema).unwrap_or(Value::Null);
        let title = schema["title"]
            .as_str()
            .unwrap_or("Columbus asks for your approval")
            .to_string();
        let offer_always = schema["properties"]["decision"]["enum"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "always_this_session"));
        // The server's last line is for clients with a plain accept or
        // decline; the window has its own buttons.
        let message = message
            .trim_end()
            .trim_end_matches("Accept to allow it, decline to refuse.")
            .trim_end()
            .to_string();
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let rx = self.ui.approve(id, title, message, offer_always);
        Ok(match rx.await {
            Ok(Answer::AllowOnce) => accept("allow_once"),
            Ok(Answer::AlwaysThisSession) if offer_always => accept("always_this_session"),
            Ok(Answer::AlwaysThisSession) => accept("allow_once"),
            Ok(Answer::Decline) | Err(_) => decline(),
        })
    }
}

fn accept(decision: &str) -> ElicitResult {
    let mut r = ElicitResult::new(ElicitationAction::Accept);
    r.content = Some(json!({ "decision": decision }));
    r
}

fn decline() -> ElicitResult {
    ElicitResult::new(ElicitationAction::Decline)
}

pub struct Chat {
    client: RunningService<RoleClient, Handler>,
    session: Option<String>,
    tools: Vec<Value>,
    history: Vec<Value>,
    system: String,
    cfg: Config,
    ui: Arc<dyn Ui>,
    http: reqwest::Client,
}

const SYSTEM: &str = "You are the GIS assistant inside the Columbus app, working for a GIS analyst. \
Your only tools are a Columbus session's: they read and process the analyst's data under their \
organization's rules, and the server enforces those rules, not you. Start a new conversation by \
calling whoami. Read every result's checks: report fails and warnings plainly with their numbers, and never \
present a result as correct over a fail. When something needs the person's approval, the app asks them \
itself; do not ask them to approve in your reply. Layers you make appear on the person's map in the \
Columbus window as you make them. Be concise and specific: dataset names, row counts, CRS codes and units.";

impl Chat {
    /// Start a session of `cfg.server` for this chat.
    pub async fn start(cfg: Config, ui: Arc<dyn Ui>) -> Result<Chat> {
        if cfg.api_key.trim().is_empty() {
            bail!("no API key is set");
        }
        let server = cfg.server.clone();
        let env = cfg.server_env.clone();
        let cmd = tokio::process::Command::new(&server).configure(move |c| {
            c.arg("serve");
            for (k, v) in &env {
                c.env(k, v);
            }
        });
        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("starting the Columbus server at {}", server.display()))?;
        let handler = Handler {
            ui: ui.clone(),
            next: Default::default(),
        };
        let client = handler
            .serve(transport)
            .await
            .map_err(|e| anyhow!("the Columbus server did not start: {e}"))?;
        let tools = client
            .list_all_tools()
            .await
            .map_err(|e| anyhow!("listing the Columbus tools failed: {e}"))?
            .into_iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description.unwrap_or_default(),
                    "input_schema": Value::Object((*t.input_schema).clone()),
                })
            })
            .collect();
        // The session's id, for its map window (the app's own call, not
        // the model's).
        let session = client
            .call_tool(CallToolRequestParams::new("whoami"))
            .await
            .ok()
            .and_then(|r| r.structured_content)
            .and_then(|v| v["result"]["session"].as_str().map(String::from));
        let mut system = SYSTEM.to_string();
        if let Some(skill) = skill_text(&cfg.server) {
            system.push_str("\n\nHow to work with Columbus (the working-with-columbus skill):\n\n");
            system.push_str(&skill);
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()?;
        Ok(Chat {
            client,
            session,
            tools,
            history: Vec::new(),
            system,
            cfg,
            ui,
            http,
        })
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// The Columbus session this chat works in.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn history(&self) -> &[Value] {
        &self.history
    }

    /// The person's message; runs until the model has answered, a limit is
    /// reached or the reply is cancelled. Events go to the window as it goes.
    pub async fn send(&mut self, text: &str, cancel: &Cancel) -> Result<()> {
        cancel.reset();
        self.history
            .push(json!({"role": "user", "content": [{"type": "text", "text": text}]}));
        let r = self.run(cancel).await;
        match &r {
            Ok(()) => self.ui.event(Event::Done),
            Err(e) => self.ui.event(Event::Error {
                text: e.to_string(),
            }),
        }
        r
    }

    async fn run(&mut self, cancel: &Cancel) -> Result<()> {
        for _ in 0..MAX_STEPS {
            self.trim_history();
            let reply = tokio::select! {
                r = self.ask_model() => r?,
                _ = cancel.cancelled() => {
                    self.forget_unanswered();
                    bail!("stopped")
                }
            };
            let content = reply["content"].as_array().cloned().unwrap_or_default();
            self.history
                .push(json!({"role": "assistant", "content": content.clone()}));
            for block in &content {
                if block["type"] == "text" {
                    if let Some(t) = block["text"].as_str().filter(|t| !t.trim().is_empty()) {
                        self.ui.event(Event::Text {
                            text: t.to_string(),
                        });
                    }
                }
            }
            let calls: Vec<&Value> = content.iter().filter(|b| b["type"] == "tool_use").collect();
            if calls.is_empty() || reply["stop_reason"] != "tool_use" {
                if reply["stop_reason"] == "max_tokens" {
                    self.ui.event(Event::Status {
                        text: "The reply reached its length limit.".into(),
                    });
                }
                return Ok(());
            }
            let mut results = Vec::new();
            for call in calls {
                let id = call["id"].as_str().unwrap_or_default().to_string();
                let name = call["name"].as_str().unwrap_or_default().to_string();
                let input = call["input"].clone();
                if cancel.is_cancelled() {
                    results.push(tool_result(
                        &id,
                        "Stopped by the person before this ran.",
                        true,
                    ));
                    continue;
                }
                self.ui.event(Event::ToolStarted {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                let (text, is_error, summary, fails, warns) = tokio::select! {
                    r = self.call_tool(&name, input) => r,
                    _ = cancel.cancelled() => (
                        "Stopped by the person while this was running.".to_string(),
                        true, "stopped".to_string(), 0, 0,
                    ),
                };
                self.ui.event(Event::ToolFinished {
                    id: id.clone(),
                    name,
                    ok: !is_error,
                    summary,
                    fails,
                    warns,
                });
                results.push(tool_result(&id, &text, is_error));
            }
            self.history
                .push(json!({"role": "user", "content": results}));
            if cancel.is_cancelled() {
                bail!("stopped");
            }
        }
        self.ui.event(Event::Status {
            text: format!("Stopped after {MAX_STEPS} steps; ask to continue if it should go on."),
        });
        Ok(())
    }

    /// A reply cancelled mid-request leaves the person's message without an
    /// answer; drop it so the conversation stays well-formed.
    fn forget_unanswered(&mut self) {
        if self.history.last().is_some_and(|m| m["role"] == "user") {
            self.history.pop();
        }
    }

    async fn ask_model(&self) -> Result<Value> {
        let body = json!({
            "model": self.cfg.model,
            "max_tokens": 8192,
            "system": self.system,
            "tools": self.tools,
            "messages": self.history,
        });
        let url = format!("{}/v1/messages", self.cfg.api_base.trim_end_matches('/'));
        let mut tries = 0;
        loop {
            tries += 1;
            let r = self
                .http
                .post(&url)
                .header("x-api-key", &self.cfg.api_key)
                .header("anthropic-version", API_VERSION)
                .json(&body)
                .send()
                .await
                .map_err(|e| anyhow!("could not reach the AI service: {}", plain(&e)))?;
            let code = r.status();
            let v: Value = r.json().await.unwrap_or(Value::Null);
            if code.is_success() {
                return Ok(v);
            }
            let msg = v["error"]["message"].as_str().unwrap_or("").to_string();
            // Busy or rate-limited: wait and try again a few times.
            if (code.as_u16() == 429 || code.as_u16() == 529 || code.is_server_error()) && tries < 4
            {
                self.ui.event(Event::Status {
                    text: "The AI service is busy; trying again…".into(),
                });
                tokio::time::sleep(Duration::from_secs(2u64.pow(tries))).await;
                continue;
            }
            bail!(match code.as_u16() {
                401 => "the API key was refused; check it in Settings".to_string(),
                403 => format!("the API key may not use this model ({msg})"),
                400 if msg.contains("credit") => format!("the AI service refused: {msg}"),
                _ => format!("the AI service answered {code}: {msg}"),
            });
        }
    }

    async fn call_tool(&self, name: &str, input: Value) -> (String, bool, String, u32, u32) {
        let mut p = CallToolRequestParams::new(name.to_string());
        if let Value::Object(m) = input {
            p = p.with_arguments(m);
        }
        match self.client.call_tool(p).await {
            Ok(r) => {
                let is_error = r.is_error == Some(true);
                let text = match &r.structured_content {
                    Some(v) => v.to_string(),
                    None => r
                        .content
                        .iter()
                        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                };
                let (fails, warns) = r
                    .structured_content
                    .as_ref()
                    .map(count_checks)
                    .unwrap_or((0, 0));
                let summary = if is_error {
                    first_line(&text, 160)
                } else {
                    result_summary(r.structured_content.as_ref())
                };
                (cut(&text), is_error, summary, fails, warns)
            }
            Err(e) => {
                let t = format!("the tool call failed: {e}");
                (t.clone(), true, first_line(&t, 160), 0, 0)
            }
        }
    }

    /// Keep the conversation within [`HISTORY_CHARS`]: replace the oldest
    /// tool results with a note.
    fn trim_history(&mut self) {
        let size = |h: &Vec<Value>| h.iter().map(|m| m.to_string().len()).sum::<usize>();
        let mut total = size(&self.history);
        let last = self.history.len().saturating_sub(2);
        for i in 0..last {
            if total <= HISTORY_CHARS {
                break;
            }
            let Some(blocks) = self.history[i]["content"].as_array_mut() else {
                continue;
            };
            for b in blocks.iter_mut() {
                if b["type"] == "tool_result" {
                    let before = b.to_string().len();
                    b["content"] = json!("[An older tool result, removed to keep the conversation short. Call the tool again if it is needed.]");
                    total = total.saturating_sub(before.saturating_sub(b.to_string().len()));
                }
            }
        }
    }

    pub async fn close(self) {
        let _ = self.client.cancel().await;
    }
}

fn tool_result(id: &str, text: &str, is_error: bool) -> Value {
    json!({"type": "tool_result", "tool_use_id": id, "content": text, "is_error": is_error})
}

fn cut(s: &str) -> String {
    if s.chars().count() <= MAX_RESULT_CHARS {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_RESULT_CHARS).collect();
    format!("{head}\n[cut at {MAX_RESULT_CHARS} characters; use read_output or narrower queries for more]")
}

fn first_line(s: &str, n: usize) -> String {
    let l = s.lines().next().unwrap_or("");
    if l.chars().count() > n {
        format!("{}…", l.chars().take(n).collect::<String>())
    } else {
        l.to_string()
    }
}

fn count_checks(v: &Value) -> (u32, u32) {
    let mut c = (0, 0);
    for k in v["checks"].as_array().into_iter().flatten() {
        match k["severity"].as_str() {
            Some("fail") => c.0 += 1,
            Some("warn") => c.1 += 1,
            _ => {}
        }
    }
    c
}

/// A few plain words about a result: rows, the layer made, the status.
fn result_summary(v: Option<&Value>) -> String {
    let Some(r) = v.map(|v| &v["result"]) else {
        return "done".into();
    };
    let mut parts = Vec::new();
    if let Some(s) = r["status"].as_str() {
        parts.push(s.replace('_', " "));
    }
    if let Some(n) = r["rows_read"].as_u64().or(r["rows"].as_u64()) {
        parts.push(format!("{n} rows"));
    }
    if let Some(n) = r["layer"]["summary"]["rows"].as_u64() {
        if !parts.iter().any(|p| p.ends_with(" rows")) {
            parts.push(format!("{n} rows"));
        }
    }
    if let Some(c) = r["layer"]["summary"]["crs"]["code"].as_str() {
        parts.push(c.to_string());
    }
    if parts.is_empty() {
        "done".into()
    } else {
        parts.join(", ")
    }
}

fn plain(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "it did not answer in time".into()
    } else if e.is_connect() {
        "no connection (offline, or blocked by your network)".into()
    } else {
        e.to_string()
    }
}

/// The `working-with-columbus` skill installed beside the server, without
/// its front matter.
fn skill_text(server: &Path) -> Option<String> {
    let dir = server.parent()?;
    let text = std::fs::read_to_string(dir.join("skills/working-with-columbus/SKILL.md")).ok()?;
    let body = match text.strip_prefix("---") {
        Some(rest) => rest.split_once("\n---").map(|(_, b)| b).unwrap_or(rest),
        None => &text,
    };
    Some(body.trim().chars().take(20_000).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_results_are_cut() {
        let s = "x".repeat(MAX_RESULT_CHARS + 10);
        assert!(cut(&s).contains("cut at"));
        assert_eq!(cut("short"), "short");
    }

    #[test]
    fn summaries() {
        let v = json!({"result": {"rows_read": 12, "layer": {"summary": {"crs": {"code": "EPSG:3081"}}}}, "checks": [{"severity": "warn"}, {"severity": "fail"}, {"severity": "info"}]});
        assert_eq!(result_summary(Some(&v)), "12 rows, EPSG:3081");
        assert_eq!(count_checks(&v), (1, 1));
    }
}
