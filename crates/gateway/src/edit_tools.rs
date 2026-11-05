//! Edits (design section 3): `propose_edits` makes a reviewable change set
//! and writes nothing; `apply_edits` applies one to a workspace copy, or
//! to the source after the person approves it.
//!
//! The rules for writing to the source, all of which must hold:
//! * organization policy allows edits (`allow_edits: true`) and the
//!   connection is read-write (the effective mode, after policy, scope and
//!   data groups, is `read-write`);
//! * the connection type is one Columbus can edit (GeoPackage, PostGIS);
//! * the change set was made by `propose_edits` in this session and is
//!   unchanged (SHA-256);
//! * the person approves this exact change set (every time, never "for the
//!   session");
//! * in the database, in one transaction, every row touched is exactly as
//!   it was when the change set was made, every write is limited to its key
//!   and the connection's row filter and touches exactly one row, and every
//!   row written is still inside the row filter afterwards.
//!
//! Fields hidden by policy cannot be read or set; the key cannot be set;
//! values travel as query parameters, never as SQL text.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Local;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use columbus_shared::approvals::Risk;
use columbus_shared::audit::AuditRecord;
use columbus_shared::policy::{Connection, EffectiveAccess, Mode};
use columbus_shared::sqlcheck::{check_select, SqlDialect};

use crate::approval_flow::{Ask, Sought};
use crate::gis_tools::{dataset_sql, file_stem, quote, Prov};
use crate::providers::{OutputMeta, OutputSource};
use crate::server::{new_prov_id, Check, Gateway, Output, Severity};

/// Connection types whose datasets can be edited.
const EDITABLE: &[&str] = &["gpkg", "geopackage", "postgis", "postgres", "postgresql"];

/// At most this many changes in one change set.
const MAX_CHANGES: usize = 1000;

/// A write to the source shows the person every change: at most this many
/// lines, or the change set must be split.
const MAX_REVIEW_LINES: u64 = 200;

/// A change set made in this session.
#[derive(Clone)]
pub(crate) struct ChangeSet {
    pub connection: String,
    pub dataset: String,
    pub key: String,
    pub keys: Vec<Value>,
    pub sha256: String,
    pub counts: Value,
    pub diff: Value,
    pub diff_total: u64,
    pub labels: Vec<String>,
}

pub(crate) type ChangeSets = std::sync::Mutex<HashMap<PathBuf, ChangeSet>>;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposeArgs {
    /// The connection id (see list_connections).
    pub connection: String,
    /// The dataset to edit (see list_datasets).
    pub dataset: String,
    /// The changes, at most 1000. Each is one of:
    /// {"op": "update", "key": <row key>, "values": {"FIELD": value, ...}, "geometry": <GeoJSON, optional>},
    /// {"op": "insert", "values": {...}, "geometry": <GeoJSON>},
    /// {"op": "delete", "key": <row key>}.
    /// The key is the dataset's primary key (for a GeoPackage, its fid);
    /// describe_dataset and query_features show it when asked for it.
    pub changes: Vec<Value>,
    /// The coordinate reference system of the GeoJSON geometries, e.g.
    /// "EPSG:4326" (the default) or the dataset's own.
    #[serde(default)]
    pub crs: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyArgs {
    /// The change set's handle, from propose_edits.
    pub change: String,
    /// "copy": a new layer in the workspace with the changes made (the
    /// source is not touched; no approval). "source": write to the dataset
    /// itself (needs edits allowed by policy, a read-write connection and
    /// the person's approval).
    pub target: String,
    /// The approval id, when an earlier call returned approval_required.
    #[serde(default)]
    pub approval_id: Option<String>,
}

/// A value as a SQL literal for the key list of a checked SELECT (the
/// gateway's parser reads it back, so nothing here becomes SQL by itself).
fn literal(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
        // No backslash: whatever a database makes of one in a literal.
        Value::String(s) if s.len() <= 200 && !s.contains('\0') && !s.contains('\\') => {
            Some(format!("'{}'", s.replace('\'', "''")))
        }
        _ => None,
    }
}

