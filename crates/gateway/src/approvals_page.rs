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
//! * The browser is opened on a single-use launch link that expires in two
//!   minutes. Using it sets a session cookie (`HttpOnly`, `SameSite=Strict`,
//!   named per port so two Columbus sessions do not collide) and the link
//!   stops working.
//! * Every decision needs that cookie, a per-session CSRF token in a
//!   header, and a same-origin `Origin` (and `Sec-Fetch-Site`, when sent).
//! * Strict Content-Security-Policy with a per-response nonce; request
//!   contents are rendered with `textContent`, never as HTML.
//! * Every decision is written to the audit log.
//!
//! Honest limit: an agent that can run arbitrary programs as the person
//! can, in principle, read another process's command line in the moment
//! the browser is launched. Use agents with shell access only where that
//! is acceptable; the database's own read-only login remains the guarantee.

use std::collections::HashMap;
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
/// A tab that asked for updates this recently is treated as open.
const SEEN_WINDOW: Duration = Duration::from_secs(6);
/// Do not open more than one new tab in this period.
const RELAUNCH_GAP: Duration = Duration::from_secs(10);

/// How the page is put in front of the person.
#[derive(Debug, Clone)]
pub enum Opener {
    /// The default browser.
    Browser,
    /// Not opened (`COLUMBUS_APPROVALS_BROWSER=none`).
    Disabled,
    /// Development and tests only: the launch link is written to this file.
    File(PathBuf),
}

impl Opener {
    pub fn from_env() -> Self {
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
    csrf: String,
    store: Arc<ApprovalStore>,
    audit: Arc<AuditLog>,
    user: String,
    session: String,
    launch: Mutex<HashMap<String, Instant>>,
    /// Session cookie value → when that tab last asked for updates.
    sessions: Mutex<HashMap<String, Instant>>,
}

impl Shared {
    fn cookie_name(&self) -> String {
        format!("columbus_{}", self.port)
    }

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
}

pub struct ApprovalsPage {
    store: Arc<ApprovalStore>,
    audit: Arc<AuditLog>,
    user: String,
    session: String,
    opener: Opener,
    running: OnceCell<Result<Arc<Shared>, String>>,
    last_launch: Mutex<Option<Instant>>,
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
            last_launch: Mutex::new(None),
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
                    csrf: token(),
                    store: self.store.clone(),
                    audit: self.audit.clone(),
                    user: self.user.clone(),
                    session: self.session.clone(),
                    launch: Mutex::new(HashMap::new()),
                    sessions: Mutex::new(HashMap::new()),
                });
                let app = Router::new()
                    .route("/", get(page))
                    .route("/open/{token}", get(open))
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
            let mut last = self.last_launch.lock().expect("last launch");
            if last.is_some_and(|t| t.elapsed() < RELAUNCH_GAP) {
                return Ok(Shown::Opened);
            }
            *last = Some(Instant::now());
        }
        let t = token();
        {
            let mut launch = shared.launch.lock().expect("launch");
            launch.retain(|_, at| at.elapsed() < LAUNCH_TTL);
            launch.insert(t.clone(), Instant::now());
        }
        let url = format!("http://127.0.0.1:{}/open/{t}", shared.port);
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

fn open_browser(url: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
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
    r
}

fn status(code: StatusCode, msg: &str) -> Response<Body> {
    let body = json!({"error": msg}).to_string();
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = code;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    harden(r, None)
}

fn host_ok(h: &HeaderMap, s: &Shared) -> bool {
    h.get(header::HOST).and_then(|v| v.to_str().ok())
        == Some(format!("127.0.0.1:{}", s.port).as_str())
}

