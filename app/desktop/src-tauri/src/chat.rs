//! The chat window's side of the app: settings, the API key in the
//! operating system's credential store, and one chat at a time driving its
//! own Columbus session (see the `columbus-chat` crate).
//!
//! Only the window labelled `chat` may call these commands
//! (capabilities/main.json), and events go to that window only. The API key
//! never reaches a web page: the window can set it, ask whether one is set,
//! or clear it, but not read it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use columbus_chat::{Answer, Cancel, Chat, Config, Event, Prompt, Ui, MODELS};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{oneshot, Mutex};

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

fn load(app: &AppHandle) -> Settings {
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

fn server(s: &Settings) -> Option<PathBuf> {
    s.server
        .clone()
        .filter(|p| p.is_file())
        .or_else(default_server)
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

fn read_key() -> Option<String> {
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
        let _ = self.app.emit_to("chat", "chat-event", e);
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
        let Some(w) = self.app.get_webview_window("chat") else {
            let _ = tx.send(Answer::Decline);
            return (id, rx);
        };
        st.pending.lock().expect("pending").insert(id, tx);
        let _ = self.app.emit_to(
            "chat",
            "chat-event",
            Event::Approval {
                id,
                title: p.title,
                summary: p.summary,
                code: p.code,
                offer_always: p.offer_always,
            },
        );
        let _ = w.unminimize();
        let _ = w.set_focus();
        (id, rx)
    }
    fn withdraw(&self, id: u64) {
        if let Some(st) = self.app.try_state::<ChatState>() {
            st.pending.lock().expect("pending").remove(&id);
        }
        let _ = self
            .app
            .emit_to("chat", "chat-event", Event::ApprovalGone { id });
    }
}

#[derive(Serialize)]
pub struct Started {
    session: Option<String>,
    tools: usize,
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
    let started = Started {
        session: chat.session().map(String::from),
        tools: chat.tool_count(),
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
    let cancel = st.cancel.lock().expect("cancel").clone();
    tauri::async_runtime::spawn(async move {
        let Ok(mut c) = chat.try_lock() else {
            let _ = app.emit_to(
                "chat",
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
        let _ = app.emit_to("chat", "chat-event", Event::ApprovalGone { id });
    }
}

#[tauri::command]
pub async fn chat_answer(
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
    Ok(())
}

/// End the chat and its session.
#[tauri::command]
pub async fn chat_end(app: AppHandle, st: State<'_, ChatState>) -> Result<(), String> {
    end(&app, &st).await;
    Ok(())
}

/// End the chat when its window closes: nothing runs unseen.
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
