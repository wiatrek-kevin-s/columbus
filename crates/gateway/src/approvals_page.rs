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
//! Honest limits, documented for IT: an agent that can run programs as the
//! person could read the browser's command line in the moment it starts
//! (the launch token is single-use, so the person would then see "already
//! used"), and an agent that controls the person's own browser (a browser
//! automation extension) can press the buttons. The database's own
//! read-only login remains the guarantee nothing bypasses.

use std::collections::HashMap;
#[cfg(debug_assertions)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, State};
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
/// Most tabs Columbus opens in one session.
const MAX_LAUNCHES: u32 = 10;

/// How the page is put in front of the person.
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// A new tab was opened (or the link handed off).
    Opened,
    /// A page is already open and will show the request.
    AlreadyOpen,
}

struct Shared {
    port: u16,
    store: Arc<ApprovalStore>,
    audit: Arc<AuditLog>,
    user: String,
    session: String,
    launch: Mutex<HashMap<String, Instant>>,
    /// Session token → when that tab last used it.
    sessions: Mutex<HashMap<String, Instant>>,
    /// When a tab was last opened, and how many since the person last decided.
    launches: Mutex<(Option<Instant>, u32)>,
}

impl Shared {
    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn seen_within(&self, d: Duration) -> bool {
        self.sessions
            .lock()
            .expect("sessions")
            .values()
            .any(|t| t.elapsed() < d)
    }

    /// The session behind a bearer token, refreshed on use. Expired
    /// sessions are dropped.
    fn session_for(&self, token: &str, now: Instant) -> Option<String> {
        let mut sessions = self.sessions.lock().expect("sessions");
        sessions.retain(|_, last| now.saturating_duration_since(*last) < SESSION_IDLE);
        let key = sessions.keys().find(|k| same(k, token)).cloned()?;
        sessions.insert(key.clone(), now);
        Some(key)
    }

    /// Spend a launch token; true if it was valid.
    fn spend_launch(&self, t: &str, now: Instant) -> bool {
        let mut launch = self.launch.lock().expect("launch");
        let found = launch.keys().find(|k| same(k, t)).cloned();
        match found {
            Some(k) => launch
                .remove(&k)
                .is_some_and(|at| now.saturating_duration_since(at) < LAUNCH_TTL),
            None => false,
        }
    }
}

pub struct ApprovalsPage {
    store: Arc<ApprovalStore>,
    audit: Arc<AuditLog>,
    user: String,
    session: String,
    opener: Opener,
    running: OnceCell<Result<Arc<Shared>, String>>,
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
    ) -> Self {
        ApprovalsPage {
            store,
            audit,
            user,
            session,
            opener,
            running: OnceCell::new(),
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
                    store: self.store.clone(),
                    audit: self.audit.clone(),
                    user: self.user.clone(),
                    session: self.session.clone(),
                    launch: Mutex::new(HashMap::new()),
                    sessions: Mutex::new(HashMap::new()),
                    launches: Mutex::new((None, 0)),
                });
                let app = Router::new()
                    .route("/", get(page))
                    .route("/api/session", post(exchange))
                    .route("/api/approvals", get(list))
                    .route("/api/approvals/{id}", post(decide))
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
        let shared = self.start().await?;
        if shared.seen_within(SEEN_WINDOW) {
            return Ok(Shown::AlreadyOpen);
        }
        {
            let mut l = shared.launches.lock().expect("launches");
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
        let t = token();
        {
            let mut launch = shared.launch.lock().expect("launch");
            launch.retain(|_, at| at.elapsed() < LAUNCH_TTL);
            launch.insert(t.clone(), Instant::now());
        }
        let url = format!("http://127.0.0.1:{}/#t={t}", shared.port);
        match &self.opener {
            Opener::Browser => {
                open_browser(&url).map_err(|e| format!("could not start the browser ({e})"))?
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
fn guard(h: &HeaderMap, s: &Shared) -> Result<String, Box<Response<Body>>> {
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
    if !s.spend_launch(&body.launch, now) {
        return status(
            StatusCode::FORBIDDEN,
            "this link has already been used or has expired",
        );
    }
    let sid = token();
    s.sessions
        .lock()
        .expect("sessions")
        .insert(sid.clone(), now);
    json_response(
        StatusCode::OK,
        json!({"token": sid, "idle_minutes": SESSION_IDLE.as_secs() / 60}),
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
    if let Err(r) = guard(&h, &s) {
        return *r;
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
    // Scripts are approved one run at a time: "for this session" counts as once.
    if body.decision == Decision::AlwaysThisSession
        && s.store
            .status(&id, Utc::now())
            .is_some_and(|a| a.risk == columbus_shared::approvals::Risk::Script)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> (tempfile::TempDir, Shared) {
        let d = tempfile::tempdir().unwrap();
        let s = Shared {
            port: 1,
            store: Arc::new(ApprovalStore::new()),
            audit: Arc::new(AuditLog::new(d.path()).unwrap()),
            user: "u".into(),
            session: "s".into(),
            launch: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            launches: Mutex::new((None, 0)),
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
        s.sessions.lock().unwrap().insert("fresh".into(), now);
        let old = now
            .checked_sub(SESSION_IDLE + Duration::from_secs(1))
            .unwrap_or(now);
        s.sessions.lock().unwrap().insert("stale".into(), old);
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
    fn launch_tokens_are_single_use_and_expire() {
        let (_d, s) = shared();
        let now = Instant::now();
        s.launch.lock().unwrap().insert("a".into(), now);
        assert!(s.spend_launch("a", now));
        assert!(!s.spend_launch("a", now), "single use");
        if let Some(old) = now.checked_sub(LAUNCH_TTL + Duration::from_secs(1)) {
            s.launch.lock().unwrap().insert("b".into(), old);
            assert!(!s.spend_launch("b", now), "expired");
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
