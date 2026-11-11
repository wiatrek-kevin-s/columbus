//! `map_view`: what the person's map shows, and steering it (D-18, REQ-40).
//!
//! The person's map is the companion page (`app/ui`). While it is on show
//! it tells this server what it shows: the extent, the zoom, which of the
//! session's layers are on, the active one, the selected feature, whether
//! the table is open and whether the street background is on
//! (`POST /api/view`, with the page's session token, every second and when
//! something changes). The server keeps the latest report of each open page
//! in memory. A map counts as open while a page that is on show reported
//! within the last few seconds; with several pages open, the person's map
//! is the one they used last.
//!
//! The assistant reads that with `map_view`, and changes it with the same
//! tool: the change waits here, the page takes it with the answer to its
//! next report, applies it with its own switches and zoom, and reports the
//! new view, which the tool returns. With no map open the tool says so and
//! nothing is changed (a change no page took in time is dropped). A window
//! that is being opened while the change waits (the app opens its panel for
//! one) gets a little longer to load and take it.
//!
//! Only what [`Report`] lists is taken from a page, and every part is
//! checked: numbers in range, handles that name this session's own
//! workspace files. Anything else in a report refuses the whole report. So
//! nothing a page knows about anything else on the map (the person's web
//! map layers: their names, sources, addresses, or whether they are on;
//! D-14, REQ-34) can reach the assistant through this tool, whatever a page
//! sends. The selected feature's attributes are not taken from the page
//! either: the server reads that one row of the session's own layer itself,
//! as `read_output` does.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::providers::OutputSource;
use crate::server::{Gateway, Output};

/// A page that reported this recently counts as open.
pub(crate) const FRESH: Duration = Duration::from_secs(5);
/// How long a change waits for the page to take it and report the new view.
const CHANGE_WAIT: Duration = Duration::from_secs(8);
/// How long it waits in all while a window of the session is being opened
/// and no page has taken it yet: a page needs a moment to load.
const CHANGE_WAIT_OPENING: Duration = Duration::from_secs(20);
/// A window counts as being opened for this long after its page was asked
/// for, or until a new page reports.
const OPENING: Duration = Duration::from_secs(15);
/// A page not heard from for this long is forgotten.
const FORGET: Duration = Duration::from_secs(120);
/// Largest report taken, in bytes.
pub(crate) const MAX_REPORT: usize = 96 * 1024;
/// Most layers a report may list as on, and the longest handle.
const MAX_LAYERS: usize = 300;
const MAX_HANDLE: usize = 200;
/// Most pages of one session remembered at a time.
const MAX_PAGES: usize = 32;
/// Most layers one change may turn on, or off.
const MAX_NAMED: usize = 50;
/// The longest a page may say it has been left alone (a week).
const MAX_IDLE_MS: u64 = 7 * 24 * 3600 * 1000;

/// What a page says about itself and its map. Unknown keys refuse the
/// whole report.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    /// The page is in front of the person: its window is visible and its
    /// map has a size.
    showing: bool,
    /// Milliseconds since the person last used this page (or it came on show).
    #[serde(default)]
    idle_ms: u64,
    /// The last change (by its number) this page has finished applying.
    #[serde(default)]
    applied: u64,
    /// The parts of that change the page could not do.
    #[serde(default)]
    not_done: Vec<String>,
    #[serde(default)]
    view: Option<ReportView>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportView {
    /// West, south, east, north in EPSG:4326: the part of the map nothing
    /// covers. East passes 180 when the view crosses the 180th meridian.
    extent: [f64; 4],
    zoom: f64,
    /// The session's layers that are on, top first.
    #[serde(default)]
    layers: Vec<String>,
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    selected: Option<ReportSelected>,
    #[serde(default)]
    table: bool,
    #[serde(default)]
    streets: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportSelected {
    layer: String,
    /// The feature's id on the map: its row in the layer, from 0.
    feature: u64,
}

/// What one page's map shows, as checked when its report came in.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub extent: [f64; 4],
    pub zoom: f64,
    pub layers: Vec<String>,
    pub active: Option<String>,
    pub selected: Option<(String, u64)>,
    pub table: bool,
    pub streets: bool,
}

/// A part of a change, as the page names the ones it could not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Show,
    Hide,
    Active,
    ZoomTo,
}

impl Part {
    fn parse(s: &str) -> Option<Part> {
        match s {
            "show" => Some(Part::Show),
            "hide" => Some(Part::Hide),
            "active" => Some(Part::Active),
            "zoom_to" => Some(Part::ZoomTo),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Part::Show => "show",
            Part::Hide => "hide",
            Part::Active => "active",
            Part::ZoomTo => "zoom_to",
        }
    }

    /// Why the page may not have done it, in this server's own words (a
    /// page's words are never passed on).
    fn why(self) -> &'static str {
        match self {
            Part::Show => {
                "the map could not draw one of the layers named: it does not list it, or its data could not be read or placed on the map (the map's list says why)"
            }
            Part::Hide | Part::Active => {
                "the map does not list one of the outputs named (its file may have changed since it was made)"
            }
            Part::ZoomTo => {
                "the map had nothing to zoom to: the layer has no geometry it can draw, or it took too long to load"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Zoom {
    Layer(String),
    Extent([f64; 4]),
}

/// What the assistant asked the map to change.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Change {
    pub show: Vec<String>,
    pub hide: Vec<String>,
    pub active: Option<String>,
    pub zoom_to: Option<Zoom>,
}

impl Change {
    fn to_json(&self, seq: u64) -> Value {
        json!({
            "seq": seq,
            "show": self.show,
            "hide": self.hide,
            "active": self.active,
            "zoom_to": match &self.zoom_to {
                Some(Zoom::Layer(h)) => json!(h),
                Some(Zoom::Extent(e)) => json!(e),
                None => Value::Null,
            },
        })
    }
}

/// The view of the person's map, and how old the report is.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub view: View,
    pub age: Duration,
}

/// How a change ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The page applied it and reported the new view.
    Changed { seen: Seen, not_done: Vec<Part> },
    /// A page took it, but did not report the new view in time.
    NotConfirmed { seen: Option<Seen> },
    /// No page on show took it: nothing was changed.
    NotOpen,
}

