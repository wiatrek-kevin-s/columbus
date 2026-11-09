//! `arcgis` connections (build brief Part 4, AF-1, REQ-8, REQ-9): one
//! ArcGIS feature or map service, or one layer in it (F-3), whose layers
//! and tables are datasets named per F-1b.
//!
//! - The gateway makes every HTTP request, here, with the same client as
//!   `registry_fetch`: one address per call, resolved once and pinned, no
//!   redirects, 30 s per request, and only to the connection's own service
//!   host, which policy `hosts` must list before any request (Part 4).
//! - Reads go through the shared pager (`arcgis_pager`) asking `f=json`
//!   (Esri JSON), so Z and M survive; the engine turns the pages into the
//!   workspace layer with GDAL's ESRIJSON driver (`esrijson_layer`).
//! - The `where` is rebuilt, never passed through (R-10, R-10b): the
//!   caller's text and every stored filter are parsed with the
//!   `query_features` where parser and written back out from the parse;
//!   hidden fields are refused in `where`, `order_by` and counts.
//! - `sql_select` is refused with `query.unsupported` (R-12).
//!
//! Sign-in (T-11, `arcgis_auth`): `Gateway::arcgis_bearer` reads the
//! connection's stored sign-in (`arcgis:<connection id>`, F-3b), refreshing
//! an expired access token once, and `ArcgisHttp::vouched` holds the hosts
//! the portal vouches for (R-18). A token goes only to those hosts, as a
//! header; any other host gets the request without it, and a refusal if it
//! needs one.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use columbus_shared::policy::{arcgis_url_host, Connection, EffectiveAccess, OrgPolicy};
use columbus_shared::sqlcheck::{rebuild_arcgis_where, where_names, SqlDialect};
use reqwest::Url;
use serde_json::{json, Value};

use crate::arcgis_auth::same_host;
use crate::arcgis_pager::{self, Envelope, HttpReply, HttpRequest, LayerQuery, Stop, Transport};
use crate::gis_tools::{file_stem, QueryArgs};
use crate::server::Gateway;

/// The longest any one request may take (R-14).
const TIMEOUT: Duration = Duration::from_secs(30);
/// The largest answer read from one request.
const MAX_BODY: usize = 256 * 1024 * 1024;
/// Most query pages one read asks for.
const MAX_PAGES: usize = 10_000;
/// Features per page when neither the connection nor the layer says.
const DEFAULT_PAGE: u64 = 1000;
/// How long a service's layer list is reused.
const CATALOG_TTL: Duration = Duration::from_secs(300);

pub(crate) use columbus_shared::policy::is_arcgis;

/// The refusal for `sql_select` (R-12).
pub(crate) fn sql_unsupported(conn: &Connection) -> String {
    format!(
        "query.unsupported: {} is an ArcGIS service, which takes no SQL. Fix: use query_features (fields, where, bbox, order_by, limit)",
        conn.id
    )
}

/// The service a connection points at, checked.
#[derive(Clone, Debug)]
pub(crate) struct Service {
    /// `…/FeatureServer` or `…/MapServer`.
    pub root: Url,
    /// The one layer, when the URL names one (`…/FeatureServer/3`).
    pub layer: Option<u64>,
    pub host: String,
}

impl Service {
    pub(crate) fn layer_url(&self, id: u64) -> Url {
        let mut u = self.root.clone();
        let path = u.path().trim_end_matches('/').to_string();
        u.set_path(&format!("{path}/{id}"));
        u
    }
}

/// A URL's host as the policy reads it: lower case, without a trailing
/// dot or an IPv6 address's brackets.
fn url_host(u: &Url) -> Option<String> {
    let h = u
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!h.is_empty()).then_some(h)
}

