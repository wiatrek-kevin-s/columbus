//! Running an engine plugin's tool through `run_tool` (design change 1,
//! section 7): validate arguments against `plugin.yaml`, turn each dataset
//! into a governed extract, hand the worker only checked inputs and
//! workspace output paths, and record what came back.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use columbus_broker::manifest::Risk as ToolRisk;
use columbus_shared::approvals::Risk;
use columbus_shared::audit::AuditRecord;
use columbus_shared::sqlcheck::{check_select, check_where, SqlDialect};
use rmcp::model::ProgressNotificationParam;
use rmcp::service::RequestContext;
use rmcp::{Json, RoleServer};
use serde_json::{json, Value};

use crate::plugins::{ParamSpec, Plugin};
use crate::providers::{OutputMeta, SavedFile};
use crate::server::{
    client_name, new_prov_id, Check, Gateway, Output, RunToolResult, Severity, SERVER_NAME,
};

/// A dataset argument: `"conn:dataset"` or `{connection, dataset, where}`.
fn dataset_arg(name: &str, v: &Value) -> Result<(String, String, Option<String>), String> {
    match v {
        Value::String(s) => {
            let (c, d) = s
                .split_once(':')
                .ok_or_else(|| format!("{name}: give a dataset as \"<connection>:<dataset>\""))?;
            Ok((c.trim().to_string(), d.trim().to_string(), None))
        }
        Value::Object(m) => {
            let get = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
            Ok((
                get("connection").ok_or_else(|| format!("{name}: missing connection"))?,
                get("dataset").ok_or_else(|| format!("{name}: missing dataset"))?,
                get("where"),
            ))
        }
        _ => Err(format!("{name}: a dataset is a string or an object")),
    }
}

impl Gateway {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run_plugin_tool(
        &self,
        ctx: &RequestContext<RoleServer>,
        mut rec: AuditRecord,
        p: Arc<Plugin>,
        tool: &str,
        args: serde_json::Map<String, Value>,
        approval_id: Option<String>,
        request: &Value,
    ) -> Result<Json<Output<RunToolResult>>, String> {
        let full = format!("{}.{tool}", p.id());
        rec.provider = Some(p.id().to_string());
        rec.provider_version = Some(p.manifest.version.clone());
        rec.tools_hash = Some(p.hash.clone());
        let Some(spec) = p.manifest.tools.get(tool).cloned() else {
            return Err(self.deny(ctx, rec, format!("{} has no tool named {tool:?}", p.id())));
        };
        if let Some(extra) = args.keys().find(|k| !spec.params.contains_key(*k)) {
            return Err(self.deny(ctx, rec, format!("{full} has no parameter {extra:?}")));
        }
        let worker = match self.state.plugins.worker(&p).await {
            Ok(w) => w,
            Err(e) => return Err(self.deny(ctx, rec, e)),
        };

        let run_id = format!("r_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let prov = new_prov_id();
        let mut params = serde_json::Map::new();
        let mut inputs = serde_json::Map::new();
        let mut input_prov = Vec::new();
        let mut bound_inputs = Vec::new();
        let mut recheck: Vec<(PathBuf, Option<String>)> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();

        // Scalars first: they are cheap to refuse.
        for (name, ps) in &spec.params {
            let v = args.get(name).cloned();
            let checked = match (ps, v) {
                (ParamSpec::Dataset { .. } | ParamSpec::File, _) => continue,
                (
                    ParamSpec::Number {
                        default, min, max, ..
                    },
                    v,
                ) => {
                    let n = match v {
                        Some(Value::Number(n)) => n.as_f64(),
                        None => *default,
                        _ => None,
                    };
                    let Some(n) = n.filter(|n| n.is_finite()) else {
                        return Err(self.deny(ctx, rec, format!("{name} must be a number")));
                    };
                    if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                        return Err(self.deny(ctx, rec, format!("{name} is out of range")));
                    }
                    json!(n)
                }
                (ParamSpec::Integer { default, min, max }, v) => {
                    let n = match v {
                        Some(Value::Number(n)) => n.as_i64(),
                        None => *default,
                        _ => None,
                    };
                    let Some(n) = n else {
                        return Err(self.deny(ctx, rec, format!("{name} must be a whole number")));
                    };
                    if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                        return Err(self.deny(ctx, rec, format!("{name} is out of range")));
                    }
                    json!(n)
                }
                (ParamSpec::String { choices, default }, v) => {
                    let s = match v {
                        Some(Value::String(s)) => Some(s),
                        None => default.clone(),
                        _ => None,
                    };
                    let Some(s) = s else {
                        return Err(self.deny(ctx, rec, format!("{name} must be text")));
                    };
                    if choices.as_ref().is_some_and(|c| !c.contains(&s)) {
                        return Err(self.deny(
                            ctx,
                            rec,
                            format!("{name} must be one of {choices:?}"),
                        ));
                    }
                    json!(s)
                }
                (ParamSpec::Boolean { default }, v) => match v
                    .as_ref()
                    .and_then(Value::as_bool)
                    .or(*default)
                {
                    Some(b) => json!(b),
                    None => {
                        return Err(self.deny(ctx, rec, format!("{name} must be true or false")))
                    }
                },
            };
            params.insert(name.clone(), checked);
        }

