//! A remote MCP server behind OAuth, for tests of `servers signin`.
//!
//! Serves, on 127.0.0.1 and a random port (printed as `listening <url>`):
//!
//! * protected resource and authorization server metadata (RFC 9728, 8414);
//! * dynamic client registration (`/register`);
//! * `/authorize`, which approves at once and redirects back with a code,
//!   and `/token` (authorization code with PKCE S256, and refresh);
//! * `/mcp`, an MCP server with one tool, `hello`, that answers only with a
//!   valid bearer token, else 401 with `WWW-Authenticate`.
//!
//! `FAKE_OAUTH_GRANT` overrides the scopes granted (to test that a wider
//! grant is refused); `FAKE_OAUTH_TOKEN_TTL` sets the access token lifetime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{RawForm, RawQuery, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::{json, Value};
use sha2::Digest;

#[derive(Clone)]
struct Remote;

impl ServerHandler for Remote {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let t: Tool = serde_json::from_value(json!({
            "name": "hello",
            "description": "Say hello from the remote server.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true}
        }))
        .unwrap();
        Ok(ListToolsResult::with_all_items(vec![t]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let r = match request.name.as_ref() {
            "hello" => CallToolResult::success(vec![ContentBlock::text(
                "hello from the remote server".to_string(),
            )]),
            other => CallToolResult::error(vec![ContentBlock::text(format!("no tool {other}"))]),
        };
        Ok(r.into())
    }
}

/// `a=b&c=d` with percent-decoding.
fn parse_pairs(s: &str) -> HashMap<String, String> {
    fn dec(v: &str) -> String {
        let bytes = v.replace('+', " ").into_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(b) =
                    u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
                {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }
    s.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (dec(k), dec(v)))
        .collect()
}

#[derive(Default)]
struct Auth {
    base: String,
    /// code -> (challenge, scope)
    codes: HashMap<String, (String, String)>,
    tokens: Vec<String>,
    refresh: Vec<String>,
    n: u64,
}

type Shared = Arc<Mutex<Auth>>;

fn next(a: &mut Auth, prefix: &str) -> String {
    a.n += 1;
    format!("{prefix}-{}", a.n)
}

async fn resource_metadata(State(s): State<Shared>) -> Json<Value> {
    let base = s.lock().unwrap().base.clone();
    Json(json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "scopes_supported": ["files.read", "files.write"],
        "bearer_methods_supported": ["header"],
    }))
}

async fn as_metadata(State(s): State<Shared>) -> Json<Value> {
    let base = s.lock().unwrap().base.clone();
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["files.read", "files.write"],
    }))
}

async fn register(Json(body): Json<Value>) -> impl IntoResponse {
    let mut out = body.clone();
    out["client_id"] = json!("fake-client");
    out["token_endpoint_auth_method"] = json!("none");
    (StatusCode::CREATED, Json(out))
}

async fn authorize(State(s): State<Shared>, RawQuery(q): RawQuery) -> Response {
    let q = parse_pairs(&q.unwrap_or_default());
    let (Some(redirect), Some(state), Some(challenge)) = (
        q.get("redirect_uri"),
        q.get("state"),
        q.get("code_challenge"),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            "missing redirect_uri, state or code_challenge",
        )
            .into_response();
    };
    if q.get("code_challenge_method").map(String::as_str) != Some("S256") {
        return (StatusCode::BAD_REQUEST, "PKCE S256 required").into_response();
    }
    if !redirect.starts_with("http://127.0.0.1:") {
        return (StatusCode::BAD_REQUEST, "redirect must be loopback").into_response();
    }
    let scope = std::env::var("FAKE_OAUTH_GRANT")
        .ok()
        .or_else(|| q.get("scope").cloned())
        .unwrap_or_default();
    let code = {
        let mut a = s.lock().unwrap();
        let c = next(&mut a, "code");
        a.codes.insert(c.clone(), (challenge.clone(), scope));
        c
    };
    Redirect::to(&format!("{redirect}?code={code}&state={state}")).into_response()
}

fn ttl() -> u64 {
    std::env::var("FAKE_OAUTH_TOKEN_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600)
}

async fn token(State(s): State<Shared>, RawForm(f): RawForm) -> Response {
    let f = parse_pairs(&String::from_utf8_lossy(&f));
    let mut a = s.lock().unwrap();
    let scope = match f.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some((challenge, scope)) = f.get("code").and_then(|c| a.codes.remove(c)) else {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                )
                    .into_response();
            };
            let verifier = f.get("code_verifier").cloned().unwrap_or_default();
            let digest = sha2::Sha256::digest(verifier.as_bytes());
            let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            if expect != challenge {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant", "error_description": "PKCE"})),
                )
                    .into_response();
            }
            scope
        }
        Some("refresh_token") => {
            let r = f.get("refresh_token").cloned().unwrap_or_default();
            if !a.refresh.contains(&r) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                )
                    .into_response();
            }
            "files.read".into()
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "unsupported_grant_type"})),
            )
                .into_response()
        }
    };
    let access = next(&mut a, "access");
    let refresh = next(&mut a, "refresh");
    a.tokens.push(access.clone());
    a.refresh.push(refresh.clone());
    Json(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ttl(),
        "refresh_token": refresh,
        "scope": scope,
    }))
    .into_response()
}

#[tokio::main]
async fn main() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let shared: Shared = Arc::new(Mutex::new(Auth {
        base: base.clone(),
        ..Default::default()
    }));
    let mcp = StreamableHttpService::new(
        || Ok(Remote),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let guard_state = shared.clone();
    let guarded = Router::new()
        .nest_service("/mcp", mcp)
        .layer(axum::middleware::from_fn(move |headers: HeaderMap, req: axum::extract::Request, next: axum::middleware::Next| {
            let s = guard_state.clone();
            async move {
                let token = headers
                    .get(header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .map(String::from);
                let (ok, base) = {
                    let a = s.lock().unwrap();
                    (token.is_some_and(|t| a.tokens.contains(&t)), a.base.clone())
                };
                if !ok {
                    let mut r = StatusCode::UNAUTHORIZED.into_response();
                    r.headers_mut().insert(
                        header::WWW_AUTHENTICATE,
                        format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"")
                            .parse()
                            .unwrap(),
                    );
                    return r;
                }
                next.run(req).await
            }
        }));
    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(resource_metadata),
        )
        .route("/.well-known/oauth-authorization-server", get(as_metadata))
        .route("/register", post(register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .with_state(shared)
        .merge(guarded);
    println!("listening {base}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    axum::serve(listener, app).await.unwrap();
}
