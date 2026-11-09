//! ArcGIS sign-in (T-11; build brief Part 4 "Sign-in" and "Host rule",
//! D-4, D-12, REQ-10, F-3b, R-17, R-18).
//!
//! - The person signs in once, from the terminal
//!   (`columbus-mcp arcgis signin <id>`): authorization code with PKCE
//!   (S256) at `<portal>/sharing/rest/oauth2/authorize` and `/token`, with
//!   the organization's own `client_id` (D-12; Columbus ships none).
//! - The redirect comes back to a one-time listener on `127.0.0.1` with a
//!   random port (`http://127.0.0.1:<port>`), its `state` and path checked,
//!   closed after one redirect or the wait. The fallback (A-2) is the
//!   registered `urn:ietf:wg:oauth:2.0:oob`: the portal shows a code, and
//!   the person pastes it into the terminal (`--paste-code`, or offered
//!   when the browser does not come back).
//! - Tokens live only in the OS credential store, entry
//!   `arcgis:<connection id>` holding the F-3b JSON
//!   `{access_token, refresh_token, expires_at, portal}`. With no
//!   credential store, sign-in is refused (R-17): tokens are never written
//!   to files. (Debug builds' development store, `COLUMBUS_DEV_SECRETS_DIR`,
//!   is the one exception, for tests.)
//! - The gateway reads the entry for each call, refreshes an expired access
//!   token once without asking, and sends it only as a header, only to the
//!   portal and to service hosts the portal vouches for (R-18). Tokens never
//!   reach the agent, the engine, the audit log, provenance, tool results or
//!   error messages.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use base64::Engine;
use chrono::{DateTime, Utc};
use columbus_shared::policy::{is_arcgis, ArcgisAuth, Connection, OrgPolicy};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;

use crate::secrets::SecretStore;
pub(crate) use columbus_shared::arcgis_state::forget as forget_state;
use columbus_shared::arcgis_state::{
    expires_at as refresh_expires_at, record as record_refresh_expiry,
};

/// The out-of-band redirect: the portal shows the code to copy (A-2).
pub(crate) const OOB: &str = "urn:ietf:wg:oauth:2.0:oob";
/// The longest any one request to the portal may take (R-14).
const TIMEOUT: Duration = Duration::from_secs(30);
/// An access token this close to its end counts as expired.
const SKEW_SECS: i64 = 60;
/// How long the terminal waits for the browser to come back.
const WAIT: Duration = Duration::from_secs(300);
/// How long a portal's list of federated servers is reused.
const SERVERS_TTL: Duration = Duration::from_secs(300);
/// R-19: a refresh token with fewer days left than this is exchanged for
/// a new one before a workflow step uses the connection (`keep_alive`).
pub(crate) const EXCHANGE_BELOW_DAYS: i64 = 7;

/// The credential-store entry holding a connection's tokens (F-3b).
pub(crate) fn entry_name(id: &str) -> String {
    format!("arcgis:{id}")
}

/// The command a person runs to sign in.
pub(crate) fn signin_command(id: &str) -> String {
    format!("columbus-mcp arcgis signin {id}")
}

/// The plain message for a sign-in that has lapsed (REQ-10).
pub(crate) fn sign_in_again(id: &str) -> String {
    format!(
        "{id}'s ArcGIS sign-in has expired. Sign in again: {}",
        signin_command(id)
    )
}

/// R-17, word for word in every refusal.
const NO_STORE: &str = "there is no credential store on this system, so Columbus cannot keep an ArcGIS sign-in (it never writes tokens to files). Public layers (auth: none) still work";

/// The F-3b entry. Never logged: `Debug` shows neither token.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// When the access token ends (ISO 8601, UTC).
    pub expires_at: String,
    /// The portal the tokens are for, as `Portal::key` writes it.
    pub portal: String,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at", &self.expires_at)
            .field("portal", &self.portal)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    /// Whether the access token has ended, or ends within a minute (an
    /// unreadable time counts as ended).
    pub(crate) fn expired(&self, now: DateTime<Utc>) -> bool {
        match DateTime::parse_from_rfc3339(&self.expires_at) {
            Ok(t) => now + chrono::Duration::seconds(SKEW_SECS) >= t.with_timezone(&Utc),
            Err(_) => true,
        }
    }
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The portal a connection signs in to, checked.
#[derive(Clone, Debug)]
pub(crate) struct Portal {
    /// `https://www.arcgis.com` or `https://portal.example.org/portal`,
    /// without `/sharing/rest` or a trailing slash.
    pub base: Url,
    pub host: String,
}

