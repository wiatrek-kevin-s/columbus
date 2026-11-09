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
//! * `/arcgis/rest/services/ROW/FeatureServer`: public, one polygon layer,
//!   `ROW`: the owned right of way along the three routes of the ROW job
//!   fixture (T-23; `ROADS.ROW_ROUTES` in `tests/fixtures/oracle.sql`),
//!   one polygon per route whose width steps at a measure (see
//!   [`row_polygons`]);
//! * `/test/log` (every request but `/test/*`: method, Host, target and the
//!   two token headers), `/test/grants` (each ArcGIS token call's
//!   `grant_type` and `redirect_uri`), `/test/expire-access` and
//!   `/test/lapse` (forget every access or refresh token).
//!
//! Its ArcGIS tokens look like `FAKE-arcgis-access-zq7k-<n>` and
//! `FAKE-arcgis-refresh-zq7k-<n>`, so a test can search for them.
//! `POST /test/issue` mints a pair without the browser dance (for tests
//! that store a sign-in directly).
//!
//! And it serves fake web map services for web layers (T-24, T-25), under
//! `/web/`, one of each kind; every image is a 1×1 PNG:
//!
//! * `/web/xyz/{z}/{x}/{y}.png`: XYZ tiles;
//! * `/web/wmts/own/WMTSCapabilities.xml` (tiles on the host asked) and
//!   `/web/wmts/cdn/WMTSCapabilities.xml` (tiles on `cdn.fake.test`), layer
//!   `ortho` in a GoogleMapsCompatible set; tiles at
//!   `/web/wmts/tile/{set}/{matrix}/{row}/{col}.png`;
//! * `/web/wms`: WMS 1.3.0 GetCapabilities (layer `parcels`, EPSG:3857) and
//!   GetMap;
//! * `/web/arcgis/rest/services/Imagery/MapServer`: a cached (tiled)
//!   MapServer in Web Mercator; `Dynamic/MapServer` (export only, layers 0
//!   and 1); `Elevation/ImageServer` (exportImage); `Parcels/FeatureServer`
//!   (layer 0: `FAKE_WEB_FEATURES` points, default 30, with fields
//!   `OBJECTID`, `OWNER`, `OWNER_SSN`; `maxRecordCount` 2000; f=geojson,
//!   paging and counts); `SecureMap/MapServer`: Imagery's twin that answers
//!   only with a valid token;
//! * `/web/redirect/…`: a 302 to the XYZ tiles.

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
    /// Each ArcGIS token call's grant_type (and redirect_uri), never a token.
    arcgis_grants: Vec<Value>,
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
    a.arcgis_grants
        .push(json!({"grant_type": f.get("grant_type"), "redirect_uri": f.get("redirect_uri")}));
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

// ---------- The ROW job's owned right of way (T-23) ----------

/// The ROW job's routes, as in `ROADS.ROW_ROUTES` (tests/fixtures/oracle.sql)
/// and the test's GeoPackage stand-in: each runs due east from lon
/// `ROW_X0` for `ROW_LEN` degrees (1,000 ft at lat 30.25) at its own
/// latitude, measures 0 to 1,000 ft.
const ROW_X0: f64 = -97.75;
const ROW_LEN: f64 = 0.003167;
/// Metres in a degree of latitude at lat 30.25 (WGS 84).
const M_PER_DEG_LAT: f64 = 110_856.6;

/// Per route: name, latitude, then the polygon's width in feet before and
/// after the measure (ft) where it steps. A: 80 ft, then 86 ft from 475;
/// B: 90 ft; C: 100 ft, then 120 ft from 825.
const ROW_WIDTHS: [(&str, f64, f64, f64, f64); 3] = [
    ("A", 30.250, 80.0, 475.0, 86.0),
    ("B", 30.252, 90.0, 500.0, 90.0),
    ("C", 30.254, 100.0, 825.0, 120.0),
];

