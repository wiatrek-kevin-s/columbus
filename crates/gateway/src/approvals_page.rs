//! The local approvals page (design section 3, "Approvals").
//!
//! When the person's AI client cannot show an approval prompt, Columbus
//! shows the request on a small web page served only on 127.0.0.1, opened
//! in the person's own browser. The agent never receives the page's
//! address.
//!
//! Protections:
//! * Bound to 127.0.0.1 on a random port; requests whose `Host` is not
//!   exactly `127.0.0.1:<port>` are refused (DNS rebinding).
//! * No cookies. Browsers send cookies for 127.0.0.1 to every port, so a
//!   program the agent starts on another port could collect them. Instead
//!   the browser is opened on `/#t=<launch token>`: the token sits in the
//!   URL fragment, which browsers never send to any server. The page's
//!   script trades it, once, for a session token that it keeps only in the
//!   page's memory (never a cookie or browser storage, which can reach
//!   disk) and sends as `Authorization: Bearer` with every call. Launch tokens expire after
//!   two minutes; sessions after 15 minutes without use.
//! * Exchanges and decisions also need a same-origin `Origin` (and
//!   `Sec-Fetch-Site`, when sent). The custom header forces a CORS
//!   preflight, which this server never answers, so other websites cannot
//!   call it.
//! * Strict Content-Security-Policy with a per-response nonce; request
//!   contents are rendered with `textContent`, never as HTML.
//! * Every decision is written to the audit log.
//! * Opening tabs is rate-limited and capped per session.
//!
//! The same page is the companion window (design section 8): with the UI
//! files installed beside the program (`ui/`), `/` serves the window that
//! shows the session's outputs on a map and in a table, and the approvals.
//! Its data calls (`/api/overview`, `/api/outputs`, `/api/layer`,
//! `/api/rows`, `/api/provenance/<id>`, `/api/state`) need a session token,
//! are read-only, and are written to the audit log.
//!
//! Two kinds of launch token, and of the session each becomes:
//! * **Approve:** only ever in an `http://127.0.0.1` link opened in the
//!   default browser, for a request waiting for the person (or when the
//!   person asks to review one). Only these sessions can decide.
//! * **View:** for the Columbus app. So that the app can find the session,
//!   the server writes `live/<session>.json` (the port only, no secret)
//!   under the person's Columbus folder; `POST /api/open`, which needs a
//!   custom header no website can send, opens a window on a view token, in
//!   the app through a `columbus://` link when the app is running
//!   (`live/app.json` has a recent heartbeat), else in the browser. Any
//!   program the person runs can register a handler for a custom URL
//!   scheme and so receive that link, so a view session can see what the
//!   session made but never approve anything: its approval cards ask the
//!   server to open the browser page instead (`POST /api/review`).
//!
//! Honest limits, documented for IT: an agent that can run programs as the
//! person could read the browser's command line in the moment it starts
//! (the launch token is single-use, so the person would then see "already
//! used"), and an agent that controls the person's own browser (a browser
//! automation extension) can press the buttons. The database's own
//! read-only login remains the guarantee nothing bypasses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use columbus_shared::approvals::{ApprovalStore, Decision};
use columbus_shared::audit::{AuditLog, AuditRecord};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::OnceCell;

const LAUNCH_TTL: Duration = Duration::from_secs(120);
const SESSION_IDLE: Duration = Duration::from_secs(15 * 60);
/// A tab that asked for updates this recently is treated as open.
const SEEN_WINDOW: Duration = Duration::from_secs(6);
/// Do not open more than one new tab in this period.
const RELAUNCH_GAP: Duration = Duration::from_secs(30);
/// Most tabs Columbus opens in one session for approvals.
const MAX_LAUNCHES: u32 = 10;
/// A window the Columbus app asked for: a short gap against double clicks,
/// and a cap.
const ASKED_GAP: Duration = Duration::from_secs(3);
const MAX_ASKED: u32 = 20;
/// The Columbus app counts as running while its heartbeat is this fresh.
const APP_HEARTBEAT: Duration = Duration::from_secs(20);

/// How the page is put in front of the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opener {
    /// The default browser.
    Browser,
    /// Not opened (`COLUMBUS_APPROVALS_BROWSER=none`).
    Disabled,
    /// Development and test builds only: the launch link is written to
    /// this file instead of opening a browser.
    #[cfg(debug_assertions)]
    File(PathBuf),
}

impl Opener {
    pub fn from_env() -> Self {
        #[cfg(debug_assertions)]
        if let Some(p) =
            std::env::var_os("COLUMBUS_APPROVALS_LAUNCH_FILE").filter(|v| !v.is_empty())
        {
            return Opener::File(PathBuf::from(p));
        }
        match std::env::var("COLUMBUS_APPROVALS_BROWSER").as_deref() {
            Ok("none") | Ok("off") => Opener::Disabled,
            _ => Opener::Browser,
        }
    }