fn session_of(h: &HeaderMap, s: &Shared) -> Option<String> {
    let name = s.cookie_name();
    let cookies = h.get_all(header::COOKIE);
    for v in cookies.iter().filter_map(|v| v.to_str().ok()) {
        for part in v.split(';') {
            if let Some((k, val)) = part.trim().split_once('=') {
                if k == name && s.sessions.lock().expect("sessions").contains_key(val) {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

/// Host and session checks for every page request.
fn guard(h: &HeaderMap, s: &Shared) -> Result<String, Box<Response<Body>>> {
    if !host_ok(h, s) {
        return Err(Box::new(status(StatusCode::FORBIDDEN, "wrong host")));
    }
    session_of(h, s).ok_or_else(|| {
        Box::new(status(
            StatusCode::FORBIDDEN,
            "not signed in to this approvals page",
        ))
    })
}

async fn not_found() -> Response<Body> {
    status(StatusCode::NOT_FOUND, "not found")
}

async fn open(State(s): State<Arc<Shared>>, Path(t): Path<String>, h: HeaderMap) -> Response<Body> {
    if !host_ok(&h, &s) {
        return status(StatusCode::FORBIDDEN, "wrong host");
    }
    let valid = {
        let mut launch = s.launch.lock().expect("launch");
        let found = launch.keys().find(|k| same(k, &t)).cloned();
        match found {
            Some(k) => launch
                .remove(&k)
                .is_some_and(|at| at.elapsed() < LAUNCH_TTL),
            None => false,
        }
    };
    if !valid {
        let mut r = Response::new(Body::from(GONE_PAGE));
        *r.status_mut() = StatusCode::FORBIDDEN;
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        return harden(
            r,
            Some("default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'".into()),
        );
    }
    let sid = token();
    s.sessions
        .lock()
        .expect("sessions")
        .insert(sid.clone(), Instant::now());
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::SEE_OTHER;
    let h = r.headers_mut();
    h.insert(header::LOCATION, HeaderValue::from_static("/"));
    h.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{}={sid}; HttpOnly; SameSite=Strict; Path=/",
            s.cookie_name()
        ))
        .expect("ascii"),
    );
    harden(r, None)
}

async fn page(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    if let Err(r) = guard(&h, &s) {
        if host_ok(&h, &s) {
            let mut r = Response::new(Body::from(GONE_PAGE));
            *r.status_mut() = StatusCode::FORBIDDEN;
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            return harden(
                r,
                Some(
                    "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'".into(),
                ),
            );
        }
        return *r;
    }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let html = PAGE
        .replace("{{NONCE}}", &nonce)
        .replace("{{CSRF}}", &s.csrf)
        .replace("{{SESSION}}", &html_escape(&s.session));
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

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

async fn list(State(s): State<Arc<Shared>>, h: HeaderMap) -> Response<Body> {
    let sid = match guard(&h, &s) {
        Ok(sid) => sid,
        Err(r) => return *r,
    };
    s.sessions
        .lock()
        .expect("sessions")
        .insert(sid, Instant::now());
    let now = Utc::now();
    let body = json!({
        "session": s.session,
        "pending": s.store.pending(now),
        "recent": s.store.recent(10, now),
    });
    let mut r = Response::new(Body::from(body.to_string()));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    harden(r, None)
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
    // Same-origin requests from the page only.
    if h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(s.origin().as_str()) {
        return status(StatusCode::FORBIDDEN, "cross-origin request refused");
    }
    if let Some(site) = h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" {
            return status(StatusCode::FORBIDDEN, "cross-site request refused");
        }
    }
    let csrf_ok = h
        .get("x-columbus-csrf")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| same(v, &s.csrf));
    if !csrf_ok {
        return status(StatusCode::FORBIDDEN, "missing or wrong page token");
    }
    let Ok(Json(body)) = body else {
        return status(
            StatusCode::BAD_REQUEST,
            "expected {\"decision\": \"decline\" | \"allow_once\" | \"always_this_session\"}",
        );
    };
    match s.store.decide_via(&id, body.decision, Utc::now(), "page") {
        Ok(a) => {
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
            let mut r = Response::new(Body::from(
                json!({"id": a.id, "state": a.state}).to_string(),
            ));
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            harden(r, None)
        }
        Err(e) => status(StatusCode::CONFLICT, &e.to_string()),
    }
}

const GONE_PAGE: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Columbus approvals</title>
<style>body{font:16px/1.5 system-ui,sans-serif;max-width:36rem;margin:4rem auto;padding:0 16px;color:#1f2328;background:#fff}@media (prefers-color-scheme:dark){body{color:#e6edf3;background:#0d1117}}</style></head>
<body><h1>This approvals link has already been used</h1><p>For safety, each link Columbus opens works once. If a request is still waiting, ask your assistant to check on it; Columbus will open a fresh page.</p></body></html>"#;

const PAGE: &str = include_str!("approvals_page.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn page_template_has_its_placeholders_and_no_inner_html() {
        for p in ["{{NONCE}}", "{{CSRF}}", "{{SESSION}}"] {
            assert!(PAGE.contains(p), "{p}");
        }
        assert!(
            !PAGE.contains("innerHTML"),
            "request text must be rendered with textContent"
        );
    }
}
