//! The Columbus desktop app (design section 8): one main window with a
//! rail (Home, Chats, Projects, Documents, Connections, Tools, Activity,
//! Settings…), the app's own chat, and an output panel that shows the
//! session's map and table; plus the companion window for sessions run by
//! any AI client.
//!
//! Every Columbus server announces its session in `live/<session>.json`
//! (its port, no secret) beside the session workspaces. This app lists
//! those sessions. The main window's output panel gets a session's page
//! over the signed channel (`shell::session_page`): the session hands a
//! view link straight to this app, which shows it in a sandboxed frame.
//! "Open in its own window" asks the session's server for a window
//! (`POST /api/open`); the server makes a fresh single-use launch token and
//! hands it to this app through a `columbus://open?port=<p>#t=<token>`
//! link, which the app opens in a window of its own. While the app runs it
//! keeps `live/app.json` fresh, which is how servers know to send these
//! windows here instead of to the browser.
//!
//! Any program can register a handler for a custom URL scheme, so a
//! `columbus://` link carries a view-only token: the window shows the
//! session's layers, tables and waiting requests, and its "Review in your
//! browser" button has the server open the browser page that can decide.
//! Approvals from other AI apps are never decided in this app; the app's
//! own chat answers its own session's prompts, as any AI client does.
//!
//! Session windows and the main window's frame show the local Columbus
//! page (127.0.0.1) and have no access to the app: no capability names a
//! remote origin, and navigation away from the app's pages (or, for the
//! frame, the session's page) is refused. Honest limit: a program the
//! person runs can write a `live/` entry for a server of its own, and its
//! page would then open in a Columbus window; such a program can already
//! show the person any window it likes. The frame is safer than that
//! window: the app gets its link only from a session that proves it holds
//! the key in `live/<session>.key`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

mod chat;
mod datagroups;
mod doc_import;
mod projects;
mod registry;
mod settings;
mod share;
mod shell;
mod workflows;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_deep_link::DeepLinkExt;

/// Where sessions announce themselves: `COLUMBUS_LIVE_DIR`, else `live/`
/// in the person's Columbus folder (the gateway's `config::live_dir`).
pub(crate) fn live_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("COLUMBUS_LIVE_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")
            .or_else(|| env("APPDATA"))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Columbus")
    } else if cfg!(target_os = "macos") {
        env("HOME")
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/Columbus")
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("columbus")
    };
    base.join("live")
}

#[derive(Serialize, Clone)]
struct Session {
    session: String,
    port: u16,
    user: Option<String>,
    started: Option<String>,
    server: Option<String>,
    /// When the session's assistant last called a tool; none if never.
    last_used: Option<String>,
}

/// A port that answers like a Columbus page: `GET /api/state` without a
/// token is refused with 401 (a page that is someone else's would not).
pub(crate) fn answers(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
    let req =
        format!("GET /api/state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 401")
}

/// The sessions running now, newest first. Announcements from servers that
/// have stopped are skipped (and removed when the server has gone).
/// The list for the main window. Commands that wait on the network run off
/// the main thread: a sync command runs on it, and the window would hang.
#[tauri::command]
async fn sessions() -> Vec<Session> {
    tauri::async_runtime::spawn_blocking(live_sessions)
        .await
        .unwrap_or_default()
}

fn live_sessions() -> Vec<Session> {
    let Ok(entries) = std::fs::read_dir(live_dir()) else {
        return vec![];
    };
    let mut out = Vec::new();
    for e in entries.filter_map(Result::ok) {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "app.json" || !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(port) = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()) else {
            continue;
        };
        if port < 1024 {
            continue;
        }
        if !answers(port) {
            let _ = std::fs::remove_file(p.with_extension("key"));
            let _ = std::fs::remove_file(&p);
            continue;
        }
        let s = |k: &str| v[k].as_str().map(String::from);
        out.push(Session {
            session: s("session").unwrap_or_default(),
            port,
            user: s("user"),
            started: s("started"),
            server: s("server"),
            last_used: s("last_used"),
        });
    }
    // The session in use first, then the most recently started. (RFC 3339
    // times from one computer sort as text.)
    out.sort_by(|a, b| {
        b.last_used
            .is_some()
            .cmp(&a.last_used.is_some())
            .then_with(|| b.last_used.cmp(&a.last_used))
            .then_with(|| b.started.cmp(&a.started))
    });
    out
}

