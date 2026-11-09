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
//!
//! It is also a fake ArcGIS portal and service, for `arcgis signin` (T-11):
//!
//! * `/sharing/rest/oauth2/authorize` (PKCE S256; client id
//!   `FakeClientId123`; redirect to `http://127.0.0.1:<port>` or the
//!   out-of-band `urn:ietf:wg:oauth:2.0:oob`, whose code is shown on
//!   `/sharing/oauth2/approval`) and `/sharing/rest/oauth2/token`
//!   (`authorization_code`, `refresh_token`, `exchange_refresh_token`),
//!   answering errors the ArcGIS way: HTTP 200 with an `error` object;
//! * `/sharing/rest/portals/self/servers`: the federated servers in
//!   `FAKE_PORTAL_SERVERS` (comma-separated URLs; `{port}` is this port);
//! * `/arcgis/rest/services/Secure/FeatureServer`: one point layer,
//!   `Assets`, that answers only with a valid token (else code 499 or 498);
//! * `/arcgis/rest/services/Public/FeatureServer`: the same layer, public
//!   (no token needed);
//! * `/test/log` (every request but `/test/*`: method, Host, target and the
//!   two token headers), `/test/expire-access` and `/test/lapse` (forget
//!   every access or refresh token).
//!
//! Its ArcGIS tokens look like `FAKE-arcgis-access-zq7k-<n>` and
//! `FAKE-arcgis-refresh-zq7k-<n>`, so a test can search for them.

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
    port: u16,
    /// ArcGIS: code -> (challenge, redirect_uri)
    arcgis_codes: HashMap<String, (String, String)>,
    arcgis_access: Vec<String>,
    arcgis_refresh: Vec<String>,
    log: Vec<Value>,
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

// ---------------------------------------------------------------------
// The fake ArcGIS portal and service.

const ARCGIS_CLIENT: &str = "FakeClientId123";
const OOB: &str = "urn:ietf:wg:oauth:2.0:oob";

/// ArcGIS reports OAuth errors in a 200 answer.
fn arcgis_error(code: u16, error: &str, text: &str) -> Response {
    Json(
        json!({"error": {"code": code, "error": error, "error_description": text,
                          "message": text, "details": []}}),
    )
    .into_response()
}

async fn arcgis_authorize(State(s): State<Shared>, RawQuery(q): RawQuery) -> Response {
    let q = parse_pairs(&q.unwrap_or_default());
    if q.get("client_id").map(String::as_str) != Some(ARCGIS_CLIENT) {
        return (StatusCode::BAD_REQUEST, "Invalid client_id").into_response();
    }
    if q.get("response_type").map(String::as_str) != Some("code")
        || q.get("code_challenge_method").map(String::as_str) != Some("S256")
    {
        return (StatusCode::BAD_REQUEST, "code with PKCE S256 required").into_response();
    }
    let (Some(redirect), Some(challenge)) = (q.get("redirect_uri"), q.get("code_challenge")) else {
        return (
            StatusCode::BAD_REQUEST,
            "missing redirect_uri or code_challenge",
        )
            .into_response();
    };
    // The registered redirect URIs: http://127.0.0.1 (any port) and oob.
    let loopback = redirect
        .strip_prefix("http://127.0.0.1:")
        .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !loopback && redirect != OOB {
        return (StatusCode::BAD_REQUEST, "Invalid redirect_uri").into_response();
    }
    let code = {
        let mut a = s.lock().unwrap();
        let c = next(&mut a, "FAKE-arcgis-code-zq7k");
        a.arcgis_codes
            .insert(c.clone(), (challenge.clone(), redirect.clone()));
        c
    };
    if redirect == OOB {
        return Redirect::to(&format!("/sharing/oauth2/approval?code={code}")).into_response();
    }
    let state = q.get("state").cloned().unwrap_or_default();
    Redirect::to(&format!("{redirect}?code={code}&state={state}")).into_response()
}

async fn arcgis_approval(RawQuery(q): RawQuery) -> Response {
    let q = parse_pairs(&q.unwrap_or_default());
    let code = q.get("code").cloned().unwrap_or_default();
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        format!("<!doctype html><title>OAuth Approval</title><p>Copy this code and paste it into the application: <code id=\"code\">{code}</code></p>"),
    )
        .into_response()
}

