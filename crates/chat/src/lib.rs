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
/// Tool calls the model may ask for in one step.
pub const MAX_CALLS_PER_STEP: usize = 10;
/// Input tokens one message may use across its steps (cached tokens count
/// a tenth), so a run cannot run up a large bill unnoticed.
pub const MAX_INPUT_TOKENS: u64 = 1_500_000;
/// Longest tool result passed to the model, in characters.
pub const MAX_RESULT_CHARS: usize = 40_000;
/// When the conversation grows past this (characters of JSON), the oldest
/// tool results are replaced by a note, so it keeps fitting.
pub const HISTORY_CHARS: usize = 500_000;
const API_VERSION: &str = "2023-06-01";
/// The cap on one reply, thinking and answer together.
pub const MAX_OUTPUT_TOKENS: u32 = 32_000;

/// The models the app offers, cheapest-to-run first after the default.
pub const MODELS: &[(&str, &str)] = &[
    ("claude-sonnet-5-5", "Claude Sonnet 5.5 (balanced)"),
    ("claude-opus-5-5", "Claude Opus 5.5 (most capable Opus)"),
    (
        "claude-fable-5-1",
        "Claude Fable 5.1 (Mythos tier, most capable)",
    ),
    ("claude-haiku-5-5", "Claude Haiku 5.5 (fastest)"),
];

