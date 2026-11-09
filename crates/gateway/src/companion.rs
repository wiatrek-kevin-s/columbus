//! What the companion window shows (design section 8): the session's
//! outputs on a map and in a table, their provenance and checks, and the
//! approvals waiting for the person.
//!
//! The window is the local Columbus page (see `approvals_page`), so it is
//! reached only with that page's session token, which the agent never sees.
//! Everything here is read-only: the person looks at what the agent made.
//! Only files a Columbus tool or an approved plugin wrote, unchanged since,
//! are read, exactly as `read_output` does; a file a connected server wrote
//! is listed but not opened.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde_json::{json, Value};

use crate::providers::OutputSource;
use crate::server::State;

/// Most features the map gets for one layer.
pub const MAP_FEATURES: u64 = 20_000;
/// Most rows the table gets per page.
pub const TABLE_ROWS: u64 = 500;

pub struct Companion {
    state: Weak<State>,
}

impl Companion {
    pub fn new(state: &Arc<State>) -> Self {
        Companion {
            state: Arc::downgrade(state),
        }
    }

    fn st(&self) -> Result<Arc<State>, String> {
        self.state
            .upgrade()
            .ok_or_else(|| "the session has ended".to_string())
    }

    /// Who, what and under which rules: the parts of `whoami` a person
    /// wants to see at a glance.
    pub fn overview(&self) -> Result<Value, String> {
        let s = self.st()?;
        s.refresh();
        let settings = s.settings();
        let p = &settings.policy;
        Ok(json!({
            "session": s.session,
            "user": s.user,
            "server": format!("columbus-mcp {}", env!("CARGO_PKG_VERSION")),
            "workspace": s.sandbox.root(),
            "policy": {
                "connection_types": p.connection_types,
                "scripts_allowed": p.allow_scripts,
                "edits_allowed": p.allow_edits,
                "rows_returned_per_call": p.return_rows,
            },
            "connections": settings.connections.iter().map(|c| json!({
                "id": c.id, "type": c.kind, "description": c.description,
            })).collect::<Vec<_>>(),
            "export_folders": p.exports.iter().map(|e| json!({
                "id": e.id, "path": e.path, "granted": e.granted,
            })).collect::<Vec<_>>(),
            "context_pack": s.context.as_ref().map(|c| c.info.name.clone()),
            "basemap_allowed": p.companion.basemap,
            "notes": s.notes,
        }))
    }

    /// The Settings window's Test button: can this session open a
    /// connection as the form describes it, before it is saved? Checked
    /// exactly as a saved one would be (the organization's policy, then the
    /// engine listing its datasets), with the password typed in the form;
    /// with none typed, the stored one is used only when the form points
    /// where the saved connection of that name already does. Nothing is
    /// saved, and the AI sees none of it. Recorded in the audit log.
    pub async fn test_connection(&self, spec: Value, password: Option<String>) -> Value {
        let s = match self.st() {
            Ok(s) => s,
            Err(e) => return json!({"ok": false, "message": e}),
        };
        let mut how = String::new();
        let r = test_connection(&s, spec, password, &mut how).await;
        let (decision, reason, conn) = match &r {
            Ok((id, _)) => ("allowed", Some(how), Some(id.clone())),
            Err((id, msg)) => ("error", Some(format!("{how}; {msg}")), id.clone()),
        };
        let _ = s.audit.append(&columbus_shared::audit::AuditRecord {
            user: s.user.clone(),
            client: "columbus-app (settings)".into(),
            session: Some(s.session.clone()),
            tool: "settings.test_connection".into(),
            connection: conn,
            decision: decision.into(),
            reason,
            ..Default::default()
        });
        match r {
            Ok((_, v)) => v,
            Err((_, msg)) => json!({"ok": false, "message": msg}),
        }
    }

    /// The Settings window's "Draft notes": the person's notes about one
    /// connection, drafted from its database (see `notes`).
    pub async fn draft_notes(&self, connection: String) -> Value {
        match self.st() {
            Ok(s) => crate::notes::draft_from_app(s, connection).await,
            Err(e) => json!({"ok": false, "message": e}),
        }
    }

    /// Whether organization policy lets the person turn on the basemap.
    pub fn basemap_allowed(&self) -> bool {
        self.st().is_ok_and(|s| s.policy().companion.basemap)
    }

    /// A number that changes whenever the outputs or provenance do, so the
    /// window knows when to refresh.
    pub fn version(&self) -> u64 {
        match self.st() {
            Ok(s) => {
                let provs = s.provs.lock().expect("provs").len() as u64;
                (s.outputs.len() as u64) << 32 | provs
            }
            Err(_) => 0,
        }
    }