/// The polygons as Esri JSON rings (outer ring clockwise), each running 20
/// ft past both ends of its route so a transect at an end crosses it whole.
fn row_polygons() -> Vec<(u64, &'static str, Vec<[f64; 2]>)> {
    let deg_lat = |ft: f64| ft * 0.3048 / M_PER_DEG_LAT;
    let ext = 20.0 / 1000.0 * ROW_LEN;
    ROW_WIDTHS
        .iter()
        .enumerate()
        .map(|(i, &(name, y, w1, at, w2))| {
            let (h1, h2) = (deg_lat(w1 / 2.0), deg_lat(w2 / 2.0));
            let (xa, xb, xe) = (
                ROW_X0 - ext,
                ROW_X0 + at / 1000.0 * ROW_LEN,
                ROW_X0 + ROW_LEN + ext,
            );
            let ring = vec![
                [xa, y - h1],
                [xa, y + h1],
                [xb, y + h1],
                [xb, y + h2],
                [xe, y + h2],
                [xe, y - h2],
                [xb, y - h2],
                [xb, y - h1],
                [xa, y - h1],
            ];
            (i as u64 + 1, name, ring)
        })
        .collect()
}

fn row_layer() -> Value {
    json!({
        "id": 0, "name": "ROW", "type": "Feature Layer", "geometryType": "esriGeometryPolygon",
        "objectIdField": "OBJECTID", "maxRecordCount": 1000,
        "advancedQueryCapabilities": {"supportsPagination": true, "supportsOrderBy": true},
        "extent": {"xmin": -97.751, "ymin": 30.249, "xmax": -97.746, "ymax": 30.255,
                   "spatialReference": {"wkid": 4326, "latestWkid": 4326}},
        "fields": [
            {"name": "OBJECTID", "type": "esriFieldTypeOID", "alias": "OBJECTID"},
            {"name": "ROUTE_NAME", "type": "esriFieldTypeString", "alias": "Route", "length": 20}
        ],
    })
}

async fn row_service(
    axum::extract::Path(rest): axum::extract::Path<String>,
    RawQuery(q): RawQuery,
    RawForm(f): RawForm,
) -> Response {
    let mut q = parse_pairs(&q.unwrap_or_default());
    q.extend(parse_pairs(&String::from_utf8_lossy(&f)));
    match rest.trim_end_matches('/') {
        "layers" => Json(json!({"layers": [row_layer()], "tables": []})).into_response(),
        "0" => Json(row_layer()).into_response(),
        "0/query" => {
            let all = row_polygons();
            if q.get("returnCountOnly").map(String::as_str) == Some("true") {
                return Json(json!({"count": all.len()})).into_response();
            }
            if q.get("returnIdsOnly").map(String::as_str) == Some("true") {
                let ids: Vec<u64> = all.iter().map(|(oid, _, _)| *oid).collect();
                return Json(json!({"objectIdFieldName": "OBJECTID", "objectIds": ids}))
                    .into_response();
            }
            let offset: usize = q
                .get("resultOffset")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let only: Option<Vec<u64>> = q
                .get("objectIds")
                .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect());
            let features: Vec<Value> = all
                .into_iter()
                .filter(|(oid, _, _)| only.as_ref().is_none_or(|o| o.contains(oid)))
                .skip(offset)
                .map(|(oid, name, ring)| {
                    json!({"attributes": {"OBJECTID": oid, "ROUTE_NAME": name},
                           "geometry": {"rings": [ring]}})
                })
                .collect();
            Json(json!({
                "objectIdFieldName": "OBJECTID", "geometryType": "esriGeometryPolygon",
                "spatialReference": {"wkid": 4326, "latestWkid": 4326},
                "fields": row_layer()["fields"], "features": features,
            }))
            .into_response()
        }
        _ => Json(json!({"error": {"code": 400, "message": "Invalid URL"}})).into_response(),
    }
}

/// A sign-in pair, as if the person had signed in.
async fn test_issue(State(s): State<Shared>) -> Json<Value> {
    let mut a = s.lock().unwrap();
    let access = next(&mut a, "FAKE-arcgis-access-zq7k");
    let refresh = next(&mut a, "FAKE-arcgis-refresh-zq7k");
    a.arcgis_access.push(access.clone());
    a.arcgis_refresh.push(refresh.clone());
    Json(json!({"access_token": access, "refresh_token": refresh}))
}

// ---------- Fake web map services (T-24) ----------

/// A 1×1 PNG.
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 80, 72, 88, 208, 0, 0, 3,
    101, 1, 161, 23, 222, 22, 204, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

