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
use crate::providers::{copy_hashed, OutputSource};
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

pub(crate) fn valid_name(n: &str) -> bool {
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

    /// A provenance record as Columbus wrote it, from memory: the copy in
    /// the workspace is for people, and is not trusted back.
    fn read_prov(&self, id: &str) -> Option<Value> {
        self.state.provs.lock().expect("provs").get(id).cloned()
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
        let cap = self.state.policy().return_rows.unwrap_or(500).clamp(1, 500);
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
        let policy = self.state.policy();
        let Some(folder) = policy.exports.iter().find(|f| f.id == args.folder) else {
            let ids: Vec<&str> = policy.exports.iter().map(|f| f.id.as_str()).collect();
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
        let format = match (&args.format, formats.is_empty()) {
            (Some(f), _) => f.clone(),
            (None, true) => "gpkg".into(),
            (None, false) => formats
                .iter()
                .find(|f| canon_format(f).is_some())
                .cloned()
                .unwrap_or_else(|| "gpkg".into()),
        };
        if !formats.is_empty()
            && !formats
                .iter()
                .any(|f| canon_format(f).is_some() && canon_format(f) == canon_format(&format))
        {
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
                    .filter(|(_, v)| meaningful(v))
                    .map(|(k, _)| norm(k))
                    .collect()
            })
            .unwrap_or_default();
        let missing: Vec<&str> = required_meta
            .iter()
            .filter(|k| !given.contains(&norm(k)))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            checks.push(fail(
                "delivery.metadata",
                format!(
                    "The delivery spec requires metadata the export does not have: {}",
                    missing.join(", ")
                ),
                Some("Pass them in metadata (ask the person for anything you do not know)"),
            ));
        }

        // Every fail the layer was built over, carried in memory by the tools
        // that made it. Provenance files are only a record: they can be
        // edited, and are not what decides.
        for f in &meta.inherited {
            let from = f.provenance.clone().unwrap_or_else(|| "?".into());
            checks.push(Check {
                id: f.id.clone(),
                severity: Severity::Fail,
                message: format!("Earlier step {from}: {}", f.message),
                detail: Some(json!({"provenance": from})),
                fix: None,
            });
        }
        if meta.truncated {
            checks.push(fail(
                "input.truncated",
                "The layer was cut off at a row limit when it was made, so it is not the whole result".into(),
                Some("Narrow the original query so it is complete, and rebuild the layer"),
            ));
        }
        let lineage = self.lineage(meta.provenance.as_deref());

        // Work in a folder of the server's own, outside the workspace that
        // confined servers and plugins can write. The engine reads a copy of
        // the source whose content is checked against what Columbus wrote,
        // and each file it makes is hashed as soon as it is made.
        let stage = match tempfile::Builder::new()
            .prefix("columbus-export-")
            .tempdir()
        {
            Ok(d) => d,
            Err(e) => {
                return Err(self.deny(&ctx, rec, format!("could not make a staging folder: {e}")))
            }
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("dat")
            .to_ascii_lowercase();
        let src_copy = stage.path().join(format!("source.{ext}"));
        match copy_hashed(&path, &src_copy) {
            Ok(h) if Some(&h) == meta.sha256.as_ref() => {}
            Ok(_) => {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!("{} changed after Columbus wrote it", args.handle),
                ))
            }
            Err(e) => {
                return Err(self.deny(&ctx, rec, format!("could not read {}: {e}", args.handle)))
            }
        }
        let out_dir = stage.path().join("out");
        if let Err(e) = std::fs::create_dir(&out_dir) {
            return Err(self.deny(&ctx, rec, format!("could not make a staging folder: {e}")));
        }
        let reply = match self
            .state
            .worker
            .core(
                "export_layer",
                json!({"path": src_copy, "out_stem": out_dir.join(&args.name), "format": format, "crs": crs, "rename": rename}),
                // A large layer takes as long as the organization lets a
                // read take, and never less than five minutes.
                Duration::from_secs(self.state.policy().read_limits().max_seconds().max(300) + 10),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.deny(&ctx, rec, e.to_string())),
        };
        checks.extend(checks_from(reply.get("checks")));
        let mut staged: Vec<(PathBuf, String)> = Vec::new();
        for f in reply
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let p = PathBuf::from(f.as_str().unwrap_or_default());
            let direct = p.parent() == Some(out_dir.as_path());
            let regular = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_file());
            match (direct && regular, columbus_shared::canon::sha256_file(&p)) {
                (true, Ok((h, _))) => staged.push((p, h)),
                _ => {
                    return Err(self.deny(
                        &ctx,
                        rec,
                        "the engine did not write the export where expected",
                    ))
                }
            }
        }
        if staged.is_empty() {
            return Err(self.deny(&ctx, rec, "the engine wrote no export"));
        }
        if canon_format(&format) == Some("geojson")
            && crs
                .as_deref()
                .is_some_and(|c| !c.eq_ignore_ascii_case("EPSG:4326"))
        {
            checks.push(Check {
                id: "export.geojson_crs".into(),
                severity: Severity::Warn,
                message: format!("GeoJSON is meant to be in EPSG:4326 (RFC 7946); this one is in {}, which many readers will ignore", crs.clone().unwrap_or_default()),
                detail: None,
                fix: Some("Deliver GeoPackage, or GeoJSON in EPSG:4326".into()),
            });
        }

        // Where it would go, and what is in the way: the files themselves,
        // the provenance file, and anything else named <name>.* (shapefile
        // parts and indexes, GeoPackage -wal and -shm files).
        let targets: Vec<PathBuf> = staged
            .iter()
            .map(|(p, _)| dest_dir.join(p.file_name().unwrap()))
            .collect();
        let sidecar = dest_dir.join(format!("{}.prov.json", args.name));
        let existing = same_name(&dest_dir, &args.name);
        let existing_names: Vec<String> = existing.iter().map(|(n, _)| n.clone()).collect();
        if !existing.is_empty() && !args.overwrite {
            return Err(self.deny(
                &ctx,
                rec,
                format!("{} already exist(s) in {}; choose another name, or pass overwrite: true (needs approval)", existing_names.join(", "), folder.id),
            ));
        }

        // Who has to say yes.
        let labels = self.state.flow.file_labels(&path);
        rec.labels = labels.iter().cloned().collect();
        let dest = format!("folder:{}", folder.id);
        let decision = columbus_flow::evaluate_to(&self.state.policy().flows, &labels, &dest);
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
            reasons.push(format!("it replaces {}", existing_names.join(", ")));
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
                "replaces": existing_names,
                "failed_checks": fails.iter().map(|c| json!({"id": c.id, "message": c.message})).collect::<Vec<_>>(),
                "delivery": args.delivery,
                "labels": from,
            });
            // Bound to the request, the exact source content, the fails the
            // person saw, the exact files that would be replaced and the
            // folder itself (where the folder id points now).
            let bound = json!({"request": request, "source_sha256": meta.sha256,
                "fails": fails.iter().map(|c| &c.id).collect::<Vec<_>>(), "replaces": existing,
                "destination": dest_dir});
            let ask = Ask {
                tool: "export_output",
                label: &format!("export to {}", folder.id),
                request: &bound,
                risk: Risk::Export,
                summary,
                detail: Some(detail),
                approval_id: args.approval_id.as_deref(),
                every_time: false,
                code: None,
                standing: None,
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
        // What will be replaced must still be exactly what was seen.
        if same_name(&dest_dir, &args.name) != existing {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "files named {} in {} changed while this export waited; ask again",
                    args.name, folder.id
                ),
            ));
        }

        // Recorded before anything leaves: a call that cannot be audited
        // does not run.
        let mut record = json!({
            "id": prov,
            "created": started.to_rfc3339(),
            "user": self.state.user,
            "client": client_name(&ctx),
            "export": {"folder": folder.id, "files": targets, "format": format, "delivery": args.delivery,
                       "crs": crs, "rename": rename, "approval": rec.approval, "replaces": existing_names},
            "metadata": args.metadata,
            "labels": labels,
            "checks": checks,
            "lineage": lineage,
            "server": format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
        });
        self.audit(
            &ctx,
            columbus_shared::audit::AuditRecord {
                decision: "exporting".into(),
                prov: Some(prov.clone()),
                ..rec.clone()
            },
        )?;

        // Copy to hidden temporary names beside the targets, checking each
        // staged file's hash as it is read; then move them all into place.
        let tmp_for = |t: &Path| {
            dest_dir.join(format!(
                ".columbus-{prov}-{}",
                t.file_name().unwrap().to_string_lossy()
            ))
        };
        let mut temps: Vec<PathBuf> = Vec::new();
        let mut written = Vec::new();
        let mut failure: Option<String> = None;
        for ((from, want), to) in staged.iter().zip(&targets) {
            let tmp = tmp_for(to);
            match copy_hashed(from, &tmp) {
                Ok(h) => {
                    temps.push(tmp);
                    if &h != want {
                        failure = Some(format!(
                            "{} changed while it was being exported",
                            from.display()
                        ));
                        break;
                    }
                    written.push(json!({"path": to, "sha256": h}));
                }
                Err(e) => {
                    failure = Some(format!("could not write {}: {e}", to.display()));
                    break;
                }
            }
        }
        record["finished"] = json!(Local::now().to_rfc3339());
        record["export"]["files"] = json!(written);
        if failure.is_none() {
            let tmp = tmp_for(&sidecar);
            match write_new(
                &tmp,
                serde_json::to_string_pretty(&record)
                    .unwrap_or_default()
                    .as_bytes(),
            ) {
                Ok(()) => temps.push(tmp),
                Err(e) => failure = Some(format!("could not write {}: {e}", sidecar.display())),
            }
        }
        if let Some(why) = failure {
            for t in &temps {
                let _ = std::fs::remove_file(t);
            }
            return Err(self.deny(&ctx, rec, format!("{why}; nothing was exported")));
        }
        // Replacing: everything of this name moves aside first (so a file
        // another program holds open stops the export before anything
        // changes), the new files move in, and on any failure the old ones
        // come back. Nothing mixed is left.
        let finals: Vec<PathBuf> = targets
            .iter()
            .cloned()
            .chain(std::iter::once(sidecar.clone()))
            .collect();
        let mut aside: Vec<(PathBuf, PathBuf)> = Vec::new();
        let restore = |aside: &[(PathBuf, PathBuf)], placed: &[PathBuf]| {
            for f in placed {
                let _ = std::fs::remove_file(f);
            }
            for (orig, bak) in aside {
                let _ = std::fs::rename(bak, orig);
            }
        };
        for (i, (n, _)) in existing.iter().enumerate() {
            let orig = dest_dir.join(n);
            let bak = dest_dir.join(format!(".columbus-{prov}-old-{i}"));
            if let Err(e) = std::fs::rename(&orig, &bak) {
                restore(&aside, &[]);
                for t in &temps {
                    let _ = std::fs::remove_file(t);
                }
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!("could not replace {n} ({e}; is it open in another program?); nothing was changed"),
                ));
            }
            aside.push((orig, bak));
        }
        let mut placed: Vec<PathBuf> = Vec::new();
        for (tmp, fin) in temps.iter().zip(&finals) {
            if let Err(e) = std::fs::rename(tmp, fin) {
                restore(&aside, &placed);
                for t in &temps {
                    let _ = std::fs::remove_file(t);
                }
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!(
                        "could not move {} into place ({e}); the folder is as it was",
                        fin.display()
                    ),
                ));
            }
            placed.push(fin.clone());
        }
        for (_, bak) in &aside {
            let _ = std::fs::remove_file(bak);
        }
        let _ = self.write_provenance(&prov, &record);
        rec.prov = Some(prov.clone());
        rec.rows = meta.rows;
        let approval = rec.approval.clone();
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({"status": "exported", "folder": folder.id, "files": written,
            "provenance_file": sidecar, "summary": reply.get("summary"),
            // Say how it was allowed, so an agent does not
            // mistake an approval given while the call waited
            // for no approval at all.
            "allowed_by": match &approval {
                Some(id) => json!({"approval": id, "how": "the person approved this export while the call waited"}),
                None => json!({"how": "the folder is granted and every check passed, so no approval was needed"}),
            }}),
            checks,
            provenance: Some(prov),
        }))
    }
}