struct Page {
    first_seen: Instant,
    last_seen: Instant,
    /// When the person last used it, as the page says.
    active_at: Instant,
    showing: bool,
    applied: u64,
    not_done: Vec<Part>,
    view: Option<View>,
}

struct Queued {
    seq: u64,
    change: Change,
    /// The page the change was handed to, once one took it.
    taken_by: Option<String>,
}

#[derive(Default)]
struct Inner {
    pages: HashMap<String, Page>,
    seq: u64,
    queued: Option<Queued>,
    /// When a window of the session was last asked for (the app took a
    /// link for its panel, or a page signed in) and has not reported yet.
    opening: Option<Instant>,
}

impl Inner {
    fn fresh(p: &Page, now: Instant) -> bool {
        p.showing && p.view.is_some() && now.saturating_duration_since(p.last_seen) < FRESH
    }

    /// The person's map: of the pages on show that reported lately, the one
    /// they used last (the newest, when none was used).
    fn current(&self, now: Instant) -> Option<&String> {
        self.pages
            .iter()
            .filter(|(_, p)| Self::fresh(p, now))
            .max_by_key(|(_, p)| (p.active_at, p.first_seen))
            .map(|(k, _)| k)
    }

    fn seen(p: &Page, now: Instant) -> Option<Seen> {
        p.view.clone().map(|view| Seen {
            view,
            age: now.saturating_duration_since(p.last_seen),
        })
    }
}

/// The session's open maps: the latest report of each page, and the change
/// waiting for one of them.
#[derive(Default)]
pub struct MapViews {
    inner: Mutex<Inner>,
    /// Woken by every report, for a change that waits.
    news: tokio::sync::Notify,
    /// One change at a time.
    one: tokio::sync::Mutex<()>,
}

/// A string that could be one of this session's workspace handles. Whether
/// it names a file the map lists is checked when the assistant asks.
fn plausible_handle(session: &str, h: &str) -> bool {
    h.len() <= MAX_HANDLE
        && h.strip_prefix(crate::providers::HANDLE_SCHEME)
            .and_then(|rest| rest.strip_prefix(session))
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
        && !h.chars().any(char::is_control)
}

/// West, south, east, north in degrees, as a page reports its view and as
/// the assistant names an extent to zoom to.
fn check_extent(e: &[f64; 4]) -> Result<(), String> {
    let [w, s, east, n] = *e;
    let ok = e.iter().all(|v| v.is_finite())
        && (-180.0..=180.0).contains(&w)
        && east > w
        && east <= w + 360.0
        && s >= -90.0
        && n <= 90.0
        && n > s;
    if ok {
        Ok(())
    } else {
        Err("an extent is [west, south, east, north] in EPSG:4326 (degrees): west from -180 to 180, east greater than west and at most 360 more, south less than north, both from -90 to 90".into())
    }
}

impl ReportView {
    fn checked(&self, session: &str) -> Result<View, String> {
        check_extent(&self.extent)?;
        if !(self.zoom.is_finite() && (0.0..=30.0).contains(&self.zoom)) {
            return Err("zoom is a number from 0 to 30".into());
        }
        if self.layers.len() > MAX_LAYERS {
            return Err(format!("at most {MAX_LAYERS} layers"));
        }
        let handle = |h: &String| {
            if plausible_handle(session, h) {
                Ok(h.clone())
            } else {
                Err(
                    "a layer is named by its workspace handle in this session, and by nothing else"
                        .to_string(),
                )
            }
        };
        Ok(View {
            extent: self.extent,
            zoom: self.zoom,
            layers: self.layers.iter().map(handle).collect::<Result<_, _>>()?,
            active: self.active.as_ref().map(handle).transpose()?,
            selected: self
                .selected
                .as_ref()
                .map(|s| handle(&s.layer).map(|h| (h, s.feature)))
                .transpose()?,
            table: self.table,
            streets: self.streets,
        })
    }
}