/// Parse and check a connection's `url` (F-3) with the HTTP client's own
/// parser: https (plain http only to 127.0.0.1 in test builds), no user
/// info, query or fragment, the same host the policy layer read, and that
/// host on policy `hosts`.
pub(crate) fn service(conn: &Connection, policy: &OrgPolicy) -> Result<Service, String> {
    let raw = conn.url.as_deref().unwrap_or("").trim();
    let listed = arcgis_url_host(raw).ok_or_else(|| {
        format!("{} has no usable ArcGIS address (an https URL ending in FeatureServer, MapServer or a layer number)", conn.id)
    })?;
    let url = Url::parse(raw).map_err(|e| format!("{}'s url is not valid: {e}", conn.id))?;
    let host = url_host(&url).ok_or_else(|| format!("{}'s url has no host", conn.id))?;
    let plain_http =
        url.scheme() == "http" && crate::fetch_tools::test_loopback() && host == "127.0.0.1";
    if url.scheme() != "https" && !plain_http {
        return Err(format!(
            "{}'s url must use https, not {}",
            conn.id,
            url.scheme()
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "{}'s url must be the service address alone, without user, query or fragment",
            conn.id
        ));
    }
    if !same_host(&host, &listed) {
        return Err(format!(
            "{}'s url reads as two different hosts ({listed} and {host}); write it plainly",
            conn.id
        ));
    }
    if !policy.host_allowed(&host) {
        return Err(columbus_shared::error::Denied::Host(host).to_string());
    }
    let path = url.path().trim_end_matches('/').to_string();
    let (root_path, layer) = match path.rsplit_once('/') {
        Some((before, last)) if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) => {
            let n = last
                .parse::<u64>()
                .map_err(|_| format!("{}'s layer number is too large", conn.id))?;
            (before.to_string(), Some(n))
        }
        _ => (path.clone(), None),
    };
    let last = root_path.rsplit('/').next().unwrap_or("");
    if !(last.eq_ignore_ascii_case("FeatureServer") || last.eq_ignore_ascii_case("MapServer")) {
        return Err(format!(
            "{}'s url must end in FeatureServer, MapServer or a layer number",
            conn.id
        ));
    }
    let mut root = url.clone();
    root.set_path(&root_path);
    Ok(Service { root, layer, host })
}

/// The HTTP transport for one tool call: the service's address resolved
/// once and pinned, no redirects, 30 s per request, and every request to
/// that one origin only.
pub(crate) struct ArcgisHttp {
    scheme: String,
    host: String,
    port: u16,
    client: reqwest::Client,
    /// Hosts the portal vouches for, which may receive a bearer token
    /// (R-18). Empty for a public service.
    vouched: Vec<String>,
    /// The signed-in connection's access token, for every request of this
    /// call (catalog, counts and pages). Never logged.
    pub(crate) token: Option<String>,
}

impl ArcgisHttp {
    pub(crate) async fn connect(svc: &Service) -> Result<Self, String> {
        let host = svc.host.clone();
        let port = svc
            .root
            .port_or_known_default()
            .ok_or_else(|| format!("{host} has no port"))?;
        let lookup = svc
            .root
            .host_str()
            .unwrap_or(&host)
            .trim_matches(['[', ']']);
        let addrs: Vec<SocketAddr> =
            match tokio::time::timeout(TIMEOUT, tokio::net::lookup_host((lookup, port))).await {
                Ok(Ok(a)) => a.collect(),
                Ok(Err(e)) => {
                    return Err(format!(
                        "ArcGIS service at {host} could not be found ({e}). Check the address or your network, then Test again."
                    ))
                }
                Err(_) => {
                    return Err(format!(
                        "ArcGIS service at {host} did not answer in 30 s. Check the address or your network, then Test again."
                    ))
                }
            };
        let addr = addrs.first().copied().ok_or_else(|| {
            format!("ArcGIS service at {host} has no address. Check the address or your network, then Test again.")
        })?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            .resolve(lookup, addr)
            .user_agent(concat!("columbus-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(ArcgisHttp {
            scheme: svc.root.scheme().to_string(),
            host,
            port,
            client,
            vouched: Vec::new(),
            token: None,
        })
    }

    fn same_origin(&self, u: &Url) -> bool {
        let Some(host) = url_host(u) else {
            return false;
        };
        u.scheme() == self.scheme
            && same_host(&host, &self.host)
            && u.port_or_known_default() == Some(self.port)
            && u.username().is_empty()
            && u.password().is_none()
    }
}

impl Transport for ArcgisHttp {
    async fn send(&mut self, req: &HttpRequest) -> Result<HttpReply, String> {
        let host = self.host.clone();
        if !self.same_origin(&req.url) {
            return Err(format!(
                "Columbus asks only {host} for this connection's data; another address was refused"
            ));
        }
        // R-18: a token goes only to hosts the portal vouches for. Any
        // other host gets the request without it, and a refusal if it
        // turns out to need one.
        let withheld = req.bearer.is_some() && !self.vouched.iter().any(|h| same_host(h, &host));
        let bearer = if withheld { None } else { req.bearer.as_ref() };
        let mut b = match &req.form {
            Some(form) => self
                .client
                .post(req.url.clone())
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form.clone()),
            None => self.client.get(req.url.clone()),
        };
        if let Some(t) = bearer {
            b = b
                .bearer_auth(t)
                .header("X-Esri-Authorization", format!("Bearer {t}"));
        }
        let mut resp = b.header("Accept", "application/json").send().await.map_err(|e| {
            if e.is_timeout() {
                format!("ArcGIS service at {host} did not answer in 30 s. Check the address or your network, then Test again.")
            } else {
                format!("ArcGIS service at {host} could not be reached. Check the address or your network, then Test again.")
            }
        })?;
        if resp.status().is_redirection() {
            return Err(format!(
                "ArcGIS service at {host} answered with a redirect, which Columbus does not follow. Put the address it moved to in the connection's url, then Test again."
            ));
        }
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut body = Vec::new();
        loop {
            let chunk = resp.chunk().await.map_err(|e| {
                if e.is_timeout() {
                    format!("ArcGIS service at {host} did not answer in 30 s. Check the address or your network, then Test again.")
                } else {
                    format!("ArcGIS service at {host} broke off its answer. Check your network, then try again.")
                }
            })?;
            let Some(chunk) = chunk else { break };
            if body.len() + chunk.len() > MAX_BODY {
                return Err(format!(
                    "ArcGIS service at {host} sent more than {} MB in one answer; lower the connection's page_size",
                    MAX_BODY / 1024 / 1024
                ));
            }
            body.extend_from_slice(&chunk);
        }
        if withheld && needs_sign_in(status, &body) {
            return Err(not_vouched(&host));
        }
        Ok(HttpReply {
            status,
            retry_after,
            body,
        })
    }
}

