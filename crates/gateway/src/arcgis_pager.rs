//! The shared ArcGIS REST pager (build brief Part 3, REQ-7, AF-4).
//!
//! One pager reads a feature or map service layer's `query` endpoint for
//! every caller: `registry_fetch` today, `arcgis` connections later. It
//!
//! - reads the layer's JSON first, for its object-id field, its
//!   `maxRecordCount` and whether it supports pagination;
//! - pages by `resultOffset`/`resultRecordCount`, sorted by the object-id
//!   field ascending (after any order the caller asks for), and keeps going
//!   while the server says `exceededTransferLimit`, even after an empty page;
//! - where the layer reports `advancedQueryCapabilities.supportsPagination:
//!   false` (or reports nothing, as servers before 10.3 do), asks for
//!   `returnIdsOnly` and then reads the ids, sorted ascending, in
//!   `objectIds` batches sent by POST;
//! - stops at the read limit (cutting the page that crosses it) or at the
//!   page cap, and says whether the data was finished ([`Stop`]);
//! - retries HTTP 429 at most 3 times, honouring `Retry-After`, and turns
//!   every other failure into a plain message naming the cause and the fix
//!   (R-14, Security baseline item 8).
//!
//! HTTP itself is the caller's [`Transport`], so each caller keeps its own
//! rules (registry_fetch: public addresses only, no redirects, a byte
//! budget; arcgis connections: a bearer token from the credential store).

use std::time::Duration;

use reqwest::Url;
use serde_json::Value;

/// Retries after HTTP 429 before giving up (Security baseline item 8).
pub(crate) const MAX_RETRIES: u32 = 3;
/// The longest `Retry-After` honoured; a longer one is waited this long.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// One request the pager wants sent.
pub(crate) struct HttpRequest {
    /// The URL (for a POST, without the form).
    pub url: Url,
    /// A form body: when present, send a POST with
    /// `application/x-www-form-urlencoded`.
    pub form: Option<String>,
    /// A token to send as `Authorization: Bearer` (and
    /// `X-Esri-Authorization: Bearer`). Never put in the URL or the form.
    pub bearer: Option<String>,
}

/// What came back. Redirects are the transport's business; every other
/// status is passed through for the pager to explain.
pub(crate) struct HttpReply {
    pub status: u16,
    /// `Retry-After`, when the server sent one in seconds.
    pub retry_after: Option<Duration>,
    pub body: Vec<u8>,
}

/// Sends one request. Errors are already plain sentences.
pub(crate) trait Transport {
    async fn send(&mut self, req: &HttpRequest) -> Result<HttpReply, String>;
}

/// A bounding box in the layer's query terms.
#[derive(Clone, Debug)]
pub(crate) struct Envelope {
    pub xmin: f64,
    pub ymin: f64,
    pub xmax: f64,
    pub ymax: f64,
    /// The box's spatial reference, as `inSR` takes it (a wkid such as `4326`).
    pub in_sr: String,
}

/// What to read from one layer.
#[derive(Clone, Debug)]
pub(crate) struct LayerQuery {
    /// The layer's own URL (`…/FeatureServer/3`), without `/query`.
    pub layer: Url,
    /// The where clause, already checked by the caller. `1=1` for all.
    pub where_clause: String,
    /// `outFields`: `*` or a comma-separated list.
    pub out_fields: String,
    pub envelope: Option<Envelope>,
    /// The caller's `orderByFields` (`NAME ASC, ...`). The object-id field,
    /// ascending, is always added last so the order is total.
    pub order_by: Option<String>,
    /// `f`: `geojson` or `json`.
    pub format: String,
    /// `outSR`, when the caller wants features reprojected by the server.
    pub out_sr: Option<String>,
    /// Most features to keep; the page that crosses it is cut.
    pub read_limit: u64,
    /// Features asked for per page (lowered to the layer's `maxRecordCount`).
    pub page_size: u64,
    /// Most query pages to read.
    pub max_pages: usize,
    pub bearer: Option<String>,
}