fn norm(f: &str) -> String {
    f.trim().to_ascii_lowercase()
}

/// A delivery format's canonical name, or None for one Columbus cannot write
/// (a spec may also list PDF and the like).
fn canon_format(f: &str) -> Option<&'static str> {
    match norm(f).as_str() {
        "gpkg" | "geopackage" => Some("gpkg"),
        "geojson" => Some("geojson"),
        "fgb" | "flatgeobuf" => Some("fgb"),
        "shp" | "shapefile" => Some("shp"),
        "csv" => Some("csv"),
        _ => None,
    }
}

/// Required metadata counts as given only with something in it.
fn meaningful(v: &Value) -> bool {
    match v {
        Value::String(t) => !t.trim().is_empty(),
        Value::Number(_) => true,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Bool(_) | Value::Null => false,
    }
}

/// Everything in `dir` named `<name>` or `<name>.*` (any case), with its
/// content hash, sorted.
/// Files an export named `name` writes or that belong with them: the
/// formats' own files, a shapefile's parts and indexes, a GeoPackage's
/// journal files, and the provenance file.
const EXPORT_SUFFIXES: &[&str] = &[
    ".gpkg",
    ".gpkg-wal",
    ".gpkg-shm",
    ".gpkg-journal",
    ".geojson",
    ".fgb",
    ".csv",
    ".shp",
    ".shx",
    ".dbf",
    ".prj",
    ".cpg",
    ".shp.xml",
    ".sbn",
    ".sbx",
    ".qix",
    ".fix",
    ".atx",
    ".aih",
    ".ain",
    ".prov.json",
];

/// Everything in `dir` that an export named `name` would write or replace
/// (any case), with its content hash, sorted.
fn same_name(dir: &Path, name: &str) -> Vec<(String, Option<String>)> {
    let names: Vec<String> = EXPORT_SUFFIXES
        .iter()
        .map(|s| format!("{name}{s}").to_ascii_lowercase())
        .collect();
    let mut out: Vec<(String, Option<String>)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            names.contains(&n.to_ascii_lowercase()).then(|| {
                let h = columbus_shared::canon::sha256_file(&e.path())
                    .ok()
                    .map(|(h, _)| h);
                (n, h)
            })
        })
        .collect();
    out.sort();
    out
}

pub(crate) fn write_new(to: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
