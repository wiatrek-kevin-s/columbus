//! Reading workspace outputs and exporting them out of the sandbox
//! (design section 3, Workspace tools; section 4, checks before export).
//!
//! `read_output` pages through a layer Columbus wrote. `export_output`
//! copies one to an export folder the organization configured, in a
//! delivery format, after the full set of checks: the layer's own, every
//! `fail` anywhere in its lineage, and the delivery spec from the context
//! pack. Exports need a person's approval unless the folder is granted, and
//! any `fail` needs an approved override wherever it goes. A provenance
//! file is written beside every export.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Local;
use columbus_shared::approvals::Risk;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::approval_flow::{Ask, Sought};
use crate::providers::OutputSource;
use crate::server::{
    checks_from, client_name, new_prov_id, Check, Gateway, Output, Severity, SERVER_NAME,
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// A workspace handle (ws://…) from a Columbus tool's result.
    pub handle: String,
    /// First row to return (0-based). Default 0.
    #[serde(default)]
    pub offset: Option<u64>,
    /// Rows to return, at most the organization's per-call limit. Default 50.
    #[serde(default)]
    pub limit: Option<u64>,
    /// Only these fields (default all).
    #[serde(default)]
    pub fields: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportArgs {
    /// The workspace handle (ws://…) of the layer to export.
    pub handle: String,
    /// The export folder's id, as whoami lists them.
    pub folder: String,
    /// File name without extension: letters, digits, space, `-`, `_` and
    /// `.`, at most 80 characters.
    pub name: String,
    /// gpkg (default), geojson, fgb, shp or csv. With a delivery spec, one
    /// of its formats.
    #[serde(default)]
    pub format: Option<String>,
    /// A delivery entry's id from the context pack: its CRS, field renames,
    /// formats and required metadata are applied and checked.
    #[serde(default)]
    pub delivery: Option<String>,
    /// Metadata for the provenance file (title, abstract, contact…); a
    /// delivery spec may require some.
    #[serde(default)]
    pub metadata: Option<serde_json::Map<String, Value>>,
    /// Replace files of the same name. Always needs approval.
    #[serde(default)]
    pub overwrite: bool,
    /// An approval the person granted for this export.
    #[serde(default)]
    pub approval_id: Option<String>,
}

/// Windows device names, which open devices whatever the extension.
const DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn valid_name(n: &str) -> bool {
    let stem = n
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    !DEVICE_NAMES.contains(&stem.as_str())
        && !n.is_empty()
        && n.len() <= 80
        && !n.starts_with(['.', ' '])
        && !n.ends_with(['.', ' '])
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        && !n.contains("..")
}

fn fail(id: &str, message: String, fix: Option<&str>) -> Check {
    Check {
        id: id.into(),
        severity: Severity::Fail,
        message,
        detail: None,
        fix: fix.map(String::from),
    }
}

impl Gateway {
    /// A workspace file Columbus or an approved plugin wrote, unchanged.
    fn own_output(&self, handle: &str) -> Result<(PathBuf, crate::providers::OutputMeta), String> {
        let path =
            crate::providers::resolve_handle(&self.state.session, &self.state.sandbox, handle)?;
        let meta = self.state.outputs.get(&path).ok_or_else(|| {
            format!("{handle} was not written by a Columbus tool in this session")
        })?;
        if meta.source == OutputSource::Provider {
            return Err(format!(
                "{handle} came from a connected server; only layers Columbus or an approved engine plugin wrote can be read or exported here"
            ));
        }
        if !self.state.outputs.unchanged(&path) {
            return Err(format!("{handle} changed after Columbus wrote it"));
        }
        Ok((path, meta))
    }

    fn read_prov(&self, id: &str) -> Option<Value> {
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
        let path = self
            .state
            .sandbox
            .resolve(&format!("provenance/{id}.prov.json"))
            .ok()?;
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    /// The provenance records behind an output: its own and, through each
    /// input's provenance id, everything it was built from.
    fn lineage(&self, first: Option<&str>) -> Vec<Value> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut todo: Vec<String> = first.map(String::from).into_iter().collect();
        while let Some(id) = todo.pop() {
            if out.len() >= 50 || !seen.insert(id.clone()) {
                continue;
            }
            let Some(rec) = self.read_prov(&id) else {
                continue;
            };
            if let Some(inputs) = rec.get("inputs").and_then(Value::as_array) {
                for i in inputs {
                    if let Some(p) = i.get("provenance").and_then(Value::as_str) {
                        todo.push(p.to_string());
                    }
                }
            }
            out.push(rec);
        }
        out
    }
}

#[tool_router(router = export_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Read rows of a workspace layer a Columbus tool wrote (a page at a time: offset, limit, fields). Geometry comes back as its type and bounding box. Use it to look further into a result than the rows the tool returned.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn read_output(
        &self,
        Parameters(args): Parameters<ReadArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({"handle": args.handle, "offset": args.offset, "limit": args.limit, "fields": args.fields});
        let mut rec = self.gate(&ctx, "read_output", &request)?;
        let (path, meta) = match self.own_output(&args.handle) {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let cap = self.state.policy.return_rows.unwrap_or(500).clamp(1, 500);
        let limit = args.limit.unwrap_or(50).clamp(1, cap);
        let reply = match self
            .state
            .worker
            .core(
                "read_rows",
                json!({"path": path, "offset": args.offset.unwrap_or(0), "limit": limit, "fields": args.fields}),
                Duration::from_secs(60),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.deny(&ctx, rec, e.to_string())),
        };
        let mut checks = Vec::new();
        if meta.truncated {
            checks.push(Check {
                id: "input.truncated".into(),
                severity: Severity::Warn,
                message: "This layer was cut off at a row limit when it was made".into(),
                detail: None,
                fix: Some("Narrow the original query so it is complete".into()),
            });
        }
        let returned = reply
            .get("rows")
            .and_then(Value::as_array)
            .map(|a| a.len() as u64)
            .unwrap_or(0);
        rec.rows = Some(returned);
        rec.prov = meta.provenance.clone();
        rec.labels = self.state.flow.file_labels(&path).into_iter().collect();
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: reply,
            checks,
            provenance: meta.provenance,
        }))
    }

    #[tool(
        description = "Export a workspace layer to one of the organization's export folders (whoami lists them), as gpkg, geojson, fgb, shp or csv. With a delivery spec from the context pack (delivery: its id), its CRS, field renames, formats and required metadata are applied and checked. Runs the full checks first; any fail, anywhere in the layer's history, blocks the export unless the person approves an override. Needs approval unless the folder is granted. Writes <name>.prov.json beside the export.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn export_output(
        &self,
        Parameters(args): Parameters<ExportArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({"handle": args.handle, "folder": args.folder, "name": args.name,
            "format": args.format, "delivery": args.delivery, "metadata": args.metadata, "overwrite": args.overwrite});
        let mut rec = self.gate(&ctx, "export_output", &request)?;
        let started = Local::now();
        let prov = new_prov_id();
        if !valid_name(&args.name) {
            return Err(self.deny(
                &ctx,
                rec,
                "name may use letters, digits, space, -, _ and . (at most 80 characters, no path)",
            ));
        }
        let (path, meta) = match self.own_output(&args.handle) {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let Some(folder) = self
            .state
            .policy
            .exports
            .iter()
            .find(|f| f.id == args.folder)
        else {
            let ids: Vec<&str> = self
                .state
                .policy
                .exports
                .iter()
                .map(|f| f.id.as_str())
                .collect();
            return Err(self.deny(
                &ctx,
                rec,
                if ids.is_empty() {
                    "organization policy configures no export folders, so nothing can be exported"
                        .to_string()
                } else {
                    format!(
                        "no export folder {:?}; the folders are: {}",
                        args.folder,
                        ids.join(", ")
                    )
                },
            ));
        };
        let dest_dir = match std::fs::canonicalize(&folder.path) {
            Ok(p) if p.is_dir() => p,
            _ => {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!(
                        "export folder {} ({}) does not exist",
                        folder.id,
                        folder.path.display()
                    ),
                ))
            }
        };

        // The delivery spec, if one was named.
        let mut checks: Vec<Check> = Vec::new();
        let mut crs: Option<String> = None;
        let mut rename = serde_json::Map::new();
        let mut formats: Vec<String> = Vec::new();
        let mut required_meta: Vec<String> = Vec::new();
        if let Some(id) = &args.delivery {
            let entry = self
                .state
                .context
                .as_ref()
                .and_then(|p| p.get(id))
                .filter(|e| e.kind == "delivery");
            let Some(entry) = entry else {
                return Err(self.deny(&ctx, rec, format!("no delivery entry {id:?} in the context pack (context_search kind delivery lists them)")));
            };
            let card = serde_json::to_value(&entry.card).unwrap_or(Value::Null);
            crs = card.get("crs").and_then(Value::as_str).map(String::from);
            if let Some(r) = card.pointer("/fields/rename").and_then(Value::as_object) {
                rename = r.clone();
            }
            formats = card
                .get("formats")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            required_meta = card
                .get("metadata")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
        }
        let norm = |f: &str| f.trim().to_ascii_lowercase();
        let format = match (&args.format, formats.is_empty()) {
            (Some(f), _) => f.clone(),
            (None, true) => "gpkg".into(),
            (None, false) => formats
                .iter()
                .find(|f| {
                    [
                        "gpkg",
                        "geopackage",
                        "geojson",
                        "fgb",
                        "flatgeobuf",
                        "shp",
                        "shapefile",
                        "csv",
                    ]
                    .contains(&norm(f).as_str())
                })
                .cloned()
                .unwrap_or_else(|| "gpkg".into()),
        };
        let same_format = |a: &str, b: &str| {
            let canon = |s: &str| match norm(s).as_str() {
                "geopackage" => "gpkg".to_string(),
                "flatgeobuf" => "fgb".to_string(),
                "shapefile" => "shp".to_string(),
                x => x.to_string(),
            };
            canon(a) == canon(b)
        };
        if !formats.is_empty() && !formats.iter().any(|f| same_format(f, &format)) {
            checks.push(fail(
                "delivery.format",
                format!(
                    "The delivery spec allows {}, not {format}",
                    formats.join(", ")
                ),
                Some("Export in one of the spec's formats"),
            ));
        }
        let given: BTreeSet<String> = args
            .metadata
            .as_ref()
            .map(|m| {
                m.iter()
                    .filter(|(_, v)| {
                        v.as_str().is_none_or(|s| !s.trim().is_empty()) && !v.is_null()
                    })
                    .map(|(k, _)| norm(k))
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<&String> = required_meta
            .iter()
            .filter(|k| !given.contains(&norm(k)))
            .collect();
        if !missing.is_empty() {
            checks.push(fail(
                "delivery.metadata",
                format!(
                    "The delivery spec requires metadata the export does not have: {}",
                    missing
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Some("Pass them in metadata (ask the person for anything you do not know)"),
            ));
        }

        // Everything the layer was built from: a fail anywhere carries over.
        let lineage = self.lineage(meta.provenance.as_deref());
        for r in &lineage {
            for c in checks_from(r.get("checks")) {
                if c.severity == Severity::Fail {
                    let from = r
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string();
                    checks.push(Check {
                        id: c.id.clone(),
                        severity: Severity::Fail,
                        message: format!("Earlier step {from}: {}", c.message),
                        detail: Some(json!({"provenance": from})),
                        fix: c.fix.clone(),
                    });
                }
            }
        }
        if meta.truncated {
            checks.push(fail(
                "input.truncated",
                "The layer was cut off at a row limit when it was made, so it is not the whole result".into(),
                Some("Narrow the original query so it is complete, and rebuild the layer"),
            ));
        }

        // Write the delivery copy in the workspace, then check it.
        let stage_stem = match self
            .state
            .sandbox
            .resolve(&format!("exports/{prov}/{}", args.name))
        {
            Ok(p) => p,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let reply = match self
            .state
            .worker
            .core(
                "export_layer",
                json!({"path": path, "out_stem": stage_stem, "format": format, "crs": crs, "rename": rename}),
                Duration::from_secs(300),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.deny(&ctx, rec, e.to_string())),
        };
        checks.extend(checks_from(reply.get("checks")));
        let staged: Vec<PathBuf> = reply
            .get("files")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        let root = self.state.sandbox.root().to_path_buf();
        if staged.is_empty() || !staged.iter().all(|p| p.starts_with(&root) && p.is_file()) {
            return Err(self.deny(
                &ctx,
                rec,
                "the engine did not write the export where expected",
            ));
        }

        // Where it would go, and whether anything is in the way.
        let targets: Vec<PathBuf> = staged
            .iter()
            .map(|p| dest_dir.join(p.file_name().unwrap()))
            .collect();
        let sidecar = dest_dir.join(format!("{}.prov.json", args.name));
        let existing: Vec<String> = targets
            .iter()
            .chain(std::iter::once(&sidecar))
            .filter(|p| std::fs::symlink_metadata(p).is_ok())
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        if !existing.is_empty() && !args.overwrite {
            return Err(self.deny(
                &ctx,
                rec,
                format!("{} already exist(s) in {}; choose another name, or pass overwrite: true (needs approval)", existing.join(", "), folder.id),
            ));
        }

        // Who has to say yes.
        let labels = self.state.flow.file_labels(&path);
        rec.labels = labels.iter().cloned().collect();
        let dest = format!("folder:{}", folder.id);
        let decision = columbus_flow::evaluate_to(&self.state.policy.flows, &labels, &dest);
        rec.flow_decision = Some(decision.label().into());
        if let columbus_flow::FlowDecision::Deny { labels, rule } = &decision {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "organization flow rule {} forbids exporting data from {} to {}",
                    rule + 1,
                    labels.join(", "),
                    folder.id
                ),
            ));
        }
        let fails: Vec<&Check> = checks
            .iter()
            .filter(|c| c.severity == Severity::Fail)
            .collect();
        let mut reasons: Vec<String> = Vec::new();
        if !folder.granted {
            reasons.push(format!("{} is not a granted export folder", folder.id));
        }
        if matches!(
            decision,
            columbus_flow::FlowDecision::Approve { rule: Some(_), .. }
        ) {
            reasons.push("an organization flow rule asks for approval".into());
        }
        if !existing.is_empty() {
            reasons.push(format!("it replaces {}", existing.join(", ")));
        }
        if !fails.is_empty() {
            reasons.push(format!(
                "{} check(s) failed ({}), so this is an override",
                fails.len(),
                fails
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !reasons.is_empty() {
            let from: Vec<String> = labels
                .iter()
                .map(|l| l.trim_start_matches("connection:").to_string())
                .collect();
            let summary = format!(
                "Export {} ({} rows{}) to {} ({}) as {}. Approval needed because {}.",
                self.ws_handle(&path).unwrap_or_default(),
                meta.rows
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "?".into()),
                if from.is_empty() {
                    String::new()
                } else {
                    format!("; from {}", from.join(", "))
                },
                folder.id,
                dest_dir.display(),
                targets
                    .iter()
                    .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", "),
                reasons.join("; ")
            );
            let detail = json!({
                "destination": dest_dir,
                "files": targets,
                "failed_checks": fails.iter().map(|c| json!({"id": c.id, "message": c.message})).collect::<Vec<_>>(),
                "delivery": args.delivery,
                "labels": from,
            });
            // Bound to the request and the exact source content.
            let bound = json!({"request": request, "source_sha256": meta.sha256, "fails": fails.iter().map(|c| &c.id).collect::<Vec<_>>()});
            let ask = Ask {
                tool: "export_output",
                label: &format!("export to {}", folder.id),
                request: &bound,
                risk: Risk::Export,
                summary,
                detail: Some(detail),
                approval_id: args.approval_id.as_deref(),
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
        // The source must still be what was checked and approved.
        if !self.state.outputs.unchanged(&path) {
            return Err(self.deny(
                &ctx,
                rec,
                format!("{} changed while waiting for approval", args.handle),
            ));
        }

        // Copy out. New files are created, never followed through a link.
        let mut written = Vec::new();
        for (from, to) in staged.iter().zip(&targets) {
            if args.overwrite && std::fs::symlink_metadata(to).is_ok() {
                if let Err(e) = std::fs::remove_file(to) {
                    return Err(self.deny(
                        &ctx,
                        rec,
                        format!("could not replace {}: {e}", to.display()),
                    ));
                }
            }
            if let Err(e) = copy_new(from, to) {
                return Err(self.deny(&ctx, rec, format!("could not write {}: {e}", to.display())));
            }
            let sha = columbus_shared::canon::sha256_file(to)
                .map(|(h, _)| h)
                .unwrap_or_default();
            written.push(json!({"path": to, "sha256": sha}));
        }
        let record = json!({
            "id": prov,
            "created": started.to_rfc3339(),
            "finished": Local::now().to_rfc3339(),
            "user": self.state.user,
            "client": client_name(&ctx),
            "export": {"folder": folder.id, "files": written, "format": format, "delivery": args.delivery,
                       "crs": crs, "rename": rename, "approval": rec.approval},
            "metadata": args.metadata,
            "labels": labels,
            "checks": checks,
            "lineage": lineage,
            "server": format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
        });
        let _ = self.write_provenance(&prov, &record);
        if args.overwrite && std::fs::symlink_metadata(&sidecar).is_ok() {
            let _ = std::fs::remove_file(&sidecar);
        }
        if let Err(e) = write_new(
            &sidecar,
            serde_json::to_string_pretty(&record)
                .unwrap_or_default()
                .as_bytes(),
        ) {
            return Err(self.deny(
                &ctx,
                rec,
                format!("exported, but could not write {}: {e}", sidecar.display()),
            ));
        }
        let _ = std::fs::remove_dir_all(stage_stem.parent().unwrap_or(Path::new("")));
        rec.prov = Some(prov.clone());
        rec.rows = meta.rows;
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({"status": "exported", "folder": folder.id, "files": written,
                           "provenance_file": sidecar, "summary": reply.get("summary")}),
            checks,
            provenance: Some(prov),
        }))
    }
}

/// Create `to` (failing if anything is there, including a link) and copy
/// `from` into it.
fn copy_new(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut src = std::fs::File::open(from)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    std::io::copy(&mut src, &mut dst)?;
    dst.sync_all()
}

fn write_new(to: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn export_names_cannot_leave_the_folder() {
        for ok in ["district 14 roads", "roads_2026-11.v2"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "../x",
            "a/b",
            "a\\b",
            "..",
            ".hidden",
            "x.",
            "C:x",
            "con:",
            "CON",
            "nul.gpkg",
            "Com1 .x",
            &"a".repeat(81),
            "a..b",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