/// R-18's refusal.
pub(crate) fn not_vouched(host: &str) -> String {
    format!(
        "the portal does not vouch for {host}, so Columbus will not send it your sign-in, and {host} needs one. Fix: use the service through the portal's own host or a server the portal lists as federated, or ask your ArcGIS administrator to federate {host} with the portal"
    )
}

/// Whether an answer says the request needed a sign-in (HTTP 401 or 403,
/// or ArcGIS's code 498 or 499 in the JSON).
fn needs_sign_in(status: u16, body: &[u8]) -> bool {
    if status == 401 || status == 403 {
        return true;
    }
    body.len() < 64 * 1024
        && serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.pointer("/error/code").and_then(Value::as_i64))
            .is_some_and(|c| matches!(c, 401 | 403 | 498 | 499))
}

/// One layer or table of a service.
#[derive(Clone, Debug)]
pub(crate) struct Layer {
    /// The dataset name (F-1b).
    pub name: String,
    pub id: u64,
    pub table: bool,
    /// The layer's own JSON (`<layer>?f=json`).
    pub json: Value,
}

impl Layer {
    /// Attribute fields, as published (geometry fields left out).
    pub(crate) fn fields(&self) -> Vec<&Value> {
        self.json
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|f| f.get("type").and_then(Value::as_str) != Some("esriFieldTypeGeometry"))
            .filter(|f| f.get("name").and_then(Value::as_str).is_some())
            .collect()
    }

    pub(crate) fn field_names(&self) -> Vec<String> {
        self.fields()
            .iter()
            .filter_map(|f| f.get("name").and_then(Value::as_str).map(String::from))
            .collect()
    }

    fn flag(&self, k: &str) -> bool {
        self.json.get(k).and_then(Value::as_bool).unwrap_or(false)
    }

    pub(crate) fn has_geometry(&self) -> bool {
        !self.table
            && self
                .json
                .get("geometryType")
                .and_then(Value::as_str)
                .is_some_and(|g| !g.is_empty())
    }

    /// The layer's spatial reference: (wkid, latestWkid).
    fn wkids(&self) -> (Option<u64>, Option<u64>) {
        let sr = self
            .json
            .pointer("/extent/spatialReference")
            .or_else(|| self.json.get("sourceSpatialReference"))
            .or_else(|| self.json.get("spatialReference"));
        let get = |k: &str| sr.and_then(|s| s.get(k)).and_then(Value::as_u64);
        (get("wkid"), get("latestWkid"))
    }

    /// The wkid to state a bbox in by default (`inSR`).
    pub(crate) fn sr(&self) -> Option<u64> {
        let (w, l) = self.wkids();
        l.or(w)
    }

    fn extent(&self) -> Option<[f64; 4]> {
        let e = self.json.get("extent")?;
        let v = |k: &str| e.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
        Some([v("xmin")?, v("ymin")?, v("xmax")?, v("ymax")?])
    }

    fn geometry_type(&self) -> Option<String> {
        let t = self.json.get("geometryType").and_then(Value::as_str)?;
        let base = match t {
            "esriGeometryPoint" => "Point",
            "esriGeometryMultipoint" => "MultiPoint",
            "esriGeometryPolyline" => "LineString",
            "esriGeometryPolygon" | "esriGeometryEnvelope" => "Polygon",
            _ => "Geometry",
        };
        let zm = match (self.flag("hasZ"), self.flag("hasM")) {
            (true, true) => " ZM",
            (true, false) => " Z",
            (false, true) => " M",
            _ => "",
        };
        Some(format!("{base}{zm}"))
    }

    /// The dataset as `list_datasets` and `describe_dataset` show it, with
    /// every field (the gateway removes hidden ones after).
    pub(crate) fn entry(&self) -> Value {
        let (wkid, latest) = self.wkids();
        let fields: Vec<Value> = self
            .fields()
            .iter()
            .map(|f| {
                let mut o = json!({
                    "name": f.get("name"),
                    "type": f.get("type"),
                    "alias": f.get("alias"),
                    "pk": f.get("type").and_then(Value::as_str) == Some("esriFieldTypeOID"),
                });
                if let Some(n) = f.get("length").and_then(Value::as_u64) {
                    o["length"] = json!(n);
                }
                if let Some(d) = f.get("domain").filter(|d| {
                    d.get("type").and_then(Value::as_str) == Some("codedValue")
                }) {
                    o["domain"] = json!({
                        "name": d.get("name"),
                        "coded_values": d.get("codedValues").and_then(Value::as_array).map(|a| {
                            a.iter().map(|c| json!({"code": c.get("code"), "name": c.get("name")})).collect::<Vec<_>>()
                        }),
                    });
                } else if let Some(d) = f.get("domain").filter(|d| !d.is_null()) {
                    o["domain"] = json!({"name": d.get("name"), "type": d.get("type"), "range": d.get("range")});
                }
                o
            })
            .collect();
        let geom = self.has_geometry();
        json!({
            "name": self.name,
            "type": if self.table { "table" } else { "layer" },
            "layer_id": self.id,
            "title": self.json.get("name"),
            "description": self.json.get("description").and_then(Value::as_str).filter(|d| !d.trim().is_empty()),
            "geometry_column": geom.then_some("geometry"),
            "geometry_type": if geom { self.geometry_type() } else { None },
            "esri_geometry_type": if geom { self.json.get("geometryType").cloned() } else { None },
            "has_z": geom && self.flag("hasZ"),
            "has_m": geom && self.flag("hasM"),
            "crs": if geom { json!({"code": self.sr().map(|w| format!("EPSG:{w}")), "wkid": wkid, "latest_wkid": latest}) } else { Value::Null },
            "extent": if geom { self.extent().map(|e| json!(e)) } else { None },
            "object_id_field": self.json.get("objectIdField"),
            "max_record_count": self.json.get("maxRecordCount"),
            "fields": fields,
        })
    }
}