async fn arcgis_token(State(s): State<Shared>, RawForm(f): RawForm) -> Response {
    let f = parse_pairs(&String::from_utf8_lossy(&f));
    let mut a = s.lock().unwrap();
    if f.get("client_id").map(String::as_str) != Some(ARCGIS_CLIENT) {
        return arcgis_error(400, "invalid_client", "Invalid client_id");
    }
    let mut new_refresh = true;
    match f.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            let Some((challenge, redirect)) = f.get("code").and_then(|c| a.arcgis_codes.remove(c))
            else {
                return arcgis_error(400, "invalid_grant", "Invalid authorization code");
            };
            if f.get("redirect_uri") != Some(&redirect) {
                return arcgis_error(400, "invalid_request", "redirect_uri does not match");
            }
            let verifier = f.get("code_verifier").cloned().unwrap_or_default();
            let digest = sha2::Sha256::digest(verifier.as_bytes());
            let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
            if expect != challenge {
                return arcgis_error(400, "invalid_request", "Invalid code_verifier");
            }
        }
        Some("refresh_token") => {
            let r = f.get("refresh_token").cloned().unwrap_or_default();
            if !a.arcgis_refresh.contains(&r) {
                return arcgis_error(400, "invalid_request", "Invalid refresh_token");
            }
            // ArcGIS keeps the refresh token on a plain refresh.
            new_refresh = false;
        }
        Some("exchange_refresh_token") => {
            let r = f.get("refresh_token").cloned().unwrap_or_default();
            if !a.arcgis_refresh.contains(&r) {
                return arcgis_error(400, "invalid_request", "Invalid refresh_token");
            }
            a.arcgis_refresh.retain(|t| t != &r);
        }
        _ => return arcgis_error(400, "unsupported_grant_type", "Invalid grant_type"),
    }
    let access = next(&mut a, "FAKE-arcgis-access-zq7k");
    a.arcgis_access.push(access.clone());
    let mut body = json!({
        "access_token": access,
        "expires_in": ttl(),
        "username": "fake_analyst",
        "ssl": true,
    });
    if new_refresh {
        let refresh = next(&mut a, "FAKE-arcgis-refresh-zq7k");
        a.arcgis_refresh.push(refresh.clone());
        body["refresh_token"] = json!(refresh);
        body["refresh_token_expires_in"] = json!(1_209_600);
    }
    Json(body).into_response()
}

/// The token a request carries, the ways ArcGIS accepts it.
fn arcgis_token_of(headers: &HeaderMap, query: &HashMap<String, String>) -> Option<String> {
    for h in [header::AUTHORIZATION.as_str(), "x-esri-authorization"] {
        if let Some(t) = headers
            .get(h)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        {
            return Some(t.to_string());
        }
    }
    query.get("token").cloned()
}