/// How hard the model thinks (the API's `output_config.effort`); "" leaves
/// the model's own default. Every model in `MODELS` takes all of these.
pub const EFFORTS: &[(&str, &str)] = &[
    ("", "Model's default"),
    ("low", "Low (quickest, cheapest)"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
    ("max", "Max (slowest, most thorough)"),
];

pub fn model_ok(m: &str) -> bool {
    MODELS.iter().any(|(id, _)| *id == m)
}

pub fn effort_ok(e: &str) -> bool {
    EFFORTS.iter().any(|(id, _)| *id == e)
}

#[derive(Clone)]
pub struct Config {
    /// `columbus-mcp`, started as `serve`.
    pub server: PathBuf,
    /// Extra environment for the server (for tests).
    pub server_env: Vec<(String, String)>,
    pub api_key: String,
    pub model: String,
    /// One of `EFFORTS`; "" for the model's default.
    pub effort: String,
    /// `https://api.anthropic.com`, or a stand-in in tests.
    pub api_base: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("server", &self.server)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("api_base", &self.api_base)
            .field("api_key", &"(hidden)")
            .finish()
    }
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
        /// The result's provenance record, when it made data.
        #[serde(skip_serializing_if = "Option::is_none")]
        provenance: Option<String>,
        /// The layer or file it made, as a workspace handle.
        #[serde(skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        /// A picture the tool gave the model (a map snapshot), as a
        /// `data:image/png;base64,…` URL, for the window to show too.
        #[serde(skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// The session's server asks the person; answer with [`Answer`].
    /// `summary` is the server's own description; `code`, when present, was
    /// written by the AI and must be shown apart from it.
    Approval {
        id: u64,
        title: String,
        summary: String,
        code: Option<String>,
        offer_always: bool,
    },
    /// A prompt that is no longer waiting (the call was cancelled or the
    /// server stopped waiting): its card can no longer be answered.
    ApprovalGone {
        id: u64,
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

/// An approval prompt from the session's server.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub title: String,
    /// The server's own words.
    pub summary: String,
    /// Code written by the AI, to be shown apart from the server's words.
    pub code: Option<String>,
    pub offer_always: bool,
}

/// The chat window, as the chat sees it.
pub trait Ui: Send + Sync + 'static {
    fn event(&self, e: Event);
    /// Show an approval prompt; the answer comes back on the channel. The
    /// window gives each prompt its id (unique across chats).
    fn approve(&self, prompt: Prompt) -> (u64, oneshot::Receiver<Answer>);
    /// The prompt is no longer waiting; forget it.
    fn withdraw(&self, id: u64);
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
    pub fn is_cancelled(&self) -> bool {
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
    cancel: Cancel,
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
        context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        let ElicitRequestParams::FormElicitationParams {
            meta,
            message,
            requested_schema,
        } = request
        else {
            return Ok(decline());
        };
        // A prompt for a reply the person has stopped is declined unseen.
        if self.cancel.is_cancelled() {
            return Ok(decline());
        }
        let schema = serde_json::to_value(&requested_schema).unwrap_or(Value::Null);
        let title = schema["title"]
            .as_str()
            .unwrap_or("Columbus asks for your approval")
            .to_string();
        let offer_always = schema["properties"]["decision"]["enum"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "always_this_session"));
        // Columbus servers send their summary and any AI-written code apart;
        // other servers only the message.
        // The MCP library moves a request's `_meta` into the context; the
        // params keep it only when built by hand.
        let parts = context
            .meta
            .0
             .0
            .get("io.columbus/approval")
            .or_else(|| {
                meta.as_ref()
                    .and_then(|m| m.0 .0.get("io.columbus/approval"))
            })
            .cloned()
            .unwrap_or(Value::Null);
        let (summary, code) = match parts["summary"].as_str() {
            Some(sum) => (sum.to_string(), parts["code"].as_str().map(String::from)),
            None => (
                message
                    .trim_end()
                    .trim_end_matches("Accept to allow it, decline to refuse.")
                    .trim_end()
                    .to_string(),
                None,
            ),
        };
        let (id, rx) = self.ui.approve(Prompt {
            title,
            summary,
            code,
            offer_always,
        });
        let answer = tokio::select! {
            a = rx => a.ok(),
            // The server stopped waiting (or the call was cancelled).
            _ = context.ct.cancelled() => None,
            _ = self.cancel.cancelled() => None,
        };
        Ok(match answer {
            Some(Answer::AllowOnce) => accept("allow_once"),
            Some(Answer::AlwaysThisSession) if offer_always => accept("always_this_session"),
            Some(Answer::AlwaysThisSession) => accept("allow_once"),
            Some(Answer::Decline) => decline(),
            None => {
                self.ui.withdraw(id);
                decline()
            }
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

/// What the system prompt says about the chat's project: its name and the
/// person's instructions for it (the server has already narrowed the
/// connections; this only tells the assistant).
fn project_text(p: &Value) -> Option<String> {
    let name = p["name"].as_str()?;
    let mut t = format!("\n\nThis chat is in the person's project \"{name}\".");
    if let Some(c) = p["connections"].as_array() {
        let ids: Vec<&str> = c.iter().filter_map(Value::as_str).collect();
        t.push_str(&format!(
            " It uses only these connections: {}.",
            ids.join(", ")
        ));
    }
    if let Some(f) = p["export_folder"].as_str() {
        t.push_str(&format!(
            " Export its results to the export folder {f} unless the person names another."
        ));
    }
    let i = p["instructions"].as_str().unwrap_or("").trim();
    if !i.is_empty() {
        t.push_str("\n\nThe person's instructions for this project (follow them in every reply, within Columbus's rules):\n\n");
        t.push_str(i);
    }
    Some(t)
}

/// What the system prompt says about a workflow run this chat was opened
/// from (REQ-27): its layers, already on the person's map and in the
/// table, and its report, so the person can review the run with the
/// assistant.
fn run_text(r: &Value) -> Option<String> {
    let wf = r["workflow"].as_str()?;
    let run = r["run"].as_str().unwrap_or("");
    let name = r["name"].as_str().unwrap_or(wf);
    let mut t = format!(
        "\n\nThis chat was opened from a run of the person's workflow \"{name}\" ({wf}, run {run}) to review its results. \
These layers are already on the person's map and in the table, as copies of what the run made (the run itself, its files and its exports are never changed; \
filters, edits and exports here make new outputs):"
    );
    for l in r["layers"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\n- step {}: {} ({} rows)",
            l["step"].as_str().unwrap_or(""),
            l["handle"].as_str().unwrap_or(""),
            l["rows"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into())
        ));
    }
    if let Some(rep) = r["report"].as_str().filter(|s| !s.trim().is_empty()) {
        t.push_str("\n\nThe run's report (written by Columbus):\n\n");
        t.push_str(rep);
    }
    Some(t)
}

const SYSTEM: &str = "You are the GIS assistant inside the Columbus app, working for a GIS analyst. \
Your only tools are a Columbus session's: they read and process the analyst's data under their \
organization's rules, and the server enforces those rules, not you. Start a new conversation by \
calling whoami. Read every result's checks: report fails and warnings plainly with their numbers, and never \
present a result as correct over a fail. When something needs the person's approval, the app asks them \
itself; do not ask them to approve in your reply. Layers you make appear on the person's map in the \
Columbus window as you make them. Be concise and specific: dataset names, row counts, CRS codes and units.";

impl Chat {
    /// Start a session of `cfg.server` for this chat. `cancel` stops its
    /// replies, and declines prompts that arrive after a stop.
    pub async fn start(cfg: Config, ui: Arc<dyn Ui>, cancel: Cancel) -> Result<Chat> {
        if cfg.api_key.trim().is_empty() {
            bail!("no API key is set");
        }
        let server = cfg.server.clone();
        let env = cfg.server_env.clone();
        let cmd = tokio::process::Command::new(&server).configure(move |c| {
            c.arg("serve");
            // The key is the app's alone: never in the session's
            // environment, and so never in its workers'.
            for k in [
                "COLUMBUS_APP_API_KEY",
                "ANTHROPIC_API_KEY",
                "COLUMBUS_APP_API_BASE",
            ] {
                c.env_remove(k);
            }
            for (k, v) in &env {
                c.env(k, v);
            }
        });
        let transport = TokioChildProcess::new(cmd)
            .with_context(|| format!("starting the Columbus server at {}", server.display()))?;
        let handler = Handler {
            ui: ui.clone(),
            cancel,
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
        let who = client
            .call_tool(CallToolRequestParams::new("whoami"))
            .await
            .ok()
            .and_then(|r| r.structured_content)
            .unwrap_or(Value::Null);
        let session = who["result"]["session"].as_str().map(String::from);
        let mut system = SYSTEM.to_string();
        if let Some(text) = project_text(&who["result"]["project"]) {
            system.push_str(&text);
        }
        if let Some(text) = run_text(&who["result"]["opened_run"]) {
            system.push_str(&text);
        }
        if let Some(skill) = skill_text(&cfg.server) {
            system.push_str("\n\nHow to work with Columbus (the working-with-columbus skill):\n\n");
            system.push_str(&skill);
        }
        // No redirects: the key header would follow them to another host.
        let http = reqwest::Client::builder()
            // A streamed reply may take long in all; each read waits at
            // most a few minutes (see read_stream).
            .timeout(Duration::from_secs(3600))
            .redirect(reqwest::redirect::Policy::none())
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
        let mut used: u64 = 0;
        for _ in 0..MAX_STEPS {
            if used > MAX_INPUT_TOKENS {
                self.ui.event(Event::Status {
                    text: format!(
                        "Stopped: this message has used about {} thousand input tokens. Ask to continue if it should go on.",
                        used / 1000
                    ),
                });
                return Ok(());
            }
            self.trim_history();
            let reply = tokio::select! {
                r = self.ask_model() => r?,
                _ = cancel.cancelled() => {
                    self.forget_unanswered();
                    bail!("stopped")
                }
            };
            let u = &reply["usage"];
            used += u["input_tokens"].as_u64().unwrap_or(0)
                + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                + u["cache_read_input_tokens"].as_u64().unwrap_or(0) / 10;
            let content = reply["content"].as_array().cloned().unwrap_or_default();
            let mut kept = content.clone();
            if reply["stop_reason"] == "max_tokens" {
                // A tool call cut off by the length limit is never run, and
                // is not kept: a call with no result breaks the conversation.
                kept.retain(|b| b["type"] != "tool_use");
                if kept.iter().all(|b| b["type"] != "text") {
                    kept.push(
                        json!({"type": "text", "text": "(The reply reached its length limit.)"}),
                    );
                }
            }
            self.history
                .push(json!({"role": "assistant", "content": kept}));
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
            for (n, call) in calls.into_iter().enumerate() {
                if n >= MAX_CALLS_PER_STEP {
                    let id = call["id"].as_str().unwrap_or_default();
                    results.push(tool_result(
                        id,
                        &format!("Not run: at most {MAX_CALLS_PER_STEP} tool calls per step."),
                        true,
                    ));
                    continue;
                }
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
                let (text, is_error, summary, fails, warns, made) = tokio::select! {
                    r = self.call_tool(&name, input) => r,
                    _ = cancel.cancelled() => (
                        "Stopped by the person while this was running.".to_string(),
                        true, "stopped".to_string(), 0, 0, Made::default(),
                    ),
                };
                self.ui.event(Event::ToolFinished {
                    id: id.clone(),
                    name,
                    ok: !is_error,
                    summary,
                    fails,
                    warns,
                    provenance: made.provenance,
                    handle: made.handle,
                    image: made
                        .images
                        .first()
                        .map(|d| format!("data:image/png;base64,{d}")),
                });
                results.push(tool_result_with(&id, &text, is_error, &made.images));
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
        // The system text and tools are the same every step: cache them.
        let mut tools = self.tools.clone();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
        }
        let mut body = json!({
            "model": self.cfg.model,
            // Thinking counts toward this too.
            "max_tokens": MAX_OUTPUT_TOKENS,
            "system": [{"type": "text", "text": self.system, "cache_control": {"type": "ephemeral"}}],
            "tools": tools,
            "messages": self.history,
        });
        if !self.cfg.effort.is_empty() {
            body["output_config"] = json!({"effort": self.cfg.effort});
        }
        // Streamed: bytes keep arriving while the model thinks, so a long
        // reply is not cut off by a proxy waiting for an idle connection.
        body["stream"] = json!(true);
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
            let streamed = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.starts_with("text/event-stream"));
            if code.is_success() && streamed {
                match read_stream(r).await {
                    // Busy, said inside the stream: as for a busy status, try again.
                    Err(e) if e.to_string().contains("(overloaded_error)") && tries < 4 => {
                        self.ui.event(Event::Status {
                            text: "The AI service is busy; trying again…".into(),
                        });
                        tokio::time::sleep(Duration::from_secs(2u64.pow(tries))).await;
                        continue;
                    }
                    other => return other,
                }
            }
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

    async fn call_tool(&self, name: &str, input: Value) -> (String, bool, String, u32, u32, Made) {
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
                let made = if is_error {
                    Made::default()
                } else {
                    let mut m = made(r.structured_content.as_ref());
                    m.images = pngs(&r.content);
                    m
                };
                (cut(&text), is_error, summary, fails, warns, made)
            }
            Err(e) => {
                let t = format!("the tool call failed: {e}");
                (t.clone(), true, first_line(&t, 160), 0, 0, Made::default())
            }
        }
    }

    /// Keep the conversation within [`HISTORY_CHARS`]: replace the oldest
    /// tool results with a note.
    fn trim_history(&mut self) {
        let size = |h: &Vec<Value>| h.iter().map(|m| m.to_string().len()).sum::<usize>();
        let last = self.history.len().saturating_sub(2);
        // Pictures go first: an earlier one is the biggest thing to keep,
        // and the words of every result are worth more.
        if size(&self.history) > HISTORY_CHARS {
            for m in self.history.iter_mut().take(last) {
                for b in m["content"].as_array_mut().into_iter().flatten() {
                    if let Some(parts) = b["content"].as_array_mut() {
                        for p in parts.iter_mut().filter(|p| p["type"] == "image") {
                            *p = json!({"type": "text", "text": "[An earlier picture, removed to keep the conversation short.]"});
                        }
                    }
                }
            }
        }
        let mut total = size(&self.history);
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

    /// Which model and effort the next message uses. A different model
    /// does not get the earlier one's thinking: it is dropped from the
    /// conversation so far (the answers and tool calls stay).
    pub fn set_model(&mut self, model: &str, effort: &str) -> Result<()> {
        if !model_ok(model) {
            bail!("unknown model");
        }
        if !effort_ok(effort) {
            bail!("unknown thinking level");
        }
        if model != self.cfg.model {
            drop_thinking(&mut self.history);
        }
        self.cfg.model = model.to_string();
        self.cfg.effort = effort.to_string();
        Ok(())
    }

    pub fn model(&self) -> (&str, &str) {
        (&self.cfg.model, &self.cfg.effort)
    }

    pub async fn close(self) {
        let _ = self.client.cancel().await;
    }
}

/// Most bytes of one streamed reply kept.
const MAX_STREAM: usize = 32 * 1024 * 1024;

/// A streamed reply (server-sent events) put together as the whole message
/// the service would have answered without streaming.
pub async fn read_stream(mut r: reqwest::Response) -> Result<Value> {
    let mut buf: Vec<u8> = Vec::new();
    let mut seen = 0usize;
    let mut s = Assembly::default();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(180), r.chunk())
            .await
            .map_err(|_| anyhow!("the AI service stopped sending in the middle of a reply"))?
            .map_err(|e| anyhow!("the reply was cut off: {}", plain(&e)))?;
        let Some(chunk) = chunk else { break };
        seen += chunk.len();
        if seen > MAX_STREAM {
            bail!("the reply was larger than Columbus takes");
        }
        // Only the new bytes (and the three before them) can end an event.
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk);
        let mut start = from;
        while let Some(end) = find_event_end(&buf[start..]).map(|e| e + start) {
            start = 0;
            let event: Vec<u8> = buf.drain(..end).collect();
            for line in String::from_utf8_lossy(&event).lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(data.trim_start()) {
                        if s.take(v)? {
                            return Ok(s.message());
                        }
                    }
                }
            }
        }
    }
    // A stream that ends before message_stop is not a whole reply: a tool
    // call in it may be cut off.
    bail!(if s.started {
        "the reply was cut off before it ended"
    } else {
        "the AI service sent no reply"
    })
}

