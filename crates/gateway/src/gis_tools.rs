//! Core GIS tools (design section 3, "Tool catalogue"): describe a
//! dataset, look at a few rows, query with structured arguments, process
//! layers, and verify them. Results are layers in the session workspace,
//! passed between tools as handles; every result carries its checks
//! (design section 4) and a provenance record.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Local;
use columbus_shared::audit::AuditRecord;
use columbus_shared::policy::{Connection, EffectiveAccess};
use columbus_shared::sqlcheck::{check_select, check_select_unlimited, check_where, SqlDialect};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::providers::{OutputMeta, OutputSource};
use crate::server::{
    checks_from, client_name, new_prov_id, out, Check, Gateway, Output, Severity, SERVER_NAME,
};

/// Most rows `sample_rows` returns.
const SAMPLE_MAX: u64 = 50;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DatasetArgs {
    /// Connection id.
    pub connection: String,
    /// Dataset name as `list_datasets` shows it.
    pub dataset: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SampleArgs {
    pub connection: String,
    pub dataset: String,
    /// How many rows, at most 50. Default 10.
    #[serde(default)]
    pub n: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QueryArgs {
    /// Connection id.
    pub connection: String,
    /// Dataset name as `list_datasets` shows it.
    pub dataset: String,
    /// Fields to return (names from `describe_dataset`). Default: all. The
    /// geometry is always included when the dataset has one.
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    /// A filter: comparisons, AND/OR/NOT, IN, LIKE, BETWEEN, IS NULL and
    /// simple functions. No subqueries. The connection's own row filter is
    /// added by the server; do not repeat it.
    #[serde(default, rename = "where")]
    pub where_: Option<String>,
    /// [min x, min y, max x, max y]: keep features whose geometry meets
    /// this box.
    #[serde(default)]
    pub bbox: Option<Vec<f64>>,
    /// CRS of `bbox`, e.g. "EPSG:4326". Default: the dataset's own CRS.
    #[serde(default)]
    pub bbox_crs: Option<String>,
    /// A field to sort by.
    #[serde(default)]
    pub order_by: Option<String>,
    /// Most rows to read. Default and maximum: the connection's read limit.
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct VectorOpArgs {
    /// One of: reproject, buffer, simplify, make_valid, clip, dissolve,
    /// intersect, join, spatial_join.
    pub op: String,
    /// Input layers by role. reproject, buffer, simplify, make_valid and
    /// dissolve take `layer`; clip takes `layer` and `mask`; intersect takes
    /// `a` and `b`; join and spatial_join take `left` and `right`. Each is a
    /// workspace handle (ws://…) from an earlier result, or a dataset as
    /// "<connection>:<dataset>" or {"connection", "dataset", "where"}.
    pub inputs: serde_json::Map<String, Value>,
    /// reproject: {crs: "EPSG:…"}. buffer: {distance, unit: m|ft|us-ft|km|mi,
    /// dissolve?}. simplify: {tolerance, unit}. dissolve: {by?}. join:
    /// {left_key, right_key?, how: left|inner, key_as_text?, trim?,
    /// ignore_case?, pad? (zero-fill width)}. spatial_join: {predicate:
    /// intersects|within|contains|touches|crosses|overlaps, how}.
    #[serde(default)]
    pub params: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct VerifyArgs {
    /// A workspace handle (ws://…) or a dataset as "<connection>:<dataset>".
    pub layer: Value,
}

/// A layer saved in the session workspace.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LayerOut {
    /// Pass this to vector_op, verify or another provider's tool.
    pub handle: String,
    /// Rows, geometry types, CRS, extent and fields.
    pub summary: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryOut {
    pub columns: Vec<String>,
    /// The first rows, up to the per-call limit; geometry is summarized.
    pub rows: Vec<Vec<Value>>,
    pub rows_read: u64,
    pub rows_returned: u64,
    /// Every row read, as a layer in the workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerOut>,
    /// The SQL the server ran, with the row filter and limit it added.
    pub executed_sql: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct VectorOpOut {
    pub op: String,
    pub layer: LayerOut,
}

/// A dataset reference: `conn:dataset` or `{connection, dataset, where}`.
fn dataset_ref(v: &Value) -> Option<(String, String, Option<String>)> {
    match v {
        Value::String(s) if !s.starts_with(crate::providers::HANDLE_SCHEME) => {
            let (c, d) = s.split_once(':')?;
            Some((c.trim().to_string(), d.trim().to_string(), None))
        }
        Value::Object(m) => Some((
            m.get("connection")?.as_str()?.to_string(),
            m.get("dataset")?.as_str()?.to_string(),
            m.get("where").and_then(Value::as_str).map(String::from),
        )),
        _ => None,
    }
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// A dataset name the GIS tools put into SQL: one to three plain
/// identifiers separated by dots (`roads`, `roads.roadway_inventory`). Only
/// letters, digits, `_` and `$`, so nothing in it can change the statement.
fn dataset_ok(d: &str) -> Result<(), String> {
    let parts: Vec<&str> = d.split('.').collect();
    if parts.len() > 3 || !parts.iter().all(|p| ident_ok(p)) {
        return Err(format!(
            "{d:?} is not a dataset name the GIS tools accept (letters, digits and _ separated by dots); use the name list_datasets shows, or sql_select"
        ));
    }
    Ok(())
}

fn quote(name: &str, d: SqlDialect) -> String {
    match d {
        SqlDialect::MsSql => format!("[{name}]"),
        _ => format!("\"{name}\""),
    }
}

fn epsg_ok(s: &str) -> bool {
    s.strip_prefix("EPSG:")
        .is_some_and(|n| !n.is_empty() && n.len() <= 6 && n.chars().all(|c| c.is_ascii_digit()))
}

/// A structured query, before it is checked.
struct Plan {
    sql: String,
    bbox: Option<Vec<f64>>,
    bbox_crs: Option<String>,
}

/// A provenance record for one tool call (design section 4).
pub(crate) struct Prov<'a> {
    pub(crate) id: &'a str,
    pub(crate) started: chrono::DateTime<Local>,
    pub(crate) inputs: Value,
    pub(crate) steps: Value,
    pub(crate) labels: Vec<String>,
    pub(crate) checks: &'a [Check],
    pub(crate) outputs: Vec<String>,
}

/// One input to vector_op or verify, ready for the worker.
pub(crate) struct Input {
    pub(crate) path: PathBuf,
    /// For the provenance record.
    pub(crate) record: Value,
    /// A check to pass on (an input that was cut off at a limit).
    pub(crate) warning: Option<Check>,
    /// (connection, dataset, rows) when the input was read from a dataset.
    pub(crate) read: Option<(String, String, u64)>,
}

/// What one governed read produced.
struct Read {
    reply: Value,
    executed_sql: String,
    path: Option<PathBuf>,
    rows_read: u64,
    hit_limit: bool,
    info: Value,
    filters: Vec<String>,
    /// The read limit this read ran under.
    max_rows: u64,
    /// Hidden fields left out by the server.
    hidden: Vec<String>,
}

impl Gateway {
    /// `describe_dataset`'s answer for a dataset, cached for the session.
    async fn dataset_info(
        &self,
        access: &EffectiveAccess,
        conn: &Connection,
        dataset: &str,
        refresh: bool,
    ) -> Result<Value, String> {
        dataset_ok(dataset)?;
        let key = (conn.id.clone(), dataset.to_string());
        if !refresh {
            if let Some(v) = self.state.datasets.lock().expect("datasets").get(&key) {
                return Ok(v.clone());
            }
        }
        let cols = self.table_columns(access, conn).await?;
        let count = check_select(
            &format!("SELECT COUNT(*) AS n FROM {dataset}"),
            access,
            2,
            &cols,
        )
        .map_err(|e| e.to_string())?;
        let handle = self.handle(conn)?;
        let timeout = access.limits.seconds.unwrap_or(120);
        let info = self
            .state
            .worker
            .core(
                "describe_dataset",
                json!({
                    "connection": handle, "dataset": dataset, "count_sql": count.sql,
                    "hide_fields": self.state.policy.hide_fields, "timeout_s": timeout,
                }),
                Duration::from_secs(timeout + 10),
            )
            .await
            .map_err(|e| e.to_string())?;
        let info = Self::visible_info(info, access, dataset);
        self.state
            .datasets
            .lock()
            .expect("datasets")
            .insert(key, info.clone());
        Ok(info)
    }

    /// The worker's description with the gateway's own reading of the
    /// policy applied on top: hidden fields (and a hidden geometry column)
    /// removed, and the stored extent dropped when a row filter applies,
    /// because it covers rows the filter excludes.
    fn visible_info(mut info: Value, access: &EffectiveAccess, dataset: &str) -> Value {
        let Some(o) = info.as_object_mut() else {
            return info;
        };
        if let Some(Value::Array(fields)) = o.get_mut("fields") {
            fields.retain(|f| {
                f.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| !access.field_hidden(n))
            });
        }
        let geom_hidden = o
            .get("geometry_column")
            .and_then(Value::as_str)
            .is_some_and(|g| access.field_hidden(g));
        if geom_hidden {
            for k in [
                "geometry_column",
                "geometry_type",
                "crs",
                "crs_spec",
                "extent",
            ] {
                o.remove(k);
            }
        }
        if !access.filters_for(dataset).is_empty() && o.get("extent").is_some_and(|e| !e.is_null())
        {
            o.insert("extent".into(), Value::Null);
            o.insert(
                "extent_note".into(),
                json!("The stored extent covers rows outside this connection's row filter, so it is not shown; query_features reports the extent of the rows you can read."),
            );
        }
        info
    }

    fn field_names(info: &Value) -> Vec<String> {
        info.get("fields")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.get("name").and_then(Value::as_str).map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Turn structured arguments into SQL, refusing anything not allowed.
    fn plan(
        &self,
        info: &Value,
        dataset: &str,
        dialect: SqlDialect,
        q: &QueryArgs,
    ) -> Result<Plan, String> {
        let known = Self::field_names(info);
        let geom = info.get("geometry_column").and_then(Value::as_str);
        let find = |f: &str| -> Result<String, String> {
            if !ident_ok(f) {
                return Err(format!("{f:?} is not a field name"));
            }
            known
                .iter()
                .find(|k| k.as_str() == f)
                .or_else(|| known.iter().find(|k| k.eq_ignore_ascii_case(f)))
                .cloned()
                .or_else(|| geom.filter(|g| g.eq_ignore_ascii_case(f)).map(String::from))
                .ok_or_else(|| format!("{dataset} has no field {f:?} (or it is hidden by organization policy); see describe_dataset"))
        };
        let cols = match &q.fields {
            Some(fields) if !fields.is_empty() => {
                let mut cols: Vec<String> = Vec::new();
                for f in fields {
                    let name = find(f)?;
                    if !cols.contains(&name) {
                        cols.push(name);
                    }
                }
                if let Some(g) = geom {
                    if !cols.iter().any(|c| c == g) {
                        cols.push(g.to_string());
                    }
                }
                cols.iter()
                    .map(|c| quote(c, dialect))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            // Every visible field by name, never `*`: the gateway, not the
            // worker, decides what is hidden.
            _ => known
                .iter()
                .map(String::as_str)
                .chain(geom.filter(|g| !known.iter().any(|k| k == g)))
                .map(|c| quote(c, dialect))
                .collect::<Vec<_>>()
                .join(", "),
        };
        if cols.is_empty() {
            return Err(format!("{dataset} has no fields you are allowed to read"));
        }
        let mut sql = format!("SELECT {cols} FROM {dataset}");
        if let Some(w) = q.where_.as_deref().filter(|w| !w.trim().is_empty()) {
            let w = check_where(w, dialect).map_err(|e| e.to_string())?;
            sql.push_str(&format!(" WHERE {w}"));
        }
        if let Some(o) = &q.order_by {
            sql.push_str(&format!(" ORDER BY {}", quote(&find(o)?, dialect)));
        }
        if let Some(b) = &q.bbox {
            if geom.is_none() {
                return Err(format!("{dataset} has no geometry, so bbox cannot apply"));
            }
            if b.len() != 4 || b.iter().any(|v| !v.is_finite()) || b[0] > b[2] || b[1] > b[3] {
                return Err("bbox must be [min x, min y, max x, max y]".into());
            }
        }
        if let Some(c) = &q.bbox_crs {
            if !epsg_ok(c) {
                return Err("bbox_crs must be an EPSG code such as \"EPSG:4326\"".into());
            }
        }
        Ok(Plan {
            sql,
            bbox: q.bbox.clone(),
            bbox_crs: q.bbox_crs.clone(),
        })
    }

    /// One governed read into the workspace (or just rows, without `stem`).
    async fn governed_read(
        &self,
        conn_id: &str,
        q: &QueryArgs,
        stem: Option<&str>,
        return_rows: u64,
        limit: Option<u64>,
    ) -> Result<Read, String> {
        let (access, conn) = self.access(conn_id).map_err(|e| e.to_string())?;
        if crate::raster_tools::is_rasters(&conn.kind) {
            return Err(crate::raster_tools::not_tabular(conn));
        }
        if !Self::engine_ready(&conn.kind) {
            return Err(format!(
                "reading {} connections is not supported yet",
                conn.kind
            ));
        }
        dataset_ok(&q.dataset)?;
        if !access.dataset_allowed(&q.dataset) {
            return Err(format!(
                "dataset {:?} is not in this connection's allowed datasets",
                q.dataset
            ));
        }
        let info = self.dataset_info(&access, conn, &q.dataset, false).await?;
        let dialect = SqlDialect::for_connection_type(&conn.kind);
        let plan = self.plan(&info, &q.dataset, dialect, q)?;
        let read_limit = access.limits.rows.unwrap_or(200_000);
        let max_rows = limit.unwrap_or(read_limit).min(read_limit).max(1);
        let cols = self.table_columns(&access, conn).await?;
        let checked = if plan.bbox.is_some() {
            check_select_unlimited(&plan.sql, &access, max_rows, &cols)
        } else {
            check_select(&plan.sql, &access, max_rows, &cols)
        }
        .map_err(|e| e.to_string())?;
        let handle = self.handle(conn)?;
        let output_stem = match stem {
            Some(s) => Some(self.state.sandbox.resolve(s).map_err(|e| e.to_string())?),
            None => None,
        };
        let timeout = access.limits.seconds.unwrap_or(120);
        let reply = self
            .state
            .worker
            .core(
                "query_layer",
                json!({
                    "connection": handle,
                    "sql": checked.sql,
                    "max_rows": max_rows,
                    "return_rows": return_rows.min(access.return_rows),
                    "output_stem": output_stem,
                    "layer_name": q.dataset.rsplit('.').next().unwrap_or("layer").to_lowercase(),
                    "hide_fields": self.state.policy.hide_fields,
                    "geometry_column": info.get("geometry_column"),
                    "crs_spec": info.get("crs").and_then(|c| c.get("code")).map(|c| json!({"code": c}))
                        .or_else(|| info.get("crs_spec").cloned()),
                    "bbox": plan.bbox,
                    "bbox_crs": plan.bbox_crs,
                    "timeout_s": timeout,
                }),
                Duration::from_secs(timeout + 10),
            )
            .await
            .map_err(|e| e.to_string())?;
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
        // Second check, in the gateway: no column the policy hides may be in
        // the reply or the layer, whatever the worker made of the patterns.
        let hidden_out: Vec<&str> = reply
            .get("columns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(
                reply
                    .get("summary")
                    .and_then(|s| s.get("fields"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
            .filter_map(Value::as_str)
            .filter(|c| access.field_hidden(c))
            .collect();
        if !hidden_out.is_empty() {
            if let Some(p) = &path {
                let _ = std::fs::remove_file(p);
            }
            return Err(
                "the engine returned a field hidden by organization policy; result discarded"
                    .into(),
            );
        }
        let rows_read = reply.get("rows_read").and_then(Value::as_u64).unwrap_or(0);
        let hit_limit = reply
            .get("hit_read_limit")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut filters: Vec<String> = checked
            .tables
            .iter()
            .flat_map(|t| access.filters_for(t))
            .collect();
        filters.dedup();
        Ok(Read {
            reply,
            executed_sql: checked.sql,
            path,
            rows_read,
            hit_limit,
            info,
            filters,
            max_rows,
            hidden: checked.hidden.clone(),
        })
    }

    fn read_checks(&self, r: &Read, has_where: bool) -> Vec<Check> {
        let mut checks = vec![Check {
            id: "query.rows".into(),
            severity: Severity::Info,
            message: format!(
                "{} rows read, {} returned",
                r.rows_read,
                r.reply
                    .get("rows_returned")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ),
            detail: None,
            fix: None,
        }];
        if !r.filters.is_empty() {
            checks.push(Check {
                id: "query.row_filter".into(),
                severity: Severity::Info,
                message: format!("Server-applied row filter: {}", r.filters.join(" AND ")),
                detail: None,
                fix: None,
            });
        }
        let total = r.info.get("rows").and_then(Value::as_u64);
        if r.rows_read == 0 {
            checks.push(Check {
                id: "filter.zero_rows".into(),
                severity: Severity::Warn,
                message: "The query returned no rows".into(),
                detail: None,
                fix: Some(
                    "Check the where clause (value case, padding, nulls), the bbox and its CRS"
                        .into(),
                ),
            });
        } else if has_where && total == Some(r.rows_read) && !r.hit_limit {
            checks.push(Check {
                id: "filter.all_rows".into(),
                severity: Severity::Warn,
                message: format!("The where clause kept all {} rows", r.rows_read),
                detail: None,
                fix: Some("Check the filter does what you meant; comparisons with NULL, or a condition always true, keep everything".into()),
            });
        }
        if r.hit_limit {
            checks.push(Check {
                id: "query.read_limit".into(),
                severity: Severity::Warn,
                message: format!(
                    "Stopped at the read limit of {} rows; the result is incomplete",
                    r.max_rows
                ),
                detail: None,
                fix: Some("Narrow the query with where or bbox".into()),
            });
        }
        let mut hidden: Vec<String> = r
            .reply
            .get("hidden_columns")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        for h in &r.hidden {
            if !hidden.contains(h) {
                hidden.push(h.clone());
            }
        }
        if !hidden.is_empty() {
            checks.push(Check {
                id: "policy.hidden_fields".into(),
                severity: Severity::Info,
                message: format!(
                    "Fields hidden by organization policy: {}",
                    hidden.join(", ")
                ),
                detail: None,
                fix: None,
            });
        }
        checks.extend(checks_from(r.reply.get("checks")));
        checks
    }

    /// A workspace file for an input: a handle as is, or a dataset read
    /// through its connection's permissions into the workspace.
    pub(crate) async fn input_file(
        &self,
        name: &str,
        v: &Value,
        prov: &str,
    ) -> Result<Input, String> {
        if let Some(h) = v
            .as_str()
            .filter(|s| s.starts_with(crate::providers::HANDLE_SCHEME))
        {
            let path =
                crate::providers::resolve_handle(&self.state.session, &self.state.sandbox, h)?;
            let meta = self.state.outputs.get(&path);
            match meta.as_ref().map(|m| m.source) {
                Some(OutputSource::Engine | OutputSource::Plugin) => {}
                Some(OutputSource::Provider) => {
                    return Err(format!(
                        "{h} came from a connected server, and the GIS tools only open layers Columbus or an approved engine plugin wrote. Load the data through a connection instead"
                    ))
                }
                None => {
                    return Err(format!(
                        "{h} was not written by a Columbus tool in this session, so the GIS tools will not open it"
                    ))
                }
            }
            if !self.state.outputs.unchanged(&path) {
                return Err(format!(
                    "{h} changed after Columbus wrote it, so the GIS tools will not open it"
                ));
            }
            let truncated = meta.as_ref().is_some_and(|m| m.truncated);
            let warning = truncated.then(|| Check {
                id: "input.truncated".into(),
                severity: Severity::Warn,
                message: format!(
                    "Input {name} ({h}) was cut off at a row limit when it was made, so this result covers only part of the data"
                ),
                detail: None,
                fix: Some("Narrow the original query with where or bbox so it is complete".into()),
            });
            return Ok(Input {
                path: path.clone(),
                record: json!({"input": name, "handle": h, "labels": self.state.flow.file_labels(&path),
                       "made_by": meta.as_ref().map(|m| m.made_by.clone()), "truncated": truncated,
                       "provenance": meta.and_then(|m| m.provenance)}),
                warning,
                read: None,
            });
        }
        let (conn, dataset, where_) = dataset_ref(v).ok_or_else(|| {
            format!("input {name:?} must be a workspace handle or \"<connection>:<dataset>\"")
        })?;
        let q = QueryArgs {
            connection: conn.clone(),
            dataset: dataset.clone(),
            fields: None,
            where_: where_.clone(),
            bbox: None,
            bbox_crs: None,
            order_by: None,
            limit: None,
        };
        let stem = format!("inputs/{prov}-{name}");
        let r = self.governed_read(&conn, &q, Some(&stem), 0, None).await?;
        if r.hit_limit {
            return Err(format!(
                "{dataset} has more rows than {conn}'s read limit; processing a cut-off dataset would give wrong results. Narrow it with \"where\""
            ));
        }
        let path = r.path.clone().ok_or("the read produced no layer")?;
        self.note_read(
            &conn,
            Some(&path),
            Some(r.rows_read),
            Some(prov),
            "dataset read",
        );
        Ok(Input {
            path: path.clone(),
            record: json!({"input": name, "connection": conn, "dataset": dataset, "where": where_,
                   "executed_sql": r.executed_sql, "rows": r.rows_read, "handle": self.ws_handle(&path)}),
            warning: None,
            read: Some((conn, dataset, r.rows_read)),
        })
    }

    pub(crate) fn provenance(&self, ctx: &RequestContext<RoleServer>, p: Prov<'_>) {
        let record = json!({
            "id": p.id,
            "created": p.started.to_rfc3339(),
            "finished": Local::now().to_rfc3339(),
            "user": self.state.user,
            "client": client_name(ctx),
            "inputs": p.inputs,
            "labels": p.labels,
            "steps": p.steps,
            "checks": p.checks,
            "outputs": p.outputs,
            "server": format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
        });
        let _ = self.write_provenance(p.id, &record);
    }
}

#[tool_router(router = gis_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Everything needed before querying a dataset: its fields and types, geometry column and type, coordinate reference system (code, units, whether it is projected, the area it is meant for), extent, and the number of rows this connection's row filter allows. For a raster (a rasters connection): bands with data type and nodata, CRS, cell size, size in cells, extent, tiling and overviews. Includes checks such as a missing or misused CRS or no nodata value.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn describe_dataset(
        &self,
        Parameters(args): Parameters<DatasetArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let mut rec = self.gate(
            &ctx,
            "describe_dataset",
            &json!({"connection": args.connection, "dataset": args.dataset}),
        )?;
        rec.connection = Some(args.connection.clone());
        rec.dataset = Some(args.dataset.clone());
        let (access, conn) = match self.access(&args.connection) {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        if !access.dataset_allowed(&args.dataset) {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "dataset {:?} is not in this connection's allowed datasets",
                    args.dataset
                ),
            ));
        }
        if !Self::engine_ready(&conn.kind) {
            return Err(self.deny(
                &ctx,
                rec,
                format!("reading {} connections is not supported yet", conn.kind),
            ));
        }
        let described = if crate::raster_tools::is_rasters(&conn.kind) {
            match crate::raster_tools::refuse_filters(&access, &args.dataset) {
                Ok(()) => self.raster_describe(conn, &args.dataset).await,
                Err(e) => Err(e),
            }
        } else {
            self.dataset_info(&access, conn, &args.dataset, true).await
        };
        let mut info = match described {
            Ok(i) => i,
            Err(e) => {
                self.audit(
                    &ctx,
                    AuditRecord {
                        decision: "error".into(),
                        reason: Some(e.clone()),
                        ..rec
                    },
                )?;
                return Err(e);
            }
        };
        let mut checks = checks_from(info.get("checks"));
        let filters = access.filters_for(&args.dataset);
        if !filters.is_empty() {
            checks.insert(
                0,
                Check {
                    id: "query.row_filter".into(),
                    severity: Severity::Info,
                    message: format!(
                        "Rows counted and returned under the server's row filter: {}",
                        filters.join(" AND ")
                    ),
                    detail: None,
                    fix: None,
                },
            );
        }
        if let Some(o) = info.as_object_mut() {
            o.remove("checks");
            o.remove("crs_spec");
            // The context pack's entry for this dataset, if it has one.
            if let Some(pack) = &self.state.context {
                if let Some(e) = pack.dataset_entry(&conn.id, &args.dataset) {
                    let today = chrono::Local::now().date_naive();
                    o.insert(
                        "context".into(),
                        json!({"card": e.card_json(pack.info.review_interval_months, today), "body": e.body}),
                    );
                }
            }
        }
        self.note_read(&conn.id, None, None, None, "describe_dataset");
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: info,
            checks,
            provenance: None,
        }))
    }

    #[tool(
        description = "Look at a few rows of a dataset (default 10, at most 50) under the connection's row filter, to see real values before writing a where clause: codes, padding, case, nulls. Geometry is summarized.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn sample_rows(
        &self,
        Parameters(args): Parameters<SampleArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<QueryOut>>, String> {
        let n = args.n.unwrap_or(10).clamp(1, SAMPLE_MAX);
        let mut rec = self.gate(
            &ctx,
            "sample_rows",
            &json!({"connection": args.connection, "dataset": args.dataset, "n": n}),
        )?;
        rec.connection = Some(args.connection.clone());
        rec.dataset = Some(args.dataset.clone());
        let q = QueryArgs {
            connection: args.connection.clone(),
            dataset: args.dataset.clone(),
            fields: None,
            where_: None,
            bbox: None,
            bbox_crs: None,
            order_by: None,
            limit: Some(n),
        };
        let r = match self
            .governed_read(&args.connection, &q, None, n, Some(n))
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        self.note_read(
            &args.connection,
            None,
            Some(r.rows_read),
            None,
            "sample_rows",
        );
        rec.rows = Some(r.rows_read);
        rec.labels = self.labels();
        let mut checks = self.read_checks(&r, false);
        checks.retain(|c| c.id != "query.read_limit");
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: QueryOut {
                columns: serde_json::from_value(
                    r.reply.get("columns").cloned().unwrap_or_default(),
                )
                .unwrap_or_default(),
                rows: serde_json::from_value(r.reply.get("rows").cloned().unwrap_or_default())
                    .unwrap_or_default(),
                rows_read: r.rows_read,
                rows_returned: r
                    .reply
                    .get("rows_returned")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                layer: None,
                executed_sql: r.executed_sql,
            },
            checks,
            provenance: None,
        }))
    }

    #[tool(
        description = "Query a dataset with structured arguments (fields, where, bbox, order_by, limit) rather than SQL. The server adds the connection's row filter and limits, keeps fields hidden by policy out, and saves every row read as a layer in the workspace (GeoPackage, or CSV without geometry). Returns the first rows plus a layer handle to pass to vector_op, verify or another provider. Checks flag empty results, filters that keep everything, invalid geometry and CRS problems.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn query_features(
        &self,
        Parameters(args): Parameters<QueryArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<QueryOut>>, String> {
        let request = json!({
            "connection": args.connection, "dataset": args.dataset, "fields": args.fields, "where": args.where_,
            "bbox": args.bbox, "bbox_crs": args.bbox_crs, "order_by": args.order_by, "limit": args.limit,
        });
        let mut rec = self.gate(&ctx, "query_features", &request)?;
        rec.connection = Some(args.connection.clone());
        rec.dataset = Some(args.dataset.clone());
        let prov = new_prov_id();
        let started = Local::now();
        let stem = format!(
            "results/{}-{prov}",
            args.dataset
                .rsplit('.')
                .next()
                .unwrap_or("query")
                .to_lowercase()
        );
        let r = match self
            .governed_read(&args.connection, &args, Some(&stem), 500, args.limit)
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let has_where = args.where_.as_deref().is_some_and(|w| !w.trim().is_empty());
        let checks = self.read_checks(&r, has_where);
        self.note_read(
            &args.connection,
            r.path.as_ref(),
            Some(r.rows_read),
            Some(&prov),
            "query_features",
        );
        if let Some(p) = &r.path {
            if r.hit_limit {
                self.state.outputs.mark_truncated(p);
            }
            self.state.outputs.note_fails(p, &checks, &prov);
        }
        let layer = r.path.as_ref().and_then(|p| {
            Some(LayerOut {
                handle: self.ws_handle(p)?,
                summary: r.reply.get("summary").cloned().unwrap_or(Value::Null),
            })
        });
        self.provenance(
            &ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!([{"connection": args.connection, "dataset": args.dataset, "request": request}]),
                steps: json!([{"tool": "query_features", "executed_sql": r.executed_sql, "engine": "python-worker"}]),
                labels: self.labels(),
                checks: &checks,
                outputs: layer.iter().map(|l| l.handle.clone()).collect(),
            },
        );
        rec.rows = Some(r.rows_read);
        rec.prov = Some(prov.clone());
        rec.labels = self.labels();
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: QueryOut {
                columns: serde_json::from_value(
                    r.reply.get("columns").cloned().unwrap_or_default(),
                )
                .unwrap_or_default(),
                rows: serde_json::from_value(r.reply.get("rows").cloned().unwrap_or_default())
                    .unwrap_or_default(),
                rows_read: r.rows_read,
                rows_returned: r
                    .reply
                    .get("rows_returned")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                layer,
                executed_sql: r.executed_sql,
            },
            checks,
            provenance: Some(prov),
        }))
    }

    #[tool(
        description = "Process layers inside the workspace: reproject, buffer, simplify, make_valid, clip, dissolve, intersect, join (attribute) or spatial_join. Inputs are handles from earlier results or governed datasets. Distances need an explicit unit and are refused in a geographic CRS (degrees): reproject first. Inputs in different CRSs are aligned to the first. Saves the result as a new layer and returns its handle. Checks report join match rates and row explosions, invalid geometry, CRS problems and implausible extents. No approval needed: nothing leaves the workspace.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn vector_op(
        &self,
        Parameters(args): Parameters<VectorOpArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<VectorOpOut>>, String> {
        let request = json!({"op": args.op, "inputs": args.inputs, "params": args.params});
        let mut rec = self.gate(&ctx, "vector_op", &request)?;
        if !args.op.chars().all(|c| c.is_ascii_lowercase() || c == '_') || args.op.is_empty() {
            return Err(self.deny(&ctx, rec, "op must be one of the listed operations"));
        }
        let prov = new_prov_id();
        let started = Local::now();
        let mut paths = serde_json::Map::new();
        let mut inputs = Vec::new();
        let mut files = Vec::new();
        let mut warnings = Vec::new();
        let mut reads = Vec::new();
        for (name, v) in &args.inputs {
            if !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                return Err(self.deny(&ctx, rec, format!("{name:?} is not an input name")));
            }
            match self.input_file(name, v, &prov).await {
                Ok(i) => {
                    paths.insert(name.clone(), json!(i.path));
                    inputs.push(i.record);
                    files.push(i.path);
                    warnings.extend(i.warning);
                    reads.extend(i.read);
                }
                Err(e) => {
                    note_reads(&mut rec, &reads);
                    return Err(self.deny(&ctx, rec, e));
                }
            }
        }
        note_reads(&mut rec, &reads);
        let labels = self.state.flow.labels_for(&files);
        let stem = match self
            .state
            .sandbox
            .resolve(&format!("results/{}-{prov}", args.op))
        {
            Ok(s) => s,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let timeout = self.state.policy.limits.seconds.unwrap_or(120).max(1);
        let reply = self
            .state
            .worker
            .core(
                "vector_op",
                json!({
                    "op": args.op, "inputs": paths, "params": args.params, "output_stem": stem,
                    // Results larger than the organization's read limit are
                    // refused before they are built.
                    "max_rows": self.state.policy.limits.rows.unwrap_or(200_000),
                }),
                Duration::from_secs(timeout + 10),
            )
            .await;
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                self.audit(
                    &ctx,
                    AuditRecord {
                        decision: "error".into(),
                        reason: Some(msg.clone()),
                        ..rec
                    },
                )?;
                return Err(msg);
            }
        };
        let path = match reply
            .get("output_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
        {
            Some(p)
                if p.starts_with(self.state.sandbox.root())
                    && p.to_string_lossy().starts_with(&*stem.to_string_lossy()) =>
            {
                p
            }
            _ => {
                return Err(self.deny(&ctx, rec, "the engine returned no output in the workspace"))
            }
        };
        let summary = reply.get("summary").cloned().unwrap_or(Value::Null);
        let rows = summary.get("rows").and_then(Value::as_u64);
        let mut checks = warnings;
        checks.extend(checks_from(reply.get("checks")));
        let inherit = self
            .state
            .outputs
            .inherit(&files)
            .with_step_fails(&checks, &prov);
        self.state.flow.record_file(&path, &labels);
        self.state.outputs.record(
            &path,
            OutputMeta {
                made_by: format!("vector_op {}", args.op),
                provenance: Some(prov.clone()),
                rows,
                source: OutputSource::Engine,
                truncated: inherit.truncated || checks.iter().any(|c| c.id == "input.truncated"),
                inherited: inherit.fails,
                sha256: None,
            },
        );
        if !labels.is_empty() {
            checks.push(Check {
                id: "flow.labels".into(),
                severity: Severity::Info,
                message: format!(
                    "The output carries data labels: {}",
                    labels.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
                detail: None,
                fix: None,
            });
        }
        let handle = self.ws_handle(&path).unwrap_or_default();
        self.provenance(
            &ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!(inputs),
                steps: json!([{"tool": "vector_op", "op": args.op, "params": args.params, "engine": reply.get("engine")}]),
                labels: labels.iter().cloned().collect(),
                checks: &checks,
                outputs: vec![handle.clone()],
            },
        );
        rec.prov = Some(prov.clone());
        rec.rows = rows;
        rec.labels = labels
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: VectorOpOut {
                op: args.op,
                layer: LayerOut { handle, summary },
            },
            checks,
            provenance: Some(prov),
        }))
    }

    #[tool(
        description = "Run the spatial checks on any layer or dataset: CRS present and plausible for the coordinates (including swapped X/Y), invalid, empty and duplicate geometries, mixed geometry types. Use before handing a result to anyone.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn verify(
        &self,
        Parameters(args): Parameters<VerifyArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let mut rec = self.gate(&ctx, "verify", &json!({"layer": args.layer}))?;
        let prov = new_prov_id();
        let started = Local::now();
        let input = match self.input_file("layer", &args.layer, &prov).await {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        note_reads(&mut rec, input.read.as_slice());
        let path = input.path.clone();
        let reply = self
            .state
            .worker
            .core(
                "verify_layer",
                json!({"path": path}),
                Duration::from_secs(120),
            )
            .await;
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                self.audit(
                    &ctx,
                    AuditRecord {
                        decision: "error".into(),
                        reason: Some(msg.clone()),
                        ..rec
                    },
                )?;
                return Err(msg);
            }
        };
        let mut checks: Vec<Check> = input.warning.into_iter().collect();
        checks.extend(checks_from(reply.get("checks")));
        self.provenance(
            &ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!([input.record]),
                steps: json!([{"tool": "verify", "engine": "python-worker"}]),
                labels: self.state.flow.file_labels(&path).into_iter().collect(),
                checks: &checks,
                outputs: vec![],
            },
        );
        rec.prov = Some(prov.clone());
        rec.labels = self.labels();
        self.audit(&ctx, rec)?;
        let mut o = out(reply.get("summary").cloned().unwrap_or(Value::Null)).map_checks(checks);
        o.0.provenance = Some(prov);
        Ok(o)
    }
}

/// Record in the audit line which datasets a call read.
pub(crate) fn note_reads(rec: &mut AuditRecord, reads: &[(String, String, u64)]) {
    if reads.is_empty() {
        return;
    }
    let join = |f: fn(&(String, String, u64)) -> &str| {
        let mut v: Vec<&str> = reads.iter().map(f).collect();
        v.dedup();
        v.join(", ")
    };
    rec.connection = Some(join(|r| &r.0));
    rec.dataset = Some(join(|r| &r.1));
    rec.rows = Some(reads.iter().map(|r| r.2).sum());
}

trait MapChecks {
    fn map_checks(self, checks: Vec<Check>) -> Self;
}

impl<T: JsonSchema> MapChecks for Json<Output<T>> {
    fn map_checks(mut self, checks: Vec<Check>) -> Self {
        self.0.checks = checks;
        self
    }
}