/// None when the token is good, else ArcGIS's answer (200 with code 499
/// for none, 498 for a bad one).
fn arcgis_check(
    s: &Shared,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Option<Response> {
    match arcgis_token_of(headers, query) {
        None => Some(
            Json(json!({"error": {"code": 499, "message": "Token Required", "details": []}}))
                .into_response(),
        ),
        Some(t) if s.lock().unwrap().arcgis_access.contains(&t) => None,
        Some(_) => Some(
            Json(json!({"error": {"code": 498, "message": "Invalid Token", "details": []}}))
                .into_response(),
        ),
    }
}

async fn arcgis_servers(
    State(s): State<Shared>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    let q = parse_pairs(&q.unwrap_or_default());
    if let Some(r) = arcgis_check(&s, &headers, &q) {
        return r;
    }
    let port = s.lock().unwrap().port;
    let servers: Vec<Value> = std::env::var("FAKE_PORTAL_SERVERS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .enumerate()
        .map(|(i, u)| {
            let u = u.replace("{port}", &port.to_string());
            json!({"id": format!("srv{i}"), "name": u, "url": u, "adminUrl": u,
                   "isHosted": false, "serverType": "ArcGIS", "serverRole": "FEDERATED_SERVER"})
        })
        .collect();
    Json(json!({"servers": servers})).into_response()
}

fn assets_layer() -> Value {
    json!({
        "id": 0, "name": "Assets", "type": "Feature Layer", "geometryType": "esriGeometryPoint",
        "objectIdField": "OBJECTID", "maxRecordCount": 1000,
        "advancedQueryCapabilities": {"supportsPagination": true, "supportsOrderBy": true},
        "extent": {"xmin": -97.8, "ymin": 30.2, "xmax": -97.7, "ymax": 30.3,
                   "spatialReference": {"wkid": 4326, "latestWkid": 4326}},
        "fields": [
            {"name": "OBJECTID", "type": "esriFieldTypeOID", "alias": "OBJECTID"},
            {"name": "NAME", "type": "esriFieldTypeString", "alias": "Name", "length": 40}
        ],
    })
}

async fn secure_service(
    State(s): State<Shared>,
    headers: HeaderMap,
    axum::extract::Path(rest): axum::extract::Path<String>,
    RawQuery(q): RawQuery,
    RawForm(f): RawForm,
) -> Response {
    let mut q = parse_pairs(&q.unwrap_or_default());
    q.extend(parse_pairs(&String::from_utf8_lossy(&f)));
    if let Some(r) = arcgis_check(&s, &headers, &q) {
        return r;
    }
    service_answer(&rest, &q)
}

/// The same layer, public: answers without a token (the app's Test of an
/// `auth: none` connection, T-12).
async fn public_service(
    axum::extract::Path(rest): axum::extract::Path<String>,
    RawQuery(q): RawQuery,
    RawForm(f): RawForm,
) -> Response {
    let mut q = parse_pairs(&q.unwrap_or_default());
    q.extend(parse_pairs(&String::from_utf8_lossy(&f)));
    service_answer(&rest, &q)
}

fn service_answer(rest: &str, q: &HashMap<String, String>) -> Response {
    match rest.trim_end_matches('/') {
        "layers" => Json(json!({"layers": [assets_layer()], "tables": []})).into_response(),
        "0" => Json(assets_layer()).into_response(),
        "0/query" => {
            if q.get("returnCountOnly").map(String::as_str) == Some("true") {
                return Json(json!({"count": 2})).into_response();
            }
            let offset: usize = q
                .get("resultOffset")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let all = [
                (1, "Pump station", -97.75, 30.25),
                (2, "Lift station", -97.74, 30.26),
            ];
            let features: Vec<Value> = all
                .iter()
                .skip(offset)
                .map(|(oid, name, x, y)| json!({"attributes": {"OBJECTID": oid, "NAME": name}, "geometry": {"x": x, "y": y}}))
                .collect();
            Json(json!({
                "objectIdFieldName": "OBJECTID", "geometryType": "esriGeometryPoint",
                "spatialReference": {"wkid": 4326, "latestWkid": 4326},
                "fields": assets_layer()["fields"], "features": features,
            }))
            .into_response()
        }
        _ => Json(json!({"error": {"code": 400, "message": "Invalid URL"}})).into_response(),
    }
}

async fn test_log(State(s): State<Shared>) -> Json<Value> {
    Json(json!(s.lock().unwrap().log.clone()))
}

async fn test_expire_access(State(s): State<Shared>) -> StatusCode {
    s.lock().unwrap().arcgis_access.clear();
    StatusCode::NO_CONTENT
}

async fn test_lapse(State(s): State<Shared>) -> StatusCode {
    s.lock().unwrap().arcgis_refresh.clear();
    StatusCode::NO_CONTENT
}

#[tokio::main]
async fn main() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let shared: Shared = Arc::new(Mutex::new(Auth {
        base: base.clone(),
        port: listener.local_addr().unwrap().port(),
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
        .route("/sharing/rest/oauth2/authorize", get(arcgis_authorize))
        .route("/sharing/oauth2/approval", get(arcgis_approval))
        .route("/sharing/rest/oauth2/token", post(arcgis_token))
        .route("/sharing/rest/portals/self/servers", get(arcgis_servers))
        .route(
            "/arcgis/rest/services/Secure/FeatureServer/{*rest}",
            get(secure_service).post(secure_service),
        )
        .route(
            "/arcgis/rest/services/Public/FeatureServer/{*rest}",
            get(public_service).post(public_service),
        )
        .route("/test/log", get(test_log))
        .route("/test/expire-access", post(test_expire_access))
        .route("/test/lapse", post(test_lapse))
        .with_state(shared.clone())
        .merge(guarded);
    // Every request but the test's own, as the service saw it.
    let log_state = shared.clone();
    let app = app.layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let s = log_state.clone();
            async move {
                let target = req
                    .uri()
                    .path_and_query()
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_default();
                if !target.starts_with("/test/") {
                    let h = |k: &str| {
                        req.headers()
                            .get(k)
                            .and_then(|v| v.to_str().ok())
                            .map(String::from)
                    };
                    let entry = json!({
                        "method": req.method().as_str(),
                        "host": h("host"),
                        "target": target,
                        "authorization": h("authorization"),
                        "x_esri_authorization": h("x-esri-authorization"),
                    });
                    s.lock().unwrap().log.push(entry);
                }
                next.run(req).await
            }
        },
    ));
    println!("listening {base}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    axum::serve(listener, app).await.unwrap();
}
