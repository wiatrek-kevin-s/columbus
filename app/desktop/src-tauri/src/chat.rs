//! The chat's side of the app: settings, the API key in the operating
//! system's credential store, and one chat at a time driving its own
//! Columbus session (see the `columbus-chat` crate). The chat lives in the
//! main window (`MAIN`); each chat's transcript is kept in the app's data
//! folder (`chats/<id>.jsonl`) so chats can be listed and searched.
//!
//! Only the main window may call these commands (capabilities/main.json),
//! and events go to that window only. The API key never reaches a web
//! page: the window can set it, ask whether one is set, or clear it, but
//! not read it.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use columbus_chat::{Answer, Cancel, Chat, Config, Event, Prompt, Ui, MODELS};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{oneshot, Mutex};

/// The window the chat lives in.
pub const MAIN: &str = "main";
const KEY_SERVICE: &str = "columbus-app";
const KEY_ACCOUNT: &str = "anthropic-api-key";

#[derive(Default)]
pub struct ChatState {
    chat: Mutex<Option<Arc<Mutex<Chat>>>>,
    /// The current chat's stop switch; each chat gets its own, so a reply
    /// of an ended chat can never be revived by the next one.
    cancel: std::sync::Mutex<Cancel>,
    /// The chat's Columbus session id, kept apart so it can be read while
    /// a reply holds the chat.
    pub session: std::sync::Mutex<Option<String>>,
    pending: std::sync::Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
    /// Prompt ids, never reused while the app runs.
    next_prompt: std::sync::atomic::AtomicU64,
    /// The current chat's transcript file.
    transcript: std::sync::Mutex<Option<PathBuf>>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub server: Option<PathBuf>,
}

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

pub(crate) fn load(app: &AppHandle) -> Settings {
    settings_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Where Columbus's server usually is: the folder the install guides use.
fn default_server() -> Option<PathBuf> {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let exe = if cfg!(windows) {
        "columbus-mcp.exe"
    } else {
        "columbus-mcp"
    };
    let mut c = Vec::new();
    if let Some(p) = std::env::var_os("COLUMBUS_SERVER").filter(|v| !v.is_empty()) {
        c.push(PathBuf::from(p));
    }
    if cfg!(windows) {
        c.push(PathBuf::from(r"C:\Columbus").join(exe));
    }
    if let Some(h) = home {
        c.push(h.join("Columbus").join(exe));
    }
    c.into_iter().find(|p| p.is_file())
}

pub(crate) fn server(s: &Settings) -> Option<PathBuf> {
    s.server
        .clone()
        .filter(|p| p.is_file())
        .or_else(default_server)
}

/// The system credential store, for the settings window too.
pub(crate) fn ensure_store() -> Result<(), String> {
    store()
}

fn store() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let s = apple_native_keyring_store::keychain::Store::new().map_err(|e| e.to_string())?;
        keyring_core::set_default_store(s);
        Ok(())
    }
    #[cfg(windows)]
    {
        let s = windows_native_keyring_store::Store::new().map_err(|e| e.to_string())?;
        keyring_core::set_default_store(s);
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Err("this system has no credential store Columbus can use yet".into())
    }
}

pub(crate) fn read_key() -> Option<String> {
    // Development and tests only.
    #[cfg(debug_assertions)]
    if let Ok(k) = std::env::var("COLUMBUS_APP_API_KEY") {
        return Some(k);
    }
    store().ok()?;
    keyring_core::Entry::new(KEY_SERVICE, KEY_ACCOUNT)
        .ok()?
        .get_password()
        .ok()
}

#[derive(Serialize)]
pub struct SettingsView {
    has_key: bool,
    model: String,
    models: Vec<(String, String)>,
    server: Option<String>,
    server_found: bool,
    chatting: bool,
}

#[tauri::command]
pub async fn chat_settings(
    app: AppHandle,
    st: State<'_, ChatState>,
) -> Result<SettingsView, String> {
    let s = load(&app);
    let srv = server(&s);
    let has_key =
        tauri::async_runtime::spawn_blocking(|| read_key().is_some_and(|k| !k.is_empty()))
            .await
            .unwrap_or(false);
    Ok(SettingsView {
        has_key,
        model: s.model.clone().unwrap_or_else(|| MODELS[0].0.to_string()),
        models: MODELS
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        server: srv
            .as_ref()
            .map(|p| p.display().to_string())
            .or(s.server.map(|p| p.display().to_string())),
        server_found: srv.is_some(),
        chatting: st.chat.lock().await.is_some(),
    })
}

