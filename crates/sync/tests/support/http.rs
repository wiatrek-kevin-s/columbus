//! What the two stand-in servers share: the accounts and repositories they
//! hold, a record of every request they were sent, switches that make them
//! misbehave the way real servers can, and the plumbing to run one on
//! 127.0.0.1.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use columbus_sync::hash::ObjectFormat;
use serde_json::{json, Value};

use super::Repo;

/// One request a stand-in server was sent.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: String,
    /// The `Authorization` header, as sent.
    pub auth: Option<String>,
    pub host: String,
    pub agent: String,
    pub api_version: Option<String>,
    /// The request's body, as text.
    pub body: String,
}

type Hook = Box<dyn FnOnce(&mut World) + Send>;

/// Ways a server can answer that a sync must cope with.
#[derive(Default)]
pub struct Knobs {
    /// Run, one per call, just before the server judges what was sent
    /// (GitHub: the branch update; Gitea: the files call): a colleague's
    /// commit made here is one that landed while a sync was under way.
    pub before_landing: Vec<Hook>,
    /// Answer every API request with a redirect to this address.
    pub redirect_to: Option<String>,
    /// Answer the next request as a server that asks to wait.
    pub rate_limit_next: bool,
    /// The most items a list gives per page (GitHub 100, Gitea 50).
    pub page_max: usize,
    /// Gitea: the most paths a list of files gives per page (1000).
    pub tree_page_max: usize,
    /// GitHub: the most paths a list of files gives before it is cut off.
    pub tree_limit: usize,
    /// Gitea: what `/version` says.
    pub version: String,
    /// Tokens that may read but not send or create.
    pub read_only: Vec<String>,
    /// Answer after this long (to test time limits).
    pub delay_ms: u64,
}

/// Everything a stand-in server holds.
pub struct World {
    /// (owner, name) to repository.
    pub repos: BTreeMap<(String, String), Repo>,
    /// Token to login.
    pub tokens: BTreeMap<String, String>,
    /// Login to the person's name.
    pub names: BTreeMap<String, String>,
    pub seen: Vec<Seen>,
    pub knobs: Knobs,
    /// The object format of repositories made through the API.
    pub format: ObjectFormat,
}

impl World {
    pub fn new() -> World {
        World {
            repos: BTreeMap::new(),
            tokens: BTreeMap::new(),
            names: BTreeMap::new(),
            seen: Vec::new(),
            knobs: Knobs {
                page_max: 100,
                tree_page_max: 1000,
                tree_limit: 100_000,
                version: "1.22.3".into(),
                ..Knobs::default()
            },
            format: ObjectFormat::Sha1,
        }
    }

    pub fn user(&mut self, login: &str, name: &str, token: &str) {
        self.tokens.insert(token.into(), login.into());
        self.names.insert(login.into(), name.into());
    }

    pub fn repo(&mut self, owner: &str, name: &str) -> &mut Repo {
        let format = self.format;
        self.repos
            .entry((owner.into(), name.into()))
            .or_insert_with(|| Repo::new(format))
    }

    pub fn get(&self, owner: &str, name: &str) -> Option<&Repo> {
        self.repos.get(&(owner.to_string(), name.to_string()))
    }

    /// The requests that changed something (anything but GET).
    pub fn writes(&self) -> Vec<&Seen> {
        self.seen.iter().filter(|s| s.method != "GET").collect()
    }
}

pub type Shared = Arc<Mutex<World>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(World::new()))
}

pub fn answer(status: u16, body: Value) -> Response {
    (StatusCode::from_u16(status).unwrap(), Json(body)).into_response()
}

/// Note a request, and apply the switches that concern every request.
/// `Err`: the answer to give instead of the route's own.
pub async fn arrive(
    world: &Shared,
    prefix: &str,
    req: Request,
) -> Result<(HeaderMap, String, String, Bytes), Response> {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, 64 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    // The API lives under a prefix (`/api/v3`, `/api/v1`); what follows
    // it is what the server is asked.
    let Some(path) = parts.uri.path().strip_prefix(prefix).map(str::to_string) else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    let query = parts.uri.query().unwrap_or_default().to_string();
    let (delay, redirect, limited) = {
        let mut w = world.lock().unwrap();
        w.seen.push(Seen {
            method: parts.method.to_string(),
            path: path.clone(),
            query: query.clone(),
            auth: header("authorization"),
            host: header("host").unwrap_or_default(),
            agent: header("user-agent").unwrap_or_default(),
            api_version: header("x-github-api-version"),
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        let limited = std::mem::take(&mut w.knobs.rate_limit_next);
        (w.knobs.delay_ms, w.knobs.redirect_to.clone(), limited)
    };
    if delay > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }
    if let Some(to) = redirect {
        let target = format!("{}{}", to.trim_end_matches('/'), path);
        return Err((StatusCode::TEMPORARY_REDIRECT, [("location", target)]).into_response());
    }
    if limited {
        return Err((
            StatusCode::FORBIDDEN,
            [("x-ratelimit-remaining", "0".to_string()), ("x-ratelimit-reset", "1794400000".to_string())],
            Json(json!({"message": "API rate limit exceeded for user ID 1.", "documentation_url": "https://docs.github.com/rest/overview/rate-limits-for-the-rest-api"})),
        )
            .into_response());
    }
    Ok((parts.headers, path, query, body))
}

pub fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == name).then(|| percent_decode(v))
    })
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One page of a list, as both servers cut them: `page` counts from 1.
pub fn page_of<T: Clone>(items: &[T], page: usize, size: usize) -> Vec<T> {
    items
        .iter()
        .skip(page.saturating_sub(1) * size)
        .take(size)
        .cloned()
        .collect()
}

/// A stand-in server, listening on 127.0.0.1 until it is dropped.
pub struct Running {
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn serve(app: axum::Router) -> Running {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Running { addr, task }
}