        // Workspace files.
        for (name, ps) in &spec.params {
            if !matches!(ps, ParamSpec::File) {
                continue;
            }
            let Some(h) = args.get(name).and_then(Value::as_str) else {
                return Err(self.deny(ctx, rec, format!("{name} needs a workspace handle")));
            };
            let path =
                match crate::providers::resolve_handle(&self.state.session, &self.state.sandbox, h)
                {
                    Ok(p) => p,
                    Err(e) => return Err(self.deny(ctx, rec, e)),
                };
            inputs.insert(name.clone(), json!({"kind": "file", "path": path}));
            recheck.push((
                path.clone(),
                columbus_shared::canon::sha256_file(&path)
                    .ok()
                    .map(|(h, _)| h),
            ));
            bound_inputs.push(match columbus_shared::canon::sha256_file(&path) {
                Ok((h, n)) => json!({"param": name, "file": h, "bytes": n}),
                Err(_) => json!({"param": name, "absent": true}),
            });
            input_prov.push(
                json!({"param": name, "handle": h, "labels": self.state.flow.file_labels(&path)}),
            );
            files.push(path);
        }

        // Datasets: governed extracts. Never credentials, never more than
        // the connection allows, never a silently truncated read.
        for (name, ps) in &spec.params {
            if !matches!(ps, ParamSpec::Dataset { .. }) {
                continue;
            }
            let Some(v) = args.get(name) else {
                return Err(self.deny(ctx, rec, format!("{name} is required")));
            };
            let (conn_id, dataset, where_) = match dataset_arg(name, v) {
                Ok(x) => x,
                Err(e) => return Err(self.deny(ctx, rec, e)),
            };
            let (access, conn) = match self.access(&conn_id) {
                Ok(x) => x,
                Err(e) => return Err(self.deny(ctx, rec, e)),
            };
            if !Self::engine_ready(&conn.kind) {
                return Err(self.deny(
                    ctx,
                    rec,
                    format!("reading {} connections is not supported yet", conn.kind),
                ));
            }
            let dialect = SqlDialect::for_connection_type(&conn.kind);
            let mut sql = format!("SELECT * FROM {dataset}");
            if let Some(w) = &where_ {
                match check_where(w, dialect) {
                    Ok(w) => sql.push_str(&format!(" WHERE {w}")),
                    Err(e) => return Err(self.deny(ctx, rec, e)),
                }
            }
            let max_rows = access.limits.rows.unwrap_or(200_000);
            let checked = match check_select(&sql, &access, max_rows) {
                Ok(c) => c,
                Err(e) => return Err(self.deny(ctx, rec, e)),
            };
            let handle = match self.handle(conn) {
                Ok(h) => h,
                Err(e) => return Err(self.deny(ctx, rec, e)),
            };
            let out = match self
                .state
                .sandbox
                .resolve(&format!("inputs/{run_id}-{name}.sqlite"))
            {
                Ok(p) => p,
                Err(e) => return Err(self.deny(ctx, rec, e)),
            };
            self.state
                .flow
                .add(columbus_flow::connection_label(&conn.id));
            let labels: Vec<String> = self.state.flow.session_labels().into_iter().collect();
            let timeout = access.limits.seconds.unwrap_or(120);
            let r = self
                .state
                .worker
                .core(
                    "extract",
                    json!({
                        "connection": handle, "connection_id": conn.id, "dataset": dataset,
                        "sql": checked.sql, "max_rows": max_rows, "output_path": out,
                        "hide_fields": self.state.policy.hide_fields, "labels": labels, "timeout_s": timeout,
                    }),
                    Duration::from_secs(timeout + 10),
                )
                .await;
            let r = match r {
                Ok(r) => r,
                Err(e) => {
                    let msg = e.to_string();
                    self.audit(
                        ctx,
                        AuditRecord {
                            decision: "error".into(),
                            reason: Some(msg.clone()),
                            ..rec
                        },
                    )?;
                    return Err(msg);
                }
            };
            if r.get("hit_read_limit").and_then(Value::as_bool) == Some(true) {
                return Err(self.deny(
                    ctx,
                    rec,
                    format!(
                        "{dataset} has more than {max_rows} rows, the read limit for {conn_id}; a plugin is never given a cut-off dataset. Narrow it with \"where\""
                    ),
                ));
            }
            let rows = r.get("rows").and_then(Value::as_u64);
            self.state
                .flow
                .record_file(&out, &self.state.flow.session_labels());
            self.state.outputs.record(
                &out,
                OutputMeta {
                    made_by: format!("extract for {full}"),
                    provenance: Some(prov.clone()),
                    rows,
                },
            );
            inputs.insert(
                name.clone(),
                json!({"kind": "dataset", "path": out, "rows": rows}),
            );
            recheck.push((
                out.clone(),
                columbus_shared::canon::sha256_file(&out)
                    .ok()
                    .map(|(h, _)| h),
            ));
            bound_inputs.push(json!({
                "param": name, "connection": conn.id, "dataset": dataset, "where": where_, "rows": rows,
                "sha256": columbus_shared::canon::sha256_file(&out).map(|(h, _)| h).ok(),
            }));
            input_prov.push(json!({
                "param": name, "connection": conn.id, "dataset": dataset, "where": where_,
                "executed_sql": checked.sql, "rows": rows, "extract": self.ws_handle(&out),
                "hidden_columns": r.get("hidden_columns"),
            }));
            files.push(out);
        }