/// Why the pager stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The server said the data is finished.
    Complete,
    /// The read limit was reached with more data left (`query.read_limit`).
    ReadLimit,
    /// The page cap was reached with more data left (`fetch.truncated`).
    PageCap,
}

impl Stop {
    pub(crate) fn truncated(self) -> bool {
        self != Stop::Complete
    }
}

/// How the layer was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Offset,
    ObjectIds,
}

/// One page of features, as the caller should store it.
pub(crate) struct Page {
    /// What was asked, without any token (for provenance).
    pub request: String,
    /// The page's JSON. When the read limit cut it, re-serialized with the
    /// kept features only.
    pub body: Vec<u8>,
    /// Size and SHA-256 of the bytes the server sent.
    pub raw_bytes: usize,
    pub raw_sha256: String,
    /// Features kept from this page.
    pub features: u64,
    pub cut: bool,
}

/// The pager's summary.
#[derive(Clone, Debug)]
pub(crate) struct Paged {
    pub rows: u64,
    pub pages: usize,
    pub stop: Stop,
    pub mode: Mode,
    /// The object-id field the order rests on, when the layer has one.
    pub oid_field: Option<String>,
}

/// What the pager needs from the layer's JSON.
#[derive(Clone, Debug, Default)]
pub(crate) struct LayerInfo {
    pub oid_field: Option<String>,
    pub supports_pagination: bool,
    pub supports_order_by: bool,
    pub max_record_count: Option<u64>,
}

impl LayerInfo {
    pub(crate) fn from_json(v: &Value) -> LayerInfo {
        let oid_field = v
            .get("objectIdField")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| {
                v.get("fields")?
                    .as_array()?
                    .iter()
                    .find(|f| f.get("type").and_then(Value::as_str) == Some("esriFieldTypeOID"))?
                    .get("name")?
                    .as_str()
                    .map(String::from)
            })
            .filter(|n| valid_field(n));
        let adv = v.get("advancedQueryCapabilities");
        let flag = |k: &str| adv.and_then(|a| a.get(k)).and_then(Value::as_bool);
        LayerInfo {
            oid_field,
            supports_pagination: flag("supportsPagination").unwrap_or(false),
            supports_order_by: flag("supportsOrderBy").unwrap_or(true),
            max_record_count: v
                .get("maxRecordCount")
                .and_then(Value::as_u64)
                .filter(|n| *n > 0),
        }
    }
}

