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
//! The chat works with any AI service that speaks one of three request
//! formats (build brief 3, D-22): Anthropic Messages, OpenAI Responses and
//! OpenAI Chat Completions. One loop here runs every chat: the steps, the
//! tool calls, the caps, Stop and the events the window shows. A format
//! ([`messages`], [`responses`], [`completions`]) owns the conversation in
//! its own shape and turns it into requests; [`services`] lists the
//! services Columbus knows and decides which addresses a key may go to.
//!
//! The API key is held by the app's Rust side only: it is read from the
//! operating system's credential store when a chat starts and is never
//! given to a web page or written to a file. It is sent only to its own
//! service's address, and it is taken out of every text a service sends
//! before that text reaches the window (R-25, R-26).

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

mod http;
mod messages;
pub mod services;
mod wire;

pub use http::{list_models, MAX_LISTED_MODELS};
pub use messages::read_stream;
pub use services::{Format, KeyHeader, KeyRule, Service, EFFORTS, MODELS};

use http::Net;
use wire::{Ask, Outcome, Reply, Stop, Tool, Wire};

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
/// The cap on one reply, thinking and answer together.
pub const MAX_OUTPUT_TOKENS: u32 = 32_000;
/// The longest a busy service may ask Columbus to wait before it asks
/// again; a longer wait is told to the person instead.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// Whether Anthropic's list offers `m` (or did, and still takes it).
pub fn model_ok(m: &str) -> bool {
    services::anthropic_listed().model_ok(m)
}

/// Whether `e` is one of Anthropic's thinking levels.
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
    /// One of the service's thinking levels; "" for the model's default.
    pub effort: String,
    /// The service's address (`https://api.anthropic.com`), or a stand-in
    /// in tests. Checked by [`services::check_address`] when a chat starts.
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

/// The window behind a guard: whatever is shown or asked has the service's
/// key, and any long piece of it, taken out first. The model never has the
/// key, but the service does, and what a service sends is untrusted (R-26):
/// its words, the tool calls it asks for and so the code on an approval
/// card could all quote it. The window writes the transcript from what it
/// is shown, so the transcript is kept clean by the same guard.
struct Guarded {
    inner: Arc<dyn Ui>,
    key: String,
}

impl Guarded {
    fn clean(&self, s: String) -> String {
        match http::scrub(&s, &self.key) {
            std::borrow::Cow::Borrowed(_) => s,
            std::borrow::Cow::Owned(t) => t,
        }
    }
}

impl Ui for Guarded {
    fn event(&self, e: Event) {
        let c = |s: String| self.clean(s);
        self.inner.event(match e {
            Event::Text { text } => Event::Text { text: c(text) },
            Event::ToolStarted { id, name, input } => Event::ToolStarted {
                id: c(id),
                name: c(name),
                input: http::scrub_json(&input, &self.key),
            },
            Event::ToolFinished {
                id,
                name,
                ok,
                summary,
                fails,
                warns,
                provenance,
                handle,
                image,
            } => Event::ToolFinished {
                id: c(id),
                name: c(name),
                ok,
                summary: c(summary),
                fails,
                warns,
                provenance: provenance.map(c),
                handle: handle.map(c),
                image,
            },
            Event::Approval {
                id,
                title,
                summary,
                code,
                offer_always,
            } => Event::Approval {
                id,
                title: c(title),
                summary: c(summary),
                code: code.map(c),
                offer_always,
            },
            Event::Status { text } => Event::Status { text: c(text) },
            Event::Error { text } => Event::Error { text: c(text) },
            e @ (Event::ApprovalGone { .. } | Event::Done) => e,
        });
    }

    fn approve(&self, p: Prompt) -> (u64, oneshot::Receiver<Answer>) {
        self.inner.approve(Prompt {
            title: self.clean(p.title),
            summary: self.clean(p.summary),
            code: p.code.map(|c| self.clean(c)),
            offer_always: p.offer_always,
        })
    }