    /// Every file in the session's results and external folders, newest
    /// first, with what Columbus knows about it.
    pub fn outputs(&self) -> Result<Value, String> {
        let s = self.st()?;
        let root = s.sandbox.root().to_path_buf();
        let mut cards = Vec::new();
        for dir in ["results", "external"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
                continue;
            };
            for e in entries.filter_map(Result::ok) {
                let path = e.path();
                let Ok(md) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if !md.is_file() {
                    continue;
                }
                let meta = s.outputs.get(&path);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let ext = path
                    .extension()
                    .map(|x| x.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                let kind = match ext.as_str() {
                    "gpkg" => "layer",
                    "csv" => "table",
                    "tif" | "tiff" => "raster",
                    _ => "file",
                };
                let viewable = meta
                    .as_ref()
                    .is_some_and(|m| m.source != OutputSource::Provider)
                    && matches!(kind, "layer" | "table");
                let prov = meta.as_ref().and_then(|m| m.provenance.clone());
                let checks = prov
                    .as_deref()
                    .and_then(|id| s.provs.lock().expect("provs").get(id).cloned())
                    .and_then(|r| r.get("checks").cloned())
                    .map(|c| summarize_checks(&c));
                let modified = md
                    .modified()
                    .ok()
                    .map(chrono::DateTime::<chrono::Local>::from)
                    .map(|t| t.to_rfc3339());
                cards.push(json!({
                    "handle": crate::providers::handle_for(&s.session, &root, &path),
                    "name": name,
                    "folder": dir,
                    "kind": kind,
                    "bytes": md.len(),
                    "modified": modified,
                    "made_by": meta.as_ref().map(|m| m.made_by.clone()),
                    "rows": meta.as_ref().and_then(|m| m.rows),
                    "truncated": meta.as_ref().is_some_and(|m| m.truncated),
                    "provenance": prov,
                    "checks": checks,
                    "labels": s.flow.file_labels(&path).into_iter().collect::<Vec<_>>(),
                    "viewable": viewable,
                }));
            }
        }
        cards.sort_by(|a, b| {
            b["modified"]
                .as_str()
                .unwrap_or("")
                .cmp(a["modified"].as_str().unwrap_or(""))
        });
        Ok(json!({ "outputs": cards, "version": self.version() }))
    }

    /// The path behind a handle, if the window may open it: written by a
    /// Columbus tool or an approved plugin, and unchanged since.
    fn viewable(&self, s: &State, handle: &str) -> Result<PathBuf, String> {
        let path = crate::providers::resolve_handle(&s.session, &s.sandbox, handle)?;
        let meta = s
            .outputs
            .get(&path)
            .ok_or_else(|| "not written by a Columbus tool in this session".to_string())?;
        if meta.source == OutputSource::Provider {
            return Err("written by a connected server; Columbus does not open it".into());
        }
        if !s.outputs.unchanged(&path) {
            return Err("changed after Columbus wrote it".into());
        }
        Ok(path)
    }

    /// A layer as EPSG:4326 GeoJSON for the map.
    pub async fn layer(&self, handle: &str) -> Result<Value, String> {
        let s = self.st()?;
        let path = self.viewable(&s, handle)?;
        s.worker
            .core(
                "display_geojson",
                json!({"path": path, "max_features": MAP_FEATURES}),
                Duration::from_secs(60),
            )
            .await
            .map_err(|e| e.to_string())
    }

    /// A page of rows for the table.
    pub async fn rows(&self, handle: &str, offset: u64, limit: u64) -> Result<Value, String> {
        let s = self.st()?;
        let path = self.viewable(&s, handle)?;
        s.worker
            .core(
                "read_rows",
                json!({"path": path, "offset": offset, "limit": limit.clamp(1, TABLE_ROWS)}),
                Duration::from_secs(60),
            )
            .await
            .map_err(|e| e.to_string())
    }

    /// A provenance record as Columbus wrote it (from memory, not the copy
    /// in the workspace).
    pub fn provenance(&self, id: &str) -> Result<Value, String> {
        let s = self.st()?;
        let found = s.provs.lock().expect("provs").get(id).cloned();
        found.ok_or_else(|| format!("no provenance record {id} in this session"))
    }
}

/// Counts of fail, warn and info checks.
fn summarize_checks(checks: &Value) -> Value {
    let mut c = (0u64, 0u64, 0u64);
    for k in checks.as_array().into_iter().flatten() {
        match k.get("severity").and_then(Value::as_str) {
            Some("fail") => c.0 += 1,
            Some("warn") => c.1 += 1,
            Some("info") => c.2 += 1,
            _ => {}
        }
    }
    json!({"fail": c.0, "warn": c.1, "info": c.2})
}

/// How long a Test may take in the engine.
const TEST_SECONDS: u64 = 20;

type Tested = Result<(String, Value), (Option<String>, String)>;