        // Output paths in the workspace.
        let mut outputs = serde_json::Map::new();
        let mut out_paths = Vec::new();
        for (name, o) in &spec.outputs {
            let path = match self
                .state
                .sandbox
                .resolve(&format!("results/{run_id}-{name}.{}", o.extension()))
            {
                Ok(p) => p,
                Err(e) => return Err(self.deny(ctx, rec, e)),
            };
            outputs.insert(name.clone(), json!({"type": o.kind(), "path": path}));
            out_paths.push((name.clone(), path));
        }
        if let Some(dir) = self.state.sandbox.root().join("results").parent() {
            let _ = std::fs::create_dir_all(dir.join("results"));
        }

        let labels = self.state.flow.labels_for(&files);
        rec.labels = labels.iter().cloned().collect();

        // Tools that change things need the person's approval.
        if spec.risk >= ToolRisk::Write {
            let summary = format!(
                "{}: run {tool}. Approval needed because the plugin marks it as {:?}.",
                p.manifest.display_name(),
                spec.risk
            );
            let detail = json!({
                "destination": p.manifest.display_name(),
                "arguments": args,
                "labels": labels.iter().map(|l| l.trim_start_matches("connection:").to_string()).collect::<Vec<_>>(),
            });
            let bound = json!({
                "request": request,
                "inputs": bound_inputs,
                "labels": labels,
            });
            let ask = crate::approval_flow::Ask {
                tool: "run_tool",
                label: &full,
                request: &bound,
                risk: Risk::Edit,
                summary,
                detail: Some(detail),
                approval_id: approval_id.as_deref(),
            };
            match self.seek_approval(ctx, ask).await {
                Ok(crate::approval_flow::Sought::Granted(id)) => rec.approval = Some(id),
                Ok(crate::approval_flow::Sought::Declined(msg)) => {
                    return Err(self.deny(ctx, rec, msg))
                }
                Ok(crate::approval_flow::Sought::Pending(needed)) => {
                    rec.approval = Some(needed.id.clone());
                    self.audit(
                        ctx,
                        AuditRecord {
                            decision: "approval_required".into(),
                            ..rec
                        },
                    )?;
                    return Ok(Json(Output {
                        result: RunToolResult {
                            provider: p.id().to_string(),
                            tool: tool.to_string(),
                            status: "approval_required".into(),
                            approval: Some(needed),
                            external: None,
                            summary: None,
                            saved: vec![],
                        },
                        checks: vec![],
                        provenance: None,
                    }));
                }
                Err(e) => return Err(self.deny(ctx, rec, e)),
            }
        }

        // Inputs must be exactly what was checked (and approved).
        if let Err(e) = crate::providers::recheck(&recheck) {
            return Err(self.deny(ctx, rec, e));
        }