/// F-1b: each layer and table under its published name; where two share a
/// name (compared case-insensitively), `<name>_<layer id>` for each.
pub(crate) fn name_layers(raw: Vec<(u64, String, bool, Value)>) -> Vec<Layer> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, n, _, _) in &raw {
        *counts.entry(n.to_lowercase()).or_default() += 1;
    }
    raw.into_iter()
        .map(|(id, name, table, json)| {
            let dup = counts.get(&name.to_lowercase()).copied().unwrap_or(0) > 1;
            Layer {
                name: if dup { format!("{name}_{id}") } else { name },
                id,
                table,
                json,
            }
        })
        .collect()
}

fn usable_name(n: &str) -> bool {
    !n.trim().is_empty() && n.len() <= 200 && !n.chars().any(char::is_control)
}

fn layer_of(v: &Value, table: bool) -> Option<(u64, String, bool, Value)> {
    let id = v.get("id").and_then(Value::as_u64)?;
    let name = v.get("name").and_then(Value::as_str)?.trim().to_string();
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    if !usable_name(&name) || matches!(kind, "Group Layer" | "Raster Layer") {
        return None;
    }
    Some((id, name, table || kind == "Table", v.clone()))
}

/// Every layer and table of the service (or the one layer its URL names).
pub(crate) async fn catalog(t: &mut ArcgisHttp, svc: &Service) -> Result<Vec<Layer>, String> {
    let token = t.token.clone();
    let get = |u: Url| HttpRequest {
        url: u,
        form: None,
        bearer: token.clone(),
    };
    let mut raw = Vec::new();
    if let Some(id) = svc.layer {
        let mut u = svc.layer_url(id);
        u.query_pairs_mut().append_pair("f", "json");
        let (_, v) = arcgis_pager::send_json(t, &get(u)).await?;
        let mut v = v;
        if v.get("id").is_none() {
            v["id"] = json!(id);
        }
        if let Some(l) = layer_of(&v, false) {
            raw.push(l);
        }
    } else {
        let mut u = svc.root.clone();
        let path = u.path().trim_end_matches('/').to_string();
        u.set_path(&format!("{path}/layers"));
        u.query_pairs_mut().append_pair("f", "json");
        let (_, v) = arcgis_pager::send_json(t, &get(u)).await?;
        for (key, table) in [("layers", false), ("tables", true)] {
            for l in v.get(key).and_then(Value::as_array).into_iter().flatten() {
                if let Some(l) = layer_of(l, table) {
                    raw.push(l);
                }
            }
        }
    }
    Ok(name_layers(raw))
}

