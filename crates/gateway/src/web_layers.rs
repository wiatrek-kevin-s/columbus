//! Web map layers, server side (T-24; build brief part 2, Part 3: AF-22,
//! REQ-31, REQ-32, REQ-34, D-14, F-13, F-14).
//!
//! Display-only layers on the person's map, from XYZ tiles, WMTS, WMS,
//! ArcGIS MapServer and ImageServer (tiled or dynamic export) and ArcGIS
//! feature layers. Every tile, image and feature request goes through
//! this gateway (D-14), never straight from the web view:
//!
//! - **Only the map page asks**, with the session token it already sends
//!   as `Authorization: Bearer` (approvals_page's `guard`); anything else
//!   is refused. The page names a saved source's id and a tile (`z/x/y`) or
//!   an extent, never a URL, so this is never an open proxy.
//! - **Hosts (F-14):** `map.allow_web_layers` false turns everything off.
//!   With `map.hosts` listed, every host a request goes to must match it;
//!   with none listed, a source's own host is allowed (the person saved it
//!   at the keyboard, or the organization listed it), and any other host a
//!   service's capabilities name (WMTS ResourceURL, WMS OnlineResource) only
//!   if it was recorded as approved when the person saved the source
//!   (`map-sources/.hosts.json`). Checked before any request.
//! - **Transport:** every address resolved once and pinned, never this
//!   computer's own or a link-local one (except test builds), no
//!   redirects, 30 s per request, answers size-capped; HTTP 429 with
//!   `Retry-After` is honoured at most 3 times, then R-14.
//! - **ArcGIS sign-in:** a source with `auth_from` gets the access token of
//!   that `arcgis` connection's sign-in, as a header, only on requests to
//!   hosts the connection's portal vouches for (R-18); any other host gets
//!   none. Nothing the page receives carries a token: only an image's or
//!   GeoJSON's bytes and its content type go back, and an answer that
//!   echoes the token is refused.
//! - **Tile cache:** `<workspaces root>/cache/tiles/<source id>/`, at most
//!   500 MB in all, oldest out first. Dropped when the source is removed or
//!   changed; a secured source's also when its `arcgis` connection signs
//!   out or is removed (`arcgis_auth::sign_out`), and before any reuse
//!   while that connection is not signed in.
//! - **No agent access:** no MCP tool adds, changes, turns on or reads web
//!   layers; `whoami` lists their titles only.
//!
//! The Columbus app adds, tests, edits and removes saved sources through
//! `columbus-mcp map-source …` (see `cli`).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use columbus_shared::map_sources::{self, MapKind, MapSource};
use columbus_shared::policy::{is_arcgis, Connection};
use reqwest::Url;
use serde_json::{json, Value};

use crate::arcgis_auth::same_host;
use crate::config::{Paths, Settings};
use crate::secrets::SecretStore;

/// The longest any one request may take (R-14).
const TIMEOUT: Duration = Duration::from_secs(30);
/// The largest tile or image read.
const MAX_IMAGE: usize = 16 * 1024 * 1024;
/// The largest capabilities document or JSON description read.
const MAX_DOC: usize = 32 * 1024 * 1024;
/// The largest page of features read.
const MAX_FEATURES_PAGE: usize = 64 * 1024 * 1024;
/// The tile cache's limit, all sources together.
pub(crate) const CACHE_LIMIT: u64 = 500 * 1024 * 1024;
/// Most features drawn for one view.
pub(crate) const MAX_FEATURES: u64 = 5000;
/// How long a service's capabilities are reused.
const CAPS_TTL: Duration = Duration::from_secs(300);
/// How long a host's pinned client is reused.
const CLIENT_TTL: Duration = Duration::from_secs(300);
/// Tiles the map asks for are 256 pixels square, in Web Mercator.
const TILE: u32 = 256;
/// Deepest zoom the page may ask for.
const MAX_ZOOM: u8 = 24;
/// Half the Web Mercator world's width, in metres.
const HALF: f64 = 20_037_508.342_789_244;
/// WMTS scale denominator of zoom 0 (256 px tiles, 0.28 mm pixels).
const WMTS_SCALE0: f64 = 559_082_264.028_717_2;
/// ArcGIS scale of zoom 0 (96 dpi).
const ARCGIS_SCALE0: f64 = 591_657_527.591_555;

/// What the page was told, with the HTTP status that goes with it.
#[derive(Debug, Clone)]
pub(crate) struct WebError {
    pub status: u16,
    pub message: String,
}

fn err(status: u16, message: impl Into<String>) -> WebError {
    WebError {
        status,
        message: message.into(),
    }
}

/// The refusal for a request that names an address.
pub(crate) const NO_URLS: &str = "web layers are named by a saved layer's id, never by an address: add the layer in the Columbus app (Connections › Map layers), then turn it on";

/// Where a source comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// Policy `map.sources`.
    Policy,
    /// `map-sources/<id>.yaml`, saved by the person.
    Saved,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Origin::Policy => "policy",
            Origin::Saved => "saved",
        }
    }
}

/// What every request needs: the settings in force, the credential store,
/// the saved sources' folder and the tile cache.
pub(crate) struct Env {
    pub settings: Arc<Settings>,
    pub store: Arc<SecretStore>,
    pub dir: PathBuf,
    pub cache: PathBuf,
}

impl Env {
    pub(crate) fn new(settings: Arc<Settings>, store: Arc<SecretStore>, paths: &Paths) -> Env {
        Env {
            settings,
            store,
            dir: paths.map_sources.clone(),
            cache: cache_root(paths),
        }
    }

    fn map(&self) -> &map_sources::MapPolicy {
        &self.settings.policy.map
    }
}

/// `<workspaces root>/cache/tiles`.
pub(crate) fn cache_root(paths: &Paths) -> PathBuf {
    paths.workspaces.join("cache").join("tiles")
}

// ---------------------------------------------------------------------
// Sources and hosts.

/// Every source: the organization's first, then the person's saved ones
/// (one whose id the organization already uses is left out), and notes on
/// files that could not be read.
pub(crate) fn all(env: &Env) -> (Vec<(MapSource, Origin)>, Vec<String>) {
    all_in(env.map(), &env.dir)
}

fn all_in(map: &map_sources::MapPolicy, dir: &Path) -> (Vec<(MapSource, Origin)>, Vec<String>) {
    let mut out: Vec<(MapSource, Origin)> = Vec::new();
    let mut notes = Vec::new();
    for s in &map.sources {
        match s.check() {
            Ok(()) => out.push((s.clone(), Origin::Policy)),
            Err(e) => notes.push(format!("policy map.sources: {e}")),
        }
    }
    let (saved, problems) = map_sources::load_dir(dir);
    notes.extend(problems);
    for s in saved {
        if out.iter().any(|(o, _)| o.id.eq_ignore_ascii_case(&s.id)) {
            notes.push(format!(
                "saved web layer {} is not used: your organization supplies one with that id",
                s.id
            ));
            continue;
        }
        out.push((s, Origin::Saved));
    }
    (out, notes)
}

/// One source by id.
pub(crate) fn find(env: &Env, id: &str) -> Result<(MapSource, Origin), WebError> {
    if !map_sources::id_ok(id) {
        return Err(
            if id.contains(':') || id.contains('/') || id.contains('.') {
                err(400, NO_URLS)
            } else {
                err(404, format!("there is no web layer {id:?}"))
            },
        );
    }
    if let Some(s) = env
        .map()
        .sources
        .iter()
        .find(|s| s.id.eq_ignore_ascii_case(id))
    {
        s.check().map_err(|e| err(409, e))?;
        return Ok((s.clone(), Origin::Policy));
    }
    match map_sources::load_one(&env.dir, id) {
        Ok(Some(s)) => Ok((s, Origin::Saved)),
        Ok(None) => Err(err(404, format!("there is no web layer {id:?}"))),
        Err(e) => Err(err(409, format!("web layer {id} cannot be read: {e}"))),
    }
}

/// Web layers are on at all (F-14).
fn allowed(env: &Env) -> Result<(), WebError> {
    if env.map().allow_web_layers {
        Ok(())
    } else {
        Err(err(
            403,
            "your organization has turned web map layers off (policy map.allow_web_layers)",
        ))
    }
}

/// A URL's host as policy reads it: lower case, without a trailing dot or
/// an IPv6 address's brackets.
fn url_host(u: &Url) -> Option<String> {
    let h = u
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!h.is_empty()).then_some(h)
}

/// Test builds only (with `COLUMBUS_FETCH_TEST_LOOPBACK`): host names that
/// stand for this computer, so fake services can play several hosts.
/// `127.0.0.1` and `localhost`, plus `COLUMBUS_WEB_TEST_HOSTS`
/// (comma-separated). Release builds have none.
fn test_host(host: &str) -> bool {
    if !crate::fetch_tools::test_loopback() {
        return false;
    }
    host == "127.0.0.1"
        || host == "localhost"
        || std::env::var("COLUMBUS_WEB_TEST_HOSTS")
            .unwrap_or_default()
            .split(',')
            .any(|h| !h.trim().is_empty() && h.trim().eq_ignore_ascii_case(host))
}

/// A source's URL (an XYZ template with its placeholders filled), checked:
/// https (plain http only to a test host in test builds), no user, no
/// fragment.
fn parse_url(src: &MapSource) -> Result<(Url, String), WebError> {
    let raw = src
        .url
        .trim()
        .replace("{z}", "0")
        .replace("{x}", "0")
        .replace("{y}", "0");
    check_url(&src.id, &raw)
}