    pub fn is_dev_handoff(&self) -> bool {
        #[cfg(debug_assertions)]
        if matches!(self, Opener::File(_)) {
            return true;
        }
        false
    }
}

/// What a launch token, and the session it becomes, may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// See the session and decide approvals: browser `http` links only.
    Approve,
    /// See the session only.
    View,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Approve => "approve",
            Scope::View => "view",
        }
    }
}

/// Why a window is being opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    /// A request is waiting and no approvals page is open.
    Approval,
    /// The person asked to review waiting requests in the browser.
    Review,
    /// The person asked the Columbus app for the session's window.
    View,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// A new tab was opened (or the link handed off).
    Opened,
    /// A page is already open and will show the request.
    AlreadyOpen,
}

struct Shared {
    port: u16,
    opener: Opener,
    /// The companion window's files (`index.html`, `assets/`), if installed.
    ui_dir: Option<PathBuf>,
    /// Where `live/<session>.json` and the app's heartbeat live.
    live_dir: Option<PathBuf>,
    companion: Option<Arc<crate::companion::Companion>>,
    /// When the person last asked for a window, and how many times.
    asked: Mutex<(Option<Instant>, u32)>,
    store: Arc<ApprovalStore>,
    audit: Arc<AuditLog>,
    user: String,
    session: String,
    launch: Mutex<HashMap<String, (Instant, Scope)>>,
    /// Session token → when that tab last used it, and what it may do.
    sessions: Mutex<HashMap<String, (Instant, Scope)>>,
    /// When a tab was last opened, and how many since the person last decided.
    launches: Mutex<(Option<Instant>, u32)>,
    /// The key the person's Columbus app sends to test a connection; it is
    /// in `live/<session>.key`, readable by this account only.
    test_key: String,
    /// One connection test at a time.
    testing: Arc<tokio::sync::Mutex<()>>,
    /// Challenges this server gave the app for a test, unused, with when.
    challenges: Mutex<Vec<(String, Instant)>>,
}

impl Shared {
    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// A page that can decide approvals asked for updates this recently.
    fn seen_within(&self, d: Duration) -> bool {
        self.sessions
            .lock()
            .expect("sessions")
            .values()
            .any(|(t, scope)| *scope == Scope::Approve && t.elapsed() < d)
    }

    /// The session behind a bearer token, refreshed on use. Expired
    /// sessions are dropped.
    fn session_for(&self, token: &str, now: Instant) -> Option<Scope> {
        let mut sessions = self.sessions.lock().expect("sessions");
        sessions.retain(|_, (last, _)| now.saturating_duration_since(*last) < SESSION_IDLE);
        let key = sessions.keys().find(|k| same(k, token)).cloned()?;
        let scope = sessions.get(&key).map(|(_, s)| *s)?;
        sessions.insert(key, (now, scope));
        Some(scope)
    }

    /// Open a window on a fresh launch token. Approval and review windows
    /// always open in the browser on an `http` link, with an approve token;
    /// the app's windows get a view token.
    fn launch(&self, why: Launch) -> Result<Shown, String> {
        match why {
            // Review opens the same page as an approval does, under the
            // same limits: an approve link in a browser's command line is
            // what a local program could try to catch.
            Launch::Approval | Launch::Review => {
                if self.seen_within(SEEN_WINDOW) {
                    return Ok(Shown::AlreadyOpen);
                }
                let mut l = self.launches.lock().expect("launches");
                if l.0.is_some_and(|t| t.elapsed() < RELAUNCH_GAP) {
                    return Ok(Shown::Opened);
                }
                if l.1 >= MAX_LAUNCHES {
                    return Err(format!(
                        "Columbus has opened its approvals page {MAX_LAUNCHES} times without a decision and will not open more tabs"
                    ));
                }
                *l = (Some(Instant::now()), l.1 + 1);
            }
            Launch::View => {
                let mut a = self.asked.lock().expect("asked");
                if a.0.is_some_and(|t| t.elapsed() < ASKED_GAP) {
                    return Ok(Shown::Opened);
                }
                if a.1 >= MAX_ASKED {
                    return Err("Columbus will not open more windows for this session".into());
                }
                *a = (Some(Instant::now()), a.1 + 1);
            }
        }
        let scope = if why == Launch::View {
            Scope::View
        } else {
            Scope::Approve
        };
        let t = token();
        {
            let mut launch = self.launch.lock().expect("launch");
            launch.retain(|_, (at, _)| at.elapsed() < LAUNCH_TTL);
            launch.insert(t.clone(), (Instant::now(), scope));
        }
        let page = format!("http://127.0.0.1:{}/#t={t}", self.port);
        let url = if scope == Scope::View && self.app_running() {
            format!("columbus://open?port={}#t={t}", self.port)
        } else {
            page.clone()
        };
        match &self.opener {
            Opener::Browser => {
                open_browser(&url).map_err(|e| format!("could not open the window ({e})"))?
            }
            Opener::Disabled => {
                return Err(
                    "opening the approvals page is turned off (COLUMBUS_APPROVALS_BROWSER=none)"
                        .into(),
                )
            }
            #[cfg(debug_assertions)]
            Opener::File(p) => std::fs::write(p, &url)
                .map_err(|e| format!("could not hand off the approvals page ({e})"))?,
        }
        Ok(Shown::Opened)
    }

