//! The chat's side of the app: settings, and one chat at a time driving its
//! own Columbus session (see the `columbus-chat` crate) on the AI service
//! the person chose (`assistant.rs`: the services, their keys in the
//! operating system's credential store, the organization's say). The chat lives in the
//! main window (`MAIN`); each chat's transcript is kept in the app's data
//! folder (`chats/<id>.jsonl`) so chats can be listed and searched.
//!
//! Only the main window may call these commands (capabilities/main.json),
//! and events go to that window only. A service's key never reaches a web
//! page: the window can set it, ask whether one is set, or clear it, but
//! not read it.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use columbus_chat::{Answer, Cancel, Chat, Config, Event, KeyRule, Prompt, Ui};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{oneshot, Mutex};

/// The window the chat lives in.
pub const MAIN: &str = "main";
/// The engine the person confirmed in Settings, kept where a program that
/// only writes the settings file cannot change it.
const ENGINE_ACCOUNT: &str = "confirmed-engine";

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
    /// The running chat's service: its id, its name and its host. A chat
    /// keeps its service to its end.
    service: std::sync::Mutex<Option<(String, String, String)>>,
    /// The models each service's own list named, since the app started.
    pub listed: std::sync::Mutex<HashMap<String, Vec<String>>>,
}

/// The app's own settings, `settings.json` (F-19 for the parts about AI
/// services).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    /// The AI service new chats use; absent, Anthropic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Anthropic's model, where earlier versions kept the one model.
    #[serde(default)]
    pub model: Option<String>,
    /// The model chosen for each service, by the service's id.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub models: std::collections::BTreeMap<String, String>,
    /// The thinking level new chats start with ("" or absent: the model's
    /// own), where the service takes it.
    #[serde(default)]
    pub effort: Option<String>,
    /// The services the person added by address.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "crate::assistant::lenient"
    )]
    pub services: Vec<crate::assistant::Added>,
    /// A server chosen in an earlier version: no longer started (the app
    /// has its own), only read to suggest the engine beside it.
    #[serde(default)]
    pub server: Option<PathBuf>,
    /// The GIS engine's Python, when the person chose one in Settings.
    #[serde(default)]
    pub engine: Option<PathBuf>,
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

/// Write the settings file (whole, or not at all).
pub(crate) fn save(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let p = settings_path(app).ok_or("no settings folder")?;
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    crate::settings::write_atomic(&p, &text)
}

/// Where a service's requests go: its own address. Test builds only: the
/// app's end-to-end tests put a stand-in in its place.
pub(crate) fn address_in_use(own: &str) -> String {
    #[cfg(debug_assertions)]
    if let Ok(b) = std::env::var("COLUMBUS_APP_API_BASE") {
        return b;
    }
    own.to_string()
}

const EXE: &str = if cfg!(windows) {
    "columbus-mcp.exe"
} else {
    "columbus-mcp"
};

/// The app's resource folder, set when the app starts.
static RESOURCES: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The app's chats folder, kept out of the servers the chat may start.
static CHATS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub(crate) fn set_dirs(app: &AppHandle) {
    if let Ok(d) = app.path().resource_dir() {
        let _ = RESOURCES.set(d);
    }
    if let Some(d) = chats_dir(app) {
        let _ = CHATS.set(d);
    }
}

/// `server_ok`, with this app's chats folder kept out too.
fn startable(p: &Path) -> Option<PathBuf> {
    server_ok(p, CHATS.get().map(PathBuf::as_path)).ok()
}

/// The server that comes with this app (release builds put the server
/// folder in the app's resources), so the chat always has one that
/// matches the app.
fn bundled_server() -> Option<PathBuf> {
    startable(&RESOURCES.get()?.join("server").join(EXE))
}

/// The person's own Columbus folder (`~/Columbus`, `%USERPROFILE%\Columbus`).
/// A folder at the top of a drive (`C:\Columbus`) is not used without the
/// person choosing it: on Windows any account may create one there.
fn home_folder() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join("Columbus"))
}

/// A server in the person's own Columbus folder, for a build without one
/// of its own (development, or an app whose own server is missing).
fn installed_server() -> Option<PathBuf> {
    startable(&home_folder()?.join(EXE))
}