fn check_url(id: &str, raw: &str) -> Result<(Url, String), WebError> {
    let url =
        Url::parse(raw).map_err(|e| err(409, format!("web layer {id}'s url is not valid: {e}")))?;
    let host =
        url_host(&url).ok_or_else(|| err(409, format!("web layer {id}'s url has no host")))?;
    let plain = url.scheme() == "http" && test_host(&host);
    if url.scheme() != "https" && !plain {
        return Err(err(
            409,
            format!("web layer {id}'s url must use https, not {}", url.scheme()),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(err(
            409,
            format!("web layer {id}'s url carries a user name or password; Columbus stores no passwords"),
        ));
    }
    if url.fragment().is_some() {
        return Err(err(
            409,
            format!("web layer {id}'s url has a fragment (#…)"),
        ));
    }
    Ok((url, host))
}

/// F-14 for one host a request of `src` would go to.
fn host_ok(
    env: &Env,
    src: &MapSource,
    origin: Origin,
    own: &str,
    host: &str,
) -> Result<(), WebError> {
    allowed(env)?;
    let map = env.map();
    if !map.hosts.is_empty() {
        return if map.host_listed(host) {
            Ok(())
        } else {
            Err(err(
                403,
                format!("web layer {}: host {host} is not on your organization's list of map hosts (policy map.hosts)", src.id),
            ))
        };
    }
    if same_host(host, own) {
        return Ok(());
    }
    if origin == Origin::Saved
        && map_sources::approved_hosts(&env.dir, src)
            .iter()
            .any(|h| same_host(h, host))
    {
        return Ok(());
    }
    Err(err(
        403,
        format!(
            "web layer {}: its service names another host, {host}, which was not approved when the layer was saved. Edit the layer and save it again to approve {host}, or ask your organization to list it in policy map.hosts",
            src.id
        ),
    ))
}

// ---------------------------------------------------------------------
// Transport.

/// R-14's wording.
fn no_answer(host: &str) -> String {
    format!("Web layer service at {host} did not answer in 30 s. Check the address or your network, then Test again.")
}

fn unreachable(host: &str) -> String {
    format!("Web layer service at {host} could not be reached. Check the address or your network, then Test again.")
}

/// Addresses a web layer must never be fetched from: this computer, the
/// local link, nothing, or many at once. (Private addresses are allowed:
/// organizations serve imagery on their own networks.)
fn local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_multicast()
                || v.octets()[0] == 0
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return local_ip(IpAddr::V4(m));
            }
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (v.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

type Clients = Mutex<HashMap<(String, String, u16), (Instant, reqwest::Client)>>;

fn clients() -> &'static Clients {
    static C: OnceLock<Clients> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// A client for `url`'s origin with its addresses resolved once, checked
/// and pinned, following no redirects.
async fn client_for(url: &Url, host: &str) -> Result<reqwest::Client, String> {
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("{host} has no port"))?;
    let key = (url.scheme().to_string(), host.to_string(), port);
    if let Some((at, c)) = clients()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        if at.elapsed() < CLIENT_TTL {
            return Ok(c.clone());
        }
    }
    let lookup = url
        .host_str()
        .unwrap_or(host)
        .trim_matches(['[', ']'])
        .to_string();
    let addrs: Vec<SocketAddr> = if test_host(host) {
        vec![SocketAddr::from(([127, 0, 0, 1], port))]
    } else {
        let found = match tokio::time::timeout(TIMEOUT, tokio::net::lookup_host((lookup.as_str(), port))).await {
            Ok(Ok(a)) => a.collect::<Vec<_>>(),
            Ok(Err(_)) => return Err(format!("Web layer service at {host} could not be found. Check the address or your network, then Test again.")),
            Err(_) => return Err(no_answer(host)),
        };
        if found.is_empty() {
            return Err(format!("Web layer service at {host} has no address. Check the address or your network, then Test again."));
        }
        if let Some(a) = found.iter().find(|a| local_ip(a.ip())) {
            return Err(format!(
                "{host} resolves to {}, an address on this computer or its local link; Columbus does not fetch web layers from there",
                a.ip()
            ));
        }
        found
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .resolve_to_addrs(&lookup, &addrs)
        .user_agent(concat!("columbus-mcp/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    clients()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), client.clone()));
    Ok(client)
}

struct Reply {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

/// One GET, with the token as a header when given. A redirect is an
/// error; 429 is retried at most 3 times.
async fn get(url: &Url, host: &str, token: Option<&str>, max: usize) -> Result<Reply, String> {
    let client = client_for(url, host).await?;
    let mut tries = 0;
    loop {
        let mut b = client.get(url.clone());
        if let Some(t) = token {
            b = b
                .bearer_auth(t)
                .header("X-Esri-Authorization", format!("Bearer {t}"));
        }
        let mut resp = b.send().await.map_err(|e| {
            if e.is_timeout() {
                no_answer(host)
            } else {
                unreachable(host)
            }
        })?;
        if resp.status().is_redirection() {
            return Err(format!(
                "Web layer service at {host} answered with a redirect, which Columbus does not follow. Put the address it moved to in the layer's URL, then Test again."
            ));
        }
        let status = resp.status().as_u16();
        if status == 429 {
            let wait = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(1);
            tries += 1;
            if tries > 3 || wait > 10 {
                return Err(format!("Web layer service at {host} is limiting requests (HTTP 429). Wait a minute, then Test again."));
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
            continue;
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let mut body = Vec::new();
        loop {
            let chunk = resp.chunk().await.map_err(|e| {
                if e.is_timeout() {
                    no_answer(host)
                } else {
                    format!("Web layer service at {host} broke off its answer. Check your network, then try again.")
                }
            })?;
            let Some(chunk) = chunk else { break };
            if body.len() + chunk.len() > max {
                return Err(format!(
                    "Web layer service at {host} sent more than {} MB in one answer",
                    max / 1024 / 1024
                ));
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(Reply {
            status,
            content_type,
            body,
        });
    }
}

/// Whether an answer says it needed a sign-in (HTTP 401 or 403, or
/// ArcGIS's 498 or 499 in JSON).
fn needs_sign_in(r: &Reply) -> bool {
    if r.status == 401 || r.status == 403 {
        return true;
    }
    r.body.len() < 64 * 1024
        && serde_json::from_slice::<Value>(&r.body)
            .ok()
            .and_then(|v| v.pointer("/error/code").and_then(Value::as_i64))
            .is_some_and(|c| matches!(c, 401 | 403 | 498 | 499))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// The token for a request of `src` to `host`: `(token, withheld)`. The
/// token only when `host` is one the sign-in's portal vouches for (R-18);
/// `withheld` when there was a sign-in but `host` gets none.
async fn token_for(
    env: &Env,
    src: &MapSource,
    host: &str,
) -> Result<(Option<String>, bool), WebError> {
    let Some(a) = src.auth_from.as_deref() else {
        return Ok((None, false));
    };
    let conn: &Connection = env
        .settings
        .connections
        .iter()
        .find(|c| c.id == a && is_arcgis(&c.kind))
        .ok_or_else(|| {
            err(
                409,
                format!("web layer {} takes its sign-in from {a}, which is not an arcgis connection here", src.id),
            )
        })?;
    match crate::arcgis_auth::bearer(&env.store, conn, &env.settings.policy).await {
        Ok(None) => Ok((None, false)),
        Ok(Some((token, portal))) => {
            let vouched = crate::arcgis_auth::vouched(&portal, &token, host).await;
            if vouched.iter().any(|h| same_host(h, host)) {
                Ok((Some(token), false))
            } else {
                Ok((None, true))
            }
        }
        Err(e) => Err(err(401, e)),
    }
}

/// GET `url` for `src`: its host checked by F-14 first, the token added
/// where vouched for, and the answer checked to be a success that does not
/// echo the token.
async fn fetch(
    env: &Env,
    src: &MapSource,
    origin: Origin,
    own: &str,
    url: &Url,
    max: usize,
) -> Result<Reply, WebError> {
    let host =
        url_host(url).ok_or_else(|| err(502, "the service named an address without a host"))?;
    let plain = url.scheme() == "http" && test_host(&host);
    if url.scheme() != "https" && !plain {
        return Err(err(
            502,
            format!(
                "web layer {}'s service named a {} address; Columbus uses https only",
                src.id,
                url.scheme()
            ),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(err(
            502,
            format!(
                "web layer {}'s service named an address with a password",
                src.id
            ),
        ));
    }
    host_ok(env, src, origin, own, &host)?;
    let (token, withheld) = token_for(env, src, &host).await?;
    let r = get(url, &host, token.as_deref(), max)
        .await
        .map_err(|e| err(502, e))?;
    if let Some(t) = &token {
        if contains(&r.body, t.as_bytes()) {
            return Err(err(
                502,
                format!("Web layer service at {host} sent back your sign-in, so Columbus did not pass its answer on"),
            ));
        }
    }
    if needs_sign_in(&r) {
        return Err(if withheld {
            err(403, crate::arcgis::not_vouched(&host))
        } else if let Some(a) = &src.auth_from {
            err(
                401,
                format!("Web layer service at {host} needs a sign-in that {a} does not give it. Sign in again: {}", crate::arcgis_auth::signin_command(a)),
            )
        } else {
            err(
                401,
                format!("Web layer service at {host} needs a sign-in. Add the layer with an arcgis connection's sign-in (auth_from), then Test again."),
            )
        });
    }
    if !(200..300).contains(&r.status) {
        return Err(err(
            502,
            format!(
                "Web layer service at {host} answered HTTP {}. Check the layer, then Test again.",
                r.status
            ),
        ));
    }
    Ok(r)
}

fn json_of(r: &Reply, host_hint: &str) -> Result<Value, WebError> {
    let v: Value = serde_json::from_slice(&r.body).map_err(|_| {
        err(
            502,
            format!("Web layer service at {host_hint} sent an unreadable answer"),
        )
    })?;
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        let msg = e
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("an error");
        return Err(err(
            502,
            format!(
                "Web layer service at {host_hint} answered with an error: {}",
                clip(msg, 200)
            ),
        ));
    }
    Ok(v)
}

/// A field the organization hides (policy `hide_fields`).
fn hidden_field(policy: &columbus_shared::policy::OrgPolicy, name: &str) -> bool {
    columbus_shared::policy::DatasetRules {
        allow: policy.hide_fields.clone(),
        deny: vec![],
    }
    .permits(name)
    .unwrap_or(true)
}

fn clip(s: &str, n: usize) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).take(n).collect();
    t
}

// ---------------------------------------------------------------------
// Capabilities.

/// How tiles are asked for.
#[derive(Clone, Debug)]
enum Draw {
    /// An XYZ template.
    Xyz { template: String },
    /// A WMTS ResourceURL template (or a KVP GetTile URL), with each map
    /// zoom's TileMatrix.
    Wmts {
        template: String,
        matrices: HashMap<u8, String>,
    },
    /// WMS GetMap for each tile's box.
    Wms { getmap: Url, crs_key: &'static str },
    /// ArcGIS cached tiles: each map zoom's level.
    ArcTiles { root: Url, levels: HashMap<u8, u64> },
    /// ArcGIS export (MapServer) or exportImage (ImageServer).
    ArcExport {
        root: Url,
        image: bool,
        layer: Option<String>,
    },
    /// An ArcGIS feature layer's query.
    Features {
        layer: Url,
        page: u64,
        paging: bool,
        fields: Vec<String>,
        geometry: Option<String>,
    },
}

/// What a service said about itself.
#[derive(Clone, Debug)]
struct Caps {
    draw: Draw,
    /// [west, south, east, north] in degrees, when the service says.
    extent: Option<[f64; 4]>,
    min_zoom: u8,
    max_zoom: u8,
}

/// One layer a service offers (for the app's "Add web layer").
#[derive(Clone, Debug)]
struct Offered {
    name: String,
    title: String,
    /// Hosts other than the service's own that drawing it would reach.
    hosts: Vec<String>,
}

type CapsCache = Mutex<HashMap<(String, String), (Instant, Arc<Caps>)>>;

fn caps_cache() -> &'static CapsCache {
    static C: OnceLock<CapsCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The source's capabilities (reused for a few minutes).
async fn caps(env: &Env, src: &MapSource, origin: Origin) -> Result<Arc<Caps>, WebError> {
    let key = (src.id.clone(), src.fingerprint());
    if let Some((at, c)) = caps_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        if at.elapsed() < CAPS_TTL {
            return Ok(c.clone());
        }
    }
    let (c, _) = read_caps(env, src, origin).await?;
    let c = Arc::new(c);
    caps_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), c.clone()));
    Ok(c)
}

/// Read a service's capabilities: what to draw `src.layer` with, and every
/// layer it offers.
async fn read_caps(
    env: &Env,
    src: &MapSource,
    origin: Origin,
) -> Result<(Caps, Vec<Offered>), WebError> {
    let (url, own) = parse_url(src)?;
    host_ok(env, src, origin, &own, &own)?;
    let layer = src
        .layer
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    match src.kind {
        MapKind::Xyz => Ok((
            Caps {
                draw: Draw::Xyz {
                    template: src.url.trim().to_string(),
                },
                extent: None,
                min_zoom: 0,
                max_zoom: 22,
            },
            vec![],
        )),
        MapKind::Wmts => {
            let mut u = url.clone();
            let has_request = u
                .query_pairs()
                .any(|(k, _)| k.eq_ignore_ascii_case("request"));
            if !has_request && !u.path().to_ascii_lowercase().ends_with(".xml") {
                u.query_pairs_mut()
                    .append_pair("SERVICE", "WMTS")
                    .append_pair("REQUEST", "GetCapabilities");
            }
            let r = fetch(env, src, origin, &own, &u, MAX_DOC).await?;
            let text = String::from_utf8_lossy(&r.body).into_owned();
            wmts_caps(&text, &u, layer, &own)
        }
        MapKind::Wms => {
            let mut u = url.clone();
            strip_ogc(&mut u);
            u.query_pairs_mut()
                .append_pair("SERVICE", "WMS")
                .append_pair("REQUEST", "GetCapabilities");
            let r = fetch(env, src, origin, &own, &u, MAX_DOC).await?;
            let text = String::from_utf8_lossy(&r.body).into_owned();
            wms_caps(&text, &url, layer, &own)
        }
        MapKind::ArcgisMap | MapKind::ArcgisImage => {
            let root = arcgis_root(&src.id, &url, &["MapServer", "ImageServer"])?;
            let mut u = root.clone();
            u.query_pairs_mut().append_pair("f", "json");
            let r = fetch(env, src, origin, &own, &u, MAX_DOC).await?;
            let v = json_of(&r, &own)?;
            arcgis_caps(&v, root, src.kind == MapKind::ArcgisImage, layer)
        }
        MapKind::ArcgisFeatures => {
            let (root, numbered) = feature_root(&src.id, &url)?;
            let lid = numbered.or_else(|| layer.map(String::from));
            let Some(lid) = lid else {
                // The service itself: offer its layers.
                let mut u = root.clone();
                u.query_pairs_mut().append_pair("f", "json");
                let r = fetch(env, src, origin, &own, &u, MAX_DOC).await?;
                let v = json_of(&r, &own)?;
                let caps = Caps {
                    draw: Draw::Features {
                        layer: root.clone(),
                        page: 1000,
                        paging: false,
                        fields: vec![],
                        geometry: None,
                    },
                    extent: None,
                    min_zoom: 0,
                    max_zoom: 22,
                };
                return Ok((caps, offered_arcgis(&v)));
            };
            let mut lu = root.clone();
            let p = lu.path().trim_end_matches('/').to_string();
            lu.set_path(&format!("{p}/{lid}"));
            let mut u = lu.clone();
            u.query_pairs_mut().append_pair("f", "json");
            let r = fetch(env, src, origin, &own, &u, MAX_DOC).await?;
            let v = json_of(&r, &own)?;
            let hidden = src.auth_from.is_some();
            let fields: Vec<String> = v
                .get("fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|f| f.get("name").and_then(Value::as_str))
                .filter(|n| !(hidden && hidden_field(&env.settings.policy, n)))
                .map(String::from)
                .collect();
            let page = v
                .get("maxRecordCount")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0)
                .unwrap_or(1000)
                .min(2000);
            let paging = v
                .pointer("/advancedQueryCapabilities/supportsPagination")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let extent = v.get("extent").and_then(extent_of);
            let name = v
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&lid)
                .to_string();
            Ok((
                Caps {
                    draw: Draw::Features {
                        layer: lu,
                        page,
                        paging,
                        fields,
                        geometry: v
                            .get("geometryType")
                            .and_then(Value::as_str)
                            .map(String::from),
                    },
                    extent,
                    min_zoom: 0,
                    max_zoom: 22,
                },
                vec![Offered {
                    name: lid.clone(),
                    title: name,
                    hosts: vec![],
                }],
            ))
        }
    }
}

/// Drop the OGC request parameters a pasted WMS URL may carry.
fn strip_ogc(u: &mut Url) {
    const OGC: &[&str] = &[
        "service",
        "request",
        "version",
        "layers",
        "styles",
        "format",
        "bbox",
        "width",
        "height",
        "crs",
        "srs",
        "transparent",
    ];
    let keep: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| !OGC.iter().any(|o| k.eq_ignore_ascii_case(o)))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    u.set_query(None);
    if !keep.is_empty() {
        let mut q = u.query_pairs_mut();
        for (k, v) in keep {
            q.append_pair(&k, &v);
        }
    }
}

/// An ArcGIS service root (`…/MapServer`), without query.
fn arcgis_root(id: &str, url: &Url, ends: &[&str]) -> Result<Url, WebError> {
    let mut root = url.clone();
    root.set_query(None);
    let path = root.path().trim_end_matches('/').to_string();
    let last = path.rsplit('/').next().unwrap_or("");
    if !ends.iter().any(|e| last.eq_ignore_ascii_case(e)) {
        return Err(err(
            409,
            format!("web layer {id}'s url must end in {}", ends.join(" or ")),
        ));
    }
    root.set_path(&path);
    Ok(root)
}

/// A feature layer's service root and, when the URL names one, its layer.
fn feature_root(id: &str, url: &Url) -> Result<(Url, Option<String>), WebError> {
    let mut root = url.clone();
    root.set_query(None);
    let path = root.path().trim_end_matches('/').to_string();
    let (base, n) = match path.rsplit_once('/') {
        Some((b, last)) if !last.is_empty() && last.bytes().all(|c| c.is_ascii_digit()) => {
            (b.to_string(), Some(last.to_string()))
        }
        _ => (path.clone(), None),
    };
    let last = base.rsplit('/').next().unwrap_or("");
    if !(last.eq_ignore_ascii_case("FeatureServer") || last.eq_ignore_ascii_case("MapServer")) {
        return Err(err(
            409,
            format!("web layer {id}'s url must end in FeatureServer, MapServer or a layer number"),
        ));
    }
    root.set_path(&base);
    Ok((root, n))
}

fn offered_arcgis(v: &Value) -> Vec<Offered> {
    v.get("layers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let id = l.get("id").and_then(Value::as_u64)?;
            Some(Offered {
                name: id.to_string(),
                title: l
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                hosts: vec![],
            })
        })
        .collect()
}

/// Web Mercator metres to degrees.
fn merc_to_deg(x: f64, y: f64) -> (f64, f64) {
    let lon = x / HALF * 180.0;
    let lat = (2.0 * (y / HALF * std::f64::consts::PI).exp().atan() - std::f64::consts::FRAC_PI_2)
        .to_degrees();
    (lon, lat)
}

/// An ArcGIS extent in degrees, when its spatial reference is one we know.
fn extent_of(e: &Value) -> Option<[f64; 4]> {
    let g = |k: &str| e.get(k).and_then(Value::as_f64);
    let (xmin, ymin, xmax, ymax) = (g("xmin")?, g("ymin")?, g("xmax")?, g("ymax")?);
    let sr = e.get("spatialReference")?;
    let wkid = sr
        .get("latestWkid")
        .or_else(|| sr.get("wkid"))
        .and_then(Value::as_u64)?;
    let out = match wkid {
        4326 => [xmin, ymin, xmax, ymax],
        3857 | 102100 | 102113 | 900913 => {
            let (w, s) = merc_to_deg(xmin, ymin);
            let (e2, n) = merc_to_deg(xmax, ymax);
            [w, s, e2, n]
        }
        _ => return None,
    };
    out.iter().all(|v| v.is_finite()).then_some(out)
}

fn arcgis_caps(
    v: &Value,
    root: Url,
    image: bool,
    layer: Option<&str>,
) -> Result<(Caps, Vec<Offered>), WebError> {
    let extent = v
        .get("fullExtent")
        .or_else(|| v.get("extent"))
        .and_then(extent_of);
    let offered = if image { vec![] } else { offered_arcgis(v) };
    if let Some(l) = layer {
        if !image && !offered.is_empty() && !offered.iter().any(|o| o.name == l) {
            return Err(err(409, format!("the service has no layer {l}")));
        }
    }
    let fused = v
        .get("singleFusedMapCache")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let levels = if fused && layer.is_none() {
        mercator_levels(v.get("tileInfo"))
    } else {
        HashMap::new()
    };
    let draw = if !levels.is_empty() {
        Draw::ArcTiles {
            root: root.clone(),
            levels: levels.clone(),
        }
    } else {
        Draw::ArcExport {
            root,
            image,
            layer: layer.map(String::from),
        }
    };
    let (min_zoom, max_zoom) = if levels.is_empty() {
        (0, 22)
    } else {
        (
            *levels.keys().min().unwrap_or(&0),
            *levels.keys().max().unwrap_or(&22),
        )
    };
    Ok((
        Caps {
            draw,
            extent,
            min_zoom,
            max_zoom,
        },
        offered,
    ))
}

/// A tiling scheme's levels that line up with Web Mercator zooms: 256 px
/// tiles, origin at the world's top left, each level's scale that of a zoom.
fn mercator_levels(ti: Option<&Value>) -> HashMap<u8, u64> {
    let mut out = HashMap::new();
    let Some(ti) = ti else { return out };
    let rows = ti.get("rows").and_then(Value::as_u64).unwrap_or(0);
    let wkid = ti
        .pointer("/spatialReference/latestWkid")
        .or_else(|| ti.pointer("/spatialReference/wkid"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let ox = ti
        .pointer("/origin/x")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let oy = ti
        .pointer("/origin/y")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if rows != u64::from(TILE)
        || !matches!(wkid, 3857 | 102100 | 102113 | 900913)
        || (ox + HALF).abs() > 1.0
        || (oy - HALF).abs() > 1.0
    {
        return out;
    }
    for lod in ti
        .get("lods")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(level), Some(scale)) = (
            lod.get("level").and_then(Value::as_u64),
            lod.get("scale").and_then(Value::as_f64),
        ) else {
            continue;
        };
        if let Some(z) = zoom_of(ARCGIS_SCALE0, scale) {
            out.insert(z, level);
        }
    }
    out
}

/// The zoom whose scale this is, within 1%.
fn zoom_of(scale0: f64, scale: f64) -> Option<u8> {
    if scale.is_nan() || scale <= 0.0 {
        return None;
    }
    let z = (scale0 / scale).log2();
    let r = z.round();
    ((z - r).abs() < 0.015 && (0.0..=f64::from(MAX_ZOOM)).contains(&r)).then_some(r as u8)
}

// --- XML helpers (namespaces vary between servers; local names are used).

fn kids<'a, 'i>(
    n: roxmltree::Node<'a, 'i>,
    name: &'static str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    n.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn kid<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &'static str) -> Option<roxmltree::Node<'a, 'i>> {
    kids(n, name).next()
}

fn text_of(n: Option<roxmltree::Node>) -> String {
    n.and_then(|n| n.text()).unwrap_or("").trim().to_string()
}

fn attr<'a>(n: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

fn parse_xml(text: &str) -> Result<roxmltree::Document<'_>, WebError> {
    roxmltree::Document::parse(text).map_err(|e| {
        err(
            502,
            format!("the service's capabilities are not readable XML ({e})"),
        )
    })
}

fn host_of(u: &str, base: &Url) -> Option<String> {
    base.join(u).ok().as_ref().and_then(url_host)
}

fn corners(b: Option<roxmltree::Node>) -> Option<[f64; 4]> {
    let b = b?;
    let lc: Vec<f64> = text_of(kid(b, "LowerCorner"))
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let uc: Vec<f64> = text_of(kid(b, "UpperCorner"))
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    (lc.len() == 2 && uc.len() == 2).then(|| [lc[0], lc[1], uc[0], uc[1]])
}

/// The chosen WMTS layer: its tile template, matrices by zoom, extent.
type Chosen = (String, HashMap<u8, String>, Option<[f64; 4]>);

/// WMTS: the layer's tile URL template (ResourceURL, else KVP GetTile) and
/// its Web Mercator TileMatrixSet's matrices by zoom.
fn wmts_caps(
    text: &str,
    base: &Url,
    layer: Option<&str>,
    own: &str,
) -> Result<(Caps, Vec<Offered>), WebError> {
    let doc = parse_xml(text)?;
    let rootn = doc.root_element();
    let contents =
        kid(rootn, "Contents").ok_or_else(|| err(502, "the WMTS capabilities have no Contents"))?;
    // KVP GetTile, if the server offers it.
    let kvp = kids(rootn, "OperationsMetadata")
        .flat_map(|o| kids(o, "Operation"))
        .find(|o| attr(*o, "name") == Some("GetTile"))
        .and_then(|o| {
            o.descendants()
                .find(|d| d.is_element() && d.tag_name().name() == "Get")
                .and_then(|g| attr(g, "href"))
                .map(String::from)
        });
    // Tile matrix sets in Web Mercator: id -> zoom -> matrix.
    let mut sets: HashMap<String, HashMap<u8, String>> = HashMap::new();
    for set in kids(contents, "TileMatrixSet") {
        let id = text_of(kid(set, "Identifier"));
        let crs = text_of(kid(set, "SupportedCRS")).to_ascii_lowercase();
        let wkss = text_of(kid(set, "WellKnownScaleSet")).to_ascii_lowercase();
        let merc = ["3857", "900913", "102100", "102113"]
            .iter()
            .any(|c| crs.ends_with(&format!(":{c}")) || crs.ends_with(&format!("/{c}")))
            || wkss.contains("googlemapscompatible");
        if !merc {
            continue;
        }
        let mut by_zoom = HashMap::new();
        for m in kids(set, "TileMatrix") {
            let mid = text_of(kid(m, "Identifier"));
            let scale: f64 = text_of(kid(m, "ScaleDenominator")).parse().unwrap_or(0.0);
            let tw: u32 = text_of(kid(m, "TileWidth")).parse().unwrap_or(0);
            let tl: Vec<f64> = text_of(kid(m, "TopLeftCorner"))
                .split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect();
            let corner_ok =
                tl.len() == 2 && (tl[0] + HALF).abs() < 1.0 && (tl[1] - HALF).abs() < 1.0;
            if tw != TILE || !corner_ok {
                continue;
            }
            if let Some(z) = zoom_of(WMTS_SCALE0, scale) {
                by_zoom.insert(z, mid);
            }
        }
        if !by_zoom.is_empty() {
            sets.insert(id, by_zoom);
        }
    }
    let mut offered = Vec::new();
    let mut chosen: Option<Chosen> = None;
    for l in kids(contents, "Layer") {
        let name = text_of(kid(l, "Identifier"));
        let title = text_of(kid(l, "Title"));
        let style = kids(l, "Style")
            .find(|s| attr(*s, "isDefault") == Some("true"))
            .or_else(|| kid(l, "Style"))
            .map(|s| text_of(kid(s, "Identifier")))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".into());
        let formats: Vec<String> = kids(l, "Format").map(|f| text_of(Some(f))).collect();
        let resource = kids(l, "ResourceURL")
            .filter(|r| attr(*r, "resourceType") == Some("tile"))
            .filter_map(|r| {
                Some((
                    attr(r, "format").unwrap_or("").to_string(),
                    attr(r, "template")?.to_string(),
                ))
            })
            .min_by_key(|(f, _)| {
                if f.contains("png") {
                    0
                } else if f.contains("jpeg") {
                    1
                } else {
                    2
                }
            });
        let set = kids(l, "TileMatrixSetLink")
            .map(|k| text_of(kid(k, "TileMatrixSet")))
            .find(|s| sets.contains_key(s));
        let (template, hosts) = match (&resource, &kvp) {
            (Some((_, t)), _) => {
                let abs = base
                    .join(t)
                    .map(|u| u.to_string().replace("%7B", "{").replace("%7D", "}"))
                    .unwrap_or_else(|_| t.clone());
                let h = host_of(t, base);
                (Some(abs), h.into_iter().collect::<Vec<_>>())
            }
            (None, Some(href)) => {
                let format = formats
                    .iter()
                    .find(|f| f.contains("png"))
                    .or_else(|| formats.first())
                    .cloned()
                    .unwrap_or_else(|| "image/png".into());
                let mut u = base
                    .join(href)
                    .map_err(|_| err(502, "the WMTS GetTile address is not valid"))?;
                u.query_pairs_mut()
                    .append_pair("SERVICE", "WMTS")
                    .append_pair("REQUEST", "GetTile")
                    .append_pair("VERSION", "1.0.0")
                    .append_pair("LAYER", &name)
                    .append_pair("STYLE", &style)
                    .append_pair("FORMAT", &format);
                let h = url_host(&u);
                (
                    Some(format!("{u}&TILEMATRIXSET={{TileMatrixSet}}&TILEMATRIX={{TileMatrix}}&TILEROW={{TileRow}}&TILECOL={{TileCol}}")),
                    h.into_iter().collect(),
                )
            }
            _ => (None, vec![]),
        };
        let hosts: Vec<String> = hosts.into_iter().filter(|h| !same_host(h, own)).collect();
        let usable = template.is_some() && set.is_some();
        if usable {
            offered.push(Offered {
                name: name.clone(),
                title: if title.is_empty() {
                    name.clone()
                } else {
                    title
                },
                hosts,
            });
        }
        if layer == Some(name.as_str()) {
            let (Some(t), Some(s)) = (template, set) else {
                return Err(err(
                    409,
                    format!("WMTS layer {name} has no tiles in Web Mercator (EPSG:3857, 256 px), which the map needs"),
                ));
            };
            let t = t
                .replace("{Style}", &style)
                .replace("{style}", &style)
                .replace("{TileMatrixSet}", &s);
            let ext = corners(kid(l, "WGS84BoundingBox"));
            chosen = Some((t, sets[&s].clone(), ext));
        }
    }
    let caps = match (layer, chosen) {
        (None, _) => Caps {
            draw: Draw::Xyz {
                template: String::new(),
            },
            extent: None,
            min_zoom: 0,
            max_zoom: 22,
        },
        (Some(l), None) => return Err(err(409, format!("the WMTS service has no layer {l}"))),
        (Some(_), Some((template, matrices, extent))) => Caps {
            min_zoom: *matrices.keys().min().unwrap_or(&0),
            max_zoom: *matrices.keys().max().unwrap_or(&22),
            draw: Draw::Wmts { template, matrices },
            extent,
        },
    };
    Ok((caps, offered))
}

/// WMS: the GetMap address and the named layer, which must be offered in
/// Web Mercator.
fn wms_caps(
    text: &str,
    base: &Url,
    layer: Option<&str>,
    own: &str,
) -> Result<(Caps, Vec<Offered>), WebError> {
    let doc = parse_xml(text)?;
    let rootn = doc.root_element();
    let version = attr(rootn, "version").unwrap_or("1.3.0").to_string();
    let v13 = !version.starts_with("1.1");
    let cap = kid(rootn, "Capability")
        .ok_or_else(|| err(502, "the WMS capabilities have no Capability"))?;
    let getmap = kid(cap, "Request").and_then(|r| kid(r, "GetMap"));
    let formats: Vec<String> = getmap
        .map(|g| kids(g, "Format").map(|f| text_of(Some(f))).collect())
        .unwrap_or_default();
    let format = formats
        .iter()
        .find(|f| f.as_str() == "image/png")
        .or_else(|| formats.iter().find(|f| f.contains("png")))
        .or_else(|| formats.iter().find(|f| f.contains("jpeg")))
        .cloned()
        .unwrap_or_else(|| "image/png".into());
    let href = getmap
        .and_then(|g| {
            g.descendants()
                .find(|d| d.is_element() && d.tag_name().name() == "Get")
                .and_then(|get| {
                    get.descendants()
                        .find(|d| d.is_element() && d.tag_name().name() == "OnlineResource")
                })
                .and_then(|o| attr(o, "href"))
        })
        .map(String::from);
    let mut gm = match &href {
        Some(h) => base
            .join(h)
            .map_err(|_| err(502, "the WMS GetMap address is not valid"))?,
        None => base.clone(),
    };
    strip_ogc(&mut gm);
    let extra: Vec<String> = url_host(&gm)
        .filter(|h| !same_host(h, own))
        .into_iter()
        .collect();
    let crs_tag = if v13 { "CRS" } else { "SRS" };
    let merc = |l: roxmltree::Node| {
        l.ancestors()
            .filter(|a| a.is_element() && a.tag_name().name() == "Layer")
            .flat_map(|a| {
                a.children()
                    .filter(|c| c.is_element() && c.tag_name().name() == crs_tag)
            })
            .flat_map(|c| {
                text_of(Some(c))
                    .split_whitespace()
                    .map(str::to_ascii_uppercase)
                    .collect::<Vec<_>>()
            })
            .any(|c| c == "EPSG:3857" || c == "EPSG:900913")
    };
    let mut offered = Vec::new();
    let mut extent = None;
    let mut found = false;
    for l in cap
        .descendants()
        .filter(|d| d.is_element() && d.tag_name().name() == "Layer")
    {
        let name = text_of(kid(l, "Name"));
        if name.is_empty() {
            continue;
        }
        let ok = merc(l);
        if ok {
            let title = text_of(kid(l, "Title"));
            offered.push(Offered {
                name: name.clone(),
                title: if title.is_empty() {
                    name.clone()
                } else {
                    title
                },
                hosts: extra.clone(),
            });
        }
        if layer == Some(name.as_str()) {
            if !ok {
                return Err(err(
                    409,
                    format!("WMS layer {name} is not offered in Web Mercator (EPSG:3857), which the map needs"),
                ));
            }
            found = true;
            extent = l
                .ancestors()
                .filter(|a| a.is_element() && a.tag_name().name() == "Layer")
                .find_map(|a| {
                    if let Some(b) = kid(a, "EX_GeographicBoundingBox") {
                        let g = |k: &'static str| text_of(kid(b, k)).parse::<f64>().ok();
                        Some([
                            g("westBoundLongitude")?,
                            g("southBoundLatitude")?,
                            g("eastBoundLongitude")?,
                            g("northBoundLatitude")?,
                        ])
                    } else {
                        let b = kid(a, "LatLonBoundingBox")?;
                        let g = |k: &str| attr(b, k).and_then(|v| v.parse::<f64>().ok());
                        Some([g("minx")?, g("miny")?, g("maxx")?, g("maxy")?])
                    }
                });
        }
    }
    if let Some(l) = layer {
        if !found {
            return Err(err(409, format!("the WMS service has no layer {l}")));
        }
        gm.query_pairs_mut()
            .append_pair("SERVICE", "WMS")
            .append_pair("REQUEST", "GetMap")
            .append_pair("VERSION", &version)
            .append_pair("LAYERS", l)
            .append_pair("STYLES", "")
            .append_pair("FORMAT", &format)
            .append_pair("TRANSPARENT", "TRUE")
            .append_pair("WIDTH", &TILE.to_string())
            .append_pair("HEIGHT", &TILE.to_string());
    }
    Ok((
        Caps {
            draw: Draw::Wms {
                getmap: gm,
                crs_key: if v13 { "CRS" } else { "SRS" },
            },
            extent,
            min_zoom: 0,
            max_zoom: 22,
        },
        offered,
    ))
}

// ---------------------------------------------------------------------
// The page's requests.

/// A tile's box in Web Mercator metres: (minx, miny, maxx, maxy).
fn tile_box(z: u8, x: u32, y: u32) -> (f64, f64, f64, f64) {
    let n = f64::from(1u32 << z);
    let size = 2.0 * HALF / n;
    let minx = -HALF + f64::from(x) * size;
    let maxy = HALF - f64::from(y) * size;
    (minx, maxy - size, minx + size, maxy)
}

/// The URL of one tile.
fn tile_url(c: &Caps, z: u8, x: u32, y: u32) -> Result<Url, WebError> {
    let (minx, miny, maxx, maxy) = tile_box(z, x, y);
    let bbox = format!("{minx},{miny},{maxx},{maxy}");
    let fill = |t: &str| -> Result<Url, WebError> {
        Url::parse(t).map_err(|_| err(502, "the service's tile address is not valid"))
    };
    match &c.draw {
        Draw::Xyz { template } => fill(
            &template
                .replace("{z}", &z.to_string())
                .replace("{x}", &x.to_string())
                .replace("{y}", &y.to_string()),
        ),
        Draw::Wmts { template, matrices } => {
            let m = matrices
                .get(&z)
                .ok_or_else(|| err(404, format!("no tiles at zoom {z}")))?;
            fill(
                &template
                    .replace("{TileMatrix}", m)
                    .replace("{TileRow}", &y.to_string())
                    .replace("{TileCol}", &x.to_string()),
            )
        }
        Draw::Wms { getmap, crs_key } => {
            let mut u = getmap.clone();
            u.query_pairs_mut()
                .append_pair(crs_key, "EPSG:3857")
                .append_pair("BBOX", &bbox);
            Ok(u)
        }
        Draw::ArcTiles { root, levels } => {
            let level = levels
                .get(&z)
                .ok_or_else(|| err(404, format!("no tiles at zoom {z}")))?;
            let mut u = root.clone();
            let p = u.path().trim_end_matches('/').to_string();
            u.set_path(&format!("{p}/tile/{level}/{y}/{x}"));
            Ok(u)
        }
        Draw::ArcExport { root, image, layer } => {
            let mut u = root.clone();
            let p = u.path().trim_end_matches('/').to_string();
            u.set_path(&format!(
                "{p}/{}",
                if *image { "exportImage" } else { "export" }
            ));
            {
                let mut q = u.query_pairs_mut();
                q.append_pair("bbox", &bbox)
                    .append_pair("bboxSR", "3857")
                    .append_pair("imageSR", "3857")
                    .append_pair("size", &format!("{TILE},{TILE}"))
                    .append_pair("format", if *image { "png" } else { "png32" })
                    .append_pair("transparent", "true")
                    .append_pair("f", "image");
                if let Some(l) = layer {
                    q.append_pair("layers", &format!("show:{l}"));
                }
            }
            Ok(u)
        }
        Draw::Features { .. } => Err(err(400, "this web layer is drawn from features, not tiles")),
    }
}

/// `GET /api/web/sources`: every web layer the person can turn on, by id
/// and title (no address). Caches of removed sources are dropped here.
pub(crate) fn sources(env: &Env) -> Value {
    let (all, notes) = all(env);
    purge_unknown(
        &env.cache,
        &all.iter().map(|(s, _)| s.id.clone()).collect::<Vec<_>>(),
    );
    let enabled = env.map().allow_web_layers;
    json!({
        "enabled": enabled,
        "sources": if enabled { all.iter().map(|(s, o)| json!({
            "id": s.id,
            "title": s.title,
            "kind": s.kind.as_str(),
            "raster": s.kind.raster(),
            "attribution": s.attribution,
            "secured": s.auth_from.is_some(),
            "origin": o.as_str(),
        })).collect::<Vec<_>>() } else { vec![] },
        "notes": notes,
    })
}

/// `GET /api/web/source/{id}`: what the map needs to draw it.
pub(crate) async fn info(env: &Env, id: &str) -> Result<Value, WebError> {
    allowed(env)?;
    let (src, origin) = find(env, id)?;
    let c = caps(env, &src, origin).await?;
    let mut v = json!({
        "id": src.id,
        "title": src.title,
        "kind": src.kind.as_str(),
        "raster": src.kind.raster(),
        "attribution": src.attribution,
        "secured": src.auth_from.is_some(),
        "extent": c.extent,
        "min_zoom": c.min_zoom,
        "max_zoom": c.max_zoom,
    });
    if let Draw::Features {
        fields, geometry, ..
    } = &c.draw
    {
        v["features"] = json!(format!("/api/web/features/{}", src.id));
        v["max_features"] = json!(MAX_FEATURES);
        v["fields"] = json!(fields);
        v["geometry_type"] = json!(geometry);
    } else {
        v["tiles"] = json!(format!("/api/web/tile/{}/{{z}}/{{x}}/{{y}}", src.id));
        v["tile_size"] = json!(TILE);
    }
    Ok(v)
}

/// `GET /api/web/tile/{id}/{z}/{x}/{y}`: one 256 px Web Mercator tile, as
/// (content type, bytes), from the cache when it has it.
pub(crate) async fn tile(
    env: &Env,
    id: &str,
    z: u8,
    x: u32,
    y: u32,
) -> Result<(String, Vec<u8>), WebError> {
    allowed(env)?;
    if z > MAX_ZOOM || u64::from(x) >= (1u64 << z) || u64::from(y) >= (1u64 << z) {
        return Err(err(400, "no such tile"));
    }
    let (src, origin) = find(env, id)?;
    if !src.kind.raster() {
        return Err(err(400, "this web layer is drawn from features, not tiles"));
    }
    let (_, own) = parse_url(&src)?;
    // The source's own host is checked before anything, even the cache.
    host_ok(env, &src, origin, &own, &own)?;
    let dir = env.cache.join(&src.id);
    if let Some(a) = &src.auth_from {
        if !crate::arcgis_auth::signed_in(&env.store, a) {
            drop_cache(&env.cache, &src.id);
        }
    }
    let fp = src.fingerprint();
    if let Some(hit) = cache_get(&dir, &fp, z, x, y) {
        return Ok(hit);
    }
    let c = caps(env, &src, origin).await?;
    let url = tile_url(&c, z, x, y)?;
    let r = fetch(env, &src, origin, &own, &url, MAX_IMAGE).await?;
    let ct = r
        .content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if !ct.starts_with("image/") {
        return Err(err(
            502,
            format!("Web layer service at {own} did not send an image for this tile. Check the layer, then Test again."),
        ));
    }
    cache_put(&env.cache, &dir, &fp, z, x, y, &ct, &r.body);
    Ok((ct, r.body))
}

/// A bounding box `w,s,e,n` in degrees.
pub(crate) fn parse_bbox(s: &str) -> Result<[f64; 4], WebError> {
    let v: Vec<f64> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    if v.len() != 4 || v.iter().any(|n| !n.is_finite()) {
        return Err(err(400, "bbox must be west,south,east,north in degrees"));
    }
    let b = [
        v[0].clamp(-180.0, 180.0),
        v[1].clamp(-90.0, 90.0),
        v[2].clamp(-180.0, 180.0),
        v[3].clamp(-90.0, 90.0),
    ];
    if b[0] >= b[2] || b[1] >= b[3] {
        return Err(err(
            400,
            "bbox must be west,south,east,north with west < east and south < north",
        ));
    }
    Ok(b)
}

/// `GET /api/web/features/{id}?bbox=w,s,e,n`: the view's features as
/// GeoJSON, at most `MAX_FEATURES`, with `total` and `exceeded` ("Zoom in
/// to see all"). Hidden fields are removed when the layer comes through an
/// `arcgis` connection.
pub(crate) async fn features(env: &Env, id: &str, bbox: [f64; 4]) -> Result<Value, WebError> {
    allowed(env)?;
    let (src, origin) = find(env, id)?;
    if src.kind != MapKind::ArcgisFeatures {
        return Err(err(400, "this web layer is drawn from tiles, not features"));
    }
    let (_, own) = parse_url(&src)?;
    let c = caps(env, &src, origin).await?;
    let Draw::Features {
        layer,
        page,
        paging,
        fields,
        ..
    } = &c.draw
    else {
        return Err(err(409, "this web layer has no feature layer"));
    };
    let envelope = format!("{},{},{},{}", bbox[0], bbox[1], bbox[2], bbox[3]);
    let query = |extra: &[(&str, String)]| {
        let mut u = layer.clone();
        let p = u.path().trim_end_matches('/').to_string();
        u.set_path(&format!("{p}/query"));
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("where", "1=1")
                .append_pair("geometry", &envelope)
                .append_pair("geometryType", "esriGeometryEnvelope")
                .append_pair("inSR", "4326")
                .append_pair("spatialRel", "esriSpatialRelIntersects");
            for (k, v) in extra {
                q.append_pair(k, v);
            }
        }
        u
    };
    let count_url = query(&[("returnCountOnly", "true".into()), ("f", "json".into())]);
    let r = fetch(env, &src, origin, &own, &count_url, MAX_DOC).await?;
    let total = json_of(&r, &own)?
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let want = total.min(MAX_FEATURES);
    let hidden = src.auth_from.is_some();
    let out_fields = if fields.is_empty() {
        "*".to_string()
    } else {
        fields.join(",")
    };
    let mut feats: Vec<Value> = Vec::new();
    let mut pages = 0;
    while (feats.len() as u64) < want && pages < 20 {
        pages += 1;
        let n = (*page).min(want - feats.len() as u64);
        let mut extra = vec![
            ("outFields", out_fields.clone()),
            ("returnGeometry", "true".into()),
            ("outSR", "4326".into()),
            ("f", "geojson".into()),
        ];
        if *paging {
            extra.push(("resultOffset", feats.len().to_string()));
            extra.push(("resultRecordCount", n.to_string()));
        }
        let r = fetch(env, &src, origin, &own, &query(&extra), MAX_FEATURES_PAGE).await?;
        let v = json_of(&r, &own)?;
        let got: Vec<Value> = v
            .get("features")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if got.is_empty() {
            break;
        }
        for mut f in got {
            if (feats.len() as u64) >= want {
                break;
            }
            if let Some(props) = f.get_mut("properties").and_then(Value::as_object_mut) {
                props.retain(|k, _| {
                    !(hidden && hidden_field(&env.settings.policy, k))
                        && (fields.is_empty() || fields.iter().any(|f| f == k))
                });
            }
            feats.push(f);
        }
        if !*paging {
            break;
        }
    }
    Ok(json!({
        "type": "FeatureCollection",
        "features": feats,
        "total": total,
        "exceeded": total > MAX_FEATURES,
    }))
}