/// A field name safe to put in `orderByFields`.
fn valid_field(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn host_of(u: &Url) -> String {
    u.host_str().unwrap_or("the service").to_string()
}

fn short(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
    s.trim().to_string()
}

/// Send with the 429 rule, then explain any failure plainly; returns the
/// raw body and its JSON.
async fn send_json<T: Transport>(t: &mut T, req: &HttpRequest) -> Result<(Vec<u8>, Value), String> {
    let host = host_of(&req.url);
    let mut attempt = 0;
    loop {
        let reply = t.send(req).await?;
        let parsed: Option<Value> = serde_json::from_slice(&reply.body).ok();
        let json_code = parsed
            .as_ref()
            .and_then(|v| v.pointer("/error/code"))
            .and_then(Value::as_i64);
        if reply.status == 429 || json_code == Some(429) {
            if attempt >= MAX_RETRIES {
                return Err(format!(
                    "ArcGIS service at {host} is limiting requests (HTTP 429) and still refused after {MAX_RETRIES} retries. Wait a few minutes, then try again."
                ));
            }
            attempt += 1;
            let wait = reply
                .retry_after
                .unwrap_or(Duration::from_secs(1))
                .min(MAX_WAIT);
            tokio::time::sleep(wait).await;
            continue;
        }
        if reply.status == 401 || reply.status == 403 || matches!(json_code, Some(498 | 499)) {
            return Err(format!(
                "ArcGIS service at {host} refused the request because it needs a valid sign-in (HTTP {}{}). Sign in again, or check that the layer is shared with you.",
                reply.status,
                json_code.map(|c| format!(", code {c}")).unwrap_or_default()
            ));
        }
        if !(200..300).contains(&reply.status) {
            return Err(format!(
                "ArcGIS service at {host} answered HTTP {}. Check the address and that the service is running, then try again.",
                reply.status
            ));
        }
        let Some(v) = parsed else {
            return Err(format!(
                "ArcGIS service at {host} did not answer with JSON. Check that the address is a feature or map service layer."
            ));
        };
        if let Some(e) = v.get("error") {
            let msg = e.get("message").and_then(Value::as_str).unwrap_or("");
            return Err(format!(
                "ArcGIS service at {host} refused the query{}: \"{}\". Check the layer address, filter and fields, then try again.",
                json_code.map(|c| format!(" (code {c})")).unwrap_or_default(),
                short(msg)
            ));
        }
        return Ok((reply.body, v));
    }
}

/// Read the layer's JSON (`?f=json`).
pub(crate) async fn layer_info<T: Transport>(
    t: &mut T,
    layer: &Url,
    bearer: Option<&str>,
) -> Result<LayerInfo, String> {
    let mut u = layer.clone();
    u.set_query(None);
    u.query_pairs_mut().append_pair("f", "json");
    let req = HttpRequest {
        url: u,
        form: None,
        bearer: bearer.map(String::from),
    };
    let (_, v) = send_json(t, &req).await?;
    Ok(LayerInfo::from_json(&v))
}

fn query_url(layer: &Url) -> Url {
    let mut u = layer.clone();
    let path = u.path().trim_end_matches('/').to_string();
    u.set_path(&format!("{path}/query"));
    u.set_query(None);
    u
}

/// The parameters every query shares: filter, box, fields, format.
fn base_pairs(q: &LayerQuery) -> Vec<(String, String)> {
    let mut p = vec![
        ("where".to_string(), q.where_clause.clone()),
        ("outFields".to_string(), q.out_fields.clone()),
        ("f".to_string(), q.format.clone()),
    ];
    if let Some(sr) = &q.out_sr {
        p.push(("outSR".into(), sr.clone()));
    }
    if let Some(e) = &q.envelope {
        p.push((
            "geometry".into(),
            format!("{},{},{},{}", e.xmin, e.ymin, e.xmax, e.ymax),
        ));
        p.push(("geometryType".into(), "esriGeometryEnvelope".into()));
        p.push(("inSR".into(), e.in_sr.clone()));
        p.push(("spatialRel".into(), "esriSpatialRelIntersects".into()));
    }
    p
}

fn order_by(q: &LayerQuery, oid: Option<&str>) -> Option<String> {
    let caller = q
        .order_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (caller, oid) {
        (Some(c), Some(o)) => {
            let has = c.split(',').any(|part| {
                part.split_whitespace()
                    .next()
                    .is_some_and(|f| f.eq_ignore_ascii_case(o))
            });
            Some(if has {
                c.to_string()
            } else {
                format!("{c}, {o} ASC")
            })
        }
        (Some(c), None) => Some(c.to_string()),
        (None, Some(o)) => Some(format!("{o} ASC")),
        (None, None) => None,
    }
}

fn encode(pairs: &[(String, String)]) -> String {
    let mut u = Url::parse("http://form.invalid/").expect("static URL");
    u.query_pairs_mut().extend_pairs(pairs);
    u.query().unwrap_or_default().to_string()
}

fn exceeded(v: &Value) -> bool {
    v.get("exceededTransferLimit")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || v.pointer("/properties/exceededTransferLimit")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn feature_count(v: &Value) -> u64 {
    v.get("features")
        .and_then(Value::as_array)
        .map(|a| a.len() as u64)
        .unwrap_or(0)
}

/// Keep at most `keep` features of a page; returns the body to store.
fn cut(raw: Vec<u8>, mut v: Value, keep: u64) -> Vec<u8> {
    if let Some(a) = v.get_mut("features").and_then(Value::as_array_mut) {
        a.truncate(keep as usize);
    }
    serde_json::to_vec(&v).unwrap_or(raw)
}

/// Read a layer, handing each page to `sink` as it arrives.
pub(crate) async fn fetch_layer<T, F>(
    t: &mut T,
    q: &LayerQuery,
    mut sink: F,
) -> Result<Paged, String>
where
    T: Transport,
    F: FnMut(Page) -> Result<(), String>,
{
    let info = layer_info(t, &q.layer, q.bearer.as_deref()).await?;
    let page_size = q
        .page_size
        .min(info.max_record_count.unwrap_or(u64::MAX))
        .max(1);
    let limit = q.read_limit;
    let url = query_url(&q.layer);
    let mut rows: u64 = 0;
    let mut pages: usize = 0;

    if info.supports_pagination {
        let order = if info.supports_order_by {
            order_by(q, info.oid_field.as_deref())
        } else {
            None
        };
        let mut offset: u64 = 0;
        loop {
            if pages >= q.max_pages {
                return Ok(done(rows, pages, Stop::PageCap, Mode::Offset, &info));
            }
            let ask = page_size.min(limit - rows).max(1);
            let mut pairs = base_pairs(q);
            if let Some(o) = &order {
                pairs.push(("orderByFields".into(), o.clone()));
            }
            pairs.push(("resultOffset".into(), offset.to_string()));
            pairs.push(("resultRecordCount".into(), ask.to_string()));
            let mut u = url.clone();
            u.query_pairs_mut().extend_pairs(&pairs);
            let req = HttpRequest {
                url: u,
                form: None,
                bearer: q.bearer.clone(),
            };
            let (raw, v) = send_json(t, &req).await?;
            pages += 1;
            let got = feature_count(&v);
            let more = exceeded(&v);
            let room = limit - rows;
            let keep = got.min(room);
            let was_cut = keep < got;
            let page = Page {
                request: req.url.to_string(),
                raw_bytes: raw.len(),
                raw_sha256: columbus_shared::canon::sha256_hex(&raw),
                body: if was_cut { cut(raw, v, keep) } else { raw },
                features: keep,
                cut: was_cut,
            };
            sink(page)?;
            rows += keep;
            if was_cut || (rows >= limit && more) {
                return Ok(done(rows, pages, Stop::ReadLimit, Mode::Offset, &info));
            }
            if !more {
                return Ok(done(rows, pages, Stop::Complete, Mode::Offset, &info));
            }
            // An empty page while the server says more remain: the window
            // held nothing for this query, so move past it rather than ask
            // the same window again.
            offset += if got > 0 { got } else { ask };
        }
    }

    // No pagination: all matching ids, then batches of them, ascending.
    let mut pairs = base_pairs(q);
    pairs.retain(|(k, _)| k != "outFields" && k != "outSR" && k != "f");
    pairs.push(("returnIdsOnly".into(), "true".into()));
    pairs.push(("f".into(), "json".into()));
    let req = HttpRequest {
        url: url.clone(),
        form: Some(encode(&pairs)),
        bearer: q.bearer.clone(),
    };
    let (_, v) = send_json(t, &req).await?;
    let ids_cut = exceeded(&v);
    let mut info = info;
    if info.oid_field.is_none() {
        info.oid_field = v
            .get("objectIdFieldName")
            .and_then(Value::as_str)
            .filter(|n| valid_field(n))
            .map(String::from);
    }
    let mut ids: Vec<i64> = v
        .get("objectIds")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    let order = if info.supports_order_by {
        order_by(q, info.oid_field.as_deref())
    } else {
        None
    };
    let mut at = 0usize;
    while at < ids.len() {
        if rows >= limit {
            return Ok(done(rows, pages, Stop::ReadLimit, Mode::ObjectIds, &info));
        }
        if pages >= q.max_pages {
            return Ok(done(rows, pages, Stop::PageCap, Mode::ObjectIds, &info));
        }
        let n = (page_size.min(limit - rows) as usize).min(ids.len() - at);
        let batch = &ids[at..at + n];
        let list = batch
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut pairs = base_pairs(q);
        pairs.push(("objectIds".into(), list));
        if let Some(o) = &order {
            pairs.push(("orderByFields".into(), o.clone()));
        }
        let req = HttpRequest {
            url: url.clone(),
            form: Some(encode(&pairs)),
            bearer: q.bearer.clone(),
        };
        let (raw, v) = send_json(t, &req).await?;
        pages += 1;
        let got = feature_count(&v);
        let room = limit - rows;
        let keep = got.min(room);
        let was_cut = keep < got;
        sink(Page {
            request: format!(
                "POST {} objectIds {}..{} ({n} ids)",
                url,
                batch[0],
                batch[n - 1]
            ),
            raw_bytes: raw.len(),
            raw_sha256: columbus_shared::canon::sha256_hex(&raw),
            body: if was_cut { cut(raw, v, keep) } else { raw },
            features: keep,
            cut: was_cut,
        })?;
        rows += keep;
        at += n;
        if was_cut {
            return Ok(done(rows, pages, Stop::ReadLimit, Mode::ObjectIds, &info));
        }
    }
    let stop = if ids_cut {
        if rows >= limit {
            Stop::ReadLimit
        } else {
            Stop::PageCap
        }
    } else {
        Stop::Complete
    };
    Ok(done(rows, pages, stop, Mode::ObjectIds, &info))
}

fn done(rows: u64, pages: usize, stop: Stop, mode: Mode, info: &LayerInfo) -> Paged {
    Paged {
        rows,
        pages,
        stop,
        mode,
        oid_field: info.oid_field.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_info_reads_oid_and_paging() {
        let i = LayerInfo::from_json(&serde_json::json!({
            "fields": [{"name": "OBJECTID_1", "type": "esriFieldTypeOID"}],
            "maxRecordCount": 2000,
            "advancedQueryCapabilities": {"supportsPagination": true}
        }));
        assert_eq!(i.oid_field.as_deref(), Some("OBJECTID_1"));
        assert!(i.supports_pagination && i.supports_order_by);
        assert_eq!(i.max_record_count, Some(2000));
        // Before 10.3 there is no advancedQueryCapabilities: no paging.
        let old = LayerInfo::from_json(&serde_json::json!({"objectIdField": "FID"}));
        assert!(!old.supports_pagination);
        // A field name that could inject into orderByFields is not used.
        let bad = LayerInfo::from_json(&serde_json::json!({"objectIdField": "x desc; drop"}));
        assert!(bad.oid_field.is_none());
    }

    #[test]
    fn order_is_total_by_object_id() {
        let mut q = LayerQuery {
            layer: Url::parse("https://example.org/arcgis/rest/services/A/FeatureServer/0")
                .unwrap(),
            where_clause: "1=1".into(),
            out_fields: "*".into(),
            envelope: None,
            order_by: None,
            format: "json".into(),
            out_sr: None,
            read_limit: 10,
            page_size: 10,
            max_pages: 10,
            bearer: None,
        };
        assert_eq!(order_by(&q, Some("OID")).as_deref(), Some("OID ASC"));
        q.order_by = Some("NAME DESC".into());
        assert_eq!(
            order_by(&q, Some("OID")).as_deref(),
            Some("NAME DESC, OID ASC")
        );
        q.order_by = Some("oid DESC".into());
        assert_eq!(order_by(&q, Some("OID")).as_deref(), Some("oid DESC"));
        assert_eq!(
            query_url(&q.layer).path(),
            "/arcgis/rest/services/A/FeatureServer/0/query"
        );
    }
}