/// A value as the person reads it, whole (values are at most 500
/// characters): text JSON-quoted, so a quote, a line break or an arrow in it
/// cannot pass for the card's own layout, and invisible or direction
/// characters written as escapes.
fn shown(v: &Value) -> String {
    let s = match v {
        Value::Null => return "(empty)".into(),
        Value::Object(o) => {
            return o
                .iter()
                .map(|(k, v)| format!("{} {}", visible(k), shown(v)))
                .collect::<Vec<_>>()
                .join(", ")
        }
        other => other.to_string(),
    };
    visible(&s)
}

/// Characters that do not show, or that reorder text, as `\u{…}`.
fn visible(s: &str) -> String {
    s.chars()
        .map(|c| {
            let hidden = c.is_control()
                || matches!(c as u32, 0xAD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180E | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0 | 0xFFF9..=0xFFFB | 0xE0000..=0xE0FFF);
            if hidden {
                format!("\\u{{{:04X}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "3 updates, 1 insertion and 2 deletions".
fn counts_text(c: &Value) -> String {
    let n = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0);
    let parts: Vec<String> = [
        (n("update"), "update", "updates"),
        (n("insert"), "insertion", "insertions"),
        (n("delete"), "deletion", "deletions"),
    ]
    .into_iter()
    .filter(|(k, _, _)| *k > 0)
    .map(|(k, a, b)| plural(k, a, b))
    .collect();
    match parts.len() {
        0 => "no changes".into(),
        1 => parts[0].clone(),
        _ => format!(
            "{} and {}",
            parts[..parts.len() - 1].join(", "),
            parts[parts.len() - 1]
        ),
    }
}

/// Plain lines for the person: what each change does.
fn diff_lines(diff: &Value, key: &str, max: usize) -> Vec<String> {
    diff.as_array()
        .map(|a| {
            a.iter()
                .take(max)
                .map(|d| {
                    let op = d["op"].as_str().unwrap_or("");
                    match op {
                        "update" => format!(
                            "{key} {}: {} {} → {}",
                            shown(&d["key"]),
                            visible(d["field"].as_str().unwrap_or("?")),
                            shown(&d["before"]),
                            shown(&d["after"])
                        ),
                        "insert" => {
                            let fields = shown(&d["after"]);
                            format!(
                                "New row: {}",
                                if fields.is_empty() {
                                    "(no values)".into()
                                } else {
                                    fields
                                }
                            )
                        }
                        // What is deleted, not only its key: a deletion is
                        // the one change that cannot be undone.
                        _ => {
                            let mut row = d["before"].clone();
                            if let Some(o) = row.as_object_mut() {
                                o.remove(key);
                            }
                            format!("Delete {key} {}: {}", shown(&d["key"]), shown(&row))
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Gateway {
    /// The connection and dataset, checked as editable at all (any mode).
    fn edit_target(
        &self,
        connection: &str,
        dataset: &str,
    ) -> Result<(EffectiveAccess, Connection), String> {
        let (access, conn) = self.access(connection).map_err(|e| e.to_string())?;
        if !EDITABLE.iter().any(|k| k.eq_ignore_ascii_case(&conn.kind)) {
            return Err(format!(
                "edits are not supported on {} connections yet (GeoPackage and PostGIS are)",
                conn.kind
            ));
        }
        crate::gis_tools::dataset_ok(dataset)?;
        if !access.dataset_allowed(dataset) {
            return Err(format!(
                "dataset {dataset:?} is not in this connection's allowed datasets"
            ));
        }
        Ok((access, conn.clone()))
    }

    /// The checked SELECT of the key, every visible field and the geometry,
    /// for the given keys (or every row when `keys` is None), with the
    /// connection's row filter applied by the checker.
    #[allow(clippy::too_many_arguments)] // each is a separate part of one checked query
    async fn edit_select(
        &self,
        access: &EffectiveAccess,
        conn: &Connection,
        dataset: &str,
        info: &Value,
        key: &str,
        keys: Option<&[Value]>,
        max_rows: u64,
    ) -> Result<String, String> {
        let dialect = SqlDialect::for_connection_type(&conn.kind);
        let mut cols = vec![quote(key, dialect)];
        for f in Self::visible_fields(info) {
            if f != key {
                cols.push(quote(&f, dialect));
            }
        }
        if let Some(g) = info.get("geometry_column").and_then(Value::as_str) {
            cols.push(quote(g, dialect));
        }
        let mut sql = format!(
            "SELECT {} FROM {}",
            cols.join(", "),
            dataset_sql(dataset, dialect)?
        );
        if let Some(keys) = keys {
            let lits: Option<Vec<String>> = keys.iter().map(literal).collect();
            let lits = lits.ok_or("each key must be a number or a short text")?;
            sql.push_str(&format!(
                " WHERE {} IN ({})",
                quote(key, dialect),
                lits.join(", ")
            ));
        }
        let catalog = self.table_columns(access, conn).await?;
        check_select(&sql, access, max_rows, &catalog)
            .map(|c| c.sql)
            .map_err(|e| e.to_string())
    }

    /// The visible fields other than the geometry column, which is only
    /// ever set through a change's checked `geometry`, never as a value.
    fn visible_fields(info: &Value) -> Vec<String> {
        let geom = info.get("geometry_column").and_then(Value::as_str);
        info.get("fields")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.get("name").and_then(Value::as_str).map(String::from))
                    .filter(|n| !geom.is_some_and(|g| g.eq_ignore_ascii_case(n)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The dataset's key column and column types, from the database.
    async fn edit_schema(
        &self,
        access: &EffectiveAccess,
        conn: &Connection,
        dataset: &str,
    ) -> Result<(String, String, Value), String> {
        let dialect = SqlDialect::for_connection_type(&conn.kind);
        let handle = self.handle(conn)?;
        let timeout = access.limits.seconds.unwrap_or(120);
        let s = self
            .state
            .worker
            .core(
                "edit_schema",
                json!({"connection": handle, "table_sql": dataset_sql(dataset, dialect)?, "timeout_s": timeout}),
                Duration::from_secs(timeout + 10),
            )
            .await
            .map_err(|e| e.to_string())?;
        let pk: Vec<String> = s["primary_key"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let key = match pk.as_slice() {
            [k] => k.clone(),
            [] => {
                return Err(format!(
                    "{dataset} has no primary key, so its rows cannot be named for editing"
                ))
            }
            _ => {
                return Err(format!(
                    "{dataset} has a key of several columns ({}); edits need a single-column key",
                    pk.join(", ")
                ))
            }
        };
        if access.field_hidden(&key) {
            return Err(format!(
                "{dataset}'s key is hidden by organization policy, so it cannot be edited"
            ));
        }
        let key_type = s["columns"][&key].as_str().unwrap_or("").to_string();
        Ok((key, key_type, s["columns"].clone()))
    }

    pub(crate) fn changes(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, ChangeSet>> {
        self.state.edits.lock().expect("change sets")
    }
}

#[tool_router(router = edit_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Propose edits to a dataset: updates (fields and/or geometry), insertions and deletions, by the dataset's primary key. Writes nothing to the source. The server checks every change against the connection's rules (allowed datasets, its row filter, fields hidden by policy, column types, geometry type and validity; geometries are GeoJSON, in EPSG:4326 unless crs says otherwise, and are reprojected to the dataset's CRS), reads the rows as they are now, and saves a change set plus a preview layer (each row as it will be, deletions as they were) to the workspace. Returns the change set's handle, a before → after list for the person to review, and the counts. Then apply_edits with target \"copy\" (a new workspace layer) or \"source\" (needs the person's approval).",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn propose_edits(
        &self,
        Parameters(args): Parameters<ProposeArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({"connection": args.connection, "dataset": args.dataset,
                             "changes": args.changes.len(), "crs": args.crs});
        let mut rec = self.gate(&ctx, "propose_edits", &request)?;
        rec.connection = Some(args.connection.clone());
        rec.dataset = Some(args.dataset.clone());
        let started = Local::now();
        let prov = new_prov_id();
        let r = async {
            if args.changes.is_empty() {
                return Err("no changes given".to_string());
            }
            if args.changes.len() > MAX_CHANGES {
                return Err(format!("at most {MAX_CHANGES} changes at a time; split the edit"));
            }
            let (access, conn) = self.edit_target(&args.connection, &args.dataset)?;
            let info = self.dataset_info(&access, &conn, &args.dataset, false).await?;
            let (key, key_type, types) = self.edit_schema(&access, &conn, &args.dataset).await?;
            // The fields that may be set: visible ones the database knows.
            let mut fields = serde_json::Map::new();
            for f in Self::visible_fields(&info) {
                if f != key {
                    if let Some(t) = types.get(&f) {
                        fields.insert(f, t.clone());
                    }
                }
            }
            let keys: Vec<Value> = args
                .changes
                .iter()
                .filter(|c| matches!(c.get("op").and_then(Value::as_str), Some("update" | "delete")))
                .filter_map(|c| c.get("key").cloned())
                .collect();
            let select = if keys.is_empty() {
                None
            } else {
                Some(
                    self.edit_select(&access, &conn, &args.dataset, &info, &key, Some(&keys), keys.len() as u64 + 1)
                        .await?,
                )
            };
            let stem = format!("results/{}-edits-{prov}", file_stem(&args.dataset));
            let change_path = self
                .state
                .sandbox
                .resolve(&format!("edits/{prov}.json"))
                .map_err(|e| e.to_string())?;
            let preview_stem = self.state.sandbox.resolve(&stem).map_err(|e| e.to_string())?;
            let timeout = access.limits.seconds.unwrap_or(120);
            let reply = self
                .state
                .worker
                .core(
                    "edit_prepare",
                    json!({
                        "connection": self.handle(&conn)?,
                        "connection_id": conn.id, "dataset": args.dataset,
                        "select_sql": select, "key": key, "key_type": key_type,
                        "fields": fields, "geometry_column": info.get("geometry_column"),
                        "geometry_type": info.get("geometry_type"),
                        "dimensions": info.get("dimensions"),
                        "crs_spec": info.get("crs").and_then(|c| c.get("code")).map(|c| json!({"code": c}))
                            .or_else(|| info.get("crs_spec").cloned()),
                        "input_crs": args.crs.as_ref().map(|c| json!({"code": c})),
                        "changes": args.changes, "change_path": change_path,
                        "preview_stem": preview_stem, "timeout_s": timeout,
                    }),
                    Duration::from_secs(timeout + 30),
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((conn, key, keys, reply, change_path))
        }
        .await;
        let (conn, key, keys, reply, change_path) = match r {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        self.note_read(
            &conn.id,
            None,
            Some(keys.len() as u64),
            Some(&prov),
            "propose_edits",
        );
        let sha = reply["sha256"].as_str().unwrap_or("").to_string();
        let counts = reply["counts"].clone();
        let handle = self.ws_handle(&change_path).unwrap_or_default();
        let preview = reply["preview_path"].as_str().map(PathBuf::from);
        if let Some(p) = &preview {
            self.state
                .flow
                .record_file(p, &self.state.flow.session_labels());
            self.state.outputs.record(
                p,
                OutputMeta {
                    made_by: "propose_edits".into(),
                    provenance: Some(prov.clone()),
                    rows: reply["preview_summary"]["rows"].as_u64(),
                    source: OutputSource::Engine,
                    truncated: false,
                    inherited: vec![],
                    sha256: None,
                },
            );
        }
        let labels = self.labels();
        self.changes().insert(
            change_path.clone(),
            ChangeSet {
                connection: conn.id.clone(),
                dataset: args.dataset.clone(),
                key: key.clone(),
                keys,
                sha256: sha.clone(),
                counts: counts.clone(),
                diff: reply["diff"].clone(),
                diff_total: reply["diff_total"].as_u64().unwrap_or(0),
                labels: labels.clone(),
            },
        );
        let lines = diff_lines(&reply["diff"], &key, 50);
        let checks = vec![Check {
            id: "edits.proposed".into(),
            severity: Severity::Info,
            message: format!(
                "Proposed {} to {} on {}. Nothing is written yet: apply_edits with target \"copy\" or \"source\"",
                counts_text(&counts),
                args.dataset,
                conn.id
            ),
            detail: None,
            fix: None,
        }];
        let preview_handle = preview.as_ref().and_then(|p| self.ws_handle(p));
        self.provenance(
            &ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!([{"connection": conn.id, "dataset": args.dataset, "request": request}]),
                steps: json!([{"tool": "propose_edits", "change_set": handle, "sha256": sha, "counts": counts}]),
                labels: labels.clone(),
                checks: &checks,
                outputs: preview_handle.iter().cloned().chain([handle.clone()]).collect(),
            },
        );
        rec.prov = Some(prov.clone());
        rec.labels = labels;
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({
                "change": handle,
                "sha256": sha,
                "key": key,
                "counts": counts,
                "changes": lines,
                "more_changes": reply["diff_total"].as_u64().unwrap_or(0).saturating_sub(50),
                "preview": preview_handle,
                "next": "Show the person the changes, then apply_edits with target \"copy\" (a new layer) or \"source\" (writes the dataset; the person approves)",
            }),
            checks,
            provenance: Some(prov),
        }))
    }

    #[tool(
        description = "Apply a change set from propose_edits. target \"copy\": a new layer in the workspace holding every row this connection may read with the changes made; the source is not touched and no approval is needed. target \"source\": writes the dataset itself, in one transaction, only when organization policy allows edits and the connection is read-write, and only after the person approves this exact change set; any row that changed since the change set was made, or a change that would move a row outside the connection's row filter, rolls everything back.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn apply_edits(
        &self,
        Parameters(args): Parameters<ApplyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({"change": args.change, "target": args.target});
        let mut rec = self.gate(&ctx, "apply_edits", &request)?;
        let started = Local::now();
        let prov = new_prov_id();
        let path = match crate::providers::resolve_handle(
            &self.state.session,
            &self.state.sandbox,
            &args.change,
        ) {
            Ok(p) => p,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let Some(cs) = self.changes().get(&path).cloned() else {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "{} is not a change set propose_edits made in this session",
                    args.change
                ),
            ));
        };
        rec.connection = Some(cs.connection.clone());
        rec.dataset = Some(cs.dataset.clone());
        let target = args.target.as_str();
        if !matches!(target, "copy" | "source") {
            return Err(self.deny(
                &ctx,
                rec,
                "target must be \"copy\" or \"source\"".to_string(),
            ));
        }
        let (access, conn) = match self.edit_target(&cs.connection, &cs.dataset) {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let info = match self.dataset_info(&access, &conn, &cs.dataset, false).await {
            Ok(i) => i,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let timeout = access.limits.seconds.unwrap_or(120);
        let crs_spec = info
            .get("crs")
            .and_then(|c| c.get("code"))
            .map(|c| json!({"code": c}))
            .or_else(|| info.get("crs_spec").cloned());

        if target == "copy" {
            let read_limit = access.limits.rows.unwrap_or(200_000);
            let r = async {
                let all = self
                    .edit_select(&access, &conn, &cs.dataset, &info, &cs.key, None, read_limit + 1)
                    .await?;
                let stem = format!("results/{}-edited-{prov}", file_stem(&cs.dataset));
                let out_stem = self.state.sandbox.resolve(&stem).map_err(|e| e.to_string())?;
                self.state
                    .worker
                    .core(
                        "edit_apply",
                        json!({"target": "copy", "connection": self.handle(&conn)?, "change_path": path,
                               "sha256": cs.sha256, "select_all_sql": all, "max_rows": read_limit,
                               "crs_spec": crs_spec, "out_stem": out_stem, "timeout_s": timeout}),
                        Duration::from_secs(timeout + 30),
                    )
                    .await
                    .map_err(|e| e.to_string())
            }
            .await;
            let reply = match r {
                Ok(v) => v,
                Err(e) => return Err(self.deny(&ctx, rec, e)),
            };
            let out = PathBuf::from(reply["path"].as_str().unwrap_or(""));
            self.note_read(
                &conn.id,
                Some(&out),
                reply["rows"].as_u64(),
                Some(&prov),
                "apply_edits",
            );
            let handle = self.ws_handle(&out);
            let checks: Vec<Check> = vec![];
            self.provenance(
                &ctx,
                Prov {
                    id: &prov,
                    started,
                    inputs: json!([{"connection": conn.id, "dataset": cs.dataset, "change_set": args.change, "sha256": cs.sha256}]),
                    steps: json!([{"tool": "apply_edits", "target": "copy", "counts": cs.counts}]),
                    labels: self.labels(),
                    checks: &checks,
                    outputs: handle.iter().cloned().collect(),
                },
            );
            rec.rows = reply["rows"].as_u64();
            rec.prov = Some(prov.clone());
            rec.labels = self.labels();
            self.audit(&ctx, rec)?;
            return Ok(Json(Output {
                result: json!({"status": "applied_to_copy", "layer": handle, "summary": reply["summary"],
                               "counts": cs.counts, "source_written": false}),
                checks,
                provenance: Some(prov),
            }));
        }

        // Writing the source.
        if !self.state.policy.allow_edits {
            return Err(self.deny(
                &ctx,
                rec,
                "organization policy does not allow edits (allow_edits); apply to a copy instead"
                    .to_string(),
            ));
        }
        if access.mode != Mode::ReadWrite {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "{} is read-only, so its datasets cannot be edited; apply to a copy instead",
                    conn.id
                ),
            ));
        }
        if cs.diff_total > MAX_REVIEW_LINES {
            return Err(self.deny(&ctx, rec, format!(
                "this change set has {} lines of changes; the person reviews every one before a write, so split it into change sets of at most {MAX_REVIEW_LINES} (or apply to a copy)",
                cs.diff_total
            )));
        }
        // The organization's flow rules: data read in this session, written
        // into this connection. Its own data going back is not a flow.
        let own = columbus_flow::connection_label(&conn.id);
        let mut labels: std::collections::BTreeSet<String> = cs.labels.iter().cloned().collect();
        labels.extend(self.labels());
        labels.remove(&own);
        let dest = format!("connection:{}", conn.id);
        let decision = columbus_flow::evaluate_to(&self.state.policy.flows, &labels, &dest);
        rec.flow_decision = Some(decision.label().into());
        if let columbus_flow::FlowDecision::Deny { labels, rule } = &decision {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "organization flow rule {} forbids writing data from {} into {}",
                    rule + 1,
                    labels.join(", "),
                    conn.id
                ),
            ));
        }
        let from: Vec<String> = labels
            .iter()
            .map(|l| l.trim_start_matches("connection:").to_string())
            .collect();
        let lines = diff_lines(&cs.diff, &cs.key, MAX_REVIEW_LINES as usize);
        let more = cs.diff_total.saturating_sub(lines.len() as u64);
        let mut summary = format!(
            "Write {} to {} on {} ({}), in one transaction. Rows changed since the edit was proposed, or changes outside the connection's row filter, roll it all back.",
            counts_text(&cs.counts),
            cs.dataset,
            conn.id,
            conn.kind,
        );
        if !from.is_empty() {
            summary.push_str(&format!(" Data in play came from: {}.", from.join(", ")));
        }
        summary.push_str("\n\nThe changes:\n");
        summary.push_str(&lines.join("\n"));
        if more > 0 {
            summary.push_str(&format!(
                "\n… and {more} more (in the change set {})",
                args.change
            ));
        }
        let bound = json!({"change": args.change, "sha256": cs.sha256, "target": "source"});
        let detail = json!({
            "files": [{"name": args.change, "rows": cs.keys.len()}],
            "labels": from,
            "arguments": {"dataset": cs.dataset, "connection": conn.id, "changes": counts_text(&cs.counts), "sha256": cs.sha256},
            "changes": lines,
        });
        let label = format!("edits to {}", cs.dataset);
        let ask = Ask {
            tool: "apply_edits",
            label: &label,
            request: &bound,
            risk: Risk::Edit,
            summary,
            detail: Some(detail),
            approval_id: args.approval_id.as_deref(),
            every_time: true,
            code: None,
        };
        match self.seek_approval(&ctx, ask).await {
            Ok(Sought::Granted(id)) => rec.approval = Some(id),
            Ok(Sought::Declined(msg)) => return Err(self.deny(&ctx, rec, msg)),
            Ok(Sought::Pending(needed)) => {
                rec.approval = Some(needed.id.clone());
                self.audit(
                    &ctx,
                    AuditRecord {
                        decision: "approval_required".into(),
                        ..rec
                    },
                )?;
                return Ok(Json(Output {
                    result: json!({"status": "approval_required", "approval": needed}),
                    checks: vec![],
                    provenance: None,
                }));
            }
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        }
        // Used up now, before the write: whatever happens next (a timeout
        // after the database committed, say), it cannot be applied twice.
        if self.changes().remove(&path).map(|c| c.sha256) != Some(cs.sha256.clone()) {
            return Err(self.deny(
                &ctx,
                rec,
                "the change set is no longer available; propose it again".to_string(),
            ));
        }
        let r = async {
            let dialect = SqlDialect::for_connection_type(&conn.kind);
            let select = if cs.keys.is_empty() {
                None
            } else {
                Some(
                    self.edit_select(&access, &conn, &cs.dataset, &info, &cs.key, Some(&cs.keys), cs.keys.len() as u64 + 1)
                        .await?,
                )
            };
            let mut columns = serde_json::Map::new();
            for f in Self::visible_fields(&info) {
                if f != cs.key {
                    columns.insert(f.clone(), json!(quote(&f, dialect)));
                }
            }
            let mut handle = self.handle(&conn)?;
            handle["write"] = json!(true);
            let table_name = cs.dataset.rsplit('.').next().unwrap_or(&cs.dataset).to_string();
            self.state
                .worker
                .core(
                    "edit_apply",
                    json!({
                        "target": "source", "connection": handle, "change_path": path, "sha256": cs.sha256,
                        "select_sql": select, "table_sql": dataset_sql(&cs.dataset, dialect)?,
                        "table_name": table_name, "key_sql": quote(&cs.key, dialect),
                        "columns_sql": columns,
                        "geometry_sql": info.get("geometry_column").and_then(Value::as_str).map(|g| quote(g, dialect)),
                        "srid": info.get("srs_id").or_else(|| info.get("srid")),
                        "gpkg_srs_id": info.get("srs_id"),
                        "filters": access.filters_for(&cs.dataset),
                        "timeout_s": timeout,
                    }),
                    Duration::from_secs(timeout + 30),
                )
                .await
                .map_err(|e| e.to_string())
        }
        .await;
        let reply = match r {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        // The dataset changed: forget its cached description.
        self.state
            .datasets
            .lock()
            .expect("datasets")
            .remove(&(conn.id.clone(), cs.dataset.clone()));
        let written = reply["written"].clone();
        let checks = vec![Check {
            id: "edits.written".into(),
            severity: Severity::Info,
            message: format!(
                "Wrote {} to {} on {}",
                counts_text(&written),
                cs.dataset,
                conn.id
            ),
            detail: None,
            fix: None,
        }];
        self.provenance(
            &ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!([{"connection": conn.id, "dataset": cs.dataset, "change_set": args.change, "sha256": cs.sha256}]),
                steps: json!([{"tool": "apply_edits", "target": "source", "written": written,
                               "inserted_keys": reply["inserted_keys"], "approval": rec.approval}]),
                labels: self.labels(),
                checks: &checks,
                outputs: vec![],
            },
        );
        rec.rows = Some(cs.keys.len() as u64);
        rec.prov = Some(prov.clone());
        rec.labels = self.labels();
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({"status": "written", "dataset": cs.dataset, "connection": conn.id,
                           "written": written, "inserted_keys": reply["inserted_keys"]}),
            checks,
            provenance: Some(prov),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_cannot_become_sql() {
        assert_eq!(literal(&json!(12)).as_deref(), Some("12"));
        assert_eq!(literal(&json!("O'Neil")).as_deref(), Some("'O''Neil'"));
        assert!(literal(&json!(1.5)).is_none());
        assert!(literal(&json!(true)).is_none());
        assert!(literal(&json!({"a": 1})).is_none());
    }

    #[test]
    fn counts_read_as_words() {
        assert_eq!(
            counts_text(&json!({"update": 3, "insert": 1, "delete": 2})),
            "3 updates, 1 insertion and 2 deletions"
        );
        assert_eq!(
            counts_text(&json!({"update": 1, "insert": 0, "delete": 0})),
            "1 update"
        );
    }

    #[test]
    fn values_the_ai_chose_cannot_pass_for_the_card() {
        // Quotes and line breaks escaped: no fake "→" or extra lines.
        assert_eq!(shown(&json!("x\" → \"y")), r#""x\" → \"y""#);
        assert_eq!(shown(&json!("a\nb")), r#""a\nb""#);
        // Direction and invisible characters as escapes.
        assert_eq!(
            shown(&json!("ab\u{202E}cd\u{200B}")),
            r#""ab\u{202E}cd\u{200B}""#
        );
        assert_eq!(shown(&Value::Null), "(empty)");
        // Whole values: never cut.
        assert_eq!(shown(&json!("x".repeat(400))).len(), 402);
    }
}