/// Titles of every web layer the person can turn on, for `whoami` (names
/// only: no address, host or setting).
pub(crate) fn titles(settings: &Arc<Settings>, paths: &Paths) -> Vec<String> {
    if !settings.policy.map.allow_web_layers {
        return vec![];
    }
    all_in(&settings.policy.map, &paths.map_sources)
        .0
        .into_iter()
        .map(|(s, _)| s.title)
        .collect()
}

// ---------------------------------------------------------------------
// The tile cache.

const STAMP: &str = ".source";

fn ext_of(ct: &str) -> Option<&'static str> {
    Some(match ct {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => return None,
    })
}

fn ct_of(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/gif",
    }
}

fn tile_name(z: u8, x: u32, y: u32) -> String {
    format!("{z}_{x}_{y}")
}

fn cache_get(dir: &Path, fp: &str, z: u8, x: u32, y: u32) -> Option<(String, Vec<u8>)> {
    if std::fs::read_to_string(dir.join(STAMP)).ok()?.trim() != fp {
        return None;
    }
    let base = tile_name(z, x, y);
    for ext in ["png", "jpg", "webp", "gif"] {
        let p = dir.join(format!("{base}.{ext}"));
        if let Ok(m) = std::fs::symlink_metadata(&p) {
            if m.is_file() {
                if let Ok(b) = std::fs::read(&p) {
                    return Some((ct_of(ext).to_string(), b));
                }
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn cache_put(root: &Path, dir: &Path, fp: &str, z: u8, x: u32, y: u32, ct: &str, bytes: &[u8]) {
    let Some(ext) = ext_of(ct) else { return };
    let stamped = std::fs::read_to_string(dir.join(STAMP))
        .ok()
        .map(|s| s.trim().to_string());
    if stamped.as_deref() != Some(fp) {
        // A changed source (or none yet): its old tiles go.
        let _ = std::fs::remove_dir_all(dir);
        if std::fs::create_dir_all(dir).is_err() || std::fs::write(dir.join(STAMP), fp).is_err() {
            return;
        }
    }
    let name = format!("{}.{ext}", tile_name(z, x, y));
    let tmp = dir.join(format!(".{name}.{}", uuid::Uuid::new_v4().simple()));
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, dir.join(&name)).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    added(root, bytes.len() as u64, CACHE_LIMIT);
}

type Totals = Mutex<HashMap<PathBuf, u64>>;

fn totals() -> &'static Totals {
    static T: OnceLock<Totals> = OnceLock::new();
    T.get_or_init(Default::default)
}

/// Count `n` new bytes in the cache under `root`; past `limit`, delete the
/// oldest tiles until it is under nine tenths of it.
pub(crate) fn added(root: &Path, n: u64, limit: u64) {
    let mut t = totals().lock().unwrap_or_else(|e| e.into_inner());
    let total = t
        .entry(root.to_path_buf())
        .or_insert_with(|| walk(root).iter().map(|f| f.2).sum());
    *total += n;
    if *total <= limit {
        return;
    }
    let mut files = walk(root);
    files.sort_by_key(|f| f.1);
    let mut sum: u64 = files.iter().map(|f| f.2).sum();
    for (p, _, len) in files {
        if sum <= limit / 10 * 9 {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            sum = sum.saturating_sub(len);
        }
    }
    *total = sum;
}

/// Every cached tile: (path, written, bytes).
fn walk(root: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut out = Vec::new();
    let Ok(dirs) = std::fs::read_dir(root) else {
        return out;
    };
    for d in dirs.flatten() {
        let Ok(files) = std::fs::read_dir(d.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            if let Ok(m) = f.metadata() {
                if m.is_file() {
                    out.push((
                        f.path(),
                        m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        m.len(),
                    ));
                }
            }
        }
    }
    out
}

/// Delete one source's cache folder.
pub(crate) fn drop_cache(root: &Path, id: &str) {
    if !map_sources::id_ok(id) {
        return;
    }
    let dir = root.join(id);
    if std::fs::symlink_metadata(&dir).is_ok() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            tracing::warn!("web layer {id}'s tile cache could not be removed: {e}");
        }
        totals()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(root);
    }
}