fn png() -> Response {
    ([(header::CONTENT_TYPE, "image/png")], PNG).into_response()
}

fn xml(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/xml")], body).into_response()
}

fn host_of(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1")
        .to_string()
}

async fn web_xyz() -> Response {
    png()
}

async fn web_wmts_caps(
    State(s): State<Shared>,
    headers: HeaderMap,
    axum::extract::Path(variant): axum::extract::Path<String>,
) -> Response {
    let port = s.lock().unwrap().port;
    let host = if variant == "cdn" {
        format!("cdn.fake.test:{port}")
    } else {
        host_of(&headers)
    };
    let mut matrices = String::new();
    for z in 0..=18u32 {
        let scale = 559_082_264.028_717_2 / f64::from(1u32 << z);
        let n = 1u32 << z;
        matrices.push_str(&format!(
            "<TileMatrix><ows:Identifier>{z}</ows:Identifier><ScaleDenominator>{scale}</ScaleDenominator>\
             <TopLeftCorner>-20037508.3427892 20037508.3427892</TopLeftCorner><TileWidth>256</TileWidth>\
             <TileHeight>256</TileHeight><MatrixWidth>{n}</MatrixWidth><MatrixHeight>{n}</MatrixHeight></TileMatrix>"
        ));
    }
    xml(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0">
<ows:ServiceIdentification><ows:Title>Fake WMTS</ows:Title></ows:ServiceIdentification>
<Contents>
<Layer><ows:Title>Orthophoto (fake)</ows:Title><ows:Identifier>ortho</ows:Identifier>
<ows:WGS84BoundingBox><ows:LowerCorner>-98.0 30.0</ows:LowerCorner><ows:UpperCorner>-97.0 31.0</ows:UpperCorner></ows:WGS84BoundingBox>
<Style isDefault="true"><ows:Identifier>default</ows:Identifier></Style><Format>image/png</Format>
<TileMatrixSetLink><TileMatrixSet>GoogleMapsCompatible</TileMatrixSet></TileMatrixSetLink>
<ResourceURL format="image/png" resourceType="tile" template="http://{host}/web/wmts/tile/{{TileMatrixSet}}/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.png"/>
</Layer>
<TileMatrixSet><ows:Identifier>GoogleMapsCompatible</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::3857</ows:SupportedCRS>
<WellKnownScaleSet>urn:ogc:def:wkss:OGC:1.0:GoogleMapsCompatible</WellKnownScaleSet>{matrices}</TileMatrixSet>
</Contents></Capabilities>"#
    ))
}

async fn web_wmts_tile() -> Response {
    png()
}

async fn web_wms(headers: HeaderMap, RawQuery(q): RawQuery) -> Response {
    let q: HashMap<String, String> = parse_pairs(&q.unwrap_or_default())
        .into_iter()
        .map(|(k, v)| (k.to_ascii_uppercase(), v))
        .collect();
    match q.get("REQUEST").map(String::as_str) {
        Some("GetCapabilities") => {
            let host = host_of(&headers);
            xml(format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Service><Name>WMS</Name><Title>Fake WMS</Title></Service>
<Capability><Request>
<GetCapabilities><Format>text/xml</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://{host}/web/wms?"/></Get></HTTP></DCPType></GetCapabilities>
<GetMap><Format>image/png</Format><Format>image/jpeg</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://{host}/web/wms?"/></Get></HTTP></DCPType></GetMap>
</Request>
<Layer><Title>Fake county</Title><CRS>EPSG:4326</CRS>
<Layer queryable="1"><Name>parcels</Name><Title>Parcels (fake)</Title><CRS>EPSG:3857</CRS>
<EX_GeographicBoundingBox><westBoundLongitude>-98.0</westBoundLongitude><eastBoundLongitude>-97.0</eastBoundLongitude><southBoundLatitude>30.0</southBoundLatitude><northBoundLatitude>31.0</northBoundLatitude></EX_GeographicBoundingBox>
</Layer>
<Layer><Name>lambert</Name><Title>Not in Web Mercator</Title><CRS>EPSG:3081</CRS></Layer>
</Layer>
</Capability></WMS_Capabilities>"#
            ))
        }
        Some("GetMap") if q.get("LAYERS").map(String::as_str) == Some("parcels") => png(),
        _ => (
            [(header::CONTENT_TYPE, "application/vnd.ogc.se_xml")],
            "<ServiceExceptionReport><ServiceException>bad request</ServiceException></ServiceExceptionReport>",
        )
            .into_response(),
    }
}