/// Rows matching `where_clause` (`returnCountOnly`).
pub(crate) async fn count(
    t: &mut ArcgisHttp,
    layer: &Url,
    where_clause: &str,
) -> Result<u64, String> {
    let mut u = layer.clone();
    let path = u.path().trim_end_matches('/').to_string();
    u.set_path(&format!("{path}/query"));
    u.query_pairs_mut()
        .append_pair("where", where_clause)
        .append_pair("returnCountOnly", "true")
        .append_pair("f", "json");
    let bearer = t.token.clone();
    let (_, v) = arcgis_pager::send_json(
        t,
        &HttpRequest {
            url: u,
            form: None,
            bearer,
        },
    )
    .await?;
    v.get("count").and_then(Value::as_u64).ok_or_else(|| {
        format!(
            "ArcGIS service at {} did not say how many rows match. Check the layer address, then try again.",
            layer.host_str().unwrap_or("the service")
        )
    })
}

/// A layer's entry with the name of a hidden object-id field left out
/// (the fields themselves are filtered where every source's are).
fn visible_entry(layer: &Layer, access: &EffectiveAccess) -> Value {
    let mut e = layer.entry();
    if e["object_id_field"]
        .as_str()
        .is_some_and(|o| access.field_hidden(o))
    {
        e["object_id_field"] = Value::Null;
    }
    e
}

/// Layer lists by (settings generation, connection id, service URL).
type Catalogs = Mutex<HashMap<(u64, String, String), (Instant, Vec<Layer>)>>;

fn catalog_cache() -> &'static Catalogs {
    static C: OnceLock<Catalogs> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The filters on a dataset, each rebuilt from its parsed form against the
/// layer's fields (R-10), in parentheses and ANDed; `None` when none apply.
fn rebuilt_filters(
    access: &EffectiveAccess,
    layer: &Layer,
    dataset: &str,
) -> Result<Vec<String>, String> {
    let fields = layer.field_names();
    access
        .filters_for(dataset)
        .iter()
        .map(|f| {
            rebuild_arcgis_where(f, &fields, &|_| false)
                .map(|w| w.sql)
                .map_err(|e| format!("the server's row filter can't be applied to {dataset}: {e}"))
        })
        .collect()
}

/// A bbox and its CRS, checked without the layer: the `inSR` the caller
/// named, if any.
fn check_bbox(q: &QueryArgs) -> Result<Option<String>, String> {
    if let Some(b) = &q.bbox {
        if b.len() != 4 || b.iter().any(|v| !v.is_finite()) || b[0] > b[2] || b[1] > b[3] {
            return Err("bbox must be [min x, min y, max x, max y]".into());
        }
    }
    match &q.bbox_crs {
        None => Ok(None),
        Some(c) => c
            .strip_prefix("EPSG:")
            .filter(|n| !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit()))
            .map(|n| Some(n.to_string()))
            .ok_or_else(|| "bbox_crs must be an EPSG code such as \"EPSG:4326\"".into()),
    }
}