/// Where the first event in `buf` ends (after its blank line).
fn find_event_end(buf: &[u8]) -> Option<usize> {
    let a = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    let b = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

#[derive(Default)]
struct Assembly {
    started: bool,
    message: Value,
    blocks: Vec<Value>,
    json: Vec<String>,
}

impl Assembly {
    /// One event; true when the message is complete.
    fn take(&mut self, v: Value) -> Result<bool> {
        let at = |v: &Value| v["index"].as_u64().map(|i| i as usize);
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                self.started = true;
                self.message = v["message"].clone();
            }
            "content_block_start" => {
                let Some(i) = at(&v) else { return Ok(false) };
                if i > 1000 {
                    bail!("the reply had too many parts");
                }
                while self.blocks.len() <= i {
                    self.blocks.push(Value::Null);
                    self.json.push(String::new());
                }
                self.blocks[i] = v["content_block"].clone();
            }
            "content_block_delta" => {
                let Some(i) = at(&v).filter(|i| *i < self.blocks.len()) else {
                    return Ok(false);
                };
                let d = &v["delta"];
                let b = &mut self.blocks[i];
                let append = |b: &mut Value, k: &str, t: &str| {
                    let now = b[k].as_str().unwrap_or("").to_string();
                    b[k] = json!(now + t);
                };
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => append(b, "text", d["text"].as_str().unwrap_or("")),
                    "thinking_delta" => append(b, "thinking", d["thinking"].as_str().unwrap_or("")),
                    "signature_delta" => {
                        append(b, "signature", d["signature"].as_str().unwrap_or(""))
                    }
                    "input_json_delta" => {
                        self.json[i].push_str(d["partial_json"].as_str().unwrap_or(""))
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(i) = at(&v).filter(|i| *i < self.blocks.len()) else {
                    return Ok(false);
                };
                if self.blocks[i]["type"] == "tool_use"
                    || self.blocks[i]["type"] == "server_tool_use"
                {
                    let j = std::mem::take(&mut self.json[i]);
                    self.blocks[i]["input"] = if j.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&j).map_err(|_| {
                            anyhow!("a tool call arrived incomplete, so it was not run")
                        })?
                    };
                }
            }
            "message_delta" => {
                for (k, x) in v["delta"].as_object().into_iter().flatten() {
                    self.message[k] = x.clone();
                }
                if let Some(u) = v["usage"].as_object() {
                    for (k, x) in u {
                        self.message["usage"][k] = x.clone();
                    }
                }
            }
            "message_stop" => return Ok(true),
            "error" => bail!(
                "the AI service answered ({}): {}",
                v["error"]["type"].as_str().unwrap_or("error"),
                v["error"]["message"].as_str().unwrap_or("an error")
            ),
            _ => {}
        }
        Ok(false)
    }

    fn message(&mut self) -> Value {
        let mut m = std::mem::take(&mut self.message);
        if !m.is_object() {
            m = json!({});
        }
        m["content"] = Value::Array(
            std::mem::take(&mut self.blocks)
                .into_iter()
                .filter(|b| !b.is_null())
                .collect(),
        );
        m
    }
}