/// Web Mercator tiling for the fake cached services.
fn tile_info() -> Value {
    let lods: Vec<Value> = (0..=19u32)
        .map(|z| {
            let scale = 591_657_527.591_555 / f64::from(1u32 << z);
            json!({"level": z, "resolution": 156_543.033_928_041 / f64::from(1u32 << z), "scale": scale})
        })
        .collect();
    json!({
        "rows": 256, "cols": 256, "dpi": 96, "format": "PNG32",
        "origin": {"x": -20037508.342787, "y": 20037508.342787},
        "spatialReference": {"wkid": 102100, "latestWkid": 3857},
        "lods": lods,
    })
}

fn mercator_extent() -> Value {
    json!({"xmin": -10_909_310.0, "ymin": 3_503_549.0, "xmax": -10_797_990.0, "ymax": 3_632_749.0,
           "spatialReference": {"wkid": 102100, "latestWkid": 3857}})
}

/// The fake parcels: `FAKE_WEB_FEATURES` points on a grid.
fn parcels() -> Vec<(u64, f64, f64)> {
    let n: u64 = std::env::var("FAKE_WEB_FEATURES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let side = ((n as f64).sqrt().ceil() as u64).max(1);
    (0..n)
        .map(|i| {
            let (r, c) = (i / side, i % side);
            (
                i + 1,
                -97.8 + 0.2 * (c as f64 + 0.5) / side as f64,
                30.2 + 0.2 * (r as f64 + 0.5) / side as f64,
            )
        })
        .collect()
}

fn web_arcgis_answer(rest: &str, q: &HashMap<String, String>) -> Response {
    let rest = rest.trim_end_matches('/');
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        ["Imagery" | "SecureMap", "MapServer"] => Json(json!({
            "currentVersion": 11.1, "serviceDescription": "Fake imagery", "mapName": "Imagery",
            "layers": [{"id": 0, "name": "Imagery"}], "singleFusedMapCache": true,
            "tileInfo": tile_info(), "fullExtent": mercator_extent(),
            "spatialReference": {"wkid": 102100, "latestWkid": 3857},
        }))
        .into_response(),
        ["Imagery" | "SecureMap", "MapServer", "tile", _, _, _] => png(),
        ["Dynamic", "MapServer"] => Json(json!({
            "currentVersion": 11.1, "mapName": "Dynamic", "singleFusedMapCache": false,
            "layers": [{"id": 0, "name": "Roads"}, {"id": 1, "name": "Signs"}],
            "fullExtent": {"xmin": -97.8, "ymin": 30.2, "xmax": -97.6, "ymax": 30.4,
                           "spatialReference": {"wkid": 4326}},
        }))
        .into_response(),
        ["Dynamic", "MapServer", "export"] | ["Elevation", "ImageServer", "exportImage"] => {
            if q.get("f").map(String::as_str) == Some("image") && q.contains_key("bbox") {
                png()
            } else {
                Json(json!({"error": {"code": 400, "message": "bbox and f=image needed"}}))
                    .into_response()
            }
        }
        ["Elevation", "ImageServer"] => Json(json!({
            "currentVersion": 11.1, "name": "Elevation", "serviceDataType": "esriImageServiceDataTypeElevation",
            "extent": mercator_extent(),
        }))
        .into_response(),
        ["Parcels", "FeatureServer"] => {
            Json(json!({"layers": [{"id": 0, "name": "Parcels"}], "tables": []})).into_response()
        }
        ["Parcels", "FeatureServer", "0"] => Json(json!({
            "id": 0, "name": "Parcels", "type": "Feature Layer", "geometryType": "esriGeometryPoint",
            "objectIdField": "OBJECTID", "maxRecordCount": 2000,
            "advancedQueryCapabilities": {"supportsPagination": true},
            "extent": {"xmin": -97.8, "ymin": 30.2, "xmax": -97.6, "ymax": 30.4,
                       "spatialReference": {"wkid": 4326, "latestWkid": 4326}},
            "fields": [
                {"name": "OBJECTID", "type": "esriFieldTypeOID"},
                {"name": "OWNER", "type": "esriFieldTypeString"},
                {"name": "OWNER_SSN", "type": "esriFieldTypeString"}
            ],
        }))
        .into_response(),
        ["Parcels", "FeatureServer", "0", "query"] => {
            let env: Vec<f64> = q
                .get("geometry")
                .map(|g| g.split(',').filter_map(|v| v.parse().ok()).collect())
                .unwrap_or_default();
            let inside = |x: f64, y: f64| {
                env.len() != 4 || (x >= env[0] && x <= env[2] && y >= env[1] && y <= env[3])
            };
            let all: Vec<(u64, f64, f64)> =
                parcels().into_iter().filter(|(_, x, y)| inside(*x, *y)).collect();
            if q.get("returnCountOnly").map(String::as_str) == Some("true") {
                return Json(json!({"count": all.len()})).into_response();
            }
            let offset: usize = q.get("resultOffset").and_then(|v| v.parse().ok()).unwrap_or(0);
            let count: usize = q
                .get("resultRecordCount")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2000)
                .min(2000);
            let wanted: Vec<String> = q
                .get("outFields")
                .map(|f| f.split(',').map(String::from).collect())
                .unwrap_or_default();
            let keep = |k: &str| wanted.iter().any(|w| w == "*" || w == k);
            let page: Vec<Value> = all
                .iter()
                .skip(offset)
                .take(count)
                .map(|(oid, x, y)| {
                    let mut props = serde_json::Map::new();
                    for (k, v) in [
                        ("OBJECTID", json!(oid)),
                        ("OWNER", json!(format!("Owner {oid}"))),
                        ("OWNER_SSN", json!(format!("900-00-{oid:04}"))),
                    ] {
                        if keep(k) {
                            props.insert(k.into(), v);
                        }
                    }
                    json!({"type": "Feature", "id": oid, "properties": props,
                           "geometry": {"type": "Point", "coordinates": [x, y]}})
                })
                .collect();
            let more = offset + page.len() < all.len();
            Json(json!({"type": "FeatureCollection", "features": page,
                        "properties": {"exceededTransferLimit": more}}))
            .into_response()
        }
        _ => Json(json!({"error": {"code": 400, "message": "Invalid URL"}})).into_response(),
    }
}