fn and_all(parts: &[String]) -> String {
    if parts.is_empty() {
        "1=1".into()
    } else {
        parts
            .iter()
            .map(|p| format!("({p})"))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

/// What one read of an ArcGIS layer produced, for `gis_tools::Read`.
pub(crate) struct ArcgisRead {
    pub reply: Value,
    pub executed: String,
    pub path: Option<PathBuf>,
    pub rows_read: u64,
    pub hit_limit: bool,
    pub info: Value,
    pub filters: Vec<String>,
    pub max_rows: u64,
    pub hidden: Vec<String>,
    /// For provenance: the layer, the request and every page (no tokens).
    pub record: Value,
}

impl Gateway {
    /// The bearer token for `conn` and the portal it is for: none for a
    /// public service (`auth: none`). Read from the credential store
    /// (`arcgis:<connection id>`, F-3b); an expired access token is
    /// refreshed once without asking; a lapsed sign-in gives "Sign in
    /// again: columbus-mcp arcgis signin <id>".
    pub(crate) async fn arcgis_bearer(
        &self,
        conn: &Connection,
    ) -> Result<Option<(String, crate::arcgis_auth::Portal)>, String> {
        crate::arcgis_auth::bearer(&self.state.secrets, conn, &self.state.policy()).await
    }

    /// Check the connection and open its transport, before any request:
    /// with the token, and the hosts the portal vouches for, when the
    /// connection signs in.
    async fn arcgis_open(&self, conn: &Connection) -> Result<(Service, ArcgisHttp), String> {
        let svc = service(conn, &self.state.policy())?;
        let auth = self.arcgis_bearer(conn).await?;
        let mut http = ArcgisHttp::connect(&svc).await?;
        if let Some((token, portal)) = auth {
            http.vouched = crate::arcgis_auth::vouched(&portal, &token, &svc.host).await;
            http.token = Some(token);
        }
        Ok((svc, http))
    }

    /// The service's layers and tables (cached for a few minutes).
    async fn arcgis_layers(
        &self,
        conn: &Connection,
        refresh: bool,
    ) -> Result<(Service, ArcgisHttp, Vec<Layer>), String> {
        let (svc, mut http) = self.arcgis_open(conn).await?;
        let key = (
            self.state.generation(),
            conn.id.clone(),
            svc.root.to_string(),
        );
        if !refresh {
            if let Some((at, layers)) = catalog_cache().lock().expect("catalog").get(&key) {
                if at.elapsed() < CATALOG_TTL {
                    return Ok((svc, http, layers.clone()));
                }
            }
        }
        let layers = catalog(&mut http, &svc).await?;
        catalog_cache()
            .lock()
            .expect("catalog")
            .insert(key, (Instant::now(), layers.clone()));
        Ok((svc, http, layers))
    }

    /// `list_datasets` for an `arcgis` connection: every layer and table
    /// with all its fields (the caller filters by policy).
    pub(crate) async fn arcgis_list(
        &self,
        access: &EffectiveAccess,
        conn: &Connection,
    ) -> Result<Vec<Value>, String> {
        let (_, _, layers) = self.arcgis_layers(conn, true).await?;
        Ok(layers.iter().map(|l| visible_entry(l, access)).collect())
    }

    async fn arcgis_find(
        &self,
        conn: &Connection,
        dataset: &str,
        refresh: bool,
    ) -> Result<(Service, ArcgisHttp, Layer), String> {
        let (svc, http, layers) = self.arcgis_layers(conn, refresh).await?;
        let found = layers
            .iter()
            .find(|l| l.name == dataset)
            .or_else(|| layers.iter().find(|l| l.name.eq_ignore_ascii_case(dataset)))
            .cloned()
            .ok_or_else(|| format!("no dataset {dataset} on this connection; see list_datasets"))?;
        Ok((svc, http, found))
    }

    /// `describe_dataset`'s answer for an ArcGIS layer: its fields,
    /// domains, geometry type, `latestWkid` and extent from the layer's
    /// JSON, and the rows the row filter allows (`returnCountOnly`).
    pub(crate) async fn arcgis_info(
        &self,
        access: &EffectiveAccess,
        conn: &Connection,
        dataset: &str,
        refresh: bool,
    ) -> Result<Value, String> {
        let (svc, mut http, layer) = self.arcgis_find(conn, dataset, refresh).await?;
        let filters = rebuilt_filters(access, &layer, dataset)?;
        let rows = count(&mut http, &svc.layer_url(layer.id), &and_all(&filters)).await?;
        let mut info = visible_entry(&layer, access);
        info["rows"] = json!(rows);
        if layer.has_geometry() {
            // CRS facts and checks from the engine, when it is there.
            let facts = self
                .state
                .worker
                .core(
                    "crs_describe",
                    json!({"code": info["crs"]["code"], "extent": info["extent"]}),
                    Duration::from_secs(60),
                )
                .await;
            if let Ok(f) = facts {
                if let Some(c) = f.get("crs").filter(|c| !c.is_null()) {
                    let mut crs = c.clone();
                    crs["wkid"] = info["crs"]["wkid"].clone();
                    crs["latest_wkid"] = info["crs"]["latest_wkid"].clone();
                    info["crs"] = crs;
                }
                info["checks"] = f.get("checks").cloned().unwrap_or(json!([]));
            }
        }
        Ok(info)
    }

    /// One governed read of an ArcGIS layer: the request built from the
    /// parsed arguments, paged by the shared pager, the pages trimmed to
    /// the allowed fields and turned into a workspace layer by the engine.
    pub(crate) async fn arcgis_read(
        &self,
        access: &EffectiveAccess,
        conn: &Connection,
        q: &QueryArgs,
        stem: Option<&str>,
        return_rows: u64,
        limit: Option<u64>,
    ) -> Result<ArcgisRead, String> {
        let dataset = q.dataset.as_str();
        if !usable_name(dataset) {
            return Err(format!("{dataset:?} is not a dataset name"));
        }
        // Everything that can be refused without the layer is refused
        // before any request (R-10b): a where outside the parser's subset,
        // and a hidden field in fields, where or order_by.
        let hidden_msg = |f: &str| {
            format!("{f} is hidden by organization policy, so it can't be read, filtered or sorted")
        };
        if let Some(w) = q.where_.as_deref().filter(|w| !w.trim().is_empty()) {
            let names = where_names(w, SqlDialect::Generic).map_err(|e| e.to_string())?;
            if let Some(h) = names.iter().find(|n| access.field_hidden(n)) {
                return Err(format!("query refused: {}", hidden_msg(h)));
            }
        }
        for f in q.fields.iter().flatten().chain(q.order_by.iter()) {
            if access.field_hidden(f.trim()) {
                return Err(hidden_msg(f.trim()));
            }
        }
        check_bbox(q)?;
        let info = self.dataset_info(access, conn, dataset, false).await?;
        let (svc, mut http, layer) = self.arcgis_find(conn, dataset, false).await?;
        let all = layer.field_names();
        let visible: Vec<String> = all
            .iter()
            .filter(|f| !access.field_hidden(f))
            .cloned()
            .collect();
        let hidden_names: Vec<String> = all
            .iter()
            .filter(|f| access.field_hidden(f))
            .cloned()
            .collect();
        let hidden = |n: &str| access.field_hidden(n);
        let find = |f: &str| -> Result<String, String> {
            if access.field_hidden(f) {
                return Err(format!(
                    "{f} is hidden by organization policy, so it can't be read, filtered or sorted"
                ));
            }
            visible
                .iter()
                .find(|k| k.as_str() == f)
                .or_else(|| visible.iter().find(|k| k.eq_ignore_ascii_case(f)))
                .cloned()
                .ok_or_else(|| format!("{dataset} has no field {f:?}; see describe_dataset"))
        };
        // Fields: by name, never `*`; the geometry comes with the layer.
        let mut out_fields: Vec<String> = Vec::new();
        match &q.fields {
            Some(fs) if !fs.is_empty() => {
                for f in fs {
                    if f.eq_ignore_ascii_case("geometry") && layer.has_geometry() {
                        continue;
                    }
                    let name = find(f)?;
                    if !out_fields.contains(&name) {
                        out_fields.push(name);
                    }
                }
            }
            _ => out_fields = visible.clone(),
        }
        if out_fields.is_empty() && !layer.has_geometry() {
            return Err(format!("{dataset} has no fields you are allowed to read"));
        }
        // The where: stored filters and the caller's, each rebuilt (R-10).
        let filters = rebuilt_filters(access, &layer, dataset)?;
        let mut parts = filters.clone();
        if let Some(w) = q.where_.as_deref().filter(|w| !w.trim().is_empty()) {
            let rebuilt = rebuild_arcgis_where(w, &all, &hidden).map_err(|e| e.to_string())?;
            parts.push(rebuilt.sql);
        }
        let where_clause = and_all(&parts);
        let order_by = match &q.order_by {
            Some(o) => Some(format!("{} ASC", find(o.trim())?)),
            None => None,
        };
        let envelope = match &q.bbox {
            None => None,
            Some(b) => {
                if !layer.has_geometry() {
                    return Err(format!("{dataset} has no geometry, so bbox cannot apply"));
                }
                let in_sr = match check_bbox(q)? {
                    Some(n) => n,
                    None => layer
                        .sr()
                        .map(|w| w.to_string())
                        .ok_or("this layer does not state its spatial reference; give bbox_crs")?,
                };
                Some(Envelope {
                    xmin: b[0],
                    ymin: b[1],
                    xmax: b[2],
                    ymax: b[3],
                    in_sr,
                })
            }
        };
        let read_limit = access.limits.max_rows();
        let max_rows = limit.unwrap_or(read_limit).min(read_limit).max(1);
        let page_size = conn
            .page_size
            .or_else(|| layer.json.get("maxRecordCount").and_then(Value::as_u64))
            .unwrap_or(DEFAULT_PAGE)
            .max(1);
        let layer_url = svc.layer_url(layer.id);
        let lq = LayerQuery {
            layer: layer_url.clone(),
            where_clause: where_clause.clone(),
            out_fields: if out_fields.is_empty() {
                // A geometry-only read still names a field, never `*`.
                layer
                    .json
                    .get("objectIdField")
                    .and_then(Value::as_str)
                    .filter(|o| !access.field_hidden(o))
                    .unwrap_or("")
                    .to_string()
            } else {
                out_fields.join(",")
            },
            envelope,
            order_by: order_by.clone(),
            format: "json".into(),
            out_sr: None,
            return_zm: layer.flag("hasZ") || layer.flag("hasM"),
            read_limit: max_rows,
            page_size: page_size.min(max_rows),
            max_pages: MAX_PAGES,
            bearer: http.token.clone(),
        };

        // The pages, trimmed to the fields asked for, in a folder of the
        // session workspace that goes when the read ends.
        let stage = tempfile::Builder::new()
            .prefix(".arcgis-")
            .tempdir_in(self.state.sandbox.root())
            .map_err(|e| format!("could not make a folder for the pages: {e}"))?;
        let keep: std::collections::BTreeSet<String> = out_fields.iter().cloned().collect();
        let mut pages: Vec<PathBuf> = Vec::new();
        let mut fetched: Vec<Value> = Vec::new();
        let dir = stage.path().to_path_buf();
        let paged = arcgis_pager::fetch_layer(&mut http, &lq, |mut pg| {
            if let Some(fs) = pg.json.get_mut("features").and_then(Value::as_array_mut) {
                for f in fs {
                    if let Some(a) = f.get_mut("attributes").and_then(Value::as_object_mut) {
                        a.retain(|k, _| keep.contains(k));
                    }
                }
            }
            if let Some(fs) = pg.json.get_mut("fields").and_then(Value::as_array_mut) {
                fs.retain(|f| {
                    f.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| keep.contains(n))
                });
            }
            let p = dir.join(format!("page-{}.json", pages.len()));
            let body = serde_json::to_vec(&pg.json).map_err(|e| e.to_string())?;
            std::fs::write(&p, body).map_err(|e| format!("could not save a page: {e}"))?;
            fetched.push(
                json!({"request": pg.request, "bytes": pg.raw_bytes, "sha256": pg.raw_sha256,
                                "features": pg.features, "cut": pg.cut}),
            );
            pages.push(p);
            Ok(())
        })
        .await?;
        let output_stem = match stem {
            Some(s) => Some(self.state.sandbox.resolve(s).map_err(|e| e.to_string())?),
            None => None,
        };
        let timeout = access.limits.max_seconds();
        let mut reply = self
            .state
            .worker
            .core(
                "esrijson_layer",
                json!({
                    "pages": pages,
                    "out_stem": output_stem,
                    "layer_name": file_stem(dataset),
                    "fields": out_fields,
                    "hide_fields": self.state.policy().hide_fields,
                    "geometry": layer.has_geometry(),
                    "return_rows": return_rows.min(access.return_rows),
                }),
                Duration::from_secs(timeout + 60),
            )
            .await
            .map_err(|e| e.to_string())?;
        drop(stage);
        let path = reply
            .get("output_path")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        if let Some(p) = &path {
            let ok = output_stem.as_ref().is_some_and(|base| {
                p.starts_with(self.state.sandbox.root())
                    && p.to_string_lossy().starts_with(&*base.to_string_lossy())
            });
            if !ok {
                return Err("the engine wrote outside the workspace; result discarded".into());
            }
        }
        let hit_limit = paged.stop != Stop::Complete;
        if let Some(o) = reply.as_object_mut() {
            o.insert("rows_read".into(), json!(paged.rows));
            o.insert("hit_read_limit".into(), json!(hit_limit));
        }
        let mut executed = format!(
            "ArcGIS query on {dataset} (layer {}): where {where_clause}",
            layer.id
        );
        executed.push_str(&format!("; outFields {}", lq.out_fields));
        if let Some(o) = &order_by {
            executed.push_str(&format!("; orderByFields {o}"));
        }
        if let Some(e) = &lq.envelope {
            executed.push_str(&format!(
                "; envelope {},{},{},{} inSR {}",
                e.xmin, e.ymin, e.xmax, e.ymax, e.in_sr
            ));
        }
        let record = json!({
            "layer": layer_url.as_str(),
            "where": where_clause,
            "out_fields": lq.out_fields,
            "order_by": order_by,
            "envelope": lq.envelope.as_ref().map(|e| json!([e.xmin, e.ymin, e.xmax, e.ymax, e.in_sr])),
            "paging": {
                "mode": match paged.mode { arcgis_pager::Mode::Offset => "offset", arcgis_pager::Mode::ObjectIds => "object_ids" },
                "order": paged.oid_field.as_ref().map(|f| format!("{f} ASC")),
                "pages": paged.pages,
                "stop": match paged.stop { Stop::Complete => "complete", Stop::ReadLimit => "read_limit", Stop::PageCap => "page_cap" },
            },
            "pages": fetched,
        });
        Ok(ArcgisRead {
            reply,
            executed,
            path,
            rows_read: paged.rows,
            hit_limit,
            info,
            filters,
            max_rows,
            hidden: hidden_names,
            record,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_names_get_the_layer_id() {
        let l = name_layers(vec![
            (0, "Roadways".into(), false, json!({})),
            (1, "Signs".into(), false, json!({})),
            (3, "signs".into(), false, json!({})),
            (4, "Inspections".into(), true, json!({})),
        ]);
        let names: Vec<&str> = l.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Roadways", "Signs_1", "signs_3", "Inspections"]);
    }

    #[test]
    fn filters_are_parenthesized_and_anded() {
        assert_eq!(and_all(&[]), "1=1");
        assert_eq!(
            and_all(&["DISTRICT = 14".into(), "LANES > 2".into()]),
            "(DISTRICT = 14) AND (LANES > 2)"
        );
    }
}