    /// The Columbus app is running: it refreshes `live/app.json` every few
    /// seconds while it is.
    fn app_running(&self) -> bool {
        self.live_dir.as_ref().is_some_and(|d| {
            std::fs::metadata(d.join("app.json"))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < APP_HEARTBEAT)
        })
    }

    /// Spend a launch token: what its session may do, if it was valid.
    fn spend_launch(&self, t: &str, now: Instant) -> Option<Scope> {
        let mut launch = self.launch.lock().expect("launch");
        let found = launch.keys().find(|k| same(k, t)).cloned()?;
        launch
            .remove(&found)
            .filter(|(at, _)| now.saturating_duration_since(*at) < LAUNCH_TTL)
            .map(|(_, scope)| scope)
    }
}

pub struct ApprovalsPage {
    store: Arc<ApprovalStore>,
    audit: Arc<AuditLog>,
    user: String,
    session: String,
    opener: Opener,
    ui_dir: Option<PathBuf>,
    live_dir: Option<PathBuf>,
    companion: std::sync::OnceLock<Arc<crate::companion::Companion>>,
    /// The `live/<session>.json` this server wrote, removed when it stops,
    /// with what it says and when it last said the session was used.
    live_file: Mutex<Option<PathBuf>>,
    live_body: Mutex<Option<(serde_json::Value, Option<std::time::Instant>)>>,
    running: OnceCell<Result<Arc<Shared>, String>>,
}