/// Delete the cache folders of sources that no longer exist.
fn purge_unknown(root: &Path, known: &[String]) {
    let Ok(dirs) = std::fs::read_dir(root) else {
        return;
    };
    for d in dirs.flatten() {
        let name = d.file_name().to_string_lossy().into_owned();
        if !known.iter().any(|k| k.eq_ignore_ascii_case(&name)) {
            drop_cache(root, &name);
        }
    }
}

/// Sign-out of `connection` (`arcgis_auth::sign_out`): drop the caches of
/// every source that takes its sign-in from it.
pub(crate) fn drop_secured(paths: &Paths, connection: &str) {
    let root = cache_root(paths);
    let (saved, _) = map_sources::load_dir(&paths.map_sources);
    let org = columbus_shared::policy::OrgPolicy::load(&paths.policy)
        .map(|p| p.map.sources)
        .unwrap_or_default();
    for s in saved.iter().chain(org.iter()) {
        if s.auth_from.as_deref() == Some(connection) {
            drop_cache(&root, &s.id);
        }
    }
}

// ---------------------------------------------------------------------
// The Columbus app's commands: `columbus-mcp map-source …`.

pub mod cli {
    use super::*;

    fn env() -> anyhow::Result<(Env, Paths)> {
        let paths = Paths::from_env();
        let (_, settings) = crate::config::read_settings(&paths);
        let settings = settings?;
        let store = Arc::new(SecretStore::from_env());
        Ok((Env::new(Arc::new(settings), store, &paths), paths))
    }