        // Progress goes to the client when it asked for it.
        let token = ctx.meta.get_progress_token();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<crate::worker::Progress>();
        let forward = token.map(|token| {
            let peer = ctx.peer.clone();
            tokio::spawn(async move {
                let mut step = 0.0;
                while let Some(pg) = rx.recv().await {
                    step += 1.0;
                    let mut n =
                        ProgressNotificationParam::new(token.clone(), pg.percent.unwrap_or(step));
                    n.total = pg.percent.map(|_| 100.0);
                    n.message = Some(pg.message);
                    let _ = peer.notify_progress(n).await;
                }
            })
        });
        let timeout = self.state.policy.limits.seconds.unwrap_or(120).max(1);
        let started = Local::now();
        let result = worker
            .run(
                json!({
                    "run": run_id, "tool": tool, "params": params, "inputs": inputs,
                    "outputs": outputs, "workspace": self.state.sandbox.root(), "timeout_s": timeout,
                }),
                Duration::from_secs(timeout),
                &mut |pg| {
                    let _ = tx.send(pg);
                },
            )
            .await;
        drop(tx);
        if let Some(f) = forward {
            let _ = f.await;
        }
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                self.audit(
                    ctx,
                    AuditRecord {
                        decision: "error".into(),
                        reason: Some(msg.clone()),
                        ..rec
                    },
                )?;
                return Err(format!("{full}: {msg}"));
            }
        };

        // Outputs: only the declared paths, only if written.
        let counts = result.get("outputs").cloned().unwrap_or(Value::Null);
        let mut saved = Vec::new();
        for (name, path) in &out_paths {
            if !path.is_file() {
                continue;
            }
            let c = counts.get(name);
            let rows = c
                .and_then(|c| c.get("rows").or_else(|| c.get("features")))
                .and_then(Value::as_u64);
            self.state.flow.record_file(path, &labels);
            self.state.outputs.record(
                path,
                OutputMeta {
                    made_by: full.clone(),
                    provenance: Some(prov.clone()),
                    rows,
                },
            );
            saved.push(SavedFile {
                handle: self.ws_handle(path).unwrap_or_default(),
                bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                kind: spec.outputs[name].kind().to_string(),
                path: path.clone(),
            });
        }
        let mut checks: Vec<Check> = result
            .get("warnings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|w| Check {
                id: w
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("plugin.warning")
                    .to_string(),
                severity: match w.get("severity").and_then(Value::as_str) {
                    Some("fail") => Severity::Fail,
                    Some("info") => Severity::Info,
                    _ => Severity::Warn,
                },
                message: w
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                detail: w.get("detail").cloned(),
                fix: w.get("fix").and_then(Value::as_str).map(String::from),
            })
            .collect();
        if !labels.is_empty() {
            checks.push(Check {
                id: "flow.labels".into(),
                severity: Severity::Info,
                message: format!(
                    "Outputs carry data labels: {}",
                    labels.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
                detail: None,
                fix: None,
            });
        }
        let declared: BTreeSet<&String> = spec.checks.iter().collect();
        if !declared.is_empty() {
            checks.push(Check {
                id: "verify.pending".into(),
                severity: Severity::Info,
                message: format!(
                    "Declared checks not yet run by Columbus (verification lands in a later build): {}",
                    declared.into_iter().cloned().collect::<Vec<_>>().join(", ")
                ),
                detail: None,
                fix: None,
            });
        }

        let provenance = json!({
            "id": prov,
            "created": started.to_rfc3339(),
            "finished": Local::now().to_rfc3339(),
            "user": self.state.user,
            "client": client_name(ctx),
            "inputs": input_prov,
            "labels": rec.labels,
            "steps": [{
                "tool": full, "plugin": p.id(), "plugin_version": p.manifest.version,
                "plugin_hash": p.hash, "run": run_id, "params": params, "approval": rec.approval,
            }],
            "checks": checks,
            "outputs": saved.iter().map(|f| f.handle.clone()).collect::<Vec<_>>(),
            "summary": result.get("summary"),
            "server": format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
        });
        let _ = self.write_provenance(&prov, &provenance);
        rec.prov = Some(prov.clone());
        self.audit(ctx, rec)?;
        Ok(Json(Output {
            result: RunToolResult {
                provider: p.id().to_string(),
                tool: tool.to_string(),
                status: "done".into(),
                approval: None,
                external: None,
                summary: result.get("summary").cloned(),
                saved,
            },
            checks,
            provenance: Some(prov),
        }))
    }
}