    fn withdraw(&self, id: u64) {
        self.inner.withdraw(id);
    }
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
    tools: Vec<Tool>,
    /// The conversation, in the service's own format.
    wire: Box<dyn Wire>,
    system: String,
    cfg: Config,
    ui: Arc<dyn Ui>,
    net: Net,
    /// Requests go without the fields the format can do without: the
    /// service refused one that carried them, and took one that did not.
    bare: AtomicBool,
    /// The one second try a chat makes has been made.
    tried_bare: AtomicBool,
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
            "\n- step {}: {} ({})",
            l["step"].as_str().unwrap_or(""),
            l["handle"].as_str().unwrap_or(""),
            l["rows"]
                .as_u64()
                .map(rows)
                .unwrap_or_else(|| "? rows".into())
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

/// Keys of AI services a person may have in their environment: none is
/// ever handed on to the session's server or its workers.
const KEY_VARIABLES: &[&str] = &[
    "COLUMBUS_APP_API_KEY",
    "COLUMBUS_APP_API_BASE",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "XAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENROUTER_API_KEY",
    "OLLAMA_API_KEY",
    "LM_API_TOKEN",
    "MISTRAL_API_KEY",
    "DEEPSEEK_API_KEY",
    "GROQ_API_KEY",
    "TOGETHER_API_KEY",
];

impl Chat {
    /// Start a session of `cfg.server` for this chat, on Anthropic with
    /// the models Columbus lists. `cancel` stops its replies, and declines
    /// prompts that arrive after a stop.
    pub async fn start(cfg: Config, ui: Arc<dyn Ui>, cancel: Cancel) -> Result<Chat> {
        Chat::start_with(cfg, services::anthropic_listed(), ui, cancel).await
    }