/// Ask a session's server for a window. It answers by opening a
/// `columbus://` link to this app with a fresh launch token.
#[tauri::command]
async fn open_session(port: u16) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || ask_for_window(port))
        .await
        .map_err(|e| e.to_string())?
}

fn ask_for_window(port: u16) -> Result<(), String> {
    if !live_sessions().iter().any(|s| s.port == port) {
        return Err("that session is no longer running".into());
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_millis(500)).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    let req = format!(
        "POST /api/open HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Columbus-Open: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 256];
    let n = s.read(&mut buf).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]);
    if head.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(format!(
            "the session did not open a window ({})",
            head.lines().next().unwrap_or("no answer")
        ))
    }
}

/// The main window's pages: the app's own files only, and the `/embed`
/// page of sessions it was given a link to (`allow_frame`), which it shows
/// in a sandboxed frame. Navigation events for frames arrive here too on macOS
/// and Linux; the frame cannot navigate the window itself (its sandbox has
/// no top navigation), and the window's own script never does.
fn main_page(app: &AppHandle, url: &tauri::Url) -> bool {
    let ports = app
        .try_state::<Frames>()
        .map(|f| f.0.lock().expect("frames").clone())
        .unwrap_or_default();
    page_allowed(url, &ports)
}

/// The rule of [`main_page`], given the ports of sessions that handed the
/// app a link.
fn page_allowed(url: &tauri::Url, ports: &std::collections::HashSet<u16>) -> bool {
    if app_page(url) || url.as_str() == "about:blank" {
        return true;
    }
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.path() == "/embed"
        && url.port().is_some_and(|p| ports.contains(&p))
}

/// Ports of sessions whose page the main window may show in its frame.
#[derive(Default)]
pub(crate) struct Frames(std::sync::Mutex<std::collections::HashSet<u16>>);

pub(crate) fn allow_frame(app: &AppHandle, port: u16) {
    if let Some(f) = app.try_state::<Frames>() {
        f.0.lock().expect("frames").insert(port);
    }
}

/// The main window: 1280 x 800, at least 1024 x 680 (design 4.4).
fn main_window(app: &AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    let handle = app.clone();
    #[allow(unused_mut)]
    let mut page = "index.html".to_string();
    // Tests only (debug builds): show a page of the test's own in the
    // window's frame, as a session's page would be, to check that a framed
    // page cannot reach the app (e2e/frame_ipc.py).
    #[cfg(debug_assertions)]
    if let Some(u) = std::env::var("COLUMBUS_APP_TEST_FRAME")
        .ok()
        .and_then(|u| Url::parse(&u).ok())
        .filter(|u| {
            u.scheme() == "http" && u.host_str() == Some("127.0.0.1") && u.path() == "/embed"
        })
    {
        if let Some(port) = u.port() {
            allow_frame(app, port);
            let q: String = url_escape(u.as_str());
            page = format!("index.html?test-frame={q}");
        }
    }
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page.into()))
        .on_navigation(move |u| main_page(&handle, u))
        .title("Columbus")
        .inner_size(1280.0, 800.0)
        .min_inner_size(1024.0, 680.0)
        .build()
}