/// `how` gets what the audit line says about the test: where it pointed
/// and which password it used (never the password).
async fn test_connection(
    s: &Arc<State>,
    spec: Value,
    password: Option<String>,
    how: &mut String,
) -> Tested {
    use columbus_shared::policy::{Connection, EffectiveAccess};
    let conn: Connection = serde_json::from_value(spec).map_err(|e| {
        (
            None,
            format!("the form is not a connection Columbus can read: {e}"),
        )
    })?;
    let id = Some(conn.id.clone());
    *how = format!("target {}", conn.location());
    let fail = |m: String| (id.clone(), m);
    conn.validate(std::path::Path::new("the form"))
        .map_err(|e| fail(e.to_string()))?;
    crate::server::Gateway::with_state(s.clone())
        .reload_settled()
        .await;
    if let Some(why) = s.problem() {
        return Err(fail(why));
    }
    let settings = s.settings();
    let access = EffectiveAccess::resolve(&settings.policy, &conn, &s.scope, chrono::Utc::now())
        .map_err(|e| fail(format!("Columbus would refuse this connection: {e}")))?;
    let arcgis = crate::arcgis::is_arcgis(&conn.kind);
    if !crate::server::Gateway::engine_ready(&conn.kind) && !arcgis {
        return Err(fail(format!(
            "Columbus cannot open {} connections yet, so it cannot test this one",
            conn.kind
        )));
    }
    let (names, total): (Vec<String>, usize) = if crate::raster_tools::is_rasters(&conn.kind) {
        let (datasets, total) = crate::raster_tools::list_rasters(&conn, &access).map_err(fail)?;
        let names = datasets
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        (names, total)
    } else if arcgis {
        // A public service needs no sign-in; the gateway reads its layer
        // list itself, under the host rule (Part 4, R-14 messages).
        let all = crate::server::Gateway::with_state(s.clone())
            .arcgis_list(&access, &conn)
            .await
            .map_err(fail)?;
        let total = all.len();
        let names = all
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .filter(|n| access.dataset_allowed(n))
            .map(String::from)
            .collect();
        (names, total)
    } else {
        let typed = password.filter(|p| !p.is_empty());
        // With the stored password, the saved connection's own rules also
        // apply: a test cannot see past them.
        let mut saved_rules = None;
        let secret = match typed {
            Some(p) => {
                how.push_str("; password typed in the form");
                Some(p)
            }
            None if conn.path.is_none() => {
                // A stored password goes only where it already goes.
                let saved = settings
                    .connections
                    .iter()
                    .find(|c| c.id == conn.id && c.location() == conn.location());
                match saved {
                    Some(c) => {
                        saved_rules = Some(
                            EffectiveAccess::resolve(
                                &settings.policy,
                                c,
                                &s.scope,
                                chrono::Utc::now(),
                            )
                            .map_err(|e| {
                                fail(format!("Columbus would refuse the saved connection: {e}"))
                            })?,
                        );
                        how.push_str("; the stored password");
                        s.secrets.get(&conn.id).map_err(|e| {
                            fail(format!("the stored password could not be read: {e}"))
                        })?
                    }
                    None => {
                        how.push_str("; no password");
                        None
                    }
                }
            }
            None => None,
        };
        let handle = crate::server::connection_handle(&conn, secret, &s.paths.workspaces);
        let reply = s
            .worker
            .core(
                "list_datasets",
                json!({"connection": handle, "timeout_s": TEST_SECONDS,
                       "schemas": crate::server::Gateway::listed_schemas(&conn)}),
                Duration::from_secs(TEST_SECONDS + 10),
            )
            .await
            .map_err(|e| fail(format!("Columbus could not open it: {e}")))?;
        let all: Vec<Value> = reply
            .get("datasets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| {
                saved_rules.as_ref().is_none_or(|r| {
                    d.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| r.dataset_allowed(n))
                })
            })
            .collect();
        let total = all.len();
        let names = all
            .iter()
            .filter_map(|d| d.get("name").and_then(Value::as_str))
            .filter(|n| access.dataset_allowed(n))
            .map(String::from)
            .collect();
        (names, total)
    };
    let shown: Vec<&String> = names.iter().take(5).collect();
    let message = match (names.len(), total) {
        (_, 0) => "Opened, but it holds no datasets Columbus can see.".to_string(),
        (0, t) => format!(
            "Opened. It holds {t} dataset{}, but this connection's dataset lists allow none of them.",
            if t == 1 { "" } else { "s" }
        ),
        (n, t) if n == t => format!(
            "Opened. Columbus can use all {t} dataset{}: {}{}.",
            if t == 1 { "" } else { "s" },
            shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            if n > shown.len() { ", …" } else { "" }
        ),
        (n, t) => format!(
            "Opened. Columbus can use {n} of its {t} datasets (the lists leave out the rest): {}{}.",
            shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            if n > shown.len() { ", …" } else { "" }
        ),
    };
    Ok((
        conn.id.clone(),
        json!({"ok": true, "message": message, "datasets": total, "usable": names.len()}),
    ))
}