impl Portal {
    /// The portal as the stored entry names it.
    pub(crate) fn key(&self) -> String {
        self.base.as_str().trim_end_matches('/').to_string()
    }

    fn endpoint(&self, rest: &str) -> Url {
        let mut u = self.base.clone();
        let path = u.path().trim_end_matches('/').to_string();
        u.set_path(&format!("{path}/sharing/rest/{rest}"));
        u
    }
}

fn url_host(u: &Url) -> Option<String> {
    let h = u
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!h.is_empty()).then_some(h)
}

pub(crate) fn same_host(a: &str, b: &str) -> bool {
    match (a.parse::<std::net::IpAddr>(), b.parse::<std::net::IpAddr>()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// The portal and client id of a signed-in connection (`auth: oauth`),
/// or None for a public one. The portal must use https (plain http only
/// to this computer, in test builds) and its host must be on policy
/// `hosts` (Part 4).
pub(crate) fn portal(
    conn: &Connection,
    policy: &OrgPolicy,
) -> Result<Option<(Portal, String)>, String> {
    let Some(o) = conn.auth.as_ref().and_then(ArcgisAuth::oauth) else {
        return Ok(None);
    };
    let raw = o.portal.trim();
    let bad = || {
        format!(
            "{}'s auth portal must be an https address such as https://www.arcgis.com, not {raw:?}",
            conn.id
        )
    };
    let mut url = Url::parse(raw).map_err(|_| bad())?;
    let host = url_host(&url).ok_or_else(bad)?;
    let plain_http = url.scheme() == "http"
        && crate::fetch_tools::test_loopback()
        && (host == "127.0.0.1" || host == "localhost");
    if (url.scheme() != "https" && !plain_http)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    if !policy.host_allowed(&host) {
        return Err(columbus_shared::error::Denied::Host(host).to_string());
    }
    let mut path = url.path().trim_end_matches('/').to_string();
    for tail in ["/sharing/rest", "/sharing"] {
        if path.to_ascii_lowercase().ends_with(tail) {
            path.truncate(path.len() - tail.len());
        }
    }
    url.set_path(&path);
    Ok(Some((
        Portal { base: url, host },
        o.client_id.trim().to_string(),
    )))
}

/// R-17: the store can keep tokens (the OS store; in debug builds also the
/// development store with a folder, for tests).
pub(crate) fn usable_store(store: &SecretStore) -> Result<(), String> {
    if store.keeps_tokens() {
        Ok(())
    } else {
        Err(NO_STORE.to_string())
    }
}

/// The stored sign-in, if any. An unreadable entry is reported without its
/// contents.
pub(crate) fn load(store: &SecretStore, id: &str) -> Result<Option<Tokens>, String> {
    usable_store(store)?;
    let text = store
        .get(&entry_name(id))
        .map_err(|e| format!("reading {id}'s ArcGIS sign-in from the credential store failed ({e}). Try again, or sign in again: {}", signin_command(id)))?;
    match text {
        None => Ok(None),
        Some(t) => serde_json::from_str(&t).map(Some).map_err(|_| {
            format!(
                "{id}'s stored ArcGIS sign-in is unreadable. Sign in again: {}",
                signin_command(id)
            )
        }),
    }
}

pub(crate) fn save(store: &SecretStore, id: &str, t: &Tokens) -> Result<(), String> {
    usable_store(store)?;
    let text = serde_json::to_string(t).map_err(|e| e.to_string())?;
    store
        .set(&entry_name(id), &text)
        .map_err(|e| format!("saving {id}'s ArcGIS sign-in in the credential store failed: {e}"))
}

/// Whether a sign-in is stored for this connection (whoami; T-24's tile
/// caches check it before reuse). Never reads past the entry's presence.
pub(crate) fn signed_in(store: &SecretStore, id: &str) -> bool {
    store.keeps_tokens() && matches!(store.get(&entry_name(id)), Ok(Some(_)))
}

/// Forget a connection's sign-in; whether there was one. This is the
/// sign-out signal (the CLI's `arcgis signout`, and deleting the
/// connection, F-3b): the tile caches of web layers secured by it go too
/// (T-24), whether or not a sign-in was stored.
pub(crate) fn sign_out(
    store: &SecretStore,
    id: &str,
    paths: &crate::config::Paths,
) -> Result<bool, String> {
    crate::web_layers::drop_secured(paths, id);
    if !store.keeps_tokens() {
        return Ok(false);
    }
    store.delete(&entry_name(id)).map_err(|e| e.to_string())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(TIMEOUT)
        .user_agent(concat!("columbus-mcp/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut u = Url::parse("http://x/").expect("static url");
    u.query_pairs_mut().extend_pairs(pairs);
    u.query().unwrap_or("").to_string()
}

fn short(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
    s.trim().to_string()
}

/// What the token endpoint answered.
#[derive(Default)]
struct Grant {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    /// How long the refresh token lasts, when the portal says (seconds).
    refresh_expires_in: Option<i64>,
    username: Option<String>,
}

enum TokenError {
    /// The refresh token was refused: sign in again.
    Lapsed,
    /// Anything else, as a plain sentence.
    Plain(String),
}

/// One POST to `<portal>/sharing/rest/oauth2/token`. ArcGIS reports OAuth
/// errors in a 200 answer (`{"error": {"code": 400, "error":
/// "invalid_grant", …}}`); standard servers in a 400. Both are read. No
/// error ever quotes the answer's tokens.
async fn token_call(
    portal: &Portal,
    client_id: &str,
    form: &[(&str, &str)],
    refreshing: bool,
) -> Result<Grant, TokenError> {
    let host = portal.host.clone();
    let url = portal.endpoint("oauth2/token");
    let mut pairs = vec![("client_id", client_id), ("f", "json")];
    pairs.extend_from_slice(form);
    let resp = client()
        .map_err(TokenError::Plain)?
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(form_body(&pairs))
        .send()
        .await
        .map_err(|e| {
            TokenError::Plain(if e.is_timeout() {
                format!("ArcGIS portal at {host} did not answer in 30 s. Check the address or your network, then try again.")
            } else {
                format!("ArcGIS portal at {host} could not be reached. Check the address or your network, then try again.")
            })
        })?;
    let status = resp.status().as_u16();
    if resp.status().is_redirection() {
        return Err(TokenError::Plain(format!(
            "ArcGIS portal at {host} answered the sign-in with a redirect, which Columbus does not follow. Check the connection's auth portal address."
        )));
    }
    let body = resp.bytes().await.map_err(|_| {
        TokenError::Plain(format!(
            "ArcGIS portal at {host} broke off its answer. Check your network, then try again."
        ))
    })?;
    let v: Value = serde_json::from_slice(&body).map_err(|_| {
        TokenError::Plain(format!(
            "ArcGIS portal at {host} did not answer the sign-in with JSON (HTTP {status}). Check the connection's auth portal address."
        ))
    })?;
    if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
        let (code, text) = match e {
            Value::String(s) => (
                s.clone(),
                v.get("error_description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            o => (
                o.get("error")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or_else(|| o.get("code").map(|c| c.to_string()))
                    .unwrap_or_default(),
                o.get("error_description")
                    .or_else(|| o.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
        };
        if code == "invalid_client" || code == "unauthorized_client" {
            return Err(TokenError::Plain(format!(
                "ArcGIS portal at {host} does not accept the client id {client_id} (\"{}\"). Ask your ArcGIS administrator to check the connection's client_id and the app's redirect URIs.",
                short(&text)
            )));
        }
        if refreshing {
            return Err(TokenError::Lapsed);
        }
        return Err(TokenError::Plain(format!(
            "ArcGIS portal at {host} refused the sign-in ({}: \"{}\"). Try again; if it keeps failing, ask your ArcGIS administrator to check the app's redirect URIs.",
            short(&code),
            short(&text)
        )));
    }
    if !(200..300).contains(&status) {
        if refreshing && (400..500).contains(&status) {
            return Err(TokenError::Lapsed);
        }
        return Err(TokenError::Plain(format!(
            "ArcGIS portal at {host} answered the sign-in with HTTP {status}. Try again later."
        )));
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    Ok(Grant {
        access_token: s("access_token").filter(|t| !t.is_empty()),
        refresh_token: s("refresh_token").filter(|t| !t.is_empty()),
        expires_in: v.get("expires_in").and_then(Value::as_i64),
        refresh_expires_in: v.get("refresh_token_expires_in").and_then(Value::as_i64),
        username: s("username"),
    })
}

fn plain(e: TokenError, id: &str) -> String {
    match e {
        TokenError::Lapsed => sign_in_again(id),
        TokenError::Plain(s) => s,
    }
}

/// Refresh an expired access token (grant_type=refresh_token). ArcGIS
/// keeps the refresh token; a new one is used if the portal sends it. A
/// refused refresh token gives exactly "Sign in again: columbus-mcp arcgis
/// signin <id>".
pub(crate) async fn refresh(
    portal: &Portal,
    client_id: &str,
    id: &str,
    t: &Tokens,
) -> Result<Tokens, String> {
    let g = token_call(
        portal,
        client_id,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &t.refresh_token),
        ],
        true,
    )
    .await
    .map_err(|e| plain(e, id))?;
    let access = g.access_token.ok_or_else(|| sign_in_again(id))?;
    Ok(Tokens {
        access_token: access,
        refresh_token: g.refresh_token.unwrap_or_else(|| t.refresh_token.clone()),
        expires_at: iso(Utc::now() + chrono::Duration::seconds(g.expires_in.unwrap_or(1800))),
        portal: t.portal.clone(),
    })
}

/// Swap the refresh token for a new one (grant_type=exchange_refresh_token,
/// R-19), keeping the access token unless a new one comes too; also how
/// long the new refresh token lasts, when the portal says. `keep_alive`
/// calls it when fewer than `EXCHANGE_BELOW_DAYS` remain. `redirect_uri`
/// must be one the app registers; `OOB` always is.
pub(crate) async fn exchange_refresh_token(
    portal: &Portal,
    client_id: &str,
    redirect_uri: &str,
    id: &str,
    t: &Tokens,
) -> Result<(Tokens, Option<i64>), String> {
    let g = token_call(
        portal,
        client_id,
        &[
            ("grant_type", "exchange_refresh_token"),
            ("redirect_uri", redirect_uri),
            ("refresh_token", &t.refresh_token),
        ],
        true,
    )
    .await
    .map_err(|e| plain(e, id))?;
    let refresh = g.refresh_token.ok_or_else(|| sign_in_again(id))?;
    let access_came = g.access_token.is_some();
    Ok((
        Tokens {
            access_token: g.access_token.unwrap_or_else(|| t.access_token.clone()),
            refresh_token: refresh,
            expires_at: match g.expires_in.filter(|_| access_came) {
                Some(s) => iso(Utc::now() + chrono::Duration::seconds(s)),
                None => t.expires_at.clone(),
            },
            portal: t.portal.clone(),
        },
        g.refresh_expires_in,
    ))
}

// ---------------------------------------------------------------------
// Keeping a sign-in alive for unattended runs (R-19, REQ-26).

/// A workflow step's stop for a sign-in that has lapsed (REQ-26), exactly.
pub(crate) fn needs_sign_in(id: &str) -> String {
    format!("needs you: sign in again ({})", signin_command(id))
}

/// What `keep_alive` did.
#[derive(Debug, PartialEq)]
pub(crate) enum Kept {
    /// A public service, or a sign-in with `EXCHANGE_BELOW_DAYS` or more left.
    AsIs,
    /// The refresh token was exchanged for a new one, and stored.
    Exchanged,
    /// The exchange could not be made for another reason (the portal did
    /// not answer, say); the step goes on with the sign-in as it is.
    NotNow(String),
}

/// R-19, before a workflow step uses `conn`: if fewer than
/// `EXCHANGE_BELOW_DAYS` of the refresh token's life remain (or that is
/// not known), swap it for a new one with `exchange_refresh_token` and
/// store it. A sign-in that has lapsed (or was never made, or is for
/// another portal) gives `needs_sign_in`. No token reaches any message.
pub(crate) async fn keep_alive(
    store: &SecretStore,
    conn: &Connection,
    policy: &OrgPolicy,
    connections: &std::path::Path,
) -> Result<Kept, String> {
    let Some((portal, client_id)) = portal(conn, policy)? else {
        return Ok(Kept::AsIs);
    };
    let id = conn.id.as_str();
    usable_store(store).map_err(|e| format!("{id} needs an ArcGIS sign-in, but {e}."))?;
    let lock = refresh_lock(id);
    let _one = lock.lock().await;
    let Some(t) = load(store, id)? else {
        return Err(needs_sign_in(id));
    };
    if t.portal != portal.key() {
        return Err(needs_sign_in(id));
    }
    let now = Utc::now();
    match refresh_expires_at(connections, id, &t.portal) {
        Some(end) if end <= now => return Err(needs_sign_in(id)),
        Some(end) if end - now >= chrono::Duration::days(EXCHANGE_BELOW_DAYS) => {
            return Ok(Kept::AsIs)
        }
        _ => {}
    }
    match exchange_refresh_token(&portal, &client_id, OOB, id, &t).await {
        Ok((fresh, secs)) => {
            save(store, id, &fresh)?;
            if let Err(e) = record_refresh_expiry(connections, id, &fresh.portal, secs) {
                tracing::warn!("{id}: when its new ArcGIS sign-in ends could not be recorded: {e}");
            }
            Ok(Kept::Exchanged)
        }
        Err(e) if e == sign_in_again(id) => Err(needs_sign_in(id)),
        Err(e) => Ok(Kept::NotNow(e)),
    }
}

/// One lock per connection, so two calls at once refresh once.
fn refresh_lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static L: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    L.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(id.to_string())
        .or_default()
        .clone()
}

/// The access token to send for `conn`, and the portal it is for: None for
/// a public service. An expired access token is refreshed once and stored.
pub(crate) async fn bearer(
    store: &SecretStore,
    conn: &Connection,
    policy: &OrgPolicy,
) -> Result<Option<(String, Portal)>, String> {
    let Some((portal, client_id)) = portal(conn, policy)? else {
        return Ok(None);
    };
    let id = conn.id.as_str();
    usable_store(store).map_err(|e| format!("{id} needs an ArcGIS sign-in, but {e}."))?;
    let lock = refresh_lock(id);
    let _one = lock.lock().await;
    let Some(t) = load(store, id)? else {
        return Err(format!(
            "{id} needs an ArcGIS sign-in, and nobody has signed in yet. Sign in: {}",
            signin_command(id)
        ));
    };
    if t.portal != portal.key() {
        return Err(format!(
            "{id}'s sign-in is for another portal than the connection now names. Sign in again: {}",
            signin_command(id)
        ));
    }
    if !t.expired(Utc::now()) {
        return Ok(Some((t.access_token, portal)));
    }
    let fresh = refresh(&portal, &client_id, id, &t).await?;
    save(store, id, &fresh)?;
    Ok(Some((fresh.access_token, portal)))
}

/// ArcGIS Online's hosted services: `services.arcgis.com`,
/// `services1.arcgis.com`, ….
fn online_services_host(h: &str) -> bool {
    h.to_ascii_lowercase()
        .strip_suffix(".arcgis.com")
        .and_then(|p| p.strip_prefix("services"))
        .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
}

type ServerLists = Mutex<HashMap<String, (Instant, Vec<String>)>>;

fn server_lists() -> &'static ServerLists {
    static S: OnceLock<ServerLists> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// The hosts of the servers the portal lists as federated
/// (`<portal>/sharing/rest/portals/self/servers`), asked with the token
/// (the portal itself may always have it).
async fn federated_hosts(portal: &Portal, token: &str) -> Result<Vec<String>, String> {
    let key = portal.key();
    if let Some((at, hosts)) = server_lists()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        if at.elapsed() < SERVERS_TTL {
            return Ok(hosts.clone());
        }
    }
    let mut u = portal.endpoint("portals/self/servers");
    u.query_pairs_mut().append_pair("f", "json");
    let resp = client()?
        .get(u)
        .bearer_auth(token)
        .header("X-Esri-Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| {
            format!(
                "ArcGIS portal at {} could not list its federated servers",
                portal.host
            )
        })?;
    let v: Value = resp.json().await.map_err(|_| {
        format!(
            "ArcGIS portal at {} sent an unreadable list of federated servers",
            portal.host
        )
    })?;
    if v.get("error").is_some_and(|e| !e.is_null()) {
        return Err(format!(
            "ArcGIS portal at {} refused to list its federated servers",
            portal.host
        ));
    }
    let mut hosts: Vec<String> = v
        .get("servers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|s| ["url", "adminUrl"].map(|k| s.get(k).and_then(Value::as_str)))
        .flatten()
        .filter_map(|s| Url::parse(s).ok())
        .filter_map(|u| url_host(&u))
        .collect();
    hosts.sort();
    hosts.dedup();
    server_lists()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (Instant::now(), hosts.clone()));
    Ok(hosts)
}

/// The service hosts that may receive the token (R-18): the portal's own
/// host, ArcGIS Online's `services*.arcgis.com` for `www.arcgis.com`, or a
/// server the portal lists as federated. Empty when the portal vouches for
/// none of them, or could not say.
pub(crate) async fn vouched(portal: &Portal, token: &str, service_host: &str) -> Vec<String> {
    if same_host(service_host, &portal.host)
        || (portal.host.eq_ignore_ascii_case("www.arcgis.com")
            && online_services_host(service_host))
    {
        return vec![service_host.to_string()];
    }
    match federated_hosts(portal, token).await {
        Ok(hosts) => hosts
            .into_iter()
            .filter(|h| same_host(h, service_host))
            .collect(),
        Err(e) => {
            tracing::warn!("{e}; no token goes to {service_host}");
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------
// Signing in.

/// `(verifier, S256 challenge)`: 64 random hex characters, and the
/// base64url SHA-256 of them.
fn pkce() -> (String, String) {
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// How the code comes back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Redirect {
    /// To a one-time listener on 127.0.0.1 (the default).
    Loopback,
    /// Shown by the portal, pasted into the terminal (A-2's fallback).
    Paste,
}

/// A sign-in under way: the address to open, and what finishing needs.
/// T-12's Sign in button can drive the same steps: `begin`, open `url`,
/// `wait_redirect` (or a pasted code), `finish`, `save`.
pub(crate) struct Pending {
    portal: Portal,
    client_id: String,
    verifier: String,
    state: String,
    redirect: String,
    listener: Option<tokio::net::TcpListener>,
    /// The portal's authorization page, to open in the browser.
    pub url: String,
}

pub(crate) async fn begin(
    portal: &Portal,
    client_id: &str,
    how: Redirect,
) -> Result<Pending, String> {
    let (listener, redirect) = match how {
        Redirect::Loopback => {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| format!("could not listen for the sign-in redirect: {e}"))?;
            let port = l.local_addr().map_err(|e| e.to_string())?.port();
            (Some(l), format!("http://127.0.0.1:{port}"))
        }
        Redirect::Paste => (None, OOB.to_string()),
    };
    let (verifier, challenge) = pkce();
    let state = uuid::Uuid::new_v4().simple().to_string();
    let mut u = portal.endpoint("oauth2/authorize");
    u.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(Pending {
        portal: portal.clone(),
        client_id: client_id.to_string(),
        verifier,
        state,
        redirect,
        listener,
        url: u.to_string(),
    })
}

/// A pasted code, checked (letters, digits and `-._~` only).
pub(crate) fn pasted_code(text: &str) -> Result<String, String> {
    let c = text.trim();
    if c.is_empty() {
        return Err("no code was entered; sign-in not completed".into());
    }
    if c.len() > 4096
        || !c
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~'))
    {
        return Err("that does not look like the code ArcGIS showed; copy it again (only the code), then try again".into());
    }
    Ok(c.to_string())
}

impl Pending {
    /// Wait for the browser's redirect: the code, or None when it did not
    /// come within `wait`. The listener takes one redirect and closes; a
    /// redirect with another `state` ends the sign-in.
    pub(crate) async fn wait_redirect(&mut self, wait: Duration) -> Result<Option<String>, String> {
        let Some(listener) = self.listener.take() else {
            return Ok(None);
        };
        let target = match tokio::time::timeout(
            wait,
            columbus_broker::oauth::receive_redirect(&listener, "/"),
        )
        .await
        {
            Err(_) => return Ok(None),
            Ok(r) => r?,
        };
        drop(listener);
        let u = Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|_| "the sign-in redirect was unreadable; try again".to_string())?;
        let q: HashMap<String, String> = u
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if q.get("state") != Some(&self.state) {
            return Err(
                "the sign-in redirect did not match this sign-in, so nothing was stored. Try again"
                    .into(),
            );
        }
        q.get("code")
            .filter(|c| !c.is_empty())
            .cloned()
            .map(Some)
            .ok_or_else(|| "the sign-in redirect carried no code; try again".to_string())
    }

    /// Exchange the code for tokens (with the PKCE verifier). Also returns
    /// the ArcGIS user name, when the portal says.
    pub(crate) async fn finish(
        self,
        code: &str,
        id: &str,
    ) -> Result<(Tokens, Option<String>, Option<i64>), String> {
        let g = token_call(
            &self.portal,
            &self.client_id,
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.redirect),
                ("code_verifier", &self.verifier),
            ],
            false,
        )
        .await
        .map_err(|e| plain(e, id))?;
        let (Some(access), Some(refresh)) = (g.access_token, g.refresh_token) else {
            return Err(format!(
                "ArcGIS portal at {} signed you in without a refresh token, so the sign-in could not last. Ask your ArcGIS administrator to check the app's settings",
                self.portal.host
            ));
        };
        Ok((
            Tokens {
                access_token: access,
                refresh_token: refresh,
                expires_at: iso(
                    Utc::now() + chrono::Duration::seconds(g.expires_in.unwrap_or(1800))
                ),
                portal: self.portal.key(),
            },
            g.username,
            g.refresh_expires_in,
        ))
    }
}

// ---------------------------------------------------------------------
// The terminal commands.

/// The connection, its portal and client id, from the settings files.
fn signin_target(id: &str) -> anyhow::Result<(Portal, String)> {
    let paths = crate::config::Paths::from_env();
    let (_, settings) = crate::config::read_settings(&paths);
    let settings = settings?;
    let conn = settings
        .connections
        .iter()
        .find(|c| c.id == id)
        .ok_or_else(|| anyhow!("no connection {id:?} in {}", paths.connections.display()))?;
    if !is_arcgis(&conn.kind) {
        bail!("{id} is not an ArcGIS connection (type: arcgis), so it has no ArcGIS sign-in");
    }
    portal(conn, &settings.policy)
        .map_err(|e| anyhow!(e))?
        .ok_or_else(|| {
            anyhow!("{id} is a public ArcGIS connection (auth: none); it needs no sign-in")
        })
}

fn open(url: &str) {
    use std::io::Write;
    if std::env::var("COLUMBUS_SIGNIN_BROWSER").as_deref() == Ok("print") {
        // Tests (and the app): the caller opens the link itself.
        println!("open: {url}");
    } else {
        println!("Your browser is opening the ArcGIS sign-in page. If it does not, open this address:\n{url}");
        if let Err(e) = crate::approvals_page::open_browser(url) {
            println!("(the browser could not be opened: {e})");
        }
    }
    let _ = std::io::stdout().flush();
}

/// Lines typed into the terminal, read on one thread so the "press Enter"
/// offer and the code prompt share them. Closed at end of input.
fn terminal_lines() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        loop {
            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

fn wait_time() -> Duration {
    std::env::var("COLUMBUS_SIGNIN_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.max(1)))
        .unwrap_or(WAIT)
}

async fn paste_flow(
    portal: &Portal,
    client_id: &str,
    id: &str,
    lines: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<(Tokens, Option<String>, Option<i64>), String> {
    use std::io::Write;
    let p = begin(portal, client_id, Redirect::Paste).await?;
    open(&p.url);
    print!("After you sign in, ArcGIS shows a code. Copy it, paste it here (into this terminal, never into a chat) and press Enter: ");
    let _ = std::io::stdout().flush();
    let line = lines
        .recv()
        .await
        .ok_or("no code was entered; sign-in not completed")?;
    println!();
    let code = pasted_code(&line)?;
    p.finish(&code, id).await
}

/// `columbus-mcp arcgis signin ID [--paste-code]`.
pub async fn signin_cli(id: &str, flags: &[&str]) -> anyhow::Result<()> {
    let paste = match flags {
        [] => false,
        ["--paste-code"] => true,
        _ => bail!("usage: columbus-mcp arcgis signin ID [--paste-code]"),
    };
    let (portal, client_id) = signin_target(id)?;
    let store = SecretStore::from_env();
    usable_store(&store).map_err(|e| anyhow!("ArcGIS sign-in refused: {e}."))?;
    println!("Signing in to ArcGIS at {} for {id}.", portal.key());
    let mut lines = terminal_lines();
    let (tokens, user, refresh_secs) = if paste {
        paste_flow(&portal, &client_id, id, &mut lines).await
    } else {
        let mut p = begin(&portal, &client_id, Redirect::Loopback)
            .await
            .map_err(|e| anyhow!(e))?;
        open(&p.url);
        let wait = wait_time();
        println!(
            "Waiting up to {} for the browser. If the page shows an error about the redirect address, press Enter here to sign in with a code instead.",
            if wait.as_secs() >= 60 {
                format!("{} minutes", wait.as_secs() / 60)
            } else {
                format!("{} seconds", wait.as_secs())
            }
        );
        let code = tokio::select! {
            r = p.wait_redirect(wait) => r.map_err(|e| anyhow!(e))?,
            Some(_) = lines.recv() => None,
        };
        match code {
            Some(code) => p.finish(&code, id).await,
            None => {
                drop(p);
                println!("The browser did not come back to Columbus. Sign in with a code instead:");
                paste_flow(&portal, &client_id, id, &mut lines).await
            }
        }
    }
    .map_err(|e| anyhow!(e))?;
    save(&store, id, &tokens).map_err(|e| anyhow!(e))?;
    // When the refresh token ends (not secret), so unattended runs can
    // exchange it in time (R-19).
    let connections = crate::config::Paths::from_env().connections;
    if let Err(e) = record_refresh_expiry(&connections, id, &tokens.portal, refresh_secs) {
        println!("(when this sign-in ends could not be recorded, so the next workflow run renews it early: {e})");
    }
    println!(
        "Signed in{} for {id}. The sign-in is kept in {} and is never shown; Columbus refreshes it by itself.",
        user.map(|u| format!(" as {u}")).unwrap_or_default(),
        store.describe()
    );
    Ok(())
}

/// `columbus-mcp arcgis signout ID`.
pub fn signout_cli(id: &str) -> anyhow::Result<()> {
    let store = SecretStore::from_env();
    usable_store(&store).map_err(|e| anyhow!("{e}."))?;
    let paths = crate::config::Paths::from_env();
    let _ = forget_state(&paths.connections, id);
    if sign_out(&store, id, &paths).map_err(|e| anyhow!(e))? {
        println!(
            "Signed out of ArcGIS for {id}: its sign-in was removed from the credential store."
        );
    } else {
        println!("{id} was not signed in to ArcGIS.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn online_services_hosts() {
        assert!(online_services_host("services.arcgis.com"));
        assert!(online_services_host("services7.arcgis.com"));
        assert!(online_services_host("Services1.ArcGIS.com"));
        assert!(!online_services_host("servicesx.arcgis.com"));
        assert!(!online_services_host("tiles.arcgis.com"));
        assert!(!online_services_host("services.arcgis.com.evil.example"));
        assert!(!online_services_host("evilservices.arcgis.com"));
    }

    #[test]
    fn expiry_and_redaction() {
        let now = Utc::now();
        let t = |secs: i64| Tokens {
            access_token: "FAKE-a".into(),
            refresh_token: "FAKE-r".into(),
            expires_at: iso(now + chrono::Duration::seconds(secs)),
            portal: "https://www.arcgis.com".into(),
        };
        assert!(!t(3600).expired(now));
        assert!(t(30).expired(now));
        assert!(t(-5).expired(now));
        let mut bad = t(3600);
        bad.expires_at = "soon".into();
        assert!(bad.expired(now));
        let shown = format!("{:?}", t(3600));
        assert!(!shown.contains("FAKE-"), "{shown}");
        // F-3b: exactly these four keys.
        let v: Value = serde_json::to_value(t(1)).unwrap();
        let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["access_token", "expires_at", "portal", "refresh_token"]
        );
    }

    #[test]
    fn pkce_is_s256() {
        let (v, c) = pkce();
        assert_eq!(v.len(), 64);
        let want = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(v.as_bytes()));
        assert_eq!(c, want);
    }

    #[test]
    fn pasted_codes() {
        assert_eq!(pasted_code("  abc-DEF.123_~\n").unwrap(), "abc-DEF.123_~");
        assert!(pasted_code("").is_err());
        assert!(pasted_code("code with spaces").is_err());
        assert!(pasted_code("<script>").is_err());
    }

    /// One canned answer from a stand-in token endpoint, and the form it got.
    async fn one_answer(body: &'static str) -> (Portal, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if rest.len() >= len || n == 0 {
                        break;
                    }
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&got).to_string()
        });
        let base = Url::parse(&format!("http://127.0.0.1:{port}/portal")).unwrap();
        (
            Portal {
                base,
                host: "127.0.0.1".into(),
            },
            h,
        )
    }

    fn stored() -> Tokens {
        Tokens {
            access_token: "FAKE-old-access".into(),
            refresh_token: "FAKE-old-refresh".into(),
            expires_at: iso(Utc::now()),
            portal: "http://127.0.0.1/portal".into(),
        }
    }

    #[tokio::test]
    async fn exchange_refresh_token_stores_the_new_refresh_token() {
        let (p, h) = one_answer(
            r#"{"access_token":"FAKE-new-access","expires_in":1800,"refresh_token":"FAKE-new-refresh","refresh_token_expires_in":1209600}"#,
        )
        .await;
        let (t, secs) = exchange_refresh_token(&p, "AppId1", OOB, "svc", &stored())
            .await
            .unwrap();
        assert_eq!(secs, Some(1_209_600));
        assert_eq!(t.refresh_token, "FAKE-new-refresh");
        assert_eq!(t.access_token, "FAKE-new-access");
        let req = h.await.unwrap();
        assert!(
            req.starts_with("POST /portal/sharing/rest/oauth2/token "),
            "{req}"
        );
        assert!(req.contains("grant_type=exchange_refresh_token"), "{req}");
        assert!(req.contains("refresh_token=FAKE-old-refresh"), "{req}");
        assert!(req.contains("client_id=AppId1"), "{req}");
    }

    #[tokio::test]
    async fn a_refused_refresh_says_sign_in_again() {
        // ArcGIS's way: HTTP 200 with an error object.
        let (p, _h) = one_answer(
            r#"{"error":{"code":400,"error":"invalid_grant","error_description":"refresh_token expired","message":"refresh_token expired"}}"#,
        )
        .await;
        let e = refresh(&p, "AppId1", "svc", &stored()).await.unwrap_err();
        assert_eq!(
            e,
            "svc's ArcGIS sign-in has expired. Sign in again: columbus-mcp arcgis signin svc"
        );
    }
}
