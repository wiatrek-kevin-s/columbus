//! `registry_fetch` (design section 6): download open data a registry entry
//! describes into the workspace, as a layer the other tools can use.
//!
//! The entry is community-written, so nothing in it is trusted: its URL must
//! be HTTPS to a public address (checked after DNS, and that address is the
//! one connected to, so a name cannot be re-pointed in between), redirects
//! are not followed, sizes and feature counts are capped, and the download
//! is parsed as JSON by the engine, never handed to GDAL. Hosts the
//! organization lists in `fetch_hosts` need no approval; any other host asks
//! the person, with the URL and the entry's licence on the card.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use chrono::Local;
use columbus_shared::approvals::Risk;
use reqwest::Url;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::approval_flow::{Ask, Sought};
use crate::providers::{OutputMeta, OutputSource};
use crate::server::{
    checks_from, client_name, new_prov_id, Check, Gateway, Output, Severity, SERVER_NAME,
};

/// Access types registry_fetch can read.
const SUPPORTED: &[&str] = &["ogc-api-features", "arcgis-rest", "download"];
/// Features per request.
const PAGE: u64 = 1000;
/// Bytes downloaded per call, across pages.
const MAX_BYTES: u64 = 200 * 1024 * 1024;
const MAX_PAGES: usize = 500;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FetchArgs {
    /// The registry entry's id (from registry_search).
    pub id: String,
    /// Which of the entry's access methods to use (0-based, as registry_get
    /// lists them). Default: the first one registry_fetch supports.
    #[serde(default)]
    pub access: Option<usize>,
    /// The collection (OGC API Features) or layer number (ArcGIS REST), when
    /// the access method does not name one.
    #[serde(default)]
    pub layer: Option<String>,
    /// [min lon, min lat, max lon, max lat] in EPSG:4326: only features in
    /// this box. Strongly recommended.
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    /// Most features to keep (default and maximum: the read limit).
    #[serde(default)]
    pub limit: Option<u64>,
    /// An approval the person granted for this download.
    #[serde(default)]
    pub approval_id: Option<String>,
}

/// Addresses a community-written URL must never reach.
pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_documentation()
                || v.is_unspecified()
                || v.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // carrier-grade NAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
                || (o[0] == 198 && (18..20).contains(&o[1])) // benchmarking
                || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(m));
            }
            let s = v.segments();
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || (s[0] == 0x2001 && s[1] == 0x0db8)) // documentation
        }
    }
}

/// Test builds may fetch from plain HTTP on 127.0.0.1, so the end-to-end
/// test can run a local server. Release builds never do.
fn test_loopback() -> bool {
    cfg!(debug_assertions) && std::env::var_os("COLUMBUS_FETCH_TEST_LOOPBACK").is_some()
}