#[tauri::command]
pub async fn chat_save_settings(
    app: AppHandle,
    model: String,
    server: Option<String>,
) -> Result<(), String> {
    if !MODELS.iter().any(|(m, _)| *m == model) {
        return Err("unknown model".into());
    }
    let s = Settings {
        model: Some(model),
        server: server.filter(|v| !v.trim().is_empty()).map(PathBuf::from),
    };
    let p = settings_path(&app).ok_or("no settings folder")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, serde_json::to_vec_pretty(&s).unwrap_or_default()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn chat_set_key(key: String) -> Result<(), String> {
    let key = key.trim().to_string();
    if key.len() < 20 || key.chars().any(char::is_whitespace) {
        return Err("that does not look like an API key".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        store()?;
        keyring_core::Entry::new(KEY_SERVICE, KEY_ACCOUNT)
            .and_then(|e| e.set_password(&key))
            .map_err(|e| format!("could not save the key: {e}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn chat_clear_key() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        store()?;
        match keyring_core::Entry::new(KEY_SERVICE, KEY_ACCOUNT).and_then(|e| e.delete_credential())
        {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The chat window, as the chat sees it: events go to the `chat` window,
/// approval prompts wait for its answer.
struct WindowUi {
    app: AppHandle,
}

impl Ui for WindowUi {
    fn event(&self, e: Event) {
        record(&self.app, &serde_json::to_value(&e).unwrap_or_default());
        let _ = self.app.emit_to(MAIN, "chat-event", e);
    }
    fn approve(&self, p: Prompt) -> (u64, oneshot::Receiver<Answer>) {
        let (tx, rx) = oneshot::channel();
        let Some(st) = self.app.try_state::<ChatState>() else {
            let _ = tx.send(Answer::Decline);
            return (0, rx);
        };
        let id = st
            .next_prompt
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        // No window to ask in: decline rather than wait unseen.
        let Some(w) = self.app.get_webview_window(MAIN) else {
            let _ = tx.send(Answer::Decline);
            return (id, rx);
        };
        st.pending.lock().expect("pending").insert(id, tx);
        let e = Event::Approval {
            id,
            title: p.title,
            summary: p.summary,
            code: p.code,
            offer_always: p.offer_always,
        };
        record(&self.app, &serde_json::to_value(&e).unwrap_or_default());
        let _ = self.app.emit_to(MAIN, "chat-event", e);
        let _ = w.unminimize();
        let _ = w.set_focus();
        (id, rx)
    }
    fn withdraw(&self, id: u64) {
        if let Some(st) = self.app.try_state::<ChatState>() {
            st.pending.lock().expect("pending").remove(&id);
        }
        let e = Event::ApprovalGone { id };
        record(&self.app, &serde_json::to_value(&e).unwrap_or_default());
        let _ = self.app.emit_to(MAIN, "chat-event", e);
    }
}

#[derive(Serialize)]
pub struct Started {
    session: Option<String>,
    tools: usize,
    /// The transcript's id, under which the chat is listed.
    chat: Option<String>,
}

/// Start a chat (its own Columbus session) if none is running.
#[tauri::command]
pub async fn chat_start(app: AppHandle, st: State<'_, ChatState>) -> Result<Started, String> {
    let mut slot = st.chat.lock().await;
    if slot.is_some() {
        // Already running (a reply may be holding it): answer without
        // waiting for the reply.
        return Ok(Started {
            session: st.session.lock().expect("session").clone(),
            tools: 0,
            chat: current_id(&st),
        });
    }
    let s = load(&app);
    let srv = server(&s).ok_or(
        "Columbus's server was not found. Unpack the Columbus folder (see the install guide), or set its place in Settings.",
    )?;
    let key = tauri::async_runtime::spawn_blocking(read_key)
        .await
        .ok()
        .flatten()
        .filter(|k| !k.is_empty())
        .ok_or("No API key is set. Add one in Settings.")?;
    #[allow(unused_mut)]
    let mut api_base = "https://api.anthropic.com".to_string();
    #[cfg(debug_assertions)]
    if let Ok(b) = std::env::var("COLUMBUS_APP_API_BASE") {
        api_base = b;
    }
    let cfg = Config {
        server: srv,
        server_env: vec![],
        api_key: key,
        model: s.model.unwrap_or_else(|| MODELS[0].0.to_string()),
        api_base,
    };
    let cancel = Cancel::default();
    *st.cancel.lock().expect("cancel") = cancel.clone();
    let chat = Chat::start(cfg, Arc::new(WindowUi { app: app.clone() }), cancel)
        .await
        .map_err(|e| e.to_string())?;
    let session = chat.session().map(String::from);
    let id = new_transcript(&app, &st, session.as_deref(), &cfg_model(&app));
    let started = Started {
        session,
        tools: chat.tool_count(),
        chat: id,
    };
    *st.session.lock().expect("session") = started.session.clone();
    *slot = Some(Arc::new(Mutex::new(chat)));
    Ok(started)
}

/// Send the person's message; the reply arrives as events.
#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    st: State<'_, ChatState>,
    text: String,
) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Ok(());
    }
    let chat = st
        .chat
        .lock()
        .await
        .clone()
        .ok_or("the chat has not started")?;
    record(&app, &serde_json::json!({"kind": "you", "text": text}));
    let cancel = st.cancel.lock().expect("cancel").clone();
    tauri::async_runtime::spawn(async move {
        let Ok(mut c) = chat.try_lock() else {
            let _ = app.emit_to(
                MAIN,
                "chat-event",
                Event::Error {
                    text: "a reply is still in progress".into(),
                },
            );
            return;
        };
        let _ = c.send(&text, &cancel).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn chat_stop(app: AppHandle, st: State<'_, ChatState>) -> Result<(), String> {
    stop(&app, &st);
    Ok(())
}

/// Stop the reply; a prompt waiting for the person is declined and its card
/// withdrawn.
fn stop(app: &AppHandle, st: &ChatState) {
    st.cancel.lock().expect("cancel").cancel();
    let drained: Vec<_> = st.pending.lock().expect("pending").drain().collect();
    for (id, tx) in drained {
        let _ = tx.send(Answer::Decline);
        record(app, &serde_json::json!({"kind": "approval_gone", "id": id}));
        let _ = app.emit_to(MAIN, "chat-event", Event::ApprovalGone { id });
    }
}

#[tauri::command]
pub async fn chat_answer(
    app: AppHandle,
    st: State<'_, ChatState>,
    id: u64,
    decision: String,
) -> Result<(), String> {
    let answer = match decision.as_str() {
        "allow_once" => Answer::AllowOnce,
        "always_this_session" => Answer::AlwaysThisSession,
        _ => Answer::Decline,
    };
    let tx = st
        .pending
        .lock()
        .expect("pending")
        .remove(&id)
        .ok_or("that request is no longer waiting")?;
    let _ = tx.send(answer);
    let decided = match answer {
        Answer::AllowOnce => "allow_once",
        Answer::AlwaysThisSession => "always_this_session",
        Answer::Decline => "decline",
    };
    record(
        &app,
        &serde_json::json!({"kind": "answer", "id": id, "decision": decided}),
    );
    Ok(())
}

/// End the chat and its session.
#[tauri::command]
pub async fn chat_end(app: AppHandle, st: State<'_, ChatState>) -> Result<(), String> {
    end(&app, &st).await;
    Ok(())
}

/// End the chat when the main window closes: nothing runs unseen.
pub fn on_chat_window_closed(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(st) = app.try_state::<ChatState>() {
            end(&app, &st).await;
        }
    });
}

async fn end(app: &AppHandle, st: &ChatState) {
    stop(app, st);
    let chat = st.chat.lock().await.take();
    *st.session.lock().expect("session") = None;
    *st.transcript.lock().expect("transcript") = None;
    if let Some(c) = chat {
        // A reply still holding the chat stops at the cancel; close the
        // session once it lets go.
        let m = c.lock().await;
        drop(m);
        if let Ok(m) = Arc::try_unwrap(c) {
            m.into_inner().close().await;
        }
    }
}

// ---------- Transcripts ----------
//
// One JSON line per event, in `chats/<id>.jsonl` under the app's data
// folder: the person's messages, the model's words, tool steps, approval
// prompts and answers, exactly as the window showed them. Plain files the
// person owns; deleting one deletes the chat. Nothing here is sent
// anywhere.

/// Most bytes of one transcript the app reads back (a very long chat shows
/// its start).
const MAX_TRANSCRIPT: u64 = 8 * 1024 * 1024;

fn chats_dir(app: &AppHandle) -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(d) = std::env::var_os("COLUMBUS_APP_CHATS").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(d));
    }
    app.path().app_data_dir().ok().map(|d| d.join("chats"))
}

/// A transcript id: `c` + time + random, letters, digits and `-` only.
pub(crate) fn id_ok(id: &str) -> bool {
    id.len() >= 8
        && id.len() <= 64
        && id.starts_with('c')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn cfg_model(app: &AppHandle) -> String {
    load(app).model.unwrap_or_else(|| MODELS[0].0.to_string())
}

fn current_id(st: &ChatState) -> Option<String> {
    st.transcript
        .lock()
        .expect("transcript")
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(String::from)
}

fn new_transcript(
    app: &AppHandle,
    st: &ChatState,
    session: Option<&str>,
    model: &str,
) -> Option<String> {
    let dir = chats_dir(app)?;
    std::fs::create_dir_all(&dir).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let id = format!("c{now}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let path = dir.join(format!("{id}.jsonl"));
    *st.transcript.lock().expect("transcript") = Some(path);
    record(
        app,
        &serde_json::json!({"kind": "meta", "id": id, "session": session, "model": model}),
    );
    Some(id)
}

/// Append one event to the current transcript, with the time. A transcript
/// that cannot be written is skipped: the chat goes on.
fn record(app: &AppHandle, v: &serde_json::Value) {
    let Some(st) = app.try_state::<ChatState>() else {
        return;
    };
    let Some(path) = st.transcript.lock().expect("transcript").clone() else {
        return;
    };
    let mut v = v.clone();
    if let Some(m) = v.as_object_mut() {
        m.insert("ts".into(), serde_json::json!(now_rfc3339()));
    }
    let Ok(mut line) = serde_json::to_string(&v) else {
        return;
    };
    line.push('\n');
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// The time now as RFC 3339 in UTC (no clock crate needed).
pub(crate) fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    use std::io::{BufRead, Read};
    let Ok(f) = std::fs::File::open(path) else {
        return vec![];
    };
    std::io::BufReader::new(f.take(MAX_TRANSCRIPT))
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

#[derive(Serialize)]
pub struct ChatSummary {
    id: String,
    title: String,
    session: Option<String>,
    model: Option<String>,
    started: Option<String>,
    updated: Option<String>,
    messages: usize,
    /// Projects are not built yet: every chat is unfiled.
    project: Option<String>,
    live: bool,
}

fn summary(id: &str, lines: &[serde_json::Value], live: bool) -> ChatSummary {
    let meta = lines.iter().find(|l| l["kind"] == "meta");
    let title = lines
        .iter()
        .rev()
        .find(|l| l["kind"] == "title")
        .and_then(|l| l["text"].as_str())
        .or_else(|| {
            lines
                .iter()
                .find(|l| l["kind"] == "you")
                .and_then(|l| l["text"].as_str())
        })
        .map(|t| {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.chars().count() > 70 {
                format!("{}…", t.chars().take(70).collect::<String>())
            } else {
                t
            }
        })
        .unwrap_or_else(|| "New chat".into());
    ChatSummary {
        id: id.to_string(),
        title,
        session: meta.and_then(|m| m["session"].as_str()).map(String::from),
        model: meta.and_then(|m| m["model"].as_str()).map(String::from),
        started: lines
            .first()
            .and_then(|l| l["ts"].as_str())
            .map(String::from),
        updated: lines
            .last()
            .and_then(|l| l["ts"].as_str())
            .map(String::from),
        messages: lines.iter().filter(|l| l["kind"] == "you").count(),
        project: None,
        live,
    }
}

/// The saved chats, newest first.
#[tauri::command]
pub async fn chats_list(
    app: AppHandle,
    st: State<'_, ChatState>,
) -> Result<Vec<ChatSummary>, String> {
    let current = current_id(&st);
    let Some(dir) = chats_dir(&app) else {
        return Ok(vec![]);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = vec![];
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let Some(id) = p
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".jsonl"))
                .filter(|id| id_ok(id))
            else {
                continue;
            };
            let lines = read_lines(&p);
            if !lines.iter().any(|l| l["kind"] == "you") && current.as_deref() != Some(id) {
                continue;
            }
            out.push(summary(id, &lines, current.as_deref() == Some(id)));
        }
        out.sort_by(|a, b| b.updated.cmp(&a.updated));
        out
    })
    .await
    .map_err(|e| e.to_string())
}

/// One saved chat, every line as the window showed it.
#[tauri::command]
pub async fn chat_transcript(app: AppHandle, id: String) -> Result<Vec<serde_json::Value>, String> {
    if !id_ok(&id) {
        return Err("not a chat".into());
    }
    let dir = chats_dir(&app).ok_or("no folder for chats")?;
    tauri::async_runtime::spawn_blocking(move || read_lines(&dir.join(format!("{id}.jsonl"))))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct Found {
    id: String,
    title: String,
    updated: Option<String>,
    /// A few words around the first match, as plain text.
    snippet: String,
}

/// Search the saved chats' titles and content (what the person and the
/// model wrote, and the tool steps), case-insensitively.
#[tauri::command]
pub async fn chats_search(app: AppHandle, query: String) -> Result<Vec<Found>, String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() || q.chars().count() > 200 {
        return Ok(vec![]);
    }
    let Some(dir) = chats_dir(&app) else {
        return Ok(vec![]);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = vec![];
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            let Some(id) = p
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".jsonl"))
                .filter(|id| id_ok(id))
                .map(String::from)
            else {
                continue;
            };
            let lines = read_lines(&p);
            let s = summary(&id, &lines, false);
            let texts = std::iter::once(s.title.clone()).chain(lines.iter().filter_map(|l| {
                ["text", "summary", "name", "title"]
                    .iter()
                    .find_map(|k| l[*k].as_str())
                    .map(String::from)
            }));
            for t in texts {
                let lower = t.to_lowercase();
                if let Some(at) = lower.find(&q) {
                    let chars: Vec<char> = t.chars().collect();
                    let start_char = lower[..at].chars().count();
                    let from = start_char.saturating_sub(40);
                    let to = (start_char + q.chars().count() + 60).min(chars.len());
                    let mut snip: String = chars[from..to].iter().collect();
                    snip = snip.split_whitespace().collect::<Vec<_>>().join(" ");
                    out.push(Found {
                        id: id.clone(),
                        title: s.title.clone(),
                        updated: s.updated.clone(),
                        snippet: snip,
                    });
                    break;
                }
            }
            if out.len() >= 50 {
                break;
            }
        }
        out.sort_by(|a, b| b.updated.cmp(&a.updated));
        out
    })
    .await
    .map_err(|e| e.to_string())
}

