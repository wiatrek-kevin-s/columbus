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
        let p = &s.policy;
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
            "connections": s.connections.iter().map(|c| json!({
                "id": c.id, "type": c.kind, "description": c.description,
            })).collect::<Vec<_>>(),
            "export_folders": p.exports.iter().map(|e| json!({
                "id": e.id, "path": e.path, "granted": e.granted,
            })).collect::<Vec<_>>(),
            "context_pack": s.context.as_ref().map(|c| c.info.name.clone()),
            "notes": s.notes,
        }))
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
                Duration::from_secs(120),
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