/// A URL as one query value (tests only).
#[cfg(debug_assertions)]
fn url_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Open (or bring forward) one of the classic windows: today's session
/// list and settings window, kept until the main window has proved itself.
#[tauri::command]
async fn open_classic(app: AppHandle, which: String) -> Result<(), String> {
    let (label, page, title, w, h) = match which.as_str() {
        "sessions" => (
            "sessions",
            "sessions.html",
            "Columbus sessions (classic)",
            560.0,
            520.0,
        ),
        "settings" => return open_settings(app).await,
        _ => return Err("no such window".into()),
    };
    if let Some(win) = app.get_webview_window(label) {
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, label, WebviewUrl::App(page.into()))
        .on_navigation(app_page)
        .title(title)
        .inner_size(w, h)
        .min_inner_size(420.0, 360.0)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The app's own pages only: the settings and classic windows never
/// navigate anywhere else (tauri://localhost on macOS and Linux,
/// http://tauri.localhost on Windows).
fn app_page(url: &tauri::Url) -> bool {
    url.scheme() == "tauri" || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
}

/// Open the settings window: connections and the organization's switches.
#[tauri::command]
async fn open_settings(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, "settings", WebviewUrl::App("settings.html".into()))
        .on_navigation(app_page)
        .title("Columbus settings (classic)")
        .inner_size(820.0, 860.0)
        .min_inner_size(560.0, 560.0)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Open the map in its own window for the chat's own session.
#[tauri::command]
async fn chat_open_map(st: tauri::State<'_, chat::ChatState>) -> Result<(), String> {
    let session = st
        .session
        .lock()
        .expect("session")
        .clone()
        .ok_or("the chat has not started")?;
    tauri::async_runtime::spawn_blocking(move || {
        let port = live_sessions()
            .into_iter()
            .find(|s| s.session == session)
            .map(|s| s.port)
            .ok_or("the chat's session is not running")?;
        ask_for_window(port)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `columbus://open?port=<p>#t=<64 hex>` for a running session, as the
/// page URL to show; anything else is ignored.
fn page_for(link: &Url) -> Option<Url> {
    if link.scheme() != "columbus" || link.host_str() != Some("open") {
        return None;
    }
    let port: u16 = link
        .query_pairs()
        .find(|(k, _)| k == "port")
        .and_then(|(_, v)| v.parse().ok())?;
    let token = link.fragment()?.strip_prefix("t=")?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if !live_sessions().iter().any(|s| s.port == port) {
        return None;
    }
    Url::parse(&format!("http://127.0.0.1:{port}/#t={token}")).ok()
}

fn show_page(app: &AppHandle, page: Url) {
    let port = page.port().unwrap_or(0);
    let label = format!("session-{port}");
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.navigate(page);
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let origin = page.origin();
    let built = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(page))
        .title("Columbus session")
        .inner_size(1360.0, 860.0)
        .min_inner_size(1024.0, 680.0)
        // Only the session's own page; anything else (a link in data, a
        // redirect) is refused.
        .on_navigation(move |u| u.origin() == origin)
        .build();
    if let Ok(w) = built {
        let _ = w.set_focus();
    }
}

/// Open the windows for links the system handed over. On macOS links
/// arrive on the main thread, where building a window (which waits for the
/// event loop) would hang the app; so the work moves to a background task.
fn handle_links(app: &AppHandle, urls: Vec<Url>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let pages = tauri::async_runtime::spawn_blocking(move || {
            urls.iter().filter_map(page_for).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for page in pages {
            show_page(&app, page);
        }
    });
}

/// Keep `live/app.json` fresh while the app runs, so servers send their
/// windows here.
fn heartbeat() {
    std::thread::spawn(|| loop {
        let dir = live_dir();
        let _ = std::fs::create_dir_all(&dir);
        let body = serde_json::json!({
            "app": "columbus-desktop",
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
        });
        let _ = std::fs::write(dir.join("app.json"), body.to_string());
        std::thread::sleep(Duration::from_secs(5));
    });
}

fn main() {
    #[allow(unused_mut)] // only Windows and Linux add a plugin here
    let mut builder = tauri::Builder::default();
    #[cfg(any(windows, target_os = "linux"))]
    {
        // A second launch (Windows and Linux deliver a link by starting the
        // app again) hands its link to the running app and exits.
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }));
    }
    builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(chat::ChatState::default())
        .manage(Frames::default())
        .invoke_handler(tauri::generate_handler![
            sessions,
            open_session,
            open_classic,
            chat_open_map,
            chat::chat_settings,
            chat::chat_save_settings,
            chat::chat_set_key,
            chat::chat_clear_key,
            chat::chat_start,
            chat::chat_send,
            chat::chat_set_model,
            workflows::workflows_state,
            workflows::workflow_install,
            workflows::workflow_decline,
            workflows::workflow_remove,
            workflows::workflow_set_schedule,
            workflows::workflow_run_now,
            workflows::workflow_runs,
            workflows::workflow_run_report,
            chat::chat_stop,
            chat::chat_answer,
            chat::chat_end,
            chat::chats_list,
            projects::projects_state,
            projects::project_save,
            projects::project_remove,
            datagroups::datagroups_state,
            datagroups::datagroup_save,
            datagroups::datagroup_remove,
            registry::registry_query,
            share::share_export,
            doc_import::doc_preview,
            shell::note_confirm,
            shell::notes_check,
            shell::note_suggestions,
            doc_import::doc_import,
            share::share_open,
            share::share_import,
            projects::project_dbs,
            registry::registry_update,
            chat::chat_transcript,
            chat::chats_search,
            chat::chat_rename,
            chat::chat_delete,
            open_settings,
            settings::settings_state,
            settings::settings_save_connection,
            settings::settings_delete_connection,
            settings::settings_set_switch,
            settings::settings_pick,
            settings::settings_test_connection,
            settings::settings_draft_notes,
            settings::settings_tools,
            settings::settings_tool_install,
            settings::settings_tool_decline,
            settings::settings_tool_remove,
            settings::settings_tool_files,
            shell::shell_info,
            shell::engine_check,
            shell::prefs_get,
            shell::prefs_set,
            shell::session_page,
            shell::session_files,
            shell::session_provenance,
            shell::sessions_live,
            shell::session_activity,
            shell::approvals_inbox,
            shell::approvals_open_browser,
            shell::audit_page,
            shell::audit_verify,
            shell::exports_list,
            shell::notes_list,
            shell::note_read,
            shell::note_mark_reviewed,
            shell::context_pack,
            shell::context_read,
            shell::project_databases,
            shell::skills_list,
        ])
        .setup(|app| {
            chat::set_dirs(app.handle());
            main_window(app.handle())?;
            heartbeat();
            let handle = app.handle().clone();
            app.deep_link()
                .on_open_url(move |event| handle_links(&handle, event.urls()));
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                handle_links(app.handle(), urls);
            }
            #[cfg(any(windows, target_os = "linux"))]
            {
                // Installed builds register the scheme; this covers running
                // from a folder.
                let _ = app.deep_link().register_all();
            }
            Ok(())
        })
        .on_window_event(|w, e| {
            if w.label() == chat::MAIN && matches!(e, tauri::WindowEvent::Destroyed) {
                chat::on_chat_window_closed(w.app_handle());
            }
        })
        .build(tauri::generate_context!())
        .expect("Columbus could not start")
        .run(|_app, event| {
            if let tauri::RunEvent::Exit = event {
                let _ = std::fs::remove_file(live_dir().join("app.json"));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_main_window_goes_only_to_its_pages_and_the_sessions_embed_page() {
        let ports: std::collections::HashSet<u16> = [40123].into_iter().collect();
        let ok = |u: &str| page_allowed(&Url::parse(u).unwrap(), &ports);
        assert!(ok("tauri://localhost/index.html"));
        assert!(ok("http://tauri.localhost/index.html"));
        assert!(ok("http://127.0.0.1:40123/embed#t=abc"));
        assert!(!ok("http://127.0.0.1:40123/#t=abc"), "the approvals page");
        assert!(!ok("http://127.0.0.1:40123/embed/x"));
        assert!(
            !ok("http://127.0.0.1:40999/embed#t=abc"),
            "a session that gave no link"
        );
        assert!(!ok("http://localhost:40123/embed"));
        assert!(!ok("https://example.org/embed"));
        assert!(!ok("http://evil.localhost/"));
    }

    /// No capability names a remote origin, so no page other than the
    /// app's own (the companion page in the main window's frame included)
    /// can call the app's commands.
    #[test]
    fn no_capability_reaches_remote_pages() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        let mut seen = 0;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let text = std::fs::read_to_string(e.path()).unwrap();
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert!(
                v.get("remote").is_none(),
                "{} has a remote entry",
                e.path().display()
            );
            assert!(
                !text.contains("\"urls\""),
                "{} names URLs",
                e.path().display()
            );
            let windows = v["windows"].as_array().unwrap();
            assert!(
                !windows
                    .iter()
                    .any(|w| w == "*" || w.as_str().is_some_and(|w| w.starts_with("session-"))),
                "{}: {windows:?}",
                e.path().display()
            );
            assert!(v.get("webviews").is_none());
            seen += 1;
        }
        assert!(seen >= 3);
        // And the frame is limited by the window's own policy too.
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let csp = conf["app"]["security"]["csp"].as_str().unwrap();
        assert!(csp.contains("frame-src http://127.0.0.1:*/embed;"), "{csp}");
        assert!(csp.contains("script-src 'self';"), "{csp}");
        assert!(conf["app"]
            .get("security")
            .unwrap()
            .get("dangerousDisableAssetCspModification")
            .is_none());
    }
}