async fn web_arcgis(
    State(s): State<Shared>,
    headers: HeaderMap,
    axum::extract::Path(rest): axum::extract::Path<String>,
    RawQuery(q): RawQuery,
) -> Response {
    let q = parse_pairs(&q.unwrap_or_default());
    if rest.starts_with("SecureMap/") || rest == "SecureMap" {
        if let Some(r) = arcgis_check(&s, &headers, &q) {
            return r;
        }
    }
    web_arcgis_answer(&rest, &q)
}

async fn web_redirect(axum::extract::Path(rest): axum::extract::Path<String>) -> Response {
    Redirect::temporary(&format!("/web/xyz/{rest}")).into_response()
}

async fn test_log(State(s): State<Shared>) -> Json<Value> {
    Json(json!(s.lock().unwrap().log.clone()))
}

async fn test_grants(State(s): State<Shared>) -> Json<Value> {
    Json(json!(s.lock().unwrap().arcgis_grants.clone()))
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
        .route(
            "/arcgis/rest/services/ROW/FeatureServer/{*rest}",
            get(row_service).post(row_service),
        )
        .route("/web/xyz/{z}/{x}/{y}", get(web_xyz))
        .route(
            "/web/wmts/{variant}/WMTSCapabilities.xml",
            get(web_wmts_caps),
        )
        .route("/web/wmts/tile/{set}/{m}/{r}/{c}", get(web_wmts_tile))
        .route("/web/wms", get(web_wms))
        .route("/web/arcgis/rest/services/{*rest}", get(web_arcgis))
        .route("/web/redirect/{*rest}", get(web_redirect))
        .route("/test/issue", post(test_issue))
        .route("/test/log", get(test_log))
        .route("/test/grants", get(test_grants))
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
