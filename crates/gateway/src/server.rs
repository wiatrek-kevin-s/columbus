//! The MCP server: tool definitions and the governance wrapped around each.
//!
//! Every tool call goes through the same steps: is the tool allowed by
//! organization policy, resolve the connection's effective access, check the
//! request, hand only the checked request to a worker, attach checks and
//! provenance, and append one audit line whatever the outcome.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use columbus_shared::approvals::ApprovalStore;
use columbus_shared::audit::{AuditLog, AuditRecord};
use columbus_shared::canon::request_hash;
use columbus_shared::policy::{Connection, EffectiveAccess, Mode, OrgPolicy, ProjectScope};
use columbus_shared::sandbox::Sandbox;
use columbus_shared::sqlcheck::{check_select, SqlDialect};
use columbus_shared::Denied;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, Json, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{Paths, PolicySource};
use crate::worker::{Worker, WorkerError};

pub const SERVER_NAME: &str = "columbus-mcp";

/// Connection types the engines can serve today. Others are accepted in
/// configuration and reported, but queries on them are refused until their
/// worker lands.
const ENGINE_READY: &[&str] = &["gpkg", "geopackage", "sqlite", "spatialite"];

// ---------- standard output envelope ----------

#[derive(Debug, Clone, Copy, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Very likely wrong; blocks exports and edits. Raised by the verification
    /// checks (design section 4), which land in a later milestone.
    #[allow(dead_code)]
    Fail,
    Warn,
    Info,
}

/// One verification finding (design section 4).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Check {
    /// Stable id, so skills and SOPs can refer to it.
    pub id: String,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn info(id: &str, message: impl Into<String>) -> Self {
        Check {
            id: id.into(),
            severity: Severity::Info,
            message: message.into(),
            detail: None,
            fix: None,
        }
    }
    fn warn(id: &str, message: impl Into<String>, fix: Option<&str>) -> Self {
        Check {
            id: id.into(),
            severity: Severity::Warn,
            message: message.into(),
            detail: None,
            fix: fix.map(String::from),
        }
    }
}

/// Every tool returns `result`, `checks` and `provenance`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Output<T: JsonSchema> {
    pub result: T,
    pub checks: Vec<Check>,
    /// Id of the provenance record, when the call produced data.
    pub provenance: Option<String>,
}

fn out<T: JsonSchema>(result: T) -> Json<Output<T>> {
    Json(Output {
        result,
        checks: vec![],
        provenance: None,
    })
}