    /// Start a session of `cfg.server` for a chat on `service`, at the
    /// address `cfg.api_base`. The chat keeps this service to its end.
    pub async fn start_with(
        cfg: Config,
        service: Service,
        ui: Arc<dyn Ui>,
        cancel: Cancel,
    ) -> Result<Chat> {
        if service.key == KeyRule::Needed && cfg.api_key.trim().is_empty() {
            bail!("no API key is set");
        }
        if !service.model_ok(&cfg.model) {
            bail!("unknown model");
        }
        if !service.effort_ok(&cfg.effort) {
            bail!("unknown thinking level");
        }
        // Where the key may go is decided before anything is started.
        let net = Net::new(&cfg.api_base, &cfg.api_key, service)?;
        let ui: Arc<dyn Ui> = Arc::new(Guarded {
            inner: ui,
            key: net.key().to_string(),
        });
        let server = cfg.server.clone();
        let env = cfg.server_env.clone();
        let cmd = tokio::process::Command::new(&server).configure(move |c| {
            c.arg("serve");
            // The key is the app's alone: never in the session's
            // environment, and so never in its workers'.
            for k in KEY_VARIABLES {
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
            .map(|t| Tool {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                schema: Value::Object((*t.input_schema).clone()),
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
        let wire: Box<dyn Wire> = match net.service.format {
            Format::Messages => Box::new(messages::Messages::new(net.service.pictures)),
            Format::Responses | Format::Chat => {
                bail!("this build of the chat does not speak that format yet")
            }
        };
        Ok(Chat {
            client,
            session,
            tools,
            wire,
            system,
            cfg,
            ui,
            net,
            bare: AtomicBool::new(false),
            tried_bare: AtomicBool::new(false),
        })
    }

    pub fn tool_count(&self) -> usize {
        self.tools.len()
    }

    /// The Columbus session this chat works in.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// The service this chat talks to, from its first message to its last.
    pub fn service(&self) -> &Service {
        &self.net.service
    }

    /// The conversation as the service is sent it, in its own format.
    pub fn history(&self) -> &[Value] {
        self.wire.history()
    }

    /// The person's message; runs until the model has answered, a limit is
    /// reached or the reply is cancelled. Events go to the window as it goes.
    pub async fn send(&mut self, text: &str, cancel: &Cancel) -> Result<()> {
        cancel.reset();
        self.wire.user(text);
        // Whatever went wrong, its words reach the window without the key.
        let r = self.run(cancel).await.map_err(|e| {
            let said = e.to_string();
            match http::scrub(&said, self.net.key()) {
                std::borrow::Cow::Borrowed(_) => e,
                std::borrow::Cow::Owned(clean) => anyhow!(clean),
            }
        });
        match &r {
            Ok(()) => self.ui.event(Event::Done),
            Err(e) => self.ui.event(Event::Error {
                text: e.to_string(),
            }),
        }
        r
    }

    fn ask_now(&self) -> Ask<'_> {
        Ask {
            model: &self.cfg.model,
            effort: &self.cfg.effort,
            system: &self.system,
            tools: &self.tools,
            bare: self.bare.load(Ordering::SeqCst),
        }
    }

    async fn run(&mut self, cancel: &Cancel) -> Result<()> {
        let mut used: u64 = 0;
        // Whether any of `used` is Columbus's own estimate.
        let mut estimated = false;
        for _ in 0..MAX_STEPS {
            if used > MAX_INPUT_TOKENS {
                let how = if estimated {
                    format!(
                        " Columbus worked that out from the size of what it sent, because {} gave no count.",
                        self.net.service.name
                    )
                } else {
                    String::new()
                };
                self.ui.event(Event::Status {
                    text: format!(
                        "Stopped: this message has used about {} thousand input tokens.{how} Ask to continue if it should go on.",
                        used / 1000
                    ),
                });
                return Ok(());
            }
            self.wire.trim();
            let (answer, estimate) = tokio::select! {
                r = self.ask_model() => r?,
                _ = cancel.cancelled() => {
                    self.wire.forget_unanswered();
                    bail!("stopped")
                }
            };
            let reply = {
                let ask = Ask {
                    model: &self.cfg.model,
                    effort: &self.cfg.effort,
                    system: &self.system,
                    tools: &self.tools,
                    bare: self.bare.load(Ordering::SeqCst),
                };
                self.wire.reply(answer, &ask)
            };
            let Reply {
                texts,
                calls,
                stop,
                input_tokens,
            } = reply.map_err(|e| self.named(e))?;
            match input_tokens {
                Some(n) => used += n,
                None => {
                    used += estimate;
                    estimated = true;
                }
            }
            for t in texts {
                self.ui.event(Event::Text { text: t });
            }
            if calls.is_empty() || stop != Stop::Tools {
                if stop == Stop::Length {
                    self.ui.event(Event::Status {
                        text: "The reply reached its length limit.".into(),
                    });
                }
                return Ok(());
            }
            let mut results = Vec::new();
            for (n, call) in calls.into_iter().enumerate() {
                let wire::Call {
                    id,
                    name,
                    input,
                    fault,
                } = call;
                let not_run = |text: String| Outcome {
                    id: id.clone(),
                    name: name.clone(),
                    text,
                    is_error: true,
                    images: vec![],
                };
                if n >= MAX_CALLS_PER_STEP {
                    results.push(not_run(format!(
                        "Not run: at most {MAX_CALLS_PER_STEP} tool calls per step."
                    )));
                    continue;
                }
                if cancel.is_cancelled() {
                    results.push(not_run("Stopped by the person before this ran.".into()));
                    continue;
                }
                // What a service sends is untrusted (R-26): a call runs
                // only if it names one of the session's tools and its
                // arguments arrived whole, and never if it carries the key.
                let shown: String = name.chars().take(80).collect();
                let refused = fault
                    .or_else(|| {
                        (!self.tools.iter().any(|t| t.name == name)).then(|| {
                            format!("Columbus has no tool named \"{shown}\", so nothing was run")
                        })
                    })
                    .or_else(|| {
                        http::holds_key(&input, self.net.key()).then(|| {
                            "this tool call carried the AI service's key, so it was not run"
                                .to_string()
                        })
                    });
                if let Some(why) = refused {
                    self.ui.event(Event::ToolStarted {
                        id: id.clone(),
                        name: shown.clone(),
                        input: json!({}),
                    });
                    self.ui.event(Event::ToolFinished {
                        id: id.clone(),
                        name: shown,
                        ok: false,
                        summary: why.clone(),
                        fails: 0,
                        warns: 0,
                        provenance: None,
                        handle: None,
                        image: None,
                    });
                    results.push(not_run(format!(
                        "Not run: {why}. Call one of the tools you were given, with its arguments as one whole JSON object."
                    )));
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
                // A picture is shown to the person as what the assistant
                // saw: only when the service's models are sent it.
                let seen = self.net.service.pictures && !is_error;
                self.ui.event(Event::ToolFinished {
                    id: id.clone(),
                    name: name.clone(),
                    ok: !is_error,
                    summary,
                    fails,
                    warns,
                    provenance: made.provenance,
                    handle: made.handle,
                    image: made
                        .images
                        .first()
                        .filter(|_| seen)
                        .map(|d| format!("data:image/png;base64,{d}")),
                });
                results.push(Outcome {
                    id,
                    name,
                    text,
                    is_error,
                    images: made.images,
                });
            }
            self.wire.results(results);
            if cancel.is_cancelled() {
                bail!("stopped");
            }
        }
        self.ui.event(Event::Status {
            text: format!("Stopped after {MAX_STEPS} steps; ask to continue if it should go on."),
        });
        Ok(())
    }

    /// An error with the service's name in place of "the AI service",
    /// where the service's own refusal was read from inside a reply.
    fn named(&self, e: anyhow::Error) -> anyhow::Error {
        match e.downcast_ref::<wire::Refused>() {
            Some(r) => anyhow!(
                "{} answered with an error: {}",
                self.net.service.name,
                r.said
            ),
            None => e,
        }
    }

    /// Ask the model for its next reply: the service's whole answer, and
    /// Columbus's own estimate of the tokens the request held.
    async fn ask_model(&self) -> Result<(Value, u64)> {
        let was_bare = self.bare.load(Ordering::SeqCst);
        let r = self.ask_until_answered().await;
        if !was_bare && self.bare.load(Ordering::SeqCst) {
            // This call made the chat's one second try.
            match &r {
                Ok(_) => self.ui.event(Event::Status {
                    text: format!(
                        "{} refused a setting Columbus sends by choice (the thinking level, the limit on a reply's length or the request for token counts), and took the request without it. This chat goes on without those settings.",
                        self.net.service.name
                    ),
                }),
                // They were not what the service minded: they stay.
                Err(_) => self.bare.store(false, Ordering::SeqCst),
            }
        }
        r
    }

    async fn ask_until_answered(&self) -> Result<(Value, u64)> {
        let name = &self.net.service.name;
        let mut tries = 0;
        loop {
            let body = self.wire.body(&self.ask_now());
            let estimate = wire::estimate_tokens(&body.json);
            tries += 1;
            let r = self
                .net
                .post(&body.json)
                .send()
                .await
                .map_err(|e| anyhow!("could not reach {name}: {}", http::plain(e)))?;
            let code = r.status();
            if code.is_redirection() {
                bail!("{}", http::redirect_text(name));
            }
            let streamed = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|t| t.starts_with("text/event-stream"));
            let busy_text = || Event::Status {
                text: format!("{name} is busy; trying again…"),
            };
            if code.is_success() && streamed {
                let mut s = self.wire.assembler();
                match http::read_sse(r, s.as_mut(), self.wire.idle()).await {
                    // Busy, said inside the stream: as for a busy status, try again.
                    Err(e) if self.wire.busy(&e) && tries < 4 => {
                        self.ui.event(busy_text());
                        tokio::time::sleep(Duration::from_secs(2u64.pow(tries))).await;
                        continue;
                    }
                    Err(e) => return Err(self.named(e)),
                    Ok(v) => return Ok((v, estimate)),
                }
            }
            if code.is_success() {
                let bytes = http::read_body(r, http::MAX_STREAM).await?;
                let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                return Ok((v, estimate));
            }
            let headers = r.headers().clone();
            let bytes = http::read_error_body(r).await;
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let said = http::Said::read(&v, &bytes, self.net.key());
            let status = code.as_u16();
            let no_credit = said.no_credit(status);
            // Busy or rate-limited: wait and try again a few times, as long
            // as the service asks for (within reason).
            if (status == 429 || status == 529 || code.is_server_error())
                && !no_credit
                && tries < 4
                && !http::told_not_to_retry(&headers)
            {
                let asked = http::retry_after(&headers);
                if asked.is_none_or(|w| w <= MAX_WAIT) {
                    self.ui.event(busy_text());
                    tokio::time::sleep(
                        asked.unwrap_or_else(|| Duration::from_secs(2u64.pow(tries))),
                    )
                    .await;
                    continue;
                }
            }
            // Refused, with fields in the request the format can do
            // without: one more try without them, once in a chat (build
            // brief 3, A-11). The second answer is the one reported.
            if status == 400
                && body.optional
                && !no_credit
                && !self.tried_bare.swap(true, Ordering::SeqCst)
            {
                self.bare.store(true, Ordering::SeqCst);
                continue;
            }
            bail!(
                "{}",
                http::refusal(
                    name,
                    &self.cfg.model,
                    status,
                    &said,
                    !self.net.key().is_empty()
                )
            );
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

    /// Which model and thinking level the next message uses, within the
    /// chat's own service. A different model does not get the earlier
    /// one's thinking: it is dropped from the conversation so far (the
    /// answers and tool calls stay).
    pub fn set_model(&mut self, model: &str, effort: &str) -> Result<()> {
        if !self.net.service.model_ok(model) {
            bail!("unknown model");
        }
        if !self.net.service.effort_ok(effort) {
            bail!("unknown thinking level");
        }
        if model != self.cfg.model {
            self.wire.model_changed();
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
    // connected server's) lists the files it saved under `result.saved`,
    // each with its kind. A script (`run_python`) lists what it wrote under
    // `result.outputs`, with no kind: there a GeoPackage is a layer and a
    // CSV a table, by the file's ending, as the map page takes them. Of
    // those files: the first layer or table (what a table can show), else
    // the first file.
    let has_rows = |f: &Value| match f["kind"].as_str() {
        Some(kind) => matches!(kind, "layer" | "table"),
        None => f["handle"].as_str().is_some_and(|h| {
            let h = h.to_ascii_lowercase();
            h.ends_with(".gpkg") || h.ends_with(".csv")
        }),
    };
    let listed = || {
        let files = [&v["result"]["saved"], &v["result"]["outputs"]]
            .into_iter()
            .find_map(|l| l.as_array().filter(|a| !a.is_empty()))?;
        files.iter().find(|f| has_rows(f)).or(files.first())?["handle"].as_str()
    };
    Made {
        provenance: v["provenance"]
            .as_str()
            .filter(|s| plain(s))
            .map(String::from),
        handle: v["result"]["layer"]["handle"]
            .as_str()
            .or(v["result"]["handle"].as_str())
            .or_else(listed)
            .filter(|s| plain(s))
            .map(String::from),
        images: vec![],
    }
}

/// A count of rows in words: "1 row", "12 rows".
fn rows(n: u64) -> String {
    if n == 1 {
        "1 row".into()
    } else {
        format!("{n} rows")
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
    let mut counted = false;
    if let Some(n) = r["rows_read"].as_u64().or(r["rows"].as_u64()) {
        parts.push(rows(n));
        counted = true;
    }
    if let Some(n) = r["layer"]["summary"]["rows"].as_u64() {
        if !counted {
            parts.push(rows(n));
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
        // One row is "1 row"; every other count keeps "rows". The count is
        // said once, whether the result gives it, its layer, or both.
        let said = |result: Value| result_summary(Some(&json!({ "result": result })));
        assert_eq!(
            said(
                json!({"rows_read": 1, "layer": {"summary": {"rows": 1, "crs": {"code": "EPSG:3081"}}}})
            ),
            "1 row, EPSG:3081"
        );
        assert_eq!(said(json!({"layer": {"summary": {"rows": 1}}})), "1 row");
        assert_eq!(said(json!({"status": "done", "rows": 1})), "done, 1 row");
        assert_eq!(said(json!({"rows": 0})), "0 rows");
        assert_eq!(said(json!({"layer": {"summary": {"rows": 2}}})), "2 rows");
        assert_eq!(said(json!({"rows_read": 1000})), "1000 rows");
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
        // A script (run_python) lists what it wrote under `outputs`, with
        // no kind: a CSV is a table and a GeoPackage a layer, by the name.
        let counts = "ws://s/results/script-p_9a8b7c6d-counts.csv";
        let v = json!({"result": {"status": "done", "exit_code": 0, "stdout": "", "stderr": "", "code_sha256": "ab12",
            "outputs": [
                {"handle": "ws://s/results/script-p_9a8b7c6d-a-note.txt", "name": "a-note.txt", "bytes": 12, "summary": null},
                {"handle": counts, "name": "counts.csv", "bytes": 24, "summary": {"rows": 3}},
                {"handle": "ws://s/results/script-p_9a8b7c6d-lines.gpkg", "name": "lines.gpkg", "bytes": 98304, "summary": {"rows": 2}}]},
            "provenance": "p_9a8b7c6d"});
        assert_eq!(handle(v).as_deref(), Some(counts));
        let v = json!({"result": {"outputs": [
            {"handle": "ws://s/results/script-p_1-readme.md", "name": "readme.md"},
            {"handle": "ws://s/results/script-p_1-Lines.GPKG", "name": "Lines.GPKG"}]}});
        assert_eq!(
            handle(v).as_deref(),
            Some("ws://s/results/script-p_1-Lines.GPKG")
        );
        // A script that wrote no layer or table: its first file.
        let v = json!({"result": {"outputs": [
            {"handle": "ws://s/results/script-p_1-a.txt", "name": "a.txt"},
            {"handle": "ws://s/results/script-p_1-b.tif", "name": "b.tif"}]}});
        assert_eq!(
            handle(v).as_deref(),
            Some("ws://s/results/script-p_1-a.txt")
        );
        // Nothing made or saved, or a handle that is not a plain id: none.
        for v in [
            json!({"result": {"status": "done"}}),
            json!({"result": {"saved": []}}),
            json!({"result": {"outputs": []}}),
            json!({"result": {"outputs": [{"handle": null, "name": "counts.csv"}]}}),
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
            "layers": [{"step": "fixed", "handle": "ws://s/results/row-widths-fixed.gpkg", "rows": 12},
                {"step": "one", "handle": "ws://s/results/row-widths-one.gpkg", "rows": 1},
                {"step": "unread", "handle": "ws://s/results/row-widths-unread.gpkg"}],
            "report": "# ROW widths\n\nSucceeded."}))
        .unwrap();
        assert!(
            t.contains("\"ROW widths\"")
                && t.contains("ws://s/results/row-widths-fixed.gpkg (12 rows)")
                && t.contains("ws://s/results/row-widths-one.gpkg (1 row)")
                && t.contains("ws://s/results/row-widths-unread.gpkg (? rows)")
                && t.contains("never changed")
                && t.contains("# ROW widths\n\nSucceeded."),
            "{t}"
        );
    }
}