impl MapViews {
    /// A page's report. Returns the answer for the page: the change it is
    /// to apply, if one waits for it. An `Err` is a report this server does
    /// not take (it keeps what it had).
    pub fn report(
        &self,
        session: &str,
        page: &str,
        body: &[u8],
        now: Instant,
    ) -> Result<Value, String> {
        let r: Report =
            serde_json::from_slice(body).map_err(|e| format!("not a view report: {e}"))?;
        // Checked whether or not it is kept: a report is taken whole or not
        // at all.
        let view = r.view.as_ref().map(|v| v.checked(session)).transpose()?;
        let not_done = r
            .not_done
            .iter()
            .map(|s| Part::parse(s).ok_or_else(|| "not_done names parts of a change".to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        if not_done.len() > 4 {
            return Err("not_done names parts of a change".into());
        }
        let idle = Duration::from_millis(r.idle_ms.min(MAX_IDLE_MS));
        let command = {
            let mut g = self.inner.lock().expect("map views");
            g.pages
                .retain(|_, p| now.saturating_duration_since(p.last_seen) < FORGET);
            let known = g.pages.contains_key(page);
            if g.pages.len() >= MAX_PAGES && !known {
                return Err("too many windows of this session are open".into());
            }
            if !known {
                // The window that was being opened is here.
                g.opening = None;
            }
            let p = g.pages.entry(page.to_string()).or_insert(Page {
                first_seen: now,
                last_seen: now,
                active_at: now,
                showing: false,
                applied: 0,
                not_done: Vec::new(),
                view: None,
            });
            p.last_seen = now;
            p.active_at = now.checked_sub(idle).unwrap_or(now);
            // A page that is not on show has no view to give.
            p.showing = r.showing && view.is_some();
            p.view = if r.showing { view } else { None };
            p.applied = r.applied;
            p.not_done = not_done;
            // A change goes to the person's map only, and only until that
            // page says it has applied it.
            let current = g.current(now).is_some_and(|k| k == page);
            match &mut g.queued {
                Some(q) if current && r.applied < q.seq => {
                    q.taken_by = Some(page.to_string());
                    Some(q.change.to_json(q.seq))
                }
                _ => None,
            }
        };
        self.news.notify_waiters();
        Ok(json!({ "command": command }))
    }

    /// A window of the session is being opened (its page was asked for at
    /// `at`): a change that finds no map yet waits a little longer for it.
    pub fn opening(&self, at: Instant) {
        self.inner.lock().expect("map views").opening = Some(at);
    }

    /// What the person's map shows now; None when no map is open.
    pub fn current(&self, now: Instant) -> Option<Seen> {
        let g = self.inner.lock().expect("map views");
        g.current(now)
            .and_then(|k| g.pages.get(k))
            .and_then(|p| Inner::seen(p, now))
    }

    /// Hand a change to the person's map and wait, at most `wait`, for the
    /// page to report the view it made; at most `wait_opening` in all when
    /// no page has taken it by then and a window is being opened.
    pub async fn change(&self, change: Change, wait: Duration, wait_opening: Duration) -> Outcome {
        let _one = self.one.lock().await;
        let seq = {
            let mut g = self.inner.lock().expect("map views");
            g.seq += 1;
            let seq = g.seq;
            g.queued = Some(Queued {
                seq,
                change,
                taken_by: None,
            });
            seq
        };
        let start = tokio::time::Instant::now();
        let mut deadline = start + wait;
        let last = start + wait.max(wait_opening);
        loop {
            // Listening before looking: a report that comes between the
            // look and the wait still wakes this.
            let news = self.news.notified();
            tokio::pin!(news);
            news.as_mut().enable();
            {
                let mut g = self.inner.lock().expect("map views");
                let now = Instant::now();
                let taker = g.queued.as_ref().and_then(|q| q.taken_by.clone());
                let done = taker
                    .and_then(|k| g.pages.get(&k))
                    .filter(|p| p.applied >= seq && Inner::fresh(p, now))
                    .and_then(|p| Inner::seen(p, now).map(|seen| (seen, p.not_done.clone())));
                if let Some((seen, not_done)) = done {
                    g.queued = None;
                    return Outcome::Changed { seen, not_done };
                }
            }
            if tokio::time::timeout_at(deadline, news).await.is_err() {
                // Out of time. A window being opened right now (the app
                // opens its panel when the assistant changes the view) has
                // had no chance to take the change yet: wait for it.
                let opening = {
                    let g = self.inner.lock().expect("map views");
                    g.queued.as_ref().is_some_and(|q| q.taken_by.is_none())
                        && g.opening.is_some_and(|t| t.elapsed() < OPENING)
                };
                if opening && deadline < last {
                    deadline = last;
                    continue;
                }
                break;
            }
        }
        // Not confirmed in time. A change no page took is dropped, so it
        // cannot move a map that opens later; one a page took may still be
        // applied, and the answer says so.
        let mut g = self.inner.lock().expect("map views");
        let now = Instant::now();
        match g.queued.take().and_then(|q| q.taken_by) {
            None => Outcome::NotOpen,
            Some(k) => Outcome::NotConfirmed {
                seen: g
                    .pages
                    .get(&k)
                    .filter(|p| Inner::fresh(p, now))
                    .and_then(|p| Inner::seen(p, now)),
            },
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MapViewArgs {
    /// Layers to turn on: workspace handles (ws://…) of this session's
    /// layers, at most 50.
    #[serde(default)]
    pub show: Option<Vec<String>>,
    /// Layers to turn off (handles), at most 50.
    #[serde(default)]
    pub hide: Option<Vec<String>>,
    /// The result to make the active one (a handle): it is drawn in the
    /// highlight colour, and its rows are the ones the table shows.
    #[serde(default)]
    pub active: Option<String>,
    /// Where to point the map: a layer's handle (the layer is turned on
    /// and fitted in view), or an extent [west, south, east, north] in
    /// EPSG:4326 degrees.
    #[serde(default)]
    #[schemars(extend("type" = ["string", "array", "null"]))]
    pub zoom_to: Option<Value>,
}

const NOT_OPEN: &str = "No map is open for this session right now. The person sees the map in the Columbus app, in the panel on the right of the chat (the Map button), or in the session's own window.";

const NOTE: &str = "extent is [west, south, east, north] in EPSG:4326: the part of the person's map that nothing covers (east passes 180 when the view crosses the 180th meridian). layers_on lists this session's layers that are on, the top one first. selected is the feature the person picked: feature is its row in the layer, counted from 0 (read_output's offset). Only this session's own layers are reported: anything else the person has put on their map is theirs alone and is never included.";

impl Gateway {
    /// One of the files the person's map lists (as `Companion::outputs`
    /// does: a plain file in the session's results, written by a Columbus
    /// tool, an approved plugin or an approved script): its path, and
    /// whether it is a layer (a GeoPackage) rather than a table.
    pub(crate) fn on_map_list(&self, handle: &str) -> Result<(PathBuf, bool), String> {
        let s = &self.state;
        let path = crate::providers::resolve_handle(&s.session, &s.sandbox, handle)?;
        let listed = ["results", "external"]
            .iter()
            .any(|d| path.parent() == Some(s.sandbox.root().join(d).as_path()));
        let ext = path
            .extension()
            .map(|x| x.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let ours = s
            .outputs
            .get(&path)
            .is_some_and(|m| m.source != OutputSource::Provider);
        if !listed || !ours || !matches!(ext.as_str(), "gpkg" | "csv") {
            return Err(format!(
                "{handle} is not a layer or table the map lists (the map shows the layers and tables this session's Columbus tools made)"
            ));
        }
        Ok((path, ext == "gpkg"))
    }

    fn map_layer(&self, handle: &str, what: &str) -> Result<String, String> {
        match self.on_map_list(handle)? {
            (_, true) => Ok(handle.to_string()),
            (_, false) => Err(format!(
                "{what}: {handle} is a table without geometry; the map draws layers only (active can name a table)"
            )),
        }
    }

    /// The change the arguments ask for; None when they ask for none (a read).
    fn map_change(&self, args: &MapViewArgs) -> Result<Option<Change>, String> {
        let list = |v: &Option<Vec<String>>, what: &str| -> Result<Vec<String>, String> {
            let v = v.as_deref().unwrap_or_default();
            if v.len() > MAX_NAMED {
                return Err(format!("{what}: at most {MAX_NAMED} layers at a time"));
            }
            let mut out: Vec<String> = Vec::new();
            for h in v {
                let h = self.map_layer(h, what)?;
                if !out.contains(&h) {
                    out.push(h);
                }
            }
            Ok(out)
        };
        let show = list(&args.show, "show")?;
        let hide = list(&args.hide, "hide")?;
        if let Some(h) = show.iter().find(|h| hide.contains(h)) {
            return Err(format!("{h} is in both show and hide"));
        }
        let active = match &args.active {
            Some(h) => {
                self.on_map_list(h).map_err(|e| format!("active: {e}"))?;
                Some(h.clone())
            }
            None => None,
        };
        let zoom_to = match &args.zoom_to {
            None | Some(Value::Null) => None,
            Some(Value::String(h)) => {
                let h = self.map_layer(h, "zoom_to")?;
                if hide.contains(&h) {
                    return Err(format!(
                        "zoom_to names {h}, which hide turns off; zooming to a layer turns it on"
                    ));
                }
                Some(Zoom::Layer(h))
            }
            Some(v @ Value::Array(_)) => {
                let e: [f64; 4] = serde_json::from_value(v.clone()).map_err(|_| {
                    "zoom_to: an extent is four numbers, [west, south, east, north] in EPSG:4326"
                        .to_string()
                })?;
                check_extent(&e).map_err(|e| format!("zoom_to: {e}"))?;
                Some(Zoom::Extent(e))
            }
            Some(_) => {
                return Err(
                    "zoom_to is a layer's handle (ws://…) or an extent [west, south, east, north] in EPSG:4326"
                        .into(),
                )
            }
        };
        let change = Change {
            show,
            hide,
            active,
            zoom_to,
        };
        Ok((change != Change::default()).then_some(change))
    }

    /// A view as the assistant gets it: only files the map lists, and the
    /// selected feature's attributes read here, from the session's own
    /// layer. The audit record gets what `read_output` would record for
    /// that row.
    async fn map_view_json(
        &self,
        seen: &Seen,
        rec: &mut columbus_shared::audit::AuditRecord,
    ) -> Value {
        let v = &seen.view;
        let layer = |h: &String| matches!(self.on_map_list(h), Ok((_, true)));
        let listed = |h: &String| self.on_map_list(h).is_ok();
        let mut selected = Value::Null;
        if let Some((h, feature)) = v.selected.as_ref().filter(|(h, _)| layer(h)) {
            selected = json!({"layer": h, "feature": feature});
            match self.selected_row(h, *feature).await {
                Ok(Some((attributes, geometry, path, prov))) => {
                    selected["attributes"] = attributes;
                    selected["geometry"] = geometry;
                    rec.rows = Some(1);
                    rec.prov = prov;
                    rec.labels = self.state.flow.file_labels(&path).into_iter().collect();
                }
                Ok(None) => {}
                Err(why) => selected["attributes_note"] = json!(why),
            }
        }
        json!({
            "crs": "EPSG:4326",
            "extent": v.extent,
            "zoom": (v.zoom * 100.0).round() / 100.0,
            "layers_on": v.layers.iter().filter(|h| layer(h)).collect::<Vec<_>>(),
            "active": v.active.as_ref().filter(|h| listed(h)),
            "selected": selected,
            "table_open": v.table,
            "street_background": v.streets,
        })
    }

    /// The selected feature's row, read as `read_output` reads one:
    /// attributes by field name, and the geometry as its type and bounding
    /// box. `Ok(None)`: this session may not be given rows; `Err`: why the
    /// row could not be read.
    async fn selected_row(
        &self,
        handle: &str,
        feature: u64,
    ) -> Result<Option<(Value, Value, PathBuf, Option<String>)>, String> {
        let policy = self.state.policy();
        if !policy.tool_allowed("read_output") || policy.return_rows == Some(0) {
            return Ok(None);
        }
        let (path, meta) = self.own_output(handle)?;
        let reply = self
            .state
            .worker
            .core(
                "read_rows",
                json!({"path": path, "offset": feature, "limit": 1}),
                Duration::from_secs(30),
            )
            .await
            .map_err(|e| format!("the row could not be read: {e}"))?;
        let columns = reply["columns"].as_array().cloned().unwrap_or_default();
        let row = reply["rows"]
            .get(0)
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "the layer has no such row".to_string())?;
        let mut attributes = serde_json::Map::new();
        let mut geometry = Value::Null;
        for (c, value) in columns.iter().zip(row) {
            let name = c.as_str().unwrap_or_default();
            // The geometry comes last, as its type and bounding box.
            if value.get("type").is_some() && value.get("bbox").is_some() {
                geometry = value;
            } else {
                attributes.insert(name.to_string(), value);
            }
        }
        Ok(Some((
            Value::Object(attributes),
            geometry,
            path,
            meta.provenance,
        )))
    }
}

#[tool_router(router = map_view_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "See what the person's map shows, and point it at a result. With no arguments it returns what they are looking at now: the extent ([west, south, east, north] in EPSG:4326), the zoom, which of this session's layers are on, the active layer, the feature they selected (its layer, its row and its attributes), whether the table is open and whether the street background is on; or it says that no map is open. With arguments it changes their map, then returns the new view: show and hide (lists of ws:// handles of this session's layers), active (a handle: that result's rows are the ones the table shows), zoom_to (a layer's handle, which also turns that layer on, or an extent [west, south, east, north] in EPSG:4326). Use it to look at what the person is looking at before you answer a question about \"this\" or \"here\", and to show them a result (turn it on and zoom to it) when you have made one. It changes only what is shown, never data, so it needs no approval; the person can move the map again at any time. With no map open nothing is changed. It reports this session's own layers only: anything else the person has put on their map is theirs alone and is never included. For a picture of the same view, call map_snapshot with extent: \"view\".",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn map_view(
        &self,
        Parameters(args): Parameters<MapViewArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({"show": args.show, "hide": args.hide, "active": args.active, "zoom_to": args.zoom_to});
        let mut rec = self.gate(&ctx, "map_view", &request)?;
        let change = match self.map_change(&args) {
            Ok(c) => c,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let views = &self.state.map_views;
        let result = match change {
            None => match views.current(Instant::now()) {
                Some(seen) => {
                    rec.reason = Some("read the person's map view".into());
                    json!({
                        "status": "open",
                        "view": self.map_view_json(&seen, &mut rec).await,
                        "seen_seconds_ago": tenths(seen.age),
                        "note": NOTE,
                    })
                }
                None => {
                    rec.reason = Some("no map is open".into());
                    json!({"status": "not_open", "message": NOT_OPEN})
                }
            },
            Some(change) => {
                let asked = change.to_json(0);
                let asked = json!({"show": asked["show"], "hide": asked["hide"], "active": asked["active"], "zoom_to": asked["zoom_to"]});
                match views.change(change, CHANGE_WAIT, CHANGE_WAIT_OPENING).await {
                    Outcome::Changed { seen, not_done } => {
                        rec.reason = Some(if not_done.is_empty() {
                            "changed the person's map view".into()
                        } else {
                            format!(
                                "changed the person's map view, but not: {}",
                                not_done
                                    .iter()
                                    .map(|p| p.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        });
                        let mut r = json!({
                            "status": if not_done.is_empty() { "changed" } else { "partly_changed" },
                            "changed": true,
                            "asked": asked,
                            "view": self.map_view_json(&seen, &mut rec).await,
                            "seen_seconds_ago": tenths(seen.age),
                            "note": NOTE,
                        });
                        if !not_done.is_empty() {
                            r["not_done"] = not_done
                                .iter()
                                .map(|p| json!({"part": p.as_str(), "why": p.why()}))
                                .collect();
                        }
                        r
                    }
                    Outcome::NotConfirmed { seen } => {
                        rec.reason = Some(
                            "the map took the change but did not report the new view in time"
                                .into(),
                        );
                        let mut r = json!({
                            "status": "not_confirmed",
                            "asked": asked,
                            "message": "The person's map took the change, but did not report the new view in time. Call map_view with no arguments to read what it shows now.",
                        });
                        if let Some(seen) = seen {
                            r["view"] = self.map_view_json(&seen, &mut rec).await;
                            r["seen_seconds_ago"] = json!(tenths(seen.age));
                            r["note"] = json!(NOTE);
                        }
                        r
                    }
                    Outcome::NotOpen => {
                        rec.reason = Some("no map is open; nothing was changed".into());
                        json!({
                            "status": "not_open",
                            "changed": false,
                            "asked": asked,
                            "message": format!("Nothing was changed. {NOT_OPEN}"),
                        })
                    }
                }
            }
        };
        self.audit(&ctx, rec)?;
        Ok(crate::server::out(result))
    }
}

fn tenths(d: Duration) -> f64 {
    (d.as_secs_f64() * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "20261111-000000-abc123";

    fn h(name: &str) -> String {
        format!("ws://{S}/results/{name}.gpkg")
    }

    fn report(showing: bool, idle_ms: u64, applied: u64, layers: &[String]) -> Vec<u8> {
        json!({
            "showing": showing,
            "idle_ms": idle_ms,
            "applied": applied,
            "view": {
                "extent": [-97.9, 30.1, -97.6, 30.4],
                "zoom": 11.5,
                "layers": layers,
                "active": layers.first(),
                "selected": layers.first().map(|l| json!({"layer": l, "feature": 2})),
                "table": true,
                "streets": false,
            },
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_report_is_kept_and_read_while_it_is_fresh() {
        let v = MapViews::default();
        let t0 = Instant::now();
        assert_eq!(v.current(t0), None, "no page has reported: no map is open");
        let a = v
            .report(S, "page-a", &report(true, 0, 0, &[h("a")]), t0)
            .unwrap();
        assert_eq!(a, json!({"command": null}));
        let seen = v.current(t0 + Duration::from_secs(1)).expect("open");
        assert_eq!(seen.view.extent, [-97.9, 30.1, -97.6, 30.4]);
        assert_eq!(seen.view.layers, vec![h("a")]);
        assert_eq!(seen.view.active, Some(h("a")));
        assert_eq!(seen.view.selected, Some((h("a"), 2)));
        assert!(seen.view.table && !seen.view.streets);
        assert_eq!(seen.age, Duration::from_secs(1));
        // A page that stops reporting is no longer open.
        assert_eq!(v.current(t0 + FRESH), None);
    }

    #[test]
    fn a_page_that_is_not_on_show_is_not_the_persons_map() {
        let v = MapViews::default();
        let t0 = Instant::now();
        v.report(S, "page-a", &report(true, 0, 0, &[h("a")]), t0)
            .unwrap();
        assert!(v.current(t0).is_some());
        // Hidden (its panel closed, its tab in the background): it says so.
        let hidden = json!({"showing": false, "idle_ms": 0, "applied": 0}).to_string();
        v.report(S, "page-a", hidden.as_bytes(), t0 + Duration::from_secs(1))
            .unwrap();
        assert_eq!(v.current(t0 + Duration::from_secs(1)), None);
        // Not on show, whatever view it sends along.
        v.report(
            S,
            "page-a",
            &report(false, 0, 0, &[h("a")]),
            t0 + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(v.current(t0 + Duration::from_secs(2)), None);
    }

    #[test]
    fn with_several_pages_the_one_used_last_is_the_persons_map() {
        let v = MapViews::default();
        let t0 = Instant::now() + Duration::from_secs(3600);
        // A was opened first and used a minute ago; B opened later, unused
        // since (it came on show 10 s ago).
        v.report(S, "a", &report(true, 60_000, 0, &[h("a")]), t0)
            .unwrap();
        v.report(S, "b", &report(true, 10_000, 0, &[h("b")]), t0)
            .unwrap();
        assert_eq!(v.current(t0).unwrap().view.layers, vec![h("b")]);
        // The person clicks in A.
        v.report(
            S,
            "a",
            &report(true, 200, 0, &[h("a")]),
            t0 + Duration::from_secs(1),
        )
        .unwrap();
        v.report(
            S,
            "b",
            &report(true, 11_000, 0, &[h("b")]),
            t0 + Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            v.current(t0 + Duration::from_secs(1)).unwrap().view.layers,
            vec![h("a")]
        );
        // A goes to the background: B is what they see.
        let hidden = json!({"showing": false}).to_string();
        v.report(S, "a", hidden.as_bytes(), t0 + Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            v.current(t0 + Duration::from_secs(2)).unwrap().view.layers,
            vec![h("b")]
        );
    }

    #[test]
    fn only_what_a_report_lists_is_taken() {
        let v = MapViews::default();
        let now = Instant::now();
        let good = |change: &dyn Fn(&mut Value)| {
            let mut r: Value = serde_json::from_slice(&report(true, 0, 0, &[h("a")])).unwrap();
            change(&mut r);
            r.to_string().into_bytes()
        };
        // A key this server does not know refuses the whole report: web
        // layers, their sources or anything else cannot ride along.
        for bad in [
            good(
                &|r| r["web_layers"] = json!([{"id": "wl-xyz", "title": "County imagery", "url": "https://imagery.example.gov/wmts"}]),
            ),
            good(&|r| r["view"]["web"] = json!(["wl-xyz"])),
            good(&|r| r["view"]["basemap"] = json!("web:wl-xyz")),
            good(&|r| r["view"]["selected"]["attributes"] = json!({"OWNER": "someone"})),
            // A layer is a handle of this session: not a web layer's id, an
            // address, a path or another session's file.
            good(&|r| r["view"]["layers"] = json!(["wl-xyz"])),
            good(&|r| r["view"]["layers"] = json!(["https://imagery.example.gov/wmts"])),
            good(&|r| r["view"]["active"] = json!("ws://another-session/results/a.gpkg")),
            good(&|r| r["view"]["active"] = json!(format!("ws://{S}"))),
            good(&|r| r["view"]["selected"]["layer"] = json!("County imagery")),
            good(&|r| {
                r["view"]["layers"] = json!([format!("ws://{S}/results/{}.gpkg", "x".repeat(300))])
            }),
            // Numbers out of range.
            good(&|r| r["view"]["extent"] = json!([-97.6, 30.1, -97.9, 30.4])),
            good(&|r| r["view"]["extent"] = json!([-97.9, 30.4, -97.6, 30.1])),
            good(&|r| r["view"]["extent"] = json!([-200.0, 30.1, -97.6, 30.4])),
            good(&|r| r["view"]["extent"] = json!([-97.9, 30.1, 300.0, 30.4])),
            good(&|r| r["view"]["extent"] = json!([-97.9, -91.0, -97.6, 30.4])),
            good(&|r| r["view"]["extent"] = json!([-97.9, 30.1, -97.6])),
            good(&|r| r["view"]["zoom"] = json!(99)),
            good(&|r| r["view"]["selected"]["feature"] = json!(-1)),
            good(&|r| r["not_done"] = json!(["https://imagery.example.gov/wmts"])),
            b"not json".to_vec(),
        ] {
            let text = String::from_utf8_lossy(&bad).into_owned();
            assert!(v.report(S, "p", &bad, now).is_err(), "taken: {text}");
            assert_eq!(v.current(now), None, "kept something of: {text}");
        }
        // The same report without the extras is taken.
        assert!(v.report(S, "p", &good(&|_| {}), now).is_ok());
        assert!(v.current(now).is_some());
        // A view across the 180th meridian: east passes 180.
        let across = good(&|r| r["view"]["extent"] = json!([170.0, 50.0, 190.0, 54.0]));
        assert!(v.report(S, "p", &across, now).is_ok());
        assert_eq!(v.current(now).unwrap().view.extent[2], 190.0);
    }

    #[test]
    fn many_windows_of_a_session_are_refused_past_a_cap() {
        let v = MapViews::default();
        let now = Instant::now();
        for i in 0..MAX_PAGES {
            v.report(S, &format!("p{i}"), &report(true, 0, 0, &[]), now)
                .unwrap();
        }
        assert!(v
            .report(S, "one-more", &report(true, 0, 0, &[]), now)
            .is_err());
        // Known pages still report; forgotten ones make room.
        assert!(v.report(S, "p0", &report(true, 0, 0, &[]), now).is_ok());
        assert!(v
            .report(S, "one-more", &report(true, 0, 0, &[]), now + FORGET)
            .is_ok());
    }

    #[tokio::test]
    async fn a_change_goes_to_the_persons_map_and_comes_back_as_its_new_view() {
        let v = std::sync::Arc::new(MapViews::default());
        v.report(S, "old", &report(true, 5_000, 0, &[h("a")]), Instant::now())
            .unwrap();
        v.report(S, "used", &report(true, 100, 0, &[h("a")]), Instant::now())
            .unwrap();
        let change = Change {
            show: vec![h("b")],
            zoom_to: Some(Zoom::Layer(h("b"))),
            ..Default::default()
        };
        let asking = {
            let v = v.clone();
            let change = change.clone();
            tokio::spawn(async move {
                v.change(change, Duration::from_secs(5), Duration::from_secs(5))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The page the person used last gets it; another window does not.
        let other = v
            .report(S, "old", &report(true, 5_050, 0, &[h("a")]), Instant::now())
            .unwrap();
        assert_eq!(other["command"], Value::Null);
        let got = v
            .report(S, "used", &report(true, 150, 0, &[h("a")]), Instant::now())
            .unwrap();
        assert_eq!(
            got["command"],
            json!({"seq": 1, "show": [h("b")], "hide": [], "active": null, "zoom_to": h("b")})
        );
        // Until it says it is done, the tool keeps waiting (and the page is
        // told again, which it ignores by the number).
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!asking.is_finished());
        let again = v
            .report(S, "used", &report(true, 200, 0, &[h("a")]), Instant::now())
            .unwrap();
        assert_eq!(again["command"]["seq"], 1);
        // Applied: the new view comes back, and nothing waits any more.
        let done = v
            .report(
                S,
                "used",
                &report(true, 250, 1, &[h("b"), h("a")]),
                Instant::now(),
            )
            .unwrap();
        assert_eq!(done["command"], Value::Null);
        match asking.await.unwrap() {
            Outcome::Changed { seen, not_done } => {
                assert_eq!(seen.view.layers, vec![h("b"), h("a")]);
                assert!(not_done.is_empty());
            }
            other => panic!("{other:?}"),
        }
        let later = v
            .report(
                S,
                "used",
                &report(true, 300, 1, &[h("b"), h("a")]),
                Instant::now(),
            )
            .unwrap();
        assert_eq!(later["command"], Value::Null);
    }

    #[tokio::test]
    async fn the_page_says_which_parts_it_could_not_do() {
        let v = std::sync::Arc::new(MapViews::default());
        v.report(S, "p", &report(true, 0, 0, &[h("a")]), Instant::now())
            .unwrap();
        let asking = {
            let v = v.clone();
            tokio::spawn(async move {
                v.change(
                    Change {
                        zoom_to: Some(Zoom::Extent([-98.0, 30.0, -97.0, 31.0])),
                        ..Default::default()
                    },
                    Duration::from_secs(5),
                    Duration::from_secs(5),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let got = v
            .report(S, "p", &report(true, 0, 0, &[h("a")]), Instant::now())
            .unwrap();
        assert_eq!(got["command"]["zoom_to"], json!([-98.0, 30.0, -97.0, 31.0]));
        let mut r: Value = serde_json::from_slice(&report(true, 0, 1, &[h("a")])).unwrap();
        r["not_done"] = json!(["zoom_to"]);
        v.report(S, "p", r.to_string().as_bytes(), Instant::now())
            .unwrap();
        match asking.await.unwrap() {
            Outcome::Changed { not_done, .. } => assert_eq!(not_done, vec![Part::ZoomTo]),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn with_no_map_open_a_change_is_dropped() {
        let v = std::sync::Arc::new(MapViews::default());
        // A page that is not on show (its panel is closed) reports, and is
        // not given the change.
        let hidden = json!({"showing": false, "applied": 0}).to_string();
        let asking = {
            let v = v.clone();
            tokio::spawn(async move {
                v.change(
                    Change {
                        show: vec![h("b")],
                        ..Default::default()
                    },
                    Duration::from_millis(300),
                    Duration::from_millis(300),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let got = v.report(S, "p", hidden.as_bytes(), Instant::now()).unwrap();
        assert_eq!(got["command"], Value::Null);
        assert_eq!(asking.await.unwrap(), Outcome::NotOpen);
        // A map that opens afterwards is not moved by the old change.
        let got = v
            .report(S, "p", &report(true, 0, 0, &[h("a")]), Instant::now())
            .unwrap();
        assert_eq!(got["command"], Value::Null);
    }

    #[tokio::test]
    async fn a_change_taken_but_not_confirmed_says_so() {
        let v = std::sync::Arc::new(MapViews::default());
        v.report(S, "p", &report(true, 0, 0, &[h("a")]), Instant::now())
            .unwrap();
        let asking = {
            let v = v.clone();
            tokio::spawn(async move {
                v.change(
                    Change {
                        hide: vec![h("a")],
                        ..Default::default()
                    },
                    Duration::from_millis(400),
                    Duration::from_millis(400),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let got = v
            .report(S, "p", &report(true, 0, 0, &[h("a")]), Instant::now())
            .unwrap();
        assert_eq!(got["command"]["hide"], json!([h("a")]));
        match asking.await.unwrap() {
            Outcome::NotConfirmed { seen } => assert!(seen.is_some()),
            other => panic!("{other:?}"),
        }
        // The next change gets the next number.
        let asking = {
            let v = v.clone();
            tokio::spawn(async move {
                v.change(
                    Change {
                        show: vec![h("a")],
                        ..Default::default()
                    },
                    Duration::from_millis(400),
                    Duration::from_millis(400),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let got = v
            .report(S, "p", &report(true, 0, 1, &[]), Instant::now())
            .unwrap();
        assert_eq!(got["command"]["seq"], 2);
        asking.abort();
    }

    #[tokio::test]
    async fn a_window_being_opened_gets_longer_to_take_a_change() {
        let change = || Change {
            zoom_to: Some(Zoom::Extent([-98.0, 30.0, -97.0, 31.0])),
            ..Default::default()
        };
        let v = std::sync::Arc::new(MapViews::default());
        // The app asked for its panel's page just now (it opens the panel
        // when the assistant changes the view): no page has reported yet.
        v.opening(Instant::now());
        let asking = {
            let v = v.clone();
            tokio::spawn(async move {
                v.change(change(), Duration::from_millis(300), Duration::from_secs(4))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert!(
            !asking.is_finished(),
            "past the usual wait, it waits for the window being opened"
        );
        // The page has loaded: its first report gets the change.
        let got = v
            .report(S, "p", &report(true, 0, 0, &[]), Instant::now())
            .unwrap();
        assert_eq!(got["command"]["seq"], 1);
        v.report(S, "p", &report(true, 0, 1, &[]), Instant::now())
            .unwrap();
        assert!(matches!(asking.await.unwrap(), Outcome::Changed { .. }));

        // A window that never loads: the change is dropped at the longer wait.
        let v = MapViews::default();
        v.opening(Instant::now());
        let t = Instant::now();
        let out = v
            .change(
                change(),
                Duration::from_millis(200),
                Duration::from_millis(700),
            )
            .await;
        assert_eq!(out, Outcome::NotOpen);
        assert!(
            t.elapsed() >= Duration::from_millis(650) && t.elapsed() < Duration::from_secs(3),
            "{:?}",
            t.elapsed()
        );
        // A window asked for long ago is not being opened any more, and
        // one whose page has reported (here: not on show) is not either.
        let v = MapViews::default();
        if let Some(long_ago) = Instant::now().checked_sub(OPENING + Duration::from_secs(1)) {
            v.opening(long_ago);
            let t = Instant::now();
            let out = v
                .change(change(), Duration::from_millis(200), Duration::from_secs(4))
                .await;
            assert_eq!(out, Outcome::NotOpen);
            assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        }
        v.opening(Instant::now());
        let hidden = json!({"showing": false}).to_string();
        v.report(S, "p", hidden.as_bytes(), Instant::now()).unwrap();
        let t = Instant::now();
        let out = v
            .change(change(), Duration::from_millis(200), Duration::from_secs(4))
            .await;
        assert_eq!(out, Outcome::NotOpen);
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn handles_and_extents() {
        assert!(plausible_handle(S, &h("a")));
        assert!(!plausible_handle(S, "ws://other/results/a.gpkg"));
        assert!(!plausible_handle(S, &format!("ws://{S}-x/results/a.gpkg")));
        assert!(!plausible_handle(S, &format!("ws://{S}/")));
        assert!(!plausible_handle(S, &format!("ws://{S}/a\nb")));
        assert!(check_extent(&[-180.0, -90.0, 180.0, 90.0]).is_ok());
        assert!(check_extent(&[170.0, 50.0, 190.0, 54.0]).is_ok());
        assert!(check_extent(&[0.0, 0.0, 0.0, 1.0]).is_err());
        assert!(check_extent(&[0.0, 0.0, 361.0, 1.0]).is_err());
        assert!(check_extent(&[f64::NAN, 0.0, 1.0, 1.0]).is_err());
    }
}