/// Rename a saved chat (a title line is added; the rest stays).
#[tauri::command]
pub async fn chat_rename(app: AppHandle, id: String, title: String) -> Result<(), String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if !id_ok(&id) || title.is_empty() || title.chars().count() > 120 {
        return Err("give a title of at most 120 characters".into());
    }
    let dir = chats_dir(&app).ok_or("no folder for chats")?;
    let path = dir.join(format!("{id}.jsonl"));
    if !path.is_file() {
        return Err("there is no such chat".into());
    }
    let line = serde_json::json!({"kind": "title", "text": title, "ts": now_rfc3339()});
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.write_all(format!("{line}\n").as_bytes())
        .map_err(|e| e.to_string())
}

/// Delete a saved chat's transcript (not the chat running now).
#[tauri::command]
pub async fn chat_delete(
    app: AppHandle,
    st: State<'_, ChatState>,
    id: String,
) -> Result<(), String> {
    if !id_ok(&id) {
        return Err("not a chat".into());
    }
    if current_id(&st).as_deref() == Some(id.as_str()) {
        return Err("this chat is running; start a new chat first".into());
    }
    let dir = chats_dir(&app).ok_or("no folder for chats")?;
    let path = dir.join(format!("{id}.jsonl"));
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("not a chat".into());
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_ids_are_plain() {
        assert!(id_ok("c1762380000-ab12cd34"));
        assert!(!id_ok("../c1762380000"));
        assert!(!id_ok("c1762/../x"));
        assert!(!id_ok("x1762380000-ab"));
        assert!(!id_ok("c1"));
    }

    #[test]
    fn times_are_rfc3339() {
        let t = now_rfc3339();
        assert_eq!(t.len(), 20, "{t}");
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T', "{t}");
    }

    #[test]
    fn a_chat_is_titled_by_its_first_message_or_a_rename() {
        let lines = vec![
            serde_json::json!({"kind": "meta", "session": "s1", "ts": "2026-11-05T10:00:00Z"}),
            serde_json::json!({"kind": "you", "text": "Which   datasets\ncan I use?", "ts": "2026-11-05T10:00:01Z"}),
        ];
        let s = summary("c1-x", &lines, false);
        assert_eq!(s.title, "Which datasets can I use?");
        assert_eq!(s.messages, 1);
        let mut more = lines.clone();
        more.push(
            serde_json::json!({"kind": "title", "text": "Datasets", "ts": "2026-11-05T10:01:00Z"}),
        );
        assert_eq!(summary("c1-x", &more, false).title, "Datasets");
    }
}