/// Check a URL and resolve its host to one public address to connect to.
async fn vet(url: &Url) -> Result<SocketAddr, String> {
    let loopback_ok = test_loopback();
    match url.scheme() {
        "https" => {}
        "http" if loopback_ok && url.host_str() == Some("127.0.0.1") => {}
        s => return Err(format!("registry_fetch downloads over https only, not {s}")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the URL carries credentials; registry_fetch is for open data".into());
    }
    let host = url.host_str().ok_or("the URL has no host")?;
    let port = url.port_or_known_default().ok_or("the URL has no port")?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .collect();
    let first = addrs
        .first()
        .copied()
        .ok_or_else(|| format!("{host} has no address"))?;
    for a in &addrs {
        let ok = public_ip(a.ip()) || (loopback_ok && a.ip().is_loopback());
        if !ok {
            return Err(format!("{host} resolves to {} , which is not a public address; registry_fetch only downloads from the internet", a.ip()));
        }
    }
    Ok(first)
}

/// One GET, pinned to the vetted address, no redirects, size-capped.
async fn get(url: &Url, budget: &mut u64) -> Result<Vec<u8>, String> {
    let addr = vet(url).await?;
    let host = url.host_str().unwrap_or_default().to_string();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(90))
        .resolve(&host, addr)
        .user_agent(concat!("columbus-mcp/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(url.clone())
        .header("Accept", "application/geo+json, application/json")
        .send()
        .await
        .map_err(|e| format!("downloading from {host}: {e}"))?;
    if resp.status().is_redirection() {
        return Err(format!(
            "{host} answered with a redirect, which registry_fetch does not follow"
        ));
    }
    if !resp.status().is_success() {
        return Err(format!("{host} answered HTTP {}", resp.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        if body.len() as u64 + chunk.len() as u64 > *budget {
            return Err(format!(
                "the download is larger than {} MB",
                MAX_BYTES / 1024 / 1024
            ));
        }
        body.extend_from_slice(&chunk);
    }
    *budget -= body.len() as u64;
    Ok(body)
}

/// The first request for an access method.
fn first_url(
    kind: &str,
    base: &str,
    layer: Option<&str>,
    bbox: Option<&[f64]>,
    page: u64,
) -> Result<Url, String> {
    let mut u =
        Url::parse(base.trim()).map_err(|e| format!("the entry's URL is not valid: {e}"))?;
    let b = bbox.map(|b| format!("{},{},{},{}", b[0], b[1], b[2], b[3]));
    match kind {
        "ogc-api-features" => {
            let path = u.path().trim_end_matches('/').to_string();
            if !path.ends_with("/items") {
                let c = layer.ok_or(
                    "this OGC API endpoint needs a collection: pass layer (the collection id)",
                )?;
                if !c
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || "-_.:".contains(ch))
                {
                    return Err("layer must be a collection id".into());
                }
                let base_path = path.trim_end_matches("/collections");
                u.set_path(&format!("{base_path}/collections/{c}/items"));
            }
            {
                let mut q = u.query_pairs_mut();
                q.append_pair("limit", &page.to_string());
                q.append_pair("f", "json");
                if let Some(b) = &b {
                    q.append_pair("bbox", b);
                }
            }
        }
        "arcgis-rest" => {
            let path = u.path().trim_end_matches('/').to_string();
            let last = path.rsplit('/').next().unwrap_or("");
            let layer_path = if last.chars().all(|c| c.is_ascii_digit()) && !last.is_empty() {
                path
            } else if last.eq_ignore_ascii_case("FeatureServer")
                || last.eq_ignore_ascii_case("MapServer")
            {
                let n =
                    layer.ok_or("this ArcGIS service needs a layer: pass layer (its number)")?;
                if !n.chars().all(|c| c.is_ascii_digit()) || n.is_empty() {
                    return Err("layer must be the layer's number".into());
                }
                format!("{path}/{n}")
            } else {
                return Err(
                    "the ArcGIS URL must end in FeatureServer, MapServer or a layer number".into(),
                );
            };
            u.set_path(&format!("{layer_path}/query"));
            u.set_query(None);
            {
                let mut q = u.query_pairs_mut();
                q.append_pair("where", "1=1");
                q.append_pair("outFields", "*");
                q.append_pair("f", "geojson");
                q.append_pair("outSR", "4326");
                q.append_pair("resultOffset", "0");
                q.append_pair("resultRecordCount", &page.to_string());
                if let Some(b) = &b {
                    q.append_pair("geometry", b);
                    q.append_pair("geometryType", "esriGeometryEnvelope");
                    q.append_pair("inSR", "4326");
                    q.append_pair("spatialRel", "esriSpatialRelIntersects");
                }
            }
        }
        "download" => {
            let p = u.path().to_ascii_lowercase();
            if !(p.ends_with(".geojson") || p.ends_with(".json")) {
                return Err("registry_fetch downloads GeoJSON files (.geojson, .json); for other formats, download through approved means".into());
            }
        }
        _ => return Err(format!("registry_fetch cannot read {kind} access yet")),
    }
    Ok(u)
}

/// The next page's URL, if any: OGC `next` links on the same host, ArcGIS
/// by offset while the server says there is more.
fn next_url(
    kind: &str,
    current: &Url,
    body: &Value,
    got: u64,
    offset: u64,
    page: u64,
) -> Option<Url> {
    match kind {
        "ogc-api-features" => {
            let href = body
                .get("links")?
                .as_array()?
                .iter()
                .find(|l| l.get("rel").and_then(Value::as_str) == Some("next"))?
                .get("href")?
                .as_str()?;
            let next = current.join(href).ok()?;
            (next.host_str() == current.host_str()
                && next.scheme() == current.scheme()
                && next.port() == current.port())
            .then_some(next)
        }
        "arcgis-rest" => {
            let more = body
                .pointer("/properties/exceededTransferLimit")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || body
                    .get("exceededTransferLimit")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            if !more || got == 0 {
                return None;
            }
            let mut next = current.clone();
            let pairs: Vec<(String, String)> = current
                .query_pairs()
                .map(|(k, v)| {
                    if k == "resultOffset" {
                        (k.into_owned(), (offset + got).to_string())
                    } else if k == "resultRecordCount" {
                        (k.into_owned(), page.to_string())
                    } else {
                        (k.into_owned(), v.into_owned())
                    }
                })
                .collect();
            next.query_pairs_mut().clear().extend_pairs(pairs);
            Some(next)
        }
        _ => None,
    }
}

#[tool_router(router = fetch_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Download open data a registry entry describes (OGC API Features, ArcGIS REST feature services, or a GeoJSON file) into the workspace as a layer, optionally only within a bbox (lon/lat). Hosts the organization pre-approved download without asking; others need the person's approval. The entry is community-written: check its licence (shown in the result) before relying on the data.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = true
        )
    )]
    async fn registry_fetch(
        &self,
        Parameters(args): Parameters<FetchArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({"id": args.id, "access": args.access, "layer": args.layer, "bbox": args.bbox, "limit": args.limit});
        let mut rec = self.gate(&ctx, "registry_fetch", &request)?;
        let started = Local::now();
        let prov = new_prov_id();
        if let Some(b) = &args.bbox {
            if b.len() != 4
                || b.iter().any(|v| !v.is_finite())
                || b[0] > b[2]
                || b[1] > b[3]
                || b[0] < -180.0
                || b[2] > 180.0
                || b[1] < -90.0
                || b[3] > 90.0
            {
                return Err(self.deny(
                    &ctx,
                    rec,
                    "bbox must be [min lon, min lat, max lon, max lat] in EPSG:4326",
                ));
            }
        }
        let (db, warn) = match self.registry_ready() {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let entry = match self
            .state
            .worker
            .core(
                "registry_get",
                json!({"db": db, "id": args.id}),
                Duration::from_secs(30),
            )
            .await
        {
            Ok(r) => r.get("entry").cloned().unwrap_or(Value::Null),
            Err(e) => return Err(self.deny(&ctx, rec, e.to_string())),
        };
        if entry.get("kind").and_then(Value::as_str) != Some("data") {
            return Err(self.deny(&ctx, rec, format!("{} is not a data entry", args.id)));
        }
        let methods: Vec<Value> = entry
            .get("access")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let supported = |m: &Value| {
            m.get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| SUPPORTED.contains(&t))
                && m.get("auth").and_then(Value::as_str).unwrap_or("none") == "none"
        };
        let (index, method) = match args.access {
            Some(i) => match methods.get(i) {
                Some(m) if supported(m) => (i, m.clone()),
                Some(m) => {
                    return Err(self.deny(&ctx, rec, format!(
                        "access {i} is {} (auth {}); registry_fetch reads {} without sign-in",
                        m.get("type").and_then(Value::as_str).unwrap_or("?"),
                        m.get("auth").and_then(Value::as_str).unwrap_or("?"),
                        SUPPORTED.join(", "))))
                }
                None => return Err(self.deny(&ctx, rec, format!("{} has {} access methods", args.id, methods.len()))),
            },
            None => match methods.iter().enumerate().find(|(_, m)| supported(m)) {
                Some((i, m)) => (i, m.clone()),
                None => return Err(self.deny(&ctx, rec, format!(
                    "{} has no access method registry_fetch can read ({} without sign-in); download it through approved means",
                    args.id, SUPPORTED.join(", ")))),
            },
        };
        let kind = method
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let base = method
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let layer = args.layer.clone().or_else(|| {
            method
                .get("index")
                .and_then(Value::as_str)
                .map(String::from)
        });
        let read_limit = self.state.policy().read_limits().max_rows();
        let cap = args.limit.unwrap_or(read_limit).clamp(1, read_limit);
        let page = PAGE.min(cap);
        let url = match first_url(&kind, &base, layer.as_deref(), args.bbox.as_deref(), page) {
            Ok(u) => u,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        if let Err(e) = vet(&url).await {
            return Err(self.deny(&ctx, rec, e));
        }
        let host = url.host_str().unwrap_or_default().to_string();

        let mut checks: Vec<Check> = warn.into_iter().collect();
        let license = entry
            .get("license")
            .and_then(Value::as_str)
            .unwrap_or("NOASSERTION")
            .to_string();
        let review = entry
            .pointer("/review/state")
            .and_then(Value::as_str)
            .unwrap_or("seeded")
            .to_string();
        if review == "seeded" {
            checks.push(Check {
                id: "registry.unreviewed".into(),
                severity: Severity::Info,
                message: "This entry is seeded and not yet reviewed by a person".into(),
                detail: None,
                fix: Some("Confirm the source, licence and terms with the publisher before relying on the data".into()),
            });
        }
        if license == "NOASSERTION" {
            checks.push(Check {
                id: "registry.license_unknown".into(),
                severity: Severity::Warn,
                message: "The entry does not state a licence".into(),
                detail: None,
                fix: Some("Find the publisher's terms before using or sharing the data".into()),
            });
        }

        // Approval unless the organization pre-approved the host.
        if !self.state.policy().fetch_host_allowed(&host) {
            let summary = format!(
                "Download {} from {host} ({}){}, licence {license}{}. Approval needed because {host} is not on the organization's list of download hosts.",
                entry.get("name").and_then(Value::as_str).unwrap_or(&args.id),
                url,
                args.bbox.as_ref().map(|b| format!(" within {b:?}")).unwrap_or_default(),
                if review == "seeded" { " (registry entry not yet reviewed)" } else { "" },
            );
            let bound = json!({"request": request, "url": url.as_str(), "entry": entry});
            let ask = Ask {
                tool: "registry_fetch",
                label: &format!("download from {host}"),
                request: &bound,
                risk: Risk::Download,
                summary,
                detail: Some(
                    json!({"url": url.as_str(), "entry": args.id, "license": license, "publisher": entry.get("publisher")}),
                ),
                approval_id: args.approval_id.as_deref(),
                every_time: false,
                code: None,
                standing: None,
                always_covers: None,
            };
            match self.seek_approval(&ctx, ask).await {
                Ok(Sought::Granted(id)) => rec.approval = Some(id),
                Ok(Sought::Declined(msg)) => return Err(self.deny(&ctx, rec, msg)),
                Ok(Sought::Pending(needed)) => {
                    rec.approval = Some(needed.id.clone());
                    self.audit(
                        &ctx,
                        columbus_shared::audit::AuditRecord {
                            decision: "approval_required".into(),
                            ..rec
                        },
                    )?;
                    return Ok(Json(Output {
                        result: json!({"status": "approval_required", "approval": needed}),
                        checks,
                        provenance: None,
                    }));
                }
                Err(e) => return Err(self.deny(&ctx, rec, e)),
            }
        }

        // Download, a page at a time, into a folder of the server's own.
        let stage = match tempfile::Builder::new().prefix("columbus-fetch-").tempdir() {
            Ok(d) => d,
            Err(e) => {
                return Err(self.deny(&ctx, rec, format!("could not make a download folder: {e}")))
            }
        };
        let mut budget = MAX_BYTES;
        let mut pages: Vec<PathBuf> = Vec::new();
        let mut fetched: Vec<Value> = Vec::new();
        let mut next = Some(url.clone());
        let mut count: u64 = 0;
        while let Some(u) = next.take() {
            let body = match get(&u, &mut budget).await {
                Ok(b) => b,
                Err(e) => return Err(self.deny(&ctx, rec, e)),
            };
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let got = parsed
                .get("features")
                .and_then(Value::as_array)
                .map(|a| a.len() as u64)
                .unwrap_or(0);
            let p = stage.path().join(format!("page-{}.json", pages.len()));
            if let Err(e) = std::fs::write(&p, &body) {
                return Err(self.deny(&ctx, rec, format!("could not save the download: {e}")));
            }
            fetched.push(json!({"url": u.as_str(), "bytes": body.len(), "sha256": columbus_shared::canon::sha256_hex(&body), "features": got}));
            pages.push(p);
            let offset = count;
            count += got;
            next = next_url(&kind, &u, &parsed, got, offset, page);
            if pages.len() >= MAX_PAGES || count >= cap {
                break;
            }
        }
        // Stopped with pages left: the layer is not the whole source.
        let more_left = next.is_some();
        let stem = match self
            .state
            .sandbox
            .resolve(&format!("results/fetch-{}-{prov}", args.id))
        {
            Ok(s) => s,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let reply = match self
            .state
            .worker
            .core(
                "geojson_layer",
                json!({"pages": pages, "out_stem": stem, "max_features": cap}),
                Duration::from_secs(300),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.deny(&ctx, rec, e.to_string())),
        };
        checks.extend(checks_from(reply.get("checks")));
        let path = match reply
            .get("output_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
        {
            Some(p) if p.starts_with(self.state.sandbox.root()) && p.is_file() => p,
            _ => return Err(self.deny(&ctx, rec, "the engine returned no layer in the workspace")),
        };
        let truncated = reply
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || more_left;
        if more_left && !checks.iter().any(|c| c.id == "fetch.truncated") {
            checks.push(Check {
                id: "fetch.truncated".into(),
                severity: Severity::Warn,
                message: format!("Stopped after {count} features; the source has more"),
                detail: None,
                fix: Some("Narrow the bbox, or raise the limit within the read limit".into()),
            });
        }
        let summary = reply.get("summary").cloned().unwrap_or(Value::Null);
        let rows = summary.get("rows").and_then(Value::as_u64);
        let inherit = crate::providers::Inheritance::default().with_step_fails(&checks, &prov);
        self.state.outputs.record(
            &path,
            OutputMeta {
                made_by: format!("registry_fetch {}", args.id),
                provenance: Some(prov.clone()),
                rows,
                source: OutputSource::Engine,
                truncated,
                inherited: inherit.fails,
                sha256: None,
            },
        );
        let record = json!({
            "id": prov,
            "created": started.to_rfc3339(),
            "finished": Local::now().to_rfc3339(),
            "user": self.state.user,
            "client": client_name(&ctx),
            "inputs": [{"registry_entry": args.id, "access": index, "type": kind, "license": license,
                        "publisher": entry.get("publisher"), "review": review, "downloads": fetched}],
            "steps": [{"tool": "registry_fetch", "bbox": args.bbox, "limit": cap}],
            "checks": checks,
            "outputs": [path],
            "server": format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
        });
        let _ = self.write_provenance(&prov, &record);
        rec.prov = Some(prov.clone());
        rec.rows = rows;
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({
                "status": "fetched",
                "layer": {"handle": self.ws_handle(&path), "summary": summary},
                "source": {"entry": args.id, "url": url.as_str(), "license": license, "publisher": entry.get("publisher")},
                "notice": "Open data from a community-written registry entry: cite the publisher and follow its licence.",
            }),
            checks,
            provenance: Some(prov),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{first_url, public_ip};

    #[test]
    fn only_public_addresses() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:10.0.0.1",
            "224.0.0.1",
        ] {
            assert!(!public_ip(bad.parse().unwrap()), "{bad}");
        }
        for ok in ["8.8.8.8", "140.211.1.1", "2606:4700::1111"] {
            assert!(public_ip(ok.parse().unwrap()), "{ok}");
        }
    }

    #[test]
    fn request_urls_are_built_not_pasted() {
        let u = first_url(
            "arcgis-rest",
            "https://example.org/arcgis/rest/services/Roads/FeatureServer",
            Some("3"),
            Some(&[-98.0, 30.0, -97.0, 31.0]),
            1000,
        )
        .unwrap();
        assert_eq!(
            u.path(),
            "/arcgis/rest/services/Roads/FeatureServer/3/query"
        );
        assert!(
            u.query().unwrap().contains("f=geojson") && u.query().unwrap().contains("outSR=4326")
        );
        assert!(first_url(
            "arcgis-rest",
            "https://example.org/arcgis/rest/services/Roads/FeatureServer",
            Some("3/../../x"),
            None,
            10
        )
        .is_err());
        let o = first_url(
            "ogc-api-features",
            "https://example.org/ogc",
            Some("roads"),
            None,
            50,
        )
        .unwrap();
        assert_eq!(o.path(), "/ogc/collections/roads/items");
        assert!(first_url(
            "ogc-api-features",
            "https://example.org/ogc",
            Some("../admin"),
            None,
            50
        )
        .is_err());
        assert!(first_url("download", "https://example.org/data.zip", None, None, 1).is_err());
    }
}