/// The GIS engine's Python in a Columbus folder (`engine/` there).
fn engine_in(dir: &Path) -> Option<PathBuf> {
    let candidates: &[&[&str]] = if cfg!(windows) {
        &[
            &["engine", "venv", "Scripts", "python.exe"],
            &["engine", ".pixi", "envs", "default", "python.exe"],
        ]
    } else {
        &[
            &["engine", "venv", "bin", "python3"],
            &["engine", ".pixi", "envs", "default", "bin", "python3"],
        ]
    };
    candidates
        .iter()
        .map(|parts| parts.iter().fold(dir.to_path_buf(), |p, s| p.join(s)))
        .find(|p| engine_ok(p, CHATS.get().map(PathBuf::as_path)).is_ok())
}

/// An engine the chat may give its server: a Python program, by its full
/// path, on this computer, not in a folder an assistant or a session writes
/// to. The path itself is kept (not the file it leads to): a virtual
/// environment's Python is a link, and works only by that name.
pub(crate) fn engine_ok(p: &Path, chats: Option<&Path>) -> Result<PathBuf, String> {
    if !p.is_absolute() {
        return Err("give the engine's full path".into());
    }
    if on_network_share(p) {
        return Err("the engine must be on this computer, not a network share".into());
    }
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    let python = stem == "python"
        || stem == "python3"
        || stem
            .strip_prefix("python3.")
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()));
    if !python {
        return Err("the engine is the Python program of the GIS engine (python3, or python.exe on Windows)".into());
    }
    let full = p
        .canonicalize()
        .map_err(|_| format!("{} is not there", p.display()))?;
    if !full.is_file() || on_network_share(&full) {
        return Err(format!("{} is not a program on this computer", p.display()));
    }
    let mut keep_out = crate::shell::session_folders();
    keep_out.extend(chats.map(Path::to_path_buf));
    for d in keep_out {
        if let Ok(d) = d.canonicalize() {
            if full.starts_with(&d) || p.starts_with(&d) {
                return Err(format!(
                    "{} is in {}, a folder your assistant's sessions write to",
                    p.display(),
                    d.display()
                ));
            }
        }
    }
    Ok(p.to_path_buf())
}

/// The engine the chat's server uses: `COLUMBUS_PYTHON`, else the one the
/// person chose in Settings, else the one beside the server, else the one
/// in the person's own Columbus folder.
pub(crate) fn engine_python(s: &Settings, server: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COLUMBUS_PYTHON").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let chats = CHATS.get().map(PathBuf::as_path);
    s.engine
        .as_deref()
        // Only the engine the person confirmed: the settings file alone is
        // not enough to name a program the server runs.
        .filter(|p| confirmed_engine().as_deref() == Some(*p))
        .and_then(|p| engine_ok(p, chats).ok())
        .or_else(|| server.and_then(Path::parent).and_then(engine_in))
        .or_else(|| home_folder().as_deref().and_then(engine_in))
}

/// Engines the person might mean, for Settings to offer (never used
/// without the person choosing one): beside a server chosen in an earlier
/// version, and in `C:\Columbus`.
pub(crate) fn engine_suggestions(s: &Settings) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = s.server.as_deref().and_then(Path::parent) {
        out.extend(engine_in(dir));
    }
    if cfg!(windows) {
        out.extend(engine_in(Path::new(r"C:\Columbus")));
    }
    out.dedup();
    out
}