impl Drop for ApprovalsPage {
    fn drop(&mut self) {
        if let Some(p) = self.live_file.lock().ok().and_then(|mut f| f.take()) {
            let _ = std::fs::remove_file(p.with_extension("key"));
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Write a secret to a new file only this account can read (an old file,
/// or a link, at that name is removed first, never followed).
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if std::fs::symlink_metadata(path).is_ok() {
        std::fs::remove_file(path)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

impl ApprovalsPage {
    pub fn new(
        store: Arc<ApprovalStore>,
        audit: Arc<AuditLog>,
        user: String,
        session: String,
        opener: Opener,
        ui_dir: Option<PathBuf>,
        live_dir: Option<PathBuf>,
    ) -> Self {
        ApprovalsPage {
            store,
            audit,
            user,
            session,
            opener,
            ui_dir,
            live_dir,
            companion: std::sync::OnceLock::new(),
            live_file: Mutex::new(None),
            live_body: Mutex::new(None),
            running: OnceCell::new(),
        }
    }

    /// Give the page the session's data, for the companion window. Before
    /// the server starts.
    pub fn attach(&self, c: Arc<crate::companion::Companion>) {
        let _ = self.companion.set(c);
    }

    /// Start the server now, not at the first approval, and say where it is
    /// in `live/<session>.json`, so the person's Columbus app can open it.
    /// `settings`: the settings files this server read (no secrets), so the
    /// Columbus app can tell when it edits other ones.
    pub async fn start_live(&self, settings: serde_json::Value) -> Result<PathBuf, String> {
        let shared = self.start().await?;
        let dir = self
            .live_dir
            .clone()
            .ok_or_else(|| "no folder for live sessions".to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("{}.json", self.session));
        let body = json!({
            "session": self.session,
            "port": shared.port,
            "pid": std::process::id(),
            "user": self.user,
            "started": chrono::Local::now().to_rfc3339(),
            "server": format!("columbus-mcp {}", env!("CARGO_PKG_VERSION")),
            "settings": settings,
        });
        // The key first: the app looks for it once the session is listed.
        write_private(&file.with_extension("key"), &shared.test_key).map_err(|e| e.to_string())?;
        write_private(
            &file,
            &String::from_utf8_lossy(&serde_json::to_vec_pretty(&body).unwrap_or_default()),
        )
        .map_err(|e| e.to_string())?;
        *self.live_file.lock().expect("live file") = Some(file.clone());
        *self.live_body.lock().expect("live body") = Some((body, None));
        Ok(file)
    }

    /// The session was just used (a tool call): say so in its live file,
    /// at most every few seconds, so the Columbus app can show which
    /// session is in use.
    pub fn touch_live(&self) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(5);
        let Some(file) = self.live_file.lock().ok().and_then(|f| f.clone()) else {
            return;
        };
        let mut g = match self.live_body.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some((body, last)) = g.as_mut() else {
            return;
        };
        if last.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        *last = Some(std::time::Instant::now());
        body["last_used"] = json!(chrono::Local::now().to_rfc3339());
        // Put the key back if something removed it (the app tidies away
        // files of sessions that did not answer in time).
        let key = file.with_extension("key");
        if std::fs::symlink_metadata(&key).is_err() {
            if let Some(shared) = self.running.get().and_then(|r| r.as_ref().ok()) {
                let _ = write_private(&key, &shared.test_key);
            }
        }
        let bytes = serde_json::to_vec_pretty(body).unwrap_or_default();
        // Written whole, then moved in place: the app never reads half a file.
        let tmp = file.with_extension("json.tmp");
        if write_private(&tmp, &String::from_utf8_lossy(&bytes)).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }

    async fn start(&self) -> Result<Arc<Shared>, String> {
        self.running
            .get_or_init(|| async {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .map_err(|e| format!("could not start the approvals page: {e}"))?;
                let port = listener.local_addr().map_err(|e| e.to_string())?.port();
                let shared = Arc::new(Shared {
                    port,
                    opener: self.opener.clone(),
                    ui_dir: self.ui_dir.clone(),
                    live_dir: self.live_dir.clone(),
                    companion: self.companion.get().cloned(),
                    asked: Mutex::new((None, 0)),
                    store: self.store.clone(),
                    audit: self.audit.clone(),
                    user: self.user.clone(),
                    session: self.session.clone(),
                    launch: Mutex::new(HashMap::new()),
                    sessions: Mutex::new(HashMap::new()),
                    launches: Mutex::new((None, 0)),
                    test_key: token(),
                    testing: Arc::new(tokio::sync::Mutex::new(())),
                    challenges: Mutex::new(Vec::new()),
                });
                let app = Router::new()
                    .route("/", get(page))
                    .route("/api/session", post(exchange))
                    .route("/api/approvals", get(list))
                    .route("/api/approvals/{id}", post(decide))
                    .route("/api/open", post(open_window))
                    .route("/api/test-hello", post(test_hello))
                    .route("/api/test-connection", post(test_connection))
                    .route("/api/review", post(review))
                    .route("/api/state", get(state))
                    .route("/api/overview", get(overview))
                    .route("/api/outputs", get(outputs))
                    .route("/api/layer", get(layer))
                    .route("/api/rows", get(rows))
                    .route("/api/provenance/{id}", get(provenance))
                    .route("/assets/{file}", get(asset))
                    .fallback(not_found)
                    .layer(axum::extract::DefaultBodyLimit::max(4096))
                    .with_state(shared.clone());
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await {
                        tracing::warn!("approvals page stopped: {e}");
                    }
                });
                Ok(shared)
            })
            .await
            .clone()
    }

    /// Put the approvals page in front of the person. Never returns or
    /// logs the launch link.
    pub async fn show(&self) -> Result<Shown, String> {
        self.start().await?.launch(Launch::Approval)
    }

    /// Whether a page tab is open right now.
    pub fn is_open(&self) -> bool {
        self.running
            .get()
            .and_then(|r| r.as_ref().ok())
            .is_some_and(|s| s.seen_within(SEEN_WINDOW))
    }
}

/// Start the person's default browser on `url`, by absolute path where the
/// platform has one, and reap the child so it does not linger.
pub(crate) fn open_browser(url: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut c = Command::new(
            std::path::Path::new(&root)
                .join("System32")
                .join("rundll32.exe"),
        );
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/open");
        c.arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// ---------- HTTP ----------

fn harden(mut r: Response<Body>, csp: Option<String>) -> Response<Body> {
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(
            &csp.unwrap_or_else(|| "default-src 'none'; frame-ancestors 'none'".into()),
        )
        .expect("ascii"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    r
}

fn json_response(code: StatusCode, body: serde_json::Value) -> Response<Body> {
    let mut r = Response::new(Body::from(body.to_string()));
    *r.status_mut() = code;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    harden(r, None)
}

fn status(code: StatusCode, msg: &str) -> Response<Body> {
    json_response(code, json!({"error": msg}))
}

fn host_ok(h: &HeaderMap, s: &Shared) -> bool {
    h.get(header::HOST).and_then(|v| v.to_str().ok())
        == Some(format!("127.0.0.1:{}", s.port).as_str())
}

/// Same-origin requests from the page only (exchanges and decisions).
fn same_origin(h: &HeaderMap, s: &Shared) -> bool {
    let origin_ok =
        h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(s.origin().as_str());
    let site_ok = h
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|site| site == "same-origin");
    origin_ok && site_ok
}

fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Host and session checks for API requests.
fn guard(h: &HeaderMap, s: &Shared) -> Result<Scope, Box<Response<Body>>> {
    if !host_ok(h, s) {
        return Err(Box::new(status(StatusCode::FORBIDDEN, "wrong host")));
    }
    bearer(h)
        .and_then(|t| s.session_for(t, Instant::now()))
        .ok_or_else(|| Box::new(status(StatusCode::UNAUTHORIZED, "not signed in")))
}

async fn not_found() -> Response<Body> {
    status(StatusCode::NOT_FOUND, "not found")
}

/// The page shell. It holds no secrets and shows nothing until its script
/// has exchanged the launch token from the URL fragment for a session.
async fn page(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if !host_ok(&h, &s) {
        return status(StatusCode::FORBIDDEN, "wrong host");
    }
    if let Some(index) = s
        .ui_dir
        .as_ref()
        .filter(|_| s.companion.is_some())
        .and_then(|d| std::fs::read_to_string(d.join("index.html")).ok())
    {
        let mut r = Response::new(Body::from(index));
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        let basemap = s.companion.as_ref().is_some_and(|c| c.basemap_allowed());
        return harden(r, Some(ui_csp(basemap)));
    }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let html = PAGE.replace("{{NONCE}}", &nonce);
    let mut r = Response::new(Body::from(html));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    let csp = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; connect-src 'self'; \
         img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    harden(r, Some(csp))
}

#[derive(Deserialize)]
struct ExchangeBody {
    launch: String,
}

async fn exchange(
    State(s): State<Arc<Shared>>,
    h: HeaderMap,
    body: Result<Json<ExchangeBody>, axum::extract::rejection::JsonRejection>,
) -> Response<Body> {
    if !host_ok(&h, &s) {
        return status(StatusCode::FORBIDDEN, "wrong host");
    }
    if !same_origin(&h, &s) {
        return status(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    let Ok(Json(body)) = body else {
        return status(StatusCode::BAD_REQUEST, "expected {\"launch\": \"…\"}");
    };
    let now = Instant::now();
    let Some(scope) = s.spend_launch(&body.launch, now) else {
        return status(
            StatusCode::FORBIDDEN,
            "this link has already been used or has expired",
        );
    };
    let sid = token();
    s.sessions
        .lock()
        .expect("sessions")
        .insert(sid.clone(), (now, scope));
    json_response(
        StatusCode::OK,
        json!({"token": sid, "idle_minutes": SESSION_IDLE.as_secs() / 60, "scope": scope.as_str()}),
    )
}

async fn list(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if let Err(r) = guard(&h, &s) {
        return *r;
    }
    let now = Utc::now();
    json_response(
        StatusCode::OK,
        json!({
            "session": s.session,
            "pending": s.store.pending(now),
            "recent": s.store.recent(10, now),
        }),
    )
}

#[derive(Deserialize)]
struct DecideBody {
    decision: Decision,
}

async fn decide(
    State(s): State<Arc<Shared>>,
    Path(id): Path<String>,
    h: HeaderMap,
    body: Result<Json<DecideBody>, axum::extract::rejection::JsonRejection>,
) -> Response<Body> {
    match guard(&h, &s) {
        Err(r) => return *r,
        Ok(Scope::View) => {
            return status(
                StatusCode::FORBIDDEN,
                "this window can show requests but not decide them; decide in the browser page Columbus opens",
            )
        }
        Ok(Scope::Approve) => {}
    }
    if !same_origin(&h, &s) {
        return status(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    let Ok(Json(mut body)) = body else {
        return status(
            StatusCode::BAD_REQUEST,
            "expected {\"decision\": \"decline\" | \"allow_once\" | \"always_this_session\"}",
        );
    };
    // Scripts are approved one run at a time, edits one change set at a
    // time: "for this session" counts as once.
    if body.decision == Decision::AlwaysThisSession
        && s.store.status(&id, Utc::now()).is_some_and(|a| {
            matches!(
                a.risk,
                columbus_shared::approvals::Risk::Script | columbus_shared::approvals::Risk::Edit
            )
        })
    {
        body.decision = Decision::AllowOnce;
    }
    match s.store.decide_via(&id, body.decision, Utc::now(), "page") {
        Ok(a) => {
            // The person is here: opening tabs is fine again.
            shared_reset_launches(&s);
            let rec = AuditRecord {
                user: s.user.clone(),
                client: "approvals-page".into(),
                tool: "approval_decision".into(),
                decision: if body.decision == Decision::Decline {
                    "declined"
                } else {
                    "approved"
                }
                .into(),
                reason: Some(format!("{:?}", body.decision).to_lowercase()),
                approval: Some(a.id.clone()),
                request: Some(a.hash.clone()),
                ..Default::default()
            };
            if let Err(e) = s.audit.append(&rec) {
                return status(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("decision recorded but the audit log could not be written: {e}"),
                );
            }
            json_response(StatusCode::OK, json!({"id": a.id, "state": a.state}))
        }
        Err(e) => status(StatusCode::CONFLICT, &e.to_string()),
    }
}

fn shared_reset_launches(s: &Shared) {
    s.launches.lock().expect("launches").1 = 0;
}

const PAGE: &str = include_str!("approvals_page.html");

/// The companion window's policy: its own scripts and styles only, data
/// from this server, and, when organization policy allows the basemap,
/// tiles, glyphs and sprites from one public host (OpenFreeMap; the window
/// works without them). MapLibre's worker is its own file or a blob.
pub(crate) fn ui_csp(basemap: bool) -> String {
    let tiles = if basemap {
        " https://tiles.openfreemap.org"
    } else {
        ""
    };
    format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'{tiles}; \
         img-src 'self' data: blob:{tiles}; font-src 'self'; worker-src 'self' blob:; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )
}

/// `POST /api/open`: a new window on a fresh launch token, for the
/// person's Columbus app. Needs `X-Columbus-Open: 1`, which a website
/// cannot send without a preflight this server never answers; it returns
/// nothing but whether a window was opened.
async fn open_window(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if !host_ok(&h, &s) {
        return status(StatusCode::FORBIDDEN, "wrong host");
    }
    if h.get("x-columbus-open").and_then(|v| v.to_str().ok()) != Some("1")
        || h.contains_key(header::ORIGIN)
    {
        return status(StatusCode::FORBIDDEN, "refused");
    }
    match s.launch(Launch::View) {
        Ok(_) => json_response(StatusCode::OK, json!({"opened": true})),
        Err(e) => status(StatusCode::TOO_MANY_REQUESTS, &e),
    }
}

#[derive(Deserialize)]
struct TestRequest {
    connection: serde_json::Value,
    #[serde(default)]
    password: Option<String>,
}

/// How long a test challenge stays good.
const CHALLENGE_TTL: Duration = Duration::from_secs(60);

/// Refuse what no Columbus app sends: another host name (DNS rebinding) or
/// any `Origin` (every web page, this server's own included).
fn from_app(h: &HeaderMap, s: &Shared) -> Result<(), Box<Response<Body>>> {
    if !host_ok(h, s) {
        return Err(Box::new(status(StatusCode::FORBIDDEN, "wrong host")));
    }
    if h.contains_key(header::ORIGIN) {
        return Err(Box::new(status(StatusCode::FORBIDDEN, "refused")));
    }
    Ok(())
}

/// `POST /api/test-hello`: the first half of a test. The app sends a fresh
/// challenge (`X-Columbus-Test-Hello`, 64 hex digits) and gets back this
/// server's proof that it holds the key in `live/<session>.key`, so the app
/// knows it talks to its own Columbus before it sends a password; and a
/// challenge of the server's own for the request that follows. The key
/// itself is never sent, either way.
async fn test_hello(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if let Err(r) = from_app(&h, &s) {
        return *r;
    }
    let Some(theirs) = h
        .get("x-columbus-test-hello")
        .and_then(|v| v.to_str().ok())
        .filter(|c| c.len() == 64 && c.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return status(StatusCode::BAD_REQUEST, "expected a challenge");
    };
    let ours = token();
    {
        let mut c = s.challenges.lock().expect("challenges");
        c.retain(|(_, at)| at.elapsed() < CHALLENGE_TTL);
        // The oldest gives way: no one can fill the table to block the app.
        if c.len() >= 8 {
            c.remove(0);
        }
        c.push((ours.clone(), Instant::now()));
    }
    let proof = columbus_shared::proof::prove(
        &s.test_key,
        columbus_shared::proof::SERVER,
        &[theirs.as_bytes()],
    );
    json_response(StatusCode::OK, json!({"challenge": ours, "proof": proof}))
}

/// `POST /api/test-connection`: the Settings window's Test button, from the
/// person's Columbus app. `X-Columbus-Test: <challenge>.<proof>`: one of
/// this server's unused challenges from `/api/test-hello`, and the app's
/// proof over it and the body, with the key only this account can read.
/// Saves nothing.
async fn test_connection(
    State(s): State<Arc<Shared>>,
    h: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    use columbus_shared::proof;
    if let Err(r) = from_app(&h, &s) {
        return *r;
    }
    let Some((challenge, given)) = h
        .get("x-columbus-test")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once('.'))
    else {
        return status(StatusCode::FORBIDDEN, "refused");
    };
    // Spent whether or not the proof holds: one try per challenge.
    let fresh = {
        let mut c = s.challenges.lock().expect("challenges");
        let found = c
            .iter()
            .position(|(k, at)| proof::same(k, challenge) && at.elapsed() < CHALLENGE_TTL);
        found.map(|i| c.swap_remove(i)).is_some()
    };
    let want = proof::prove(
        &s.test_key,
        proof::APP,
        &[challenge.as_bytes(), proof::body_hash(&body).as_bytes()],
    );
    if !fresh || !proof::same(given, &want) {
        return status(StatusCode::FORBIDDEN, "refused");
    }
    let Some(companion) = s.companion.clone() else {
        return status(
            StatusCode::SERVICE_UNAVAILABLE,
            "this session cannot test connections",
        );
    };
    let Ok(req) = serde_json::from_slice::<TestRequest>(&body) else {
        return status(StatusCode::BAD_REQUEST, "expected {connection, password}");
    };
    let Ok(one) = s.testing.clone().try_lock_owned() else {
        return status(StatusCode::TOO_MANY_REQUESTS, "a test is already running");
    };
    // Runs to the end, holding the one-at-a-time guard, even if the app
    // stops waiting: the engine is never left half way through a request.
    let job = tokio::spawn(async move {
        let v = companion
            .test_connection(req.connection, req.password)
            .await;
        drop(one);
        v
    });
    match job.await {
        Ok(v) => json_response(StatusCode::OK, v),
        Err(e) => status(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// `POST /api/review`: from a window that may not decide, open the
/// browser page that can, for requests waiting now.
async fn review(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if let Err(r) = guard(&h, &s) {
        return *r;
    }
    if !same_origin(&h, &s) {
        return status(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    if s.store.pending(Utc::now()).is_empty() {
        return status(StatusCode::CONFLICT, "nothing is waiting for a decision");
    }
    match s.launch(Launch::Review) {
        Ok(_) => json_response(StatusCode::OK, json!({"opened": true})),
        Err(e) => status(StatusCode::TOO_MANY_REQUESTS, &e),
    }
}

/// A window read, in the audit log beside the agent's own calls.
fn audit_view(s: &Shared, scope: Scope, tool: &str, handle: &str) {
    let _ = s.audit.append(&AuditRecord {
        user: s.user.clone(),
        client: format!("companion-window ({})", scope.as_str()),
        tool: tool.into(),
        decision: "allowed".into(),
        reason: Some(handle.to_string()),
        ..Default::default()
    });
}

fn companion(s: &Shared) -> Result<&crate::companion::Companion, Box<Response<Body>>> {
    s.companion
        .as_deref()
        .ok_or_else(|| Box::new(status(StatusCode::NOT_FOUND, "no companion data here")))
}

fn answer(r: Result<serde_json::Value, String>) -> Response<Body> {
    match r {
        Ok(v) => json_response(StatusCode::OK, v),
        Err(e) => status(StatusCode::UNPROCESSABLE_ENTITY, &e),
    }
}

/// `GET /api/state`: what changed, for the window's polling.
async fn state(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    let scope = match guard(&h, &s) {
        Ok(scope) => scope,
        Err(r) => return *r,
    };
    let now = Utc::now();
    json_response(
        StatusCode::OK,
        json!({
            "session": s.session,
            "version": s.companion.as_ref().map(|c| c.version()).unwrap_or(0),
            "pending": s.store.pending(now).len(),
            "scope": scope.as_str(),
        }),
    )
}

async fn overview(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if let Err(r) = guard(&h, &s) {
        return *r;
    }
    match companion(&s) {
        Ok(c) => answer(c.overview()),
        Err(r) => *r,
    }
}

async fn outputs(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if let Err(r) = guard(&h, &s) {
        return *r;
    }
    match companion(&s) {
        Ok(c) => answer(c.outputs()),
        Err(r) => *r,
    }
}

#[derive(Deserialize)]
struct LayerQuery {
    handle: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

async fn layer(
    State(s): State<Arc<Shared>>,
    h: HeaderMap,
    q: Result<Query<LayerQuery>, axum::extract::rejection::QueryRejection>,
) -> Response<Body> {
    let scope = match guard(&h, &s) {
        Ok(scope) => scope,
        Err(r) => return *r,
    };
    let Ok(Query(q)) = q else {
        return status(StatusCode::BAD_REQUEST, "expected ?handle=ws://…");
    };
    audit_view(&s, scope, "window_layer", &q.handle);
    match companion(&s) {
        Ok(c) => answer(c.layer(&q.handle).await),
        Err(r) => *r,
    }
}

async fn rows(
    State(s): State<Arc<Shared>>,
    h: HeaderMap,
    q: Result<Query<LayerQuery>, axum::extract::rejection::QueryRejection>,
) -> Response<Body> {
    let scope = match guard(&h, &s) {
        Ok(scope) => scope,
        Err(r) => return *r,
    };
    let Ok(Query(q)) = q else {
        return status(
            StatusCode::BAD_REQUEST,
            "expected ?handle=ws://…&offset=0&limit=100",
        );
    };
    audit_view(&s, scope, "window_rows", &q.handle);
    match companion(&s) {
        Ok(c) => answer(
            c.rows(&q.handle, q.offset.unwrap_or(0), q.limit.unwrap_or(100))
                .await,
        ),
        Err(r) => *r,
    }
}

async fn provenance(
    State(s): State<Arc<Shared>>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Response<Body> {
    if let Err(r) = guard(&h, &s) {
        return *r;
    }
    match companion(&s) {
        Ok(c) => answer(c.provenance(&id)),
        Err(r) => *r,
    }
}

/// `GET /assets/<file>`: the companion window's scripts, styles and
/// images, by plain file name only, from the installed `ui/assets`.
async fn asset(
    State(s): State<Arc<Shared>>,
    Path(file): Path<String>,
    h: HeaderMap,
) -> Response<Body> {
    if !host_ok(&h, &s) {
        return status(StatusCode::FORBIDDEN, "wrong host");
    }
    let plain = !file.is_empty()
        && file.len() <= 128
        && !file.starts_with('.')
        && file
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    let Some(dir) = s.ui_dir.as_ref().filter(|_| plain) else {
        return status(StatusCode::NOT_FOUND, "not found");
    };
    let path = dir.join("assets").join(&file);
    let is_file = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
    let Some(bytes) = is_file.then(|| std::fs::read(&path).ok()).flatten() else {
        return status(StatusCode::NOT_FOUND, "not found");
    };
    let kind = match file.rsplit('.').next().unwrap_or("") {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    let mut r = Response::new(Body::from(bytes));
    r.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    // A script may run as a worker, which follows the policy it was served
    // with, not the page's: the map's worker fetches the basemap tiles.
    let csp = kind
        .starts_with("text/javascript")
        .then(|| ui_csp(s.companion.as_ref().is_some_and(|c| c.basemap_allowed())));
    let mut r = harden(r, csp);
    // Built file names carry a content hash; they never change.
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=86400"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> (tempfile::TempDir, Shared) {
        let d = tempfile::tempdir().unwrap();
        let s = Shared {
            port: 1,
            opener: Opener::Disabled,
            ui_dir: None,
            live_dir: None,
            companion: None,
            asked: Mutex::new((None, 0)),
            store: Arc::new(ApprovalStore::new()),
            audit: Arc::new(AuditLog::new(d.path()).unwrap()),
            user: "u".into(),
            session: "s".into(),
            launch: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            launches: Mutex::new((None, 0)),
            test_key: token(),
            testing: Arc::new(tokio::sync::Mutex::new(())),
            challenges: Mutex::new(Vec::new()),
        };
        (d, s)
    }

    #[test]
    fn constant_time_compare() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn sessions_expire_when_idle() {
        let (_d, s) = shared();
        let now = Instant::now();
        s.sessions
            .lock()
            .unwrap()
            .insert("fresh".into(), (now, Scope::Approve));
        let old = now
            .checked_sub(SESSION_IDLE + Duration::from_secs(1))
            .unwrap_or(now);
        s.sessions
            .lock()
            .unwrap()
            .insert("stale".into(), (old, Scope::Approve));
        assert!(s.session_for("fresh", now).is_some());
        if old < now {
            assert!(s.session_for("stale", now).is_none());
            assert!(
                !s.sessions.lock().unwrap().contains_key("stale"),
                "expired sessions are dropped"
            );
        }
        assert!(s.session_for("guess", now).is_none());
    }

    #[test]
    fn review_links_are_limited_like_approval_links() {
        let (d, mut s) = shared();
        s.opener = Opener::File(d.path().join("link.txt"));
        assert_eq!(s.launch(Launch::Review).unwrap(), Shown::Opened);
        let link = std::fs::read_to_string(d.path().join("link.txt")).unwrap();
        assert!(
            link.starts_with("http://127.0.0.1:"),
            "approve links are http: {link}"
        );
        // A second ask straight after mints nothing new.
        let _ = s.launch(Launch::Review);
        let _ = s.launch(Launch::Approval);
        assert_eq!(s.launch.lock().unwrap().len(), 1);
        // App windows have their own allowance, and view tokens.
        s.launch(Launch::View).unwrap();
        let scopes: Vec<Scope> = s
            .launch
            .lock()
            .unwrap()
            .values()
            .map(|(_, sc)| *sc)
            .collect();
        assert!(scopes.contains(&Scope::View) && scopes.contains(&Scope::Approve));
    }

    #[test]
    fn launch_tokens_are_single_use_and_expire() {
        let (_d, s) = shared();
        let now = Instant::now();
        s.launch
            .lock()
            .unwrap()
            .insert("a".into(), (now, Scope::View));
        assert_eq!(
            s.spend_launch("a", now),
            Some(Scope::View),
            "the scope comes back"
        );
        assert_eq!(s.spend_launch("a", now), None, "single use");
        if let Some(old) = now.checked_sub(LAUNCH_TTL + Duration::from_secs(1)) {
            s.launch
                .lock()
                .unwrap()
                .insert("b".into(), (old, Scope::Approve));
            assert_eq!(s.spend_launch("b", now), None, "expired");
        }
    }

    #[test]
    fn page_template_has_no_secrets_and_no_inner_html() {
        assert!(PAGE.contains("{{NONCE}}"));
        assert!(!PAGE.contains("{{CSRF}}") && !PAGE.contains("{{SESSION}}"));
        assert!(
            !PAGE.contains("innerHTML"),
            "request text must be rendered with textContent"
        );
        assert!(!PAGE.contains("document.cookie"));
        assert!(
            !PAGE.contains("sessionStorage") && !PAGE.contains("localStorage"),
            "the session token is kept in memory only"
        );
        assert!(
            PAGE.contains("hashchange"),
            "a link pasted into an open tab works"
        );
        assert!(
            PAGE.contains("history.replaceState"),
            "the launch token is removed from the address bar"
        );
    }
}