    fn print(v: Value, ok: bool) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        if ok {
            Ok(())
        } else {
            std::process::exit(1)
        }
    }

    fn fail(msg: impl Into<String>) -> anyhow::Result<()> {
        print(json!({"ok": false, "message": msg.into()}), false)
    }

    /// Guess the kind from a pasted URL.
    fn guess(url: &str) -> Option<MapKind> {
        let l = url.to_ascii_lowercase();
        let path = l
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        let last = path.rsplit('/').next().unwrap_or("");
        let before = path.rsplit('/').nth(1).unwrap_or("");
        if l.contains("{z}") {
            Some(MapKind::Xyz)
        } else if last == "featureserver" || before == "featureserver" {
            Some(MapKind::ArcgisFeatures)
        } else if last == "mapserver" {
            Some(MapKind::ArcgisMap)
        } else if last == "imageserver" {
            Some(MapKind::ArcgisImage)
        } else if l.contains("wmts") {
            Some(MapKind::Wmts)
        } else if l.contains("service=wms") || l.contains("wms") {
            Some(MapKind::Wms)
        } else {
            None
        }
    }

    /// `map-source inspect URL [--kind K] [--auth-from CONNECTION]`: read
    /// the service's capabilities and offer its layers. Nothing is saved.
    pub async fn inspect(url: &str, flags: &[&str]) -> anyhow::Result<()> {
        let (env, _) = env()?;
        let mut kind = None;
        let mut auth_from = None;
        let mut it = flags.iter();
        while let Some(f) = it.next() {
            match *f {
                "--kind" => kind = it.next().and_then(|k| MapKind::parse(k)),
                "--auth-from" => auth_from = it.next().map(|s| s.to_string()),
                other => return fail(format!("unknown option {other}")),
            }
        }
        let Some(kind) = kind.or_else(|| guess(url)) else {
            return fail("Columbus cannot tell what kind of service this is; choose xyz, wmts, wms, arcgis-map, arcgis-image or arcgis-features");
        };
        if let Err(e) = allowed(&env) {
            return fail(e.message);
        }
        let src = MapSource {
            id: "inspect".into(),
            title: "inspect".into(),
            kind,
            url: url.trim().to_string(),
            layer: None,
            auth_from,
            attribution: None,
        };
        let (_, own) = match parse_url(&src) {
            Ok(u) => u,
            Err(e) => return fail(e.message),
        };
        let map = env.map();
        if !map.hosts.is_empty() && !map.host_listed(&own) {
            return fail(format!(
                "host {own} is not on your organization's list of map hosts (policy map.hosts)"
            ));
        }
        // Reading capabilities is the person's own action: only the host
        // they pasted is asked (capability hosts are not contacted here).
        let (_, offered) = match read_caps(&env, &src, Origin::Policy).await {
            Ok(c) => c,
            Err(e) => return fail(e.message),
        };
        let layers: Vec<Value> = offered
            .iter()
            .map(|o| {
                let refused: Vec<&String> = o
                    .hosts
                    .iter()
                    .filter(|h| !map.hosts.is_empty() && !map.host_listed(h))
                    .collect();
                json!({"layer": o.name, "title": o.title, "hosts": o.hosts, "refused_hosts": refused})
            })
            .collect();
        print(
            json!({
                "ok": true,
                "kind": kind.as_str(),
                "url": src.url,
                "host": own,
                "layers": layers,
                "confirm": map.hosts.is_empty(),
            }),
            true,
        )
    }

    /// `map-source save JSON|- [--replace]`: save an F-13 entry after
    /// reading its capabilities; with `map.hosts` empty, the app has had
    /// the person confirm at the keyboard first.
    pub async fn save(arg: &str, flags: &[&str]) -> anyhow::Result<()> {
        let (env, paths) = env()?;
        let replace = flags.contains(&"--replace");
        if let Some(f) = flags.iter().find(|f| **f != "--replace") {
            return fail(format!("unknown option {f}"));
        }
        let text = if arg == "-" {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            s
        } else {
            arg.to_string()
        };
        let src: MapSource = match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(e) => return fail(format!("not a web layer (F-13): {e}")),
        };
        if let Err(e) = src.check() {
            return fail(e);
        }
        if let Err(e) = allowed(&env) {
            return fail(e.message);
        }
        if env
            .map()
            .sources
            .iter()
            .any(|s| s.id.eq_ignore_ascii_case(&src.id))
        {
            return fail(format!(
                "your organization supplies a web layer named {}; choose another id",
                src.id
            ));
        }
        let old = match map_sources::load_one(&env.dir, &src.id) {
            Ok(o) => o,
            Err(e) if replace => {
                tracing::warn!("{e}");
                None
            }
            Err(e) => return fail(e),
        };
        let exists = map_sources::file(&env.dir, &src.id).exists();
        if exists && !replace {
            return fail(format!("a web layer named {} already exists", src.id));
        }
        if !exists && replace {
            return fail(format!("there is no web layer {} any more", src.id));
        }
        if let Some(a) = &src.auth_from {
            if !env
                .settings
                .connections
                .iter()
                .any(|c| &c.id == a && is_arcgis(&c.kind))
            {
                return fail(format!("{a} is not an arcgis connection"));
            }
        }
        let (_, own) = match parse_url(&src) {
            Ok(u) => u,
            Err(e) => return fail(e.message),
        };
        let map = env.map();
        if !map.hosts.is_empty() && !map.host_listed(&own) {
            return fail(format!(
                "host {own} is not on your organization's list of map hosts (policy map.hosts)"
            ));
        }
        let (_, offered) = match read_caps(&env, &src, Origin::Policy).await {
            Ok(c) => c,
            Err(e) => return fail(e.message),
        };
        let hosts: Vec<String> = offered
            .iter()
            .find(|o| Some(o.name.as_str()) == src.layer.as_deref().map(str::trim))
            .map(|o| o.hosts.clone())
            .unwrap_or_default();
        if !map.hosts.is_empty() {
            if let Some(h) = hosts.iter().find(|h| !map.host_listed(h)) {
                return fail(format!(
                    "this layer's tiles come from {h}, which is not on your organization's list of map hosts (policy map.hosts)"
                ));
            }
        }
        std::fs::create_dir_all(&env.dir)?;
        let file = map_sources::file(&env.dir, &src.id);
        let body = format!(
            "# A web map layer, saved in the Columbus app (F-13).\n{}",
            serde_yaml::to_string(&src)?
        );
        if let Err(e) = columbus_shared::tools::write_atomic(&file, body.as_bytes()) {
            return fail(e);
        }
        let recorded = if map.hosts.is_empty() {
            hosts.clone()
        } else {
            vec![]
        };
        if let Err(e) = map_sources::record_hosts(&env.dir, &src, &recorded) {
            return fail(format!(
                "saved, but its approved hosts could not be recorded: {e}"
            ));
        }
        if old.is_none_or(|o| o.fingerprint() != src.fingerprint()) {
            drop_cache(&cache_root(&paths), &src.id);
        }
        print(
            json!({"ok": true, "id": src.id, "file": file.display().to_string(), "hosts": hosts}),
            true,
        )
    }

    /// `map-source test ID`: is it reachable, as the map would ask?
    pub async fn test(id: &str) -> anyhow::Result<()> {
        let (env, _) = env()?;
        let r = async {
            allowed(&env)?;
            let (src, origin) = find(&env, id)?;
            let c = read_caps(&env, &src, origin).await?.0;
            match &c.draw {
                Draw::Features { .. } => {
                    features(&env, id, [-180.0, -85.0, 180.0, 85.0]).await.map(|v| {
                        format!("reachable: {} features", v["total"])
                    })
                }
                _ => {
                    let (z, x, y) = (c.min_zoom, 0, 0);
                    let (_, own) = parse_url(&src)?;
                    let url = tile_url(&c, z, x, y)?;
                    let r = fetch(&env, &src, origin, &own, &url, MAX_IMAGE).await?;
                    if r.content_type.starts_with("image/") {
                        Ok("reachable: it sent a tile".to_string())
                    } else {
                        Err(err(502, format!("Web layer service at {own} did not send an image. Check the layer, then Test again.")))
                    }
                }
            }
        }
        .await;
        match r {
            Ok(m) => print(json!({"ok": true, "message": m}), true),
            Err(e) => fail(e.message),
        }
    }

    /// `map-source remove ID`: the file, its approved hosts and its tile
    /// cache go.
    pub fn remove(id: &str) -> anyhow::Result<()> {
        let (env, paths) = env()?;
        if !map_sources::id_ok(id) {
            return fail(format!("there is no web layer {id:?}"));
        }
        if env
            .map()
            .sources
            .iter()
            .any(|s| s.id.eq_ignore_ascii_case(id))
        {
            return fail(format!(
                "{id} is supplied by your organization and cannot be removed here"
            ));
        }
        let file = map_sources::file(&env.dir, id);
        let existed = std::fs::symlink_metadata(&file).is_ok();
        if existed {
            std::fs::remove_file(&file)?;
        }
        let _ = map_sources::forget_hosts(&env.dir, id);
        drop_cache(&cache_root(&paths), id);
        if existed {
            print(json!({"ok": true, "id": id}), true)
        } else {
            fail(format!("there is no web layer {id:?}"))
        }
    }

    /// `map-source list`: every source, for Connections › Map layers.
    pub fn list() -> anyhow::Result<()> {
        let (env, _) = env()?;
        let (all, notes) = all(&env);
        let map = env.map();
        print(
            json!({
                "ok": true,
                "allow_web_layers": map.allow_web_layers,
                "hosts": map.hosts,
                "confirm": map.hosts.is_empty(),
                "sources": all.iter().map(|(s, o)| {
                    let mut v = serde_json::to_value(s).unwrap_or_default();
                    v["origin"] = json!(o.as_str());
                    v["approved_hosts"] = json!(if *o == Origin::Saved { map_sources::approved_hosts(&env.dir, s) } else { vec![] });
                    v
                }).collect::<Vec<_>>(),
                "notes": notes,
            }),
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_boxes_cover_the_world() {
        let (a, b, c, d) = tile_box(0, 0, 0);
        assert!((a + HALF).abs() < 1e-6 && (b + HALF).abs() < 1e-6);
        assert!((c - HALF).abs() < 1e-6 && (d - HALF).abs() < 1e-6);
        let (minx, _, _, maxy) = tile_box(1, 1, 0);
        assert!(minx.abs() < 1e-6 && (maxy - HALF).abs() < 1e-6);
    }

    #[test]
    fn zooms_from_scales() {
        assert_eq!(zoom_of(WMTS_SCALE0, WMTS_SCALE0), Some(0));
        assert_eq!(zoom_of(WMTS_SCALE0, 1_091_957.546_931_088), Some(9));
        assert_eq!(zoom_of(ARCGIS_SCALE0, 72_223.819_286), Some(13));
        assert_eq!(zoom_of(WMTS_SCALE0, 1_000_000.0), None);
    }

    #[test]
    fn local_addresses_are_refused() {
        assert!(local_ip("127.0.0.1".parse().unwrap()));
        assert!(local_ip("169.254.169.254".parse().unwrap()));
        assert!(local_ip("::1".parse().unwrap()));
        assert!(!local_ip("10.1.2.3".parse().unwrap()));
        assert!(!local_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn the_cache_keeps_under_its_limit_oldest_out_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tiles");
        let a = root.join("a");
        let fp = "f";
        for i in 0..5u32 {
            cache_put(&root, &a, fp, 3, i, 0, "image/png", &[0u8; 100]);
            // Distinct write times, oldest first.
            std::thread::sleep(Duration::from_millis(20));
        }
        // 500 bytes so far; a limit of 300 keeps the newest (at most 270).
        added(&root, 0, 300);
        let left: Vec<String> = walk(&root)
            .into_iter()
            .map(|f| f.0.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left.contains(&"3_4_0.png".to_string()) && left.contains(&"3_3_0.png".to_string()));
        assert!(cache_get(&a, fp, 3, 4, 0).is_some());
        assert!(cache_get(&a, "changed", 3, 4, 0).is_none());
        drop_cache(&root, "a");
        assert!(!a.exists());
    }

    #[test]
    fn wmts_capabilities_give_mercator_matrices_and_tile_hosts() {
        let caps = r#"<Capabilities xmlns="http://www.opengis.net/wmts/1.0" xmlns:ows="http://www.opengis.net/ows/1.1" version="1.0.0">
 <Contents>
  <Layer><ows:Title>Ortho</ows:Title><ows:Identifier>ortho</ows:Identifier>
   <ows:WGS84BoundingBox><ows:LowerCorner>-98 30</ows:LowerCorner><ows:UpperCorner>-97 31</ows:UpperCorner></ows:WGS84BoundingBox>
   <Style isDefault="true"><ows:Identifier>default</ows:Identifier></Style>
   <Format>image/png</Format>
   <TileMatrixSetLink><TileMatrixSet>gm</TileMatrixSet></TileMatrixSetLink>
   <ResourceURL format="image/png" resourceType="tile" template="https://cdn.example/wmts/ortho/{Style}/{TileMatrixSet}/{TileMatrix}/{TileRow}/{TileCol}.png"/>
  </Layer>
  <TileMatrixSet><ows:Identifier>gm</ows:Identifier><ows:SupportedCRS>urn:ogc:def:crs:EPSG::3857</ows:SupportedCRS>
   <TileMatrix><ows:Identifier>00</ows:Identifier><ScaleDenominator>559082264.0287178</ScaleDenominator><TopLeftCorner>-20037508.3427892 20037508.3427892</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>1</MatrixWidth><MatrixHeight>1</MatrixHeight></TileMatrix>
   <TileMatrix><ows:Identifier>01</ows:Identifier><ScaleDenominator>279541132.0143589</ScaleDenominator><TopLeftCorner>-20037508.3427892 20037508.3427892</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>2</MatrixWidth><MatrixHeight>2</MatrixHeight></TileMatrix>
  </TileMatrixSet>
 </Contents>
</Capabilities>"#;
        let base = Url::parse("https://imagery.example/wmts/WMTSCapabilities.xml").unwrap();
        let (c, offered) = wmts_caps(caps, &base, Some("ortho"), "imagery.example").unwrap();
        assert_eq!(offered[0].hosts, vec!["cdn.example".to_string()]);
        let u = tile_url(&c, 1, 1, 0).unwrap();
        assert_eq!(
            u.as_str(),
            "https://cdn.example/wmts/ortho/default/gm/01/0/1.png"
        );
        assert_eq!(c.extent, Some([-98.0, 30.0, -97.0, 31.0]));
        assert!(tile_url(&c, 5, 0, 0).is_err());
    }

    #[test]
    fn wms_layers_need_web_mercator() {
        let caps = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink">
<Capability><Request><GetMap><Format>image/jpeg</Format><Format>image/png</Format>
<DCPType><HTTP><Get><OnlineResource xlink:href="https://maps.example/wms?map=a&amp;"/></Get></HTTP></DCPType></GetMap></Request>
<Layer><Title>Root</Title><CRS>EPSG:4326</CRS><CRS>EPSG:3857</CRS>
 <Layer><Name>parcels</Name><Title>Parcels</Title>
  <EX_GeographicBoundingBox><westBoundLongitude>-98</westBoundLongitude><eastBoundLongitude>-97</eastBoundLongitude><southBoundLatitude>30</southBoundLatitude><northBoundLatitude>31</northBoundLatitude></EX_GeographicBoundingBox></Layer>
</Layer></Capability></WMS_Capabilities>"#;
        let base =
            Url::parse("https://maps.example/wms?SERVICE=WMS&REQUEST=GetCapabilities").unwrap();
        let (c, offered) = wms_caps(caps, &base, Some("parcels"), "maps.example").unwrap();
        assert_eq!(offered.len(), 1);
        let u = tile_url(&c, 0, 0, 0).unwrap();
        let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
        assert_eq!(q["LAYERS"], "parcels");
        assert_eq!(q["CRS"], "EPSG:3857");
        assert_eq!(q["FORMAT"], "image/png");
        assert_eq!(q["map"], "a");
        assert!(wms_caps(caps, &base, Some("nope"), "maps.example").is_err());
    }
}