/// `columbus-mcp 0.9.1` as (0, 9, 1).
fn parse_version(out: &str) -> Option<(u32, u32, u32)> {
    let v = out.split_whitespace().nth(1)?;
    let mut it = v.split(['.', '-', '+']).map(|n| n.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// What a server says its version is (`--version`, cached per file; given
/// up after a few seconds).
pub(crate) fn server_version(p: &Path) -> Option<(u32, u32, u32)> {
    type Seen = Vec<(PathBuf, std::time::SystemTime, (u32, u32, u32))>;
    static SEEN: std::sync::Mutex<Seen> = std::sync::Mutex::new(Vec::new());
    let modified = std::fs::metadata(p).and_then(|m| m.modified()).ok()?;
    if let Some(v) = SEEN
        .lock()
        .ok()?
        .iter()
        .find(|(q, t, _)| q == p && *t == modified)
        .map(|x| x.2)
    {
        return Some(v);
    }
    let mut cmd = std::process::Command::new(p);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // no console window
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child
        .stdout
        .take()?
        .take(4096)
        .read_to_string(&mut out)
        .ok()?;
    let v = parse_version(&out)?;
    if let Ok(mut g) = SEEN.lock() {
        g.push((p.to_path_buf(), modified, v));
    }
    Some(v)
}

pub(crate) fn app_version() -> (u32, u32, u32) {
    parse_version(&format!("x {}", env!("CARGO_PKG_VERSION"))).unwrap_or((0, 0, 0))
}

/// The server the chat starts: the one that comes with the app, else (in
/// a build without one) the one in the person's own Columbus folder. Not
/// a file named in the settings file: the app has its own.
pub(crate) fn server(_s: &Settings) -> Option<PathBuf> {
    // Test builds only: the app's end-to-end tests point at the server they built.
    #[cfg(debug_assertions)]
    if let Some(p) = std::env::var_os("COLUMBUS_SERVER").filter(|v| !v.is_empty()) {
        return startable(Path::new(&p));
    }
    bundled_server().or_else(installed_server)
}

/// Whether `p` is the server that comes with the app.
pub(crate) fn is_bundled(p: &Path) -> bool {
    bundled_server().is_some_and(|b| p.canonicalize().ok().as_ref() == Some(&b))
}

/// The environment the chat gives its server: the engine, unless it is the
/// one beside the server (which the server finds itself).
pub(crate) fn server_env(s: &Settings, srv: &Path) -> Vec<(String, String)> {
    if std::env::var_os("COLUMBUS_PYTHON").is_some_and(|v| !v.is_empty()) {
        return vec![];
    }
    let engine = engine_python(s, Some(srv));
    if engine.is_some() && engine == srv.parent().and_then(engine_in) {
        return vec![];
    }
    engine
        .map(|p| vec![("COLUMBUS_PYTHON".to_string(), p.display().to_string())])
        .unwrap_or_default()
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

/// The engine path recorded when the person confirmed it.
fn confirmed_engine() -> Option<PathBuf> {
    crate::assistant::secret(ENGINE_ACCOUNT)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

fn record_engine(p: Option<&Path>) -> Result<(), String> {
    match p {
        Some(p) => crate::assistant::set_secret(ENGINE_ACCOUNT, &p.display().to_string()),
        None => crate::assistant::delete_secret(ENGINE_ACCOUNT),
    }
}

#[derive(Serialize)]
pub struct SettingsView {
    /// The service new chats use, every service there is, and what the
    /// organization says of them.
    #[serde(flatten)]
    services: crate::assistant::Services,
    /// Of the service new chats use: whether a key is saved for it.
    has_key: bool,
    /// The saved key's last four characters, so the person can tell which
    /// key it is; the key itself never reaches the window.
    key_hint: Option<String>,
    /// Whether that service needs a key at all.
    key_needed: bool,
    /// Whether a chat could start on it now: it has the key it needs and a
    /// model, and the organization allows it.
    ready: bool,
    /// Its model and thinking level for new chats, and what it offers.
    model: String,
    models: Vec<(String, String)>,
    effort: String,
    efforts: Vec<(String, String)>,
    server_found: bool,
    /// The server a new chat starts, and what is known about it.
    server_in_use: Option<String>,
    server_version: Option<String>,
    server_bundled: bool,
    /// The server in use is older than this app: some of the app does not
    /// work with it.
    server_older: bool,
    app_version: String,
    /// The engine a new chat uses, the one chosen in Settings, and others
    /// found that the person might mean.
    engine: Option<String>,
    engine_chosen: Option<String>,
    engine_suggestions: Vec<String>,
    chatting: bool,
    /// The running chat's own Columbus session, so the page never lists it
    /// among other AI apps' sessions (after a reload, say).
    session: Option<String>,
    /// The running chat's service, which it keeps to its end.
    chat_service: Option<String>,
    chat_service_name: Option<String>,
}

struct ServerInfo {
    in_use: Option<PathBuf>,
    version: Option<(u32, u32, u32)>,
    bundled: bool,
    engine: Option<PathBuf>,
    suggestions: Vec<PathBuf>,
}

fn server_info(s: &Settings) -> ServerInfo {
    let in_use = server(s);
    let version = in_use.as_deref().and_then(server_version);
    let bundled = in_use.as_deref().is_some_and(is_bundled);
    let engine = engine_python(s, in_use.as_deref());
    let suggestions = engine_suggestions(s)
        .into_iter()
        .filter(|p| Some(p) != engine.as_ref())
        .collect();
    ServerInfo {
        in_use,
        version,
        bundled,
        engine,
        suggestions,
    }
}

#[tauri::command]
pub async fn chat_settings(
    app: AppHandle,
    st: State<'_, ChatState>,
) -> Result<SettingsView, String> {
    let s = load(&app);
    let s2 = s.clone();
    let info = tauri::async_runtime::spawn_blocking(move || server_info(&s2))
        .await
        .map_err(|e| e.to_string())?;
    // (The credential store is read off the main thread: on a Mac it may
    // ask the person first.)
    let s3 = s.clone();
    let listed = st.listed.lock().expect("listed").clone();
    let (services, now) = tauri::async_runtime::spawn_blocking(move || {
        (
            crate::assistant::services_view(&s3, &listed),
            crate::assistant::now(&s3, &listed),
        )
    })
    .await
    .map_err(|e| e.to_string())?;
    let running = st.service.lock().expect("service").clone();
    Ok(SettingsView {
        services,
        has_key: now.has_key,
        key_hint: now.key_hint,
        key_needed: now.key_needed,
        ready: now.ready,
        model: now.model,
        models: now.models,
        effort: now.effort,
        efforts: now.efforts,
        server_found: info.in_use.is_some(),
        server_in_use: info.in_use.as_ref().map(|p| p.display().to_string()),
        server_version: info.version.map(|(a, b, c)| format!("{a}.{b}.{c}")),
        server_bundled: info.bundled,
        server_older: info.version.is_some_and(|v| v < app_version()),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        engine: info.engine.map(|p| p.display().to_string()),
        engine_chosen: s.engine.as_ref().map(|p| p.display().to_string()),
        engine_suggestions: info
            .suggestions
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        chatting: st.chat.lock().await.is_some(),
        session: st.session.lock().expect("session").clone(),
        chat_service: running.as_ref().map(|r| r.0.clone()),
        chat_service_name: running.map(|r| r.1),
    })
}

/// Save the model and the thinking level new chats start with on the
/// service in use, and the GIS engine. A model is one the service lists
/// or any plain model id the person typed; with none given, the choice
/// stays as it is.
#[tauri::command]
pub async fn chat_save_settings(
    app: AppHandle,
    model: Option<String>,
    effort: Option<String>,
    engine: Option<String>,
) -> Result<(), String> {
    let old = load(&app);
    let id = crate::assistant::chosen(&old);
    let r = crate::assistant::resolve(&old, &id)?;
    let model = model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    if let Some(m) = &model {
        if !r.service.model_ok(m) {
            return Err("that is not a model id: use letters, digits and . _ : / @ + - only, at most 200 characters".into());
        }
    }
    if let Some(e) = &effort {
        if !r.service.effort_ok(e) {
            return Err("unknown thinking level".into());
        }
    }
    let wanted = engine
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let chats = chats_dir(&app);
    // The engine is a program the chat's server runs: choosing one is
    // confirmed by the person at the keyboard, like any widening setting.
    let old_engine = old.engine.clone();
    let engine =
        tauri::async_runtime::spawn_blocking(move || -> Result<Option<PathBuf>, String> {
            let Some(w) = wanted else {
                let _ = record_engine(None);
                return Ok(None);
            };
            let w = engine_ok(&w, chats.as_deref())?;
            if old_engine.as_ref() != Some(&w) || confirmed_engine().as_ref() != Some(&w) {
                crate::settings::confirm(&format!(
                    "let the app's chat run {} as the GIS engine",
                    w.display()
                ))?;
                record_engine(Some(&w)).map_err(|e| {
                    format!(
                        "the engine could not be recorded in this computer's credential store: {e}"
                    )
                })?;
            }
            Ok(Some(w))
        })
        .await
        .map_err(|e| e.to_string())??;
    let mut s = old;
    s.engine = engine;
    if let Some(m) = model {
        if id == crate::assistant::DEFAULT_SERVICE {
            // Where earlier versions kept it, too.
            s.model = Some(m.clone());
        }
        s.models.insert(id, m);
    }
    // A service that takes no level leaves the saved one as it is, for the
    // services that do.
    if let Some(e) = effort.filter(|_| r.service.efforts.len() > 1) {
        s.effort = Some(e);
    }
    save(&app, &s)
}

/// A Windows path to another computer (`\\server\share\...`) or a device
/// path (`\\.\...`). A normal path like `C:\Columbus\...` is fine, including
/// the `\\?\C:\...` form Windows gives back after canonicalizing.
fn on_network_share(p: &Path) -> bool {
    use std::path::{Component, Prefix};
    matches!(
        p.components().next(),
        Some(Component::Prefix(x))
            if matches!(x.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::DeviceNS(..))
    )
}

/// A server the chat may start: an existing file named `columbus-mcp`
/// (`columbus-mcp.exe` on Windows), by its full path, and not in a folder
/// an assistant or a session writes to (workspaces, notes, chats).
pub(crate) fn server_ok(p: &Path, chats: Option<&Path>) -> Result<PathBuf, String> {
    let exe = EXE;
    if !p.is_absolute() {
        return Err("give the server's full path".into());
    }
    if on_network_share(p) {
        return Err("the server must be on this computer, not a network share".into());
    }
    let name_ok = p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(exe));
    if !name_ok {
        return Err(format!("the server is the program named {exe}"));
    }
    let full = p
        .canonicalize()
        .map_err(|_| format!("{} is not there", p.display()))?;
    if !full.is_file() {
        return Err(format!("{} is not a file", p.display()));
    }
    if on_network_share(&full) {
        return Err("the server must be on this computer, not a network share".into());
    }
    let full_name_ok = full
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case(exe));
    if !full_name_ok {
        return Err(format!("the server is the program named {exe}"));
    }
    let mut keep_out = crate::shell::session_folders();
    keep_out.extend(chats.map(Path::to_path_buf));
    for d in keep_out {
        if let Ok(d) = d.canonicalize() {
            if full.starts_with(&d) {
                return Err(format!(
                    "{} is in {}, a folder your assistant's sessions write to; use the Columbus folder you unpacked",
                    full.display(),
                    d.display()
                ));
            }
        }
    }
    Ok(full)
}

/// The chat window, as the chat sees it: events go to the `chat` window,
/// approval prompts wait for its answer.
struct WindowUi {
    app: AppHandle,
}

impl Ui for WindowUi {
    fn event(&self, e: Event) {
        let mut v = serde_json::to_value(&e).unwrap_or_default();
        // A picture stays in the workspace (its handle is kept), not in the transcript.
        if let Some(m) = v.as_object_mut() {
            m.remove("image");
        }
        record(&self.app, &v);
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

/// A workflow run whose results a new chat opens (REQ-27).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OpenRun {
    pub workflow: String,
    pub run: String,
}

#[derive(Serialize)]
pub struct Started {
    session: Option<String>,
    tools: usize,
    /// The transcript's id, under which the chat is listed.
    chat: Option<String>,
    /// The service the chat runs on, to its end.
    service: Option<String>,
    service_name: Option<String>,
}

/// Start a chat (its own Columbus session) if none is running.
#[tauri::command]
pub async fn chat_start(
    app: AppHandle,
    st: State<'_, ChatState>,
    model: Option<String>,
    effort: Option<String>,
    project: Option<String>,
    run: Option<OpenRun>,
) -> Result<Started, String> {
    let mut slot = st.chat.lock().await;
    if slot.is_some() {
        // Already running (a reply may be holding it): answer without
        // waiting for the reply.
        let running = st.service.lock().expect("service").clone();
        return Ok(Started {
            session: st.session.lock().expect("session").clone(),
            tools: 0,
            chat: current_id(&st),
            service: running.as_ref().map(|r| r.0.clone()),
            service_name: running.map(|r| r.1),
        });
    }
    let s = load(&app);
    // The service new chats use: set up, as the person confirmed it, and
    // allowed by the organization (checked here, whatever the page showed).
    let s2 = s.clone();
    let (service, key) = tauri::async_runtime::spawn_blocking(move || {
        let id = crate::assistant::chosen(&s2);
        let r = crate::assistant::usable(&s2, &id)?;
        let key = if r.service.key == KeyRule::None {
            None
        } else {
            crate::assistant::read_key(&s2, &id)
        };
        Ok::<_, String>((r, key))
    })
    .await
    .map_err(|e| e.to_string())??;
    let name = service.service.name.clone();
    if service.service.key == KeyRule::Needed && key.is_none() {
        return Err(format!(
            "No key is saved for {name}. Add one in Settings › Assistant."
        ));
    }
    // This chat's own choice, else the default from Settings.
    let model = model
        .filter(|m| !m.is_empty())
        .or_else(|| crate::assistant::model_for(&s, &service))
        .ok_or_else(|| {
            format!("No model is chosen for {name}. Choose one in Settings › Assistant.")
        })?;
    let effort = effort.unwrap_or_else(|| crate::assistant::effort_for(&s, &service));
    if !service.service.model_ok(&model) {
        return Err("unknown model".into());
    }
    if !service.service.effort_ok(&effort) {
        return Err("unknown thinking level".into());
    }
    // A chat in a project: the project must be readable now (the server
    // refuses to start otherwise; this says why first).
    let project = project.filter(|p| !p.is_empty());
    if let Some(id) = project.clone() {
        tauri::async_runtime::spawn_blocking(move || columbus_shared::projects::load(&id))
            .await
            .map_err(|e| e.to_string())??;
    }
    // A chat opened from a workflow run's report (REQ-27): the run is
    // checked here first (finished, its layers unchanged on disk), so a
    // refusal names the file; the server checks again and copies them.
    if let Some(r) = run.clone() {
        tauri::async_runtime::spawn_blocking(move || {
            columbus_shared::workflows::open_check(
                &columbus_shared::workflows::workflows_dir(),
                None,
                &r.workflow,
                &r.run,
            )
            .map(|_| ())
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    let s2 = s.clone();
    let srv = tauri::async_runtime::spawn_blocking(move || server(&s2))
        .await
        .map_err(|e| e.to_string())?
        .ok_or(
            "Columbus's server was not found. Unpack the Columbus folder (see the install guide), or set its place in Settings.",
        )?;
    // Whatever wrote the settings file, only a Columbus server is started.
    let chats = chats_dir(&app);
    let srv = tauri::async_runtime::spawn_blocking(move || server_ok(&srv, chats.as_deref()))
        .await
        .map_err(|e| e.to_string())??;
    let api_base = address_in_use(&service.address);
    let mut env = server_env(&s, &srv);
    // Always set, so an Unfiled chat never inherits a project from the
    // app's own environment (empty means none).
    env.push((
        "COLUMBUS_PROJECT".into(),
        project.clone().unwrap_or_default(),
    ));
    // Likewise always set: a chat opens a run only when asked to.
    env.push((
        "COLUMBUS_OPEN_RUN".into(),
        run.as_ref()
            .map(|r| format!("{}/{}", r.workflow, r.run))
            .unwrap_or_default(),
    ));
    let cfg = Config {
        server_env: env,
        server: srv,
        api_key: key.unwrap_or_default(),
        model: model.clone(),
        effort: effort.clone(),
        api_base,
    };
    let cancel = Cancel::default();
    *st.cancel.lock().expect("cancel") = cancel.clone();
    let service_id = service.service.id.clone();
    let chat = Chat::start_with(
        cfg,
        service.service.clone(),
        Arc::new(WindowUi { app: app.clone() }),
        cancel,
    )
    .await
    .map_err(|e| e.to_string())?;
    let session = chat.session().map(String::from);
    let id = new_transcript(
        &app,
        &st,
        session.as_deref(),
        (&service_id, &name),
        &model,
        &effort,
        project.as_deref(),
        run.as_ref(),
    );
    let started = Started {
        session,
        tools: chat.tool_count(),
        chat: id,
        service: Some(service_id.clone()),
        service_name: Some(name.clone()),
    };
    *st.service.lock().expect("service") = Some((service_id, name, service.host.clone()));
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
    // The organization's list is checked before each message: a chat that
    // was running when the rules changed sends nothing more to a service
    // they no longer allow.
    let running = st.service.lock().expect("service").clone();
    if let Some((_, name, host)) = running {
        tauri::async_runtime::spawn_blocking(move || crate::assistant::org_allows(&name, &host))
            .await
            .map_err(|e| e.to_string())??;
    }
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

/// Change the running chat's model or thinking level; the next message
/// uses it. Not while a reply is in progress.
#[tauri::command]
pub async fn chat_set_model(
    app: AppHandle,
    st: State<'_, ChatState>,
    model: String,
    effort: String,
) -> Result<(), String> {
    let chat = st
        .chat
        .lock()
        .await
        .clone()
        .ok_or("the chat has not started")?;
    let Ok(mut c) = chat.try_lock() else {
        return Err("wait for the reply to finish, then change the model".into());
    };
    if c.model() == (model.as_str(), effort.as_str()) {
        return Ok(());
    }
    c.set_model(&model, &effort).map_err(|e| e.to_string())?;
    record(
        &app,
        &serde_json::json!({"kind": "model", "model": model, "effort": effort}),
    );
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
    *st.service.lock().expect("service") = None;
    if let Some(c) = chat {
        // A reply still holding the chat stops at the cancel; close the
        // session once it lets go, in the background: the chat is already
        // ended for the window (a new one can start), and closing a
        // session can take a while.
        tauri::async_runtime::spawn(async move {
            let m = c.lock().await;
            drop(m);
            if let Ok(m) = Arc::try_unwrap(c) {
                m.into_inner().close().await;
            }
        });
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

pub(crate) fn chats_dir(app: &AppHandle) -> Option<PathBuf> {
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
    service: (&str, &str),
    model: &str,
    effort: &str,
    project: Option<&str>,
    run: Option<&OpenRun>,
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
        &serde_json::json!({"kind": "meta", "id": id, "session": session, "service": service.0, "service_name": service.1,
            "model": model, "effort": effort, "project": project, "run": run}),
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
    let _ = append_plain(&path, line.as_bytes());
}

/// Append to a transcript that is a plain file (never through a link),
/// creating it new if it is not there yet.
fn append_plain(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut o = std::fs::OpenOptions::new();
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => {
            o.append(true);
        }
        Ok(_) => {
            return Err(std::io::Error::other(
                "a transcript that is not a plain file",
            ))
        }
        Err(_) => {
            o.write(true).create_new(true);
        }
    }
    // What was said and shown may hold data: this account's alone.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
        // A link put there between the check above and here is refused too.
        o.custom_flags(libc::O_NOFOLLOW);
    }
    o.open(path)?.write_all(bytes)
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
    /// The AI service it ran on (none: a chat from before services, on Anthropic).
    service: Option<String>,
    model: Option<String>,
    started: Option<String>,
    updated: Option<String>,
    messages: usize,
    /// The project it is in; none is Unfiled.
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
        service: meta
            .and_then(|m| m["service_name"].as_str())
            .map(String::from),
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
        project: meta.and_then(|m| m["project"].as_str()).map(String::from),
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
    let q = query.trim().to_string();
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
                if let Some(snip) = snippet(&t, &q) {
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

/// One character as search compares it: lower case, one letter at a time
/// (so a word's last Σ is σ on both sides, never the final ς of only one).
fn fold_char(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase().map(|l| if l == 'ς' { 'σ' } else { l })
}

/// Where `q` is in `text`, compared as [`fold_char`] folds them, as
/// positions in `text`'s own characters: a character that lowercases to
/// several (`İ`) still maps back to itself.
fn find_folded(text: &str, q: &str) -> Option<(usize, usize)> {
    let want: Vec<char> = q.chars().flat_map(fold_char).collect();
    if want.is_empty() {
        return None;
    }
    let mut lower = Vec::new();
    let mut owner = Vec::new();
    for (i, c) in text.chars().enumerate() {
        for l in fold_char(c) {
            lower.push(l);
            owner.push(i);
        }
    }
    let at = lower
        .windows(want.len())
        .position(|w| w == want.as_slice())?;
    Some((owner[at], owner[at + want.len() - 1] + 1))
}

/// A few words around the first match of `q` in `text`, as plain text.
fn snippet(text: &str, q: &str) -> Option<String> {
    let (start, end) = find_folded(text, q)?;
    let chars: Vec<char> = text.chars().collect();
    let from = start.saturating_sub(40);
    let to = (end + 60).min(chars.len());
    let snip: String = chars[from..to].iter().collect();
    Some(snip.split_whitespace().collect::<Vec<_>>().join(" "))
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
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
        return Err("there is no such chat".into());
    }
    let line = serde_json::json!({"kind": "title", "text": title, "ts": now_rfc3339()});
    append_plain(&path, format!("{line}\n").as_bytes()).map_err(|e| e.to_string())
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
    #[cfg(unix)]
    #[test]
    fn only_a_python_program_on_this_computer_is_an_engine() {
        let dir = std::env::temp_dir().join(format!("engine-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let d = &dir;
        let py = d.join("python3");
        std::fs::write(&py, "#!/bin/sh\n").unwrap();
        let other = d.join("evil");
        std::fs::write(&other, "#!/bin/sh\n").unwrap();
        // A venv's python is a link: the link's own path is what is kept.
        let link = d.join("python3.12");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert_eq!(super::engine_ok(&py, None).unwrap(), py);
        assert_eq!(super::engine_ok(&link, None).unwrap(), link);
        assert!(super::engine_ok(&other, None).is_err());
        assert!(super::engine_ok(std::path::Path::new("python3"), None).is_err());
        assert!(super::engine_ok(&d.join("python"), None).is_err());
        assert!(
            super::engine_ok(&py, Some(d)).is_err(),
            "not in the chats folder"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn server_versions_are_read_from_their_version_line() {
        assert_eq!(
            super::parse_version("columbus-mcp 0.9.1\n"),
            Some((0, 9, 1))
        );
        assert_eq!(
            super::parse_version("columbus-mcp 1.10.0-rc.1"),
            Some((1, 10, 0))
        );
        assert_eq!(super::parse_version("columbus-mcp"), None);
        assert_eq!(super::parse_version("something else entirely"), None);
        assert!(super::parse_version("columbus-mcp 0.8.1") < Some(super::app_version()));
    }

    #[cfg(windows)]
    #[test]
    fn a_server_on_a_network_share_is_refused() {
        use std::path::Path;
        assert!(super::on_network_share(Path::new(
            r"\\host\share\columbus-mcp.exe"
        )));
        assert!(super::on_network_share(Path::new(
            r"\\?\UNC\host\share\columbus-mcp.exe"
        )));
        assert!(!super::on_network_share(Path::new(
            r"C:\Columbus\columbus-mcp.exe"
        )));
        assert!(!super::on_network_share(Path::new(
            r"\\?\C:\Columbus\columbus-mcp.exe"
        )));
    }

    use super::*;

    #[test]
    fn only_a_columbus_server_outside_session_folders_is_started() {
        let d = std::env::temp_dir().join(format!("columbus-server-test-{}", std::process::id()));
        let good = d.join("Columbus");
        let chats = d.join("chats");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::create_dir_all(&chats).unwrap();
        let exe = if cfg!(windows) {
            "columbus-mcp.exe"
        } else {
            "columbus-mcp"
        };
        std::fs::write(good.join(exe), "").unwrap();
        std::fs::write(good.join("other.exe"), "").unwrap();
        std::fs::write(chats.join(exe), "").unwrap();
        assert!(server_ok(&good.join(exe), Some(&chats)).is_ok());
        assert!(server_ok(&good.join("other.exe"), Some(&chats)).is_err());
        assert!(
            server_ok(Path::new(exe), Some(&chats)).is_err(),
            "a relative path"
        );
        assert!(server_ok(&good.join("missing").join(exe), Some(&chats)).is_err());
        assert!(
            server_ok(&chats.join(exe), Some(&chats)).is_err(),
            "inside a folder sessions write"
        );
        #[cfg(unix)]
        {
            // A link named like the server, to another program.
            std::os::unix::fs::symlink(good.join("other.exe"), d.join(exe)).unwrap();
            assert!(server_ok(&d.join(exe), Some(&chats)).is_err());
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_greek_word_is_found_whatever_its_last_sigma() {
        let t = "Η ΟΔΟΣ ΑΘΗΝΩΝ ήταν κλειστή· η οδός άνοιξε";
        assert!(snippet(t, "ΟΔΟΣ").is_some());
        assert!(snippet(t, "οδοσ").is_some());
        assert!(snippet(t, "Οδός").is_some());
    }

    #[test]
    fn search_snippets_survive_characters_that_lowercase_to_several() {
        // `İ` lowercases to two characters; `ẞ` and `Ω` change too.
        let t = "İİİİİİİİİİ Straße district ẞ Ω İstanbul route";
        assert!(snippet(t, "istanbul").is_none(), "İ is i plus a dot, not i");
        let s = snippet(t, &"İstanbul".to_lowercase()).unwrap();
        assert!(s.contains("İstanbul"), "{s}");
        assert!(snippet(t, "route").unwrap().ends_with("route"));
        assert!(snippet(t, "ω").unwrap().contains('Ω'));
        let long = format!("{}{}", "İ".repeat(500), "match here");
        assert!(snippet(&long, "match").unwrap().contains("match here"));
        assert!(snippet("abc", "").is_none());
        assert!(snippet("", "a").is_none());
        // Every place a match can start, in a string of such characters.
        let odd = "İaİbİ";
        for q in ["a", "b", "i̇", "i̇a", "bi̇"] {
            let _ = snippet(odd, q);
        }
    }

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