// ---------- tool parameters and results ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConnectionArg {
    /// Connection id, as listed by `list_connections`.
    pub connection: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchToolsArgs {
    /// Words describing the job, e.g. "query a table" or "what datasets exist".
    pub query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DescribeToolArgs {
    /// Exact tool name.
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SqlSelectArgs {
    /// Connection id.
    pub connection: String,
    /// One SELECT statement in the connection's SQL dialect. Qualify tables
    /// as they appear in `list_datasets`. No WITH, no writes, one statement.
    pub sql: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApprovalArg {
    /// Approval id from an `approval_required` result.
    pub id: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApprovalStatus {
    pub id: String,
    /// `pending`, `approved`, `declined`, `used`, `expired` or `unknown`.
    pub state: String,
    pub summary: Option<String>,
    pub expires: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ConnectionCard {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub description: Option<String>,
    /// `read` or `read-write`, after organization policy.
    pub mode: Option<Mode>,
    /// `ok`, or why this connection cannot be used right now.
    pub status: String,
    /// Whether the engines can query this connection type yet.
    pub engine_ready: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WhoAmI {
    pub server: String,
    pub user: String,
    pub client: String,
    pub session: String,
    pub policy: PolicySummary,
    pub connections: Vec<ConnectionCard>,
    pub connections_folder: String,
    pub workspace: String,
    pub granted_folders: Vec<String>,
    pub audit_log: String,
    /// Organization rules that always apply (from the context pack, once loaded).
    pub always_on_rules: Vec<String>,
    pub engines: Value,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PolicySummary {
    pub source: PolicySource,
    pub connection_types: Vec<String>,
    pub scripts_allowed: bool,
    pub edits_allowed: bool,
    pub rows_returned_per_call: u64,
    pub rows_read_per_query: Option<u64>,
    pub query_seconds: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ConnectionDetail {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub description: Option<String>,
    pub mode: Mode,
    pub dialect: SqlDialect,
    pub allowed_datasets: Vec<String>,
    pub denied_datasets: Vec<String>,
    /// Filter the server adds to every read; the agent does not need to repeat it.
    pub row_filter: Option<String>,
    pub rows_read_per_query: Option<u64>,
    pub query_seconds: Option<u64>,
    pub rows_returned_per_call: u64,
    pub expires: Option<String>,
    pub engine_ready: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolCard {
    pub name: String,
    pub summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ToolDetail {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DatasetList {
    pub connection: String,
    pub datasets: Vec<Value>,
    /// Datasets present but outside this connection's allow list (count only).
    pub not_allowed: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub rows_read: u64,
    pub rows_returned: u64,
    /// Full result in the session workspace when more rows were read than returned.
    pub workspace_file: Option<String>,
    /// The SQL the server actually ran, after adding filters and limits.
    pub executed_sql: String,
}

// ---------- server state ----------

pub struct State {
    pub paths: Paths,
    pub policy: OrgPolicy,
    pub policy_source: PolicySource,
    pub connections: Vec<Connection>,
    pub scope: ProjectScope,
    pub sandbox: Sandbox,
    pub audit: AuditLog,
    pub approvals: ApprovalStore,
    pub worker: Worker,
    pub session: String,
    pub user: String,
    pub notes: Vec<String>,
}

#[derive(Clone)]
pub struct Gateway {
    state: Arc<State>,
    tool_router: ToolRouter<Self>,
}

fn current_user() -> String {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown".into());
    match std::env::var("USERDOMAIN") {
        Ok(d) if cfg!(windows) && !d.is_empty() => format!("{d}\\{user}"),
        _ => user,
    }
}

impl State {
    pub fn load(paths: Paths) -> anyhow::Result<Self> {
        let (policy, policy_source) = crate::config::load_policy(&paths)?;
        let connections = crate::config::load_connections(&paths)?;
        let session = format!(
            "{}-{}",
            Local::now().format("%Y%m%d-%H%M%S"),
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        );
        let sandbox = Sandbox::new(paths.workspaces.join(&session), vec![])?;
        let mut notes = Vec::new();
        let audit = match AuditLog::new(&paths.audit) {
            Ok(a) => a,
            Err(e) => {
                // ProgramData is admin-writable; a per-user install without the
                // installer's ACL falls back to the user's own folder and says so.
                let fallback = paths
                    .workspaces
                    .parent()
                    .unwrap_or(&paths.workspaces)
                    .join("audit");
                notes.push(format!(
                    "Audit log could not be written at {} ({e}); using {} instead.",
                    paths.audit.display(),
                    fallback.display()
                ));
                AuditLog::new(fallback)?
            }
        };
        let worker = Worker::new(paths.python.clone(), paths.worker_script.clone());
        Ok(State {
            paths,
            policy,
            policy_source,
            connections,
            scope: ProjectScope::default(),
            sandbox,
            audit,
            approvals: ApprovalStore::new(),
            worker,
            session,
            user: current_user(),
            notes,
        })
    }
}

fn client_name(ctx: &RequestContext<RoleServer>) -> String {
    ctx.peer
        .peer_info()
        .map(|i| i.client_info.name.clone())
        .unwrap_or_else(|| "unknown".into())
}

fn new_prov_id() -> String {
    format!("p_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

impl Gateway {
    pub fn new(state: State) -> Self {
        Gateway {
            state: Arc::new(state),
            tool_router: Self::tool_router(),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    /// Append an audit line. A failure to audit is itself a refusal: a call
    /// that cannot be recorded does not run.
    fn audit(&self, ctx: &RequestContext<RoleServer>, mut rec: AuditRecord) -> Result<(), String> {
        rec.user = self.state.user.clone();
        rec.client = client_name(ctx);
        self.state
            .audit
            .append(&rec)
            .map(|_| ())
            .map_err(|e| format!("refused: the audit log could not be written ({e})"))
    }

    /// Policy gate for every tool. Records the refusal when there is one.
    fn gate(
        &self,
        ctx: &RequestContext<RoleServer>,
        tool: &str,
        args: &Value,
    ) -> Result<AuditRecord, String> {
        let rec = AuditRecord {
            tool: tool.into(),
            decision: "allowed".into(),
            request: Some(request_hash(tool, args)),
            ..Default::default()
        };
        if !self.state.policy.tool_allowed(tool) {
            let denied = Denied::Tool(tool.into());
            self.audit(
                ctx,
                AuditRecord {
                    decision: "denied".into(),
                    reason: Some(denied.to_string()),
                    ..rec
                },
            )?;
            return Err(denied.to_string());
        }
        Ok(rec)
    }

    fn deny(
        &self,
        ctx: &RequestContext<RoleServer>,
        rec: AuditRecord,
        why: impl ToString,
    ) -> String {
        let why = why.to_string();
        // If even the denial cannot be audited, report that instead.
        match self.audit(
            ctx,
            AuditRecord {
                decision: "denied".into(),
                reason: Some(why.clone()),
                ..rec
            },
        ) {
            Ok(()) => why,
            Err(e) => e,
        }
    }

    fn connection(&self, id: &str) -> Result<&Connection, Denied> {
        self.state
            .connections
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Denied::UnknownConnection(id.into()))
    }

    fn access(&self, id: &str) -> Result<(EffectiveAccess, &Connection), Denied> {
        let conn = self.connection(id)?;
        let access =
            EffectiveAccess::resolve(&self.state.policy, conn, &self.state.scope, self.now())?;
        Ok((access, conn))
    }

    fn engine_ready(kind: &str) -> bool {
        ENGINE_READY.iter().any(|k| k.eq_ignore_ascii_case(kind))
    }

    fn handle(conn: &Connection) -> Value {
        json!({"kind": conn.kind, "path": conn.path})
    }

    fn worker_error(e: WorkerError) -> String {
        e.to_string()
    }

    fn connection_cards(&self) -> Vec<ConnectionCard> {
        self.state
            .connections
            .iter()
            .map(|c| {
                let resolved =
                    EffectiveAccess::resolve(&self.state.policy, c, &self.state.scope, self.now());
                ConnectionCard {
                    id: c.id.clone(),
                    kind: c.kind.clone(),
                    description: c.description.clone(),
                    mode: resolved.as_ref().ok().map(|a| a.mode),
                    status: match &resolved {
                        Ok(_) => "ok".into(),
                        Err(e) => e.to_string(),
                    },
                    engine_ready: Self::engine_ready(&c.kind),
                }
            })
            .collect()
    }

    fn write_provenance(&self, id: &str, record: &Value) -> Result<PathBuf, String> {
        let path = self
            .state
            .sandbox
            .resolve(&format!("provenance/{id}.prov.json"))
            .map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

#[tool_router(router = tool_router)]
impl Gateway {
    #[tool(
        description = "Start here. Who you are acting for, the permissions in force (organization policy, connections and their modes, limits), the session workspace, organization rules that always apply, and which GIS engines are available. Call once at the start of every session.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn whoami(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<WhoAmI>>, String> {
        let rec = self.gate(&ctx, "whoami", &json!({}))?;
        let engines = match self
            .state
            .worker
            .call("ping", json!({}), Duration::from_secs(20))
            .await
        {
            Ok(v) => v,
            Err(e) => json!({"error": e.to_string()}),
        };
        let p = &self.state.policy;
        let limits = columbus_shared::policy::DEFAULT_LIMITS.narrow(p.limits);
        let result = WhoAmI {
            server: format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
            user: self.state.user.clone(),
            client: client_name(&ctx),
            session: self.state.session.clone(),
            policy: PolicySummary {
                source: self.state.policy_source.clone(),
                connection_types: p.connection_types.clone(),
                scripts_allowed: p.allow_scripts,
                edits_allowed: p.allow_edits,
                rows_returned_per_call: p.return_rows.unwrap_or(500).min(500),
                rows_read_per_query: limits.rows,
                query_seconds: limits.seconds,
            },
            connections: self.connection_cards(),
            connections_folder: self.state.paths.connections.display().to_string(),
            workspace: self.state.sandbox.root().display().to_string(),
            granted_folders: self
                .state
                .sandbox
                .granted()
                .iter()
                .map(|g| g.display().to_string())
                .collect(),
            audit_log: self.state.audit.dir().display().to_string(),
            always_on_rules: vec![],
            engines,
            notes: self.state.notes.clone(),
        };
        self.audit(&ctx, rec)?;
        Ok(out(result))
    }

    #[tool(
        description = "Find the right tool for a job by describing it in plain words. Returns tool names with one-line summaries; call describe_tool for the full description and arguments.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn search_tools(
        &self,
        Parameters(args): Parameters<SearchToolsArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Vec<ToolCard>>>, String> {
        let rec = self.gate(&ctx, "search_tools", &json!({"query": args.query}))?;
        let words: Vec<String> = args
            .query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(String::from)
            .collect();
        let mut scored: Vec<(usize, ToolCard)> = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| self.state.policy.tool_allowed(&t.name))
            .map(|t| {
                let desc = t.description.as_deref().unwrap_or("").to_string();
                let hay = format!("{} {}", t.name.replace('_', " "), desc).to_lowercase();
                let score = words.iter().filter(|w| hay.contains(w.as_str())).count();
                let summary = desc
                    .split(". ")
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('.')
                    .to_string();
                (
                    score,
                    ToolCard {
                        name: t.name.to_string(),
                        summary,
                    },
                )
            })
            .filter(|(s, _)| words.is_empty() || *s > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
        self.audit(&ctx, rec)?;
        Ok(out(scored.into_iter().map(|(_, c)| c).collect()))
    }

    #[tool(
        description = "Full description, argument schema and safety notes for one tool.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn describe_tool(
        &self,
        Parameters(args): Parameters<DescribeToolArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<ToolDetail>>, String> {
        let rec = self.gate(&ctx, "describe_tool", &json!({"name": args.name}))?;
        let tool = match self.tool_router.get(&args.name) {
            Some(t) if self.state.policy.tool_allowed(&t.name) => t.clone(),
            _ => {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!("no tool named {:?} is available", args.name),
                ))
            }
        };
        self.audit(&ctx, rec)?;
        Ok(out(ToolDetail {
            name: tool.name.to_string(),
            description: tool.description.as_deref().unwrap_or("").to_string(),
            input_schema: Value::Object((*tool.input_schema).clone()),
            read_only: tool
                .annotations
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false),
        }))
    }

    #[tool(
        description = "List the database, service and file connections available in this session, with each one's mode (read or read-write) and whether it can be used now. Never returns secrets.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_connections(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Vec<ConnectionCard>>>, String> {
        let rec = self.gate(&ctx, "list_connections", &json!({}))?;
        let cards = self.connection_cards();
        self.audit(&ctx, rec)?;
        Ok(out(cards))
    }

    #[tool(
        description = "Details of one connection: type, SQL dialect, effective mode, which datasets it allows, the row filter and limits the server applies to every read, and expiry. Never returns secrets.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn describe_connection(
        &self,
        Parameters(args): Parameters<ConnectionArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<ConnectionDetail>>, String> {
        let mut rec = self.gate(
            &ctx,
            "describe_connection",
            &json!({"connection": args.connection}),
        )?;
        rec.connection = Some(args.connection.clone());
        let (access, conn) = match self.access(&args.connection) {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let detail = ConnectionDetail {
            id: conn.id.clone(),
            kind: conn.kind.clone(),
            description: conn.description.clone(),
            mode: access.mode,
            dialect: SqlDialect::for_connection_type(&conn.kind),
            allowed_datasets: conn.datasets.allow.clone(),
            denied_datasets: conn.datasets.deny.clone(),
            row_filter: access.row_filter.clone(),
            rows_read_per_query: access.limits.rows,
            query_seconds: access.limits.seconds,
            rows_returned_per_call: access.return_rows,
            expires: conn.expires.map(|d| d.to_string()),
            engine_ready: Self::engine_ready(&conn.kind),
        };
        self.audit(&ctx, rec)?;
        Ok(out(detail))
    }

    #[tool(
        description = "List the datasets (tables, views, feature classes) a connection allows, with geometry type, spatial reference, extent and fields where the source records them. Datasets outside the allow list are counted but not named.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_datasets(
        &self,
        Parameters(args): Parameters<ConnectionArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<DatasetList>>, String> {
        let mut rec = self.gate(
            &ctx,
            "list_datasets",
            &json!({"connection": args.connection}),
        )?;
        rec.connection = Some(args.connection.clone());
        let (access, conn) = match self.access(&args.connection) {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        if !Self::engine_ready(&conn.kind) {
            return Err(self.deny(
                &ctx,
                rec,
                format!("listing {} connections is not supported yet", conn.kind),
            ));
        }
        let timeout = access.limits.seconds.unwrap_or(120);
        let reply = self
            .state
            .worker
            .call(
                "list_datasets",
                json!({"connection": Self::handle(conn), "timeout_s": timeout}),
                Duration::from_secs(timeout + 10),
            )
            .await;
        let reply = match reply {
            Ok(v) => v,
            Err(e) => {
                let msg = Self::worker_error(e);
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
        let all = reply
            .get("datasets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let total = all.len();
        let mut datasets: Vec<Value> = all
            .into_iter()
            .filter(|d| {
                d.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| access.dataset_allowed(n))
            })
            .collect();
        for d in &mut datasets {
            if let Some(Value::Array(fields)) = d.get_mut("fields") {
                fields.retain(|f| {
                    !f.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| access.field_hidden(n))
                });
            }
        }
        let result = DatasetList {
            connection: conn.id.clone(),
            not_allowed: total - datasets.len(),
            datasets,
        };
        self.audit(&ctx, rec)?;
        Ok(out(result))
    }

    #[tool(
        description = "Check an approval you are waiting on. Approvals are decided only by the person, in their own interface; this tool cannot approve anything. Once approved, repeat the original call with the approval id.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn approval_status(
        &self,
        Parameters(args): Parameters<ApprovalArg>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<ApprovalStatus>>, String> {
        let mut rec = self.gate(&ctx, "approval_status", &json!({"id": args.id}))?;
        rec.approval = Some(args.id.clone());
        let status = match self.state.approvals.status(&args.id, self.now()) {
            Some(a) => ApprovalStatus {
                id: a.id,
                state: serde_json::to_value(a.state)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                summary: Some(a.summary),
                expires: Some(a.expires.with_timezone(&Local).to_rfc3339()),
            },
            None => ApprovalStatus {
                id: args.id.clone(),
                state: "unknown".into(),
                summary: None,
                expires: None,
            },
        };
        self.audit(&ctx, rec)?;
        Ok(out(status))
    }

    #[tool(
        description = "Run one read-only SELECT on a connection. The server checks that it parses as a single SELECT in the connection's dialect and that every table is allowed, adds the connection's row filter and a row limit itself, and runs it read-only. Returns up to the per-call row limit; larger results are saved in the session workspace and their path returned. Refused queries say why.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn sql_select(
        &self,
        Parameters(args): Parameters<SqlSelectArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<QueryResult>>, String> {
        let request = json!({"connection": args.connection, "sql": args.sql});
        let mut rec = self.gate(&ctx, "sql_select", &request)?;
        rec.connection = Some(args.connection.clone());
        let (access, conn) = match self.access(&args.connection) {
            Ok(v) => v,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let max_rows = access.limits.rows.unwrap_or(200_000);
        let checked = match check_select(&args.sql, &access, max_rows) {
            Ok(c) => c,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        rec.dataset = Some(checked.tables.join(","));
        if !Self::engine_ready(&conn.kind) {
            return Err(self.deny(
                &ctx,
                rec,
                format!("the query passed checks, but running queries on {} connections is not supported yet", conn.kind),
            ));
        }

        let prov = new_prov_id();
        let output_path = self
            .state
            .sandbox
            .resolve(&format!("results/{prov}.csv"))
            .map_err(|e| e.to_string())?;
        let timeout = access.limits.seconds.unwrap_or(120);
        let started = Local::now();
        let reply = self
            .state
            .worker
            .call(
                "sql_select",
                json!({
                    "connection": Self::handle(conn),
                    "sql": checked.sql,
                    "max_rows": max_rows,
                    "return_rows": access.return_rows,
                    "output_path": output_path,
                    "timeout_s": timeout,
                }),
                Duration::from_secs(timeout + 10),
            )
            .await;
        let reply = match reply {
            Ok(v) => v,
            Err(e) => {
                let msg = Self::worker_error(e);
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

        let columns: Vec<String> = reply
            .get("columns")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let keep: Vec<bool> = columns.iter().map(|c| !access.field_hidden(c)).collect();
        let hidden: Vec<String> = columns
            .iter()
            .zip(&keep)
            .filter(|(_, k)| !**k)
            .map(|(c, _)| c.clone())
            .collect();
        let rows: Vec<Vec<Value>> = reply
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_array)
                    .map(|r| {
                        r.iter()
                            .zip(&keep)
                            .filter(|(_, k)| **k)
                            .map(|(v, _)| v.clone())
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rows_read = reply.get("rows_read").and_then(Value::as_u64).unwrap_or(0);
        let rows_returned = rows.len() as u64;
        let hit_limit = reply
            .get("hit_read_limit")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let workspace_file = reply
            .get("output_path")
            .and_then(Value::as_str)
            .map(String::from);

        let mut checks = vec![Check::info(
            "query.rows",
            format!("{rows_read} rows read, {rows_returned} returned"),
        )];
        let filters: Vec<String> = checked
            .tables
            .iter()
            .flat_map(|t| access.filters_for(t))
            .collect();
        if !filters.is_empty() {
            let mut f = filters.clone();
            f.dedup();
            checks.push(Check::info(
                "query.row_filter",
                format!("Server-applied row filter: {}", f.join(" AND ")),
            ));
        }
        if rows_read == 0 {
            checks.push(Check::warn(
                "filter.zero_rows",
                "The query returned no rows",
                Some("Check the where clause, value case and padding, and that the row filter is not excluding everything"),
            ));
        }
        if hit_limit {
            checks.push(Check::warn(
                "query.read_limit",
                format!("Stopped at the read limit of {max_rows} rows; the result is incomplete"),
                Some("Narrow the query with a where clause or a smaller area"),
            ));
        }
        if !hidden.is_empty() {
            checks.push(Check::info(
                "policy.hidden_fields",
                format!(
                    "Fields hidden by organization policy: {}",
                    hidden.join(", ")
                ),
            ));
        }

        let provenance = json!({
            "id": prov,
            "created": started.to_rfc3339(),
            "finished": Local::now().to_rfc3339(),
            "user": self.state.user,
            "client": client_name(&ctx),
            "inputs": [{
                "connection": conn.id,
                "type": conn.kind,
                "tables": checked.tables,
                "query": args.sql,
            }],
            "steps": [{
                "tool": "sql_select",
                "engine": "python-worker/sqlite",
                "executed_sql": checked.sql,
                "seconds": reply.get("seconds"),
            }],
            "checks": checks,
            "outputs": workspace_file.iter().collect::<Vec<_>>(),
            "server": format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
        });
        let _ = self.write_provenance(&prov, &provenance);

        rec.rows = Some(rows_read);
        rec.prov = Some(prov.clone());
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: QueryResult {
                columns: columns
                    .into_iter()
                    .zip(&keep)
                    .filter(|(_, k)| **k)
                    .map(|(c, _)| c)
                    .collect(),
                rows,
                rows_read,
                rows_returned,
                workspace_file,
                executed_sql: checked.sql,
            },
            checks,
            provenance: Some(prov),
        }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Columbus gives you governed access to GIS data. Call whoami first. \
                 The server enforces permissions; you can ask for anything, and it will say \
                 what it refuses and why. Read the `checks` on every result and never report \
                 success over a warn or fail without saying so.",
            )
    }
}