/// Remove thinking from the assistant's earlier turns.
fn drop_thinking(history: &mut [Value]) {
    for m in history.iter_mut().filter(|m| m["role"] == "assistant") {
        if let Some(blocks) = m["content"].as_array_mut() {
            blocks.retain(|b| b["type"] != "thinking" && b["type"] != "redacted_thinking");
            if blocks.is_empty() {
                blocks.push(json!({"type": "text", "text": "(thinking only)"}));
            }
        }
    }
}

/// A tool result with its pictures, as the AI service takes them.
fn tool_result_with(id: &str, text: &str, is_error: bool, images: &[String]) -> Value {
    if images.is_empty() || is_error {
        return tool_result(id, text, is_error);
    }
    let mut content = vec![json!({"type": "text", "text": text})];
    for d in images {
        content.push(json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": d}}));
    }
    json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": false})
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

/// What a result made, for the chat's provenance link.
#[derive(Debug, Default)]
struct Made {
    provenance: Option<String>,
    handle: Option<String>,
    /// PNG pictures in the result, base64, for the model to see.
    images: Vec<String>,
}

/// Largest picture passed on to the model, as base64 characters.
const MAX_IMAGE_B64: usize = 5_000_000;

/// The PNG pictures in a tool's result: base64 only, at most two, each of
/// a size the AI service takes.
fn pngs(content: &[rmcp::model::ContentBlock]) -> Vec<String> {
    content
        .iter()
        .filter_map(|c| c.as_image())
        .filter(|i| i.mime_type == "image/png")
        .map(|i| i.data.clone())
        .filter(|d| {
            !d.is_empty()
                && d.len() <= MAX_IMAGE_B64
                && d.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        })
        .take(2)
        .collect()
}

/// The provenance record and layer a result names, if any. Plain ids only:
/// anything else is ignored.
fn made(v: Option<&Value>) -> Made {
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= 200
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-./:".contains(c))
    };
    let Some(v) = v else {
        return Made::default();
    };
    // A tool run through `run_tool` (a toolbox's, the person's own, a
    // connected server's) lists the files it saved under `result.saved`:
    // the first layer or table among them (what a table can show), else
    // the first file.
    let saved = || {
        let files = v["result"]["saved"].as_array()?;
        files
            .iter()
            .find(|f| matches!(f["kind"].as_str(), Some("layer" | "table")))
            .or(files.first())?["handle"]
            .as_str()
    };
    Made {
        provenance: v["provenance"]
            .as_str()
            .filter(|s| plain(s))
            .map(String::from),
        handle: v["result"]["layer"]["handle"]
            .as_str()
            .or(v["result"]["handle"].as_str())
            .or_else(saved)
            .filter(|s| plain(s))
            .map(String::from),
        images: vec![],
    }
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

    #[test]
    fn a_result_names_the_layer_or_the_file_it_made() {
        let handle = |v: Value| made(Some(&v)).handle;
        // A reading or processing tool: its layer.
        let v = json!({"result": {"layer": {"handle": "ws://s/results/roads-p_1a2b3c4d.gpkg"}}, "provenance": "p_1a2b3c4d"});
        let m = made(Some(&v));
        assert_eq!(
            m.handle.as_deref(),
            Some("ws://s/results/roads-p_1a2b3c4d.gpkg")
        );
        assert_eq!(m.provenance.as_deref(), Some("p_1a2b3c4d"));
        // A toolbox's tool, run through run_tool: the file it saved.
        let copy = "ws://s/results/geolibre-vector_copy-p_5e6f7a8b-output.gpkg";
        let v = json!({"result": {"provider": "geolibre", "tool": "vector_copy", "status": "done",
            "saved": [{"handle": copy, "bytes": 98304, "kind": "layer"}]}, "provenance": "p_5e6f7a8b"});
        assert_eq!(handle(v).as_deref(), Some(copy));
        // Several files: the first one a table can show (a layer or a table).
        let v = json!({"result": {"saved": [
            {"handle": "ws://s/results/t-hillshade.tif", "bytes": 1, "kind": "raster"},
            {"handle": "ws://s/results/t-report.txt", "bytes": 1, "kind": "file"},
            {"handle": "ws://s/results/t-stats.csv", "bytes": 1, "kind": "table"},
            {"handle": "ws://s/results/t-lines.gpkg", "bytes": 1, "kind": "layer"}]}});
        assert_eq!(handle(v).as_deref(), Some("ws://s/results/t-stats.csv"));
        // None of them has rows: the first file.
        let v = json!({"result": {"saved": [
            {"handle": "ws://s/results/t-hillshade.tif", "bytes": 1, "kind": "raster"},
            {"handle": "ws://s/external/vault.report-p_1-0.json", "bytes": 1, "kind": "application/json"}]}});
        assert_eq!(handle(v).as_deref(), Some("ws://s/results/t-hillshade.tif"));
        // The layer a result names itself comes first.
        let v = json!({"result": {"layer": {"handle": "ws://s/results/a.gpkg"},
            "saved": [{"handle": "ws://s/results/b.gpkg", "kind": "layer"}]}});
        assert_eq!(handle(v).as_deref(), Some("ws://s/results/a.gpkg"));
        // Nothing made or saved, or a handle that is not a plain id: none.
        for v in [
            json!({"result": {"status": "done"}}),
            json!({"result": {"saved": []}}),
            json!({"result": {"saved": "ws://s/results/a.gpkg"}}),
            json!({"result": {"saved": [{"kind": "layer"}]}}),
            json!({"result": {"saved": [{"handle": "ws://s/results/<b>.gpkg", "kind": "layer"}]}}),
        ] {
            assert_eq!(handle(v.clone()), None, "{v}");
        }
        assert_eq!(made(None).handle, None);
    }

    #[test]
    fn the_project_is_told_to_the_assistant() {
        assert!(project_text(&Value::Null).is_none());
        let t = project_text(&json!({"name": "Parcel review", "connections": ["county"], "export_folder": "outbox", "instructions": "Report areas in acres."})).unwrap();
        assert!(
            t.contains("\"Parcel review\"")
                && t.contains("county")
                && t.contains("outbox")
                && t.contains("acres"),
            "{t}"
        );
        let t = project_text(
            &json!({"name": "All", "connections": "all of yours", "instructions": ""}),
        )
        .unwrap();
        assert!(
            !t.contains("only these connections") && !t.contains("instructions"),
            "{t}"
        );
    }

    #[test]
    fn an_opened_run_and_its_report_are_told_to_the_assistant() {
        assert!(run_text(&Value::Null).is_none());
        let t = run_text(&json!({"workflow": "row-widths", "run": "2026-11-06T06-00-00-000-1a2b", "name": "ROW widths",
            "layers": [{"step": "fixed", "handle": "ws://s/results/row-widths-fixed.gpkg", "rows": 12}],
            "report": "# ROW widths\n\nSucceeded."}))
        .unwrap();
        assert!(
            t.contains("\"ROW widths\"")
                && t.contains("ws://s/results/row-widths-fixed.gpkg (12 rows)")
                && t.contains("never changed")
                && t.contains("# ROW widths\n\nSucceeded."),
            "{t}"
        );
    }

    #[test]
    fn a_tool_call_cut_off_in_the_stream_is_refused() {
        let mut s = Assembly::default();
        s.take(json!({"type": "message_start", "message": {"role": "assistant", "content": []}}))
            .unwrap();
        s.take(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "sql_select", "input": {}}})).unwrap();
        s.take(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"sql\": \"select"}})).unwrap();
        let e = s
            .take(json!({"type": "content_block_stop", "index": 0}))
            .unwrap_err();
        assert!(e.to_string().contains("incomplete"), "{e}");
    }

    #[test]
    fn an_error_in_the_stream_names_its_type() {
        let mut s = Assembly::default();
        let e = s
            .take(json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}))
            .unwrap_err();
        assert!(e.to_string().contains("(overloaded_error)"), "{e}");
    }
}
