//! Project databases: a local DuckDB file the assistant builds for a
//! project from governed results (dozens of tables pulled from the
//! organization's databases), adds keys, indexes and views to, and joins
//! with SQL. It writes only that file; sources stay read-only.
//!
//! The gateway owns everything governance needs, outside the reach of the
//! SQL: where the file is (`projectdb/<name>/` in the session workspace, or
//! `projects/<name>/` under the workspaces root for one kept across
//! sessions, reached without following links), what may be loaded (only
//! workspace layers Columbus's own tools wrote, unchanged, which already
//! leave hidden fields out), and a record beside the file
//! (`columbus-project.json`) of the data labels it carries, what each
//! connection allowed when its data came in, the hidden fields then, each
//! table's source, CRS, truncation and fails. Anything read from the file
//! carries its labels; a change of settings that narrows any of them
//! removes the file (as it removes workspace files), and a file whose
//! labels the settings in force no longer cover is withheld until deleted.
//! The engine side (`columbus_projectdb.py`) locks DuckDB so no statement
//! reaches another file, URL or setting, and allows only DDL and DML.
//!
//! Writing the person's own project database needs no approval; deleting a
//! whole one does, and exporting from it is `export_output` on a query's
//! result layer, under the usual export rules. Every statement is audited.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Local;
use columbus_shared::approvals::Risk;
use columbus_shared::audit::AuditRecord;
use columbus_shared::canon::{canonical_json, request_hash, sha256_hex};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approval_flow::{Ask, Sought};
use crate::gis_tools::Prov;
use crate::providers::{InheritedFail, OutputMeta, OutputSource};
use crate::server::{checks_from, Check, Gateway, Output, Severity};

/// The record beside each project database.
const SIDECAR: &str = "columbus-project.json";
/// Most entries kept in a database's history.
const HISTORY: usize = 1000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProjectDbArgs {
    /// One of: list; create {db, scope?, description?}; load {db, layer,
    /// table, mode?, primary_key?, types?, description?}; execute {db, sql};
    /// query {db, sql, crs?, limit?}; describe {db}; delete {db,
    /// approval_id?}.
    pub action: String,
    /// The project database's name: lower-case letters, digits, `_` and
    /// `-`, starting with a letter or digit (at most 63). Not for list.
    #[serde(default)]
    pub db: Option<String>,
    /// `session` (default): in this session's workspace. `project`: in a
    /// named project folder under the workspaces root, kept for later
    /// sessions.
    #[serde(default)]
    pub scope: Option<String>,
    /// load: the workspace handle (ws://…) of a layer a Columbus tool wrote
    /// (a query_features or sql_select result, a vector_op output…).
    #[serde(default)]
    pub layer: Option<String>,
    /// load: the table to load into (a letter or `_`, then letters, digits
    /// and `_`).
    #[serde(default)]
    pub table: Option<String>,
    /// load: `create` (default; the table must be new), `replace` or
    /// `append` (same columns).
    #[serde(default)]
    pub mode: Option<String>,
    /// load: the primary key column(s), enforced (duplicates or nulls
    /// refuse the load).
    #[serde(default)]
    pub primary_key: Option<Vec<String>>,
    /// load: `infer` (default) makes CSV columns integer or real where every
    /// value reads back exactly as written ("0042" stays text); `text`
    /// keeps them all text.
    #[serde(default)]
    pub types: Option<String>,
    /// execute: one or more DDL/DML statements separated by `;`, run in one
    /// transaction (all or nothing). query: exactly one SELECT.
    #[serde(default)]
    pub sql: Option<String>,
    /// query: the CRS of the result's geometry (`EPSG:2277`). Default: the
    /// CRS every table loaded with geometry shares.
    #[serde(default)]
    pub crs: Option<String>,
    /// query: most rows to write (default and maximum: the read limit).
    #[serde(default)]
    pub limit: Option<u64>,
    /// create and load: what the database or table holds, kept as its
    /// comment (documents the schema).
    #[serde(default)]
    pub description: Option<String>,
    /// delete: the approval id from an earlier `approval_required` answer.
    #[serde(default)]
    pub approval_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct TableRecord {
    pub source: String,
    pub made_by: Option<String>,
    pub provenance: Option<String>,
    pub rows: u64,
    pub crs: Option<String>,
    pub geometry_column: Option<String>,
    pub labels: Vec<String>,
    pub loaded: String,
    #[serde(default)]
    pub truncated: bool,
}

/// What the gateway records beside a project database.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Sidecar {
    pub name: String,
    pub scope: String,
    pub created: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Union of the labels of everything loaded.
    #[serde(default)]
    pub labels: BTreeSet<String>,
    /// What each connection allowed when its data first came in.
    #[serde(default)]
    pub fingerprints: BTreeMap<String, String>,
    /// The fields hidden at every load (their intersection): data in the
    /// file may hold any field outside this list.
    #[serde(default)]
    pub hidden: Option<Vec<String>>,
    #[serde(default)]
    pub tables: BTreeMap<String, TableRecord>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub inherited: Vec<InheritedFail>,
    #[serde(default)]
    pub history: Vec<Value>,
}

/// One project database, resolved.
pub(crate) struct Db {
    pub name: String,
    pub scope: String,
    pub dir: PathBuf,
    pub file: PathBuf,
}

impl Db {
    fn sidecar_path(&self) -> PathBuf {
        self.dir.join(SIDECAR)
    }
}

fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 63
        && n.chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// What decides which data a connection gives, for telling later whether
/// the settings still allow what a project database holds.
pub(crate) fn connection_fingerprint(
    settings: &crate::config::Settings,
    label: &str,
) -> Option<String> {
    let id = label.strip_prefix("connection:")?;
    let c = settings.connections.iter().find(|c| c.id == id)?;
    if !settings
        .policy
        .connection_types
        .iter()
        .any(|t| t.eq_ignore_ascii_case(&c.kind))
    {
        return None;
    }
    let mut v = serde_json::to_value(c).ok()?;
    if let Some(o) = v.as_object_mut() {
        o.remove("description");
        o.remove("mode");
    }
    Some(sha256_hex(canonical_json(&v).as_bytes()))
}

/// Why the settings in force no longer cover what the database holds, if
/// they do not.
pub(crate) fn withheld(settings: &crate::config::Settings, side: &Sidecar) -> Option<String> {
    for l in side.labels.iter().filter(|l| l.starts_with("connection:")) {
        let now = connection_fingerprint(settings, l);
        if now.is_none() {
            return Some(format!(
                "it holds data from {}, which this session's settings no longer have",
                l.trim_start_matches("connection:")
            ));
        }
        if side
            .fingerprints
            .get(l)
            .is_some_and(|f| Some(f) != now.as_ref())
        {
            return Some(format!(
                "the settings for {} changed since its data was loaded",
                l.trim_start_matches("connection:")
            ));
        }
    }
    if let Some(hidden) = &side.hidden {
        if let Some(f) = settings
            .policy
            .hide_fields
            .iter()
            .find(|f| !hidden.contains(f))
        {
            return Some(format!(
                "the organization now hides fields matching {f:?}, which may be in it"
            ));
        }
    }
    None
}

fn read_sidecar(db: &Db) -> Result<Option<Sidecar>, String> {
    let p = db.sidecar_path();
    match std::fs::symlink_metadata(&p) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err(format!("{} is not a plain file", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("the project database's record ({SIDECAR}) cannot be read: {e}"))
}

fn write_sidecar(db: &Db, side: &Sidecar) -> Result<(), String> {
    let text = serde_json::to_string_pretty(side).map_err(|e| e.to_string())?;
    let tmp = db.dir.join(format!("{SIDECAR}.new"));
    let _ = std::fs::remove_file(&tmp);
    let dir = crate::safefs::Dir::open_beneath(&db.dir, &[], false).map_err(|e| e.to_string())?;
    {
        use std::io::Write;
        let mut f = dir
            .create_file(&format!("{SIDECAR}.new"))
            .map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, db.sidecar_path()).map_err(|e| e.to_string())
}

/// Delete a project database's folder and everything in it.
pub(crate) fn remove_db_dir(dir: &Path) -> bool {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => std::fs::remove_dir_all(dir).is_ok(),
        Ok(_) => std::fs::remove_file(dir).is_ok(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

impl Gateway {
    fn workspaces_root(&self) -> Result<PathBuf, String> {
        self.state
            .paths
            .workspaces
            .canonicalize()
            .map_err(|e| format!("the workspaces folder cannot be found: {e}"))
    }

    /// The database a request names, its folder reached (and made, with
    /// `create`) without following links.
    fn project_db_at(&self, name: &str, scope: &str, create: bool) -> Result<Db, String> {
        if !name_ok(name) {
            return Err("db must be a name of lower-case letters, digits, _ and -, starting with a letter or digit (at most 63)".into());
        }
        let (root, comps): (PathBuf, [&str; 2]) = match scope {
            "session" => (self.state.sandbox.root().to_path_buf(), ["projectdb", name]),
            "project" => (self.workspaces_root()?, ["projects", name]),
            _ => return Err("scope must be session or project".into()),
        };
        let dir = match crate::safefs::Dir::open_beneath(&root, &comps, create) {
            Ok(d) => d.path().to_path_buf(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "there is no {scope} project database {name:?}; create it first (list shows those there are)"
                ))
            }
            Err(e) => {
                return Err(format!(
                    "the project database's folder cannot be used safely: {e}"
                ))
            }
        };
        let file = dir.join(format!("{name}.duckdb"));
        if let Ok(m) = std::fs::symlink_metadata(&file) {
            if !m.is_file() {
                return Err(format!("{} is not a plain file", file.display()));
            }
        }
        Ok(Db {
            name: name.to_string(),
            scope: scope.to_string(),
            dir,
            file,
        })
    }

    /// Note that this session has the database open: a change of settings
    /// that narrows its labels removes it, and its labels follow what is
    /// read from it.
    fn track_db(&self, db: &Db, side: &Sidecar) {
        self.state
            .project_dbs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(db.dir.clone(), db.file.clone());
        self.state.flow.record_file(&db.file, &side.labels);
    }

    fn worker_params(&self, db: &Db) -> serde_json::Map<String, Value> {
        let p = self.state.policy();
        let mut m = serde_json::Map::new();
        m.insert("path".into(), json!(db.file));
        m.insert(
            "memory_mb".into(),
            json!(p.project_db.memory_mb.clamp(64, 65536)),
        );
        m.insert("timeout_s".into(), json!(p.read_limits().max_seconds()));
        m
    }

    async fn project_worker(
        &self,
        method: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        let timeout = self.state.policy().read_limits().max_seconds();
        self.state
            .worker
            .core(
                method,
                Value::Object(params),
                Duration::from_secs(timeout + 30),
            )
            .await
            .map_err(|e| e.to_string())
    }

    fn list_dbs(&self) -> Vec<Value> {
        let settings = self.state.settings();
        let mut out = Vec::new();
        let roots: Vec<(String, Option<PathBuf>)> = vec![
            (
                "session".into(),
                Some(self.state.sandbox.root().join("projectdb")),
            ),
            (
                "project".into(),
                self.workspaces_root().ok().map(|r| r.join("projects")),
            ),
        ];
        for (scope, dir) in roots {
            let Some(dir) = dir else { continue };
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| name_ok(n))
                .collect();
            names.sort();
            for name in names {
                let Ok(db) = self.project_db_at(&name, &scope, false) else {
                    continue;
                };
                let side = read_sidecar(&db).ok().flatten();
                let bytes = std::fs::metadata(&db.file).map(|m| m.len()).ok();
                out.push(json!({
                    "db": name,
                    "scope": scope,
                    "bytes": bytes,
                    "tables": side.as_ref().map(|s| s.tables.keys().cloned().collect::<Vec<_>>()),
                    "labels": side.as_ref().map(|s| s.labels.clone()),
                    "description": side.as_ref().and_then(|s| s.description.clone()),
                    "withheld": side.as_ref().and_then(|s| withheld(&settings, s)),
                }));
            }
        }
        out
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ProjectDbOut {
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `done` or `approval_required`.
    pub status: String,
    /// What the action did or found.
    pub result: Value,
    /// Data labels the database carries (and anything read from it).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<crate::server::ApprovalNeeded>,
}

fn now() -> String {
    Local::now().to_rfc3339()
}

#[tool_router(router = project_db_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "A local project database (DuckDB) for joining many large tables with SQL. Actions (see the action argument): list, create, load (a ws:// layer from a Columbus result into a table, streamed, with its primary key), execute (DDL/DML: tables, keys, indexes, views, comments, INSERT … FROM; many statements separated by ; in one transaction), query (one statement; the result becomes a layer with a handle), describe (tables, columns, keys, indexes, comments, row counts) and delete (the whole database; asks the person). It writes only its own file: no ATTACH, COPY, file readers, extensions or settings. Writing it needs no approval, so batch statements into one execute. It carries the data labels of everything loaded, and so does anything read from it. Bring data in from Columbus results first (query_features, or SQL in the source), filtered there; export results with export_output.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = false
        )
    )]
    async fn project_db(
        &self,
        Parameters(args): Parameters<ProjectDbArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<ProjectDbOut>>, String> {
        let request = json!({
            "action": args.action, "db": args.db, "scope": args.scope, "layer": args.layer,
            "table": args.table, "mode": args.mode, "primary_key": args.primary_key, "types": args.types,
            "sql": args.sql, "crs": args.crs, "limit": args.limit, "description": args.description,
        });
        let mut rec = self.gate(&ctx, "project_db", &request)?;
        if !self.state.policy().project_db.enabled {
            return Err(self.deny(
                &ctx,
                rec,
                "the organization turned project databases off (policy project_db.enabled: false)",
            ));
        }
        let action = args.action.trim().to_ascii_lowercase();
        if action == "list" {
            let dbs = self.list_dbs();
            self.audit(&ctx, rec)?;
            return Ok(Json(Output {
                result: ProjectDbOut {
                    action,
                    db: None,
                    scope: None,
                    status: "done".into(),
                    result: json!({"databases": dbs}),
                    labels: vec![],
                    approval: None,
                },
                checks: vec![],
                provenance: None,
            }));
        }
        let Some(name) = args.db.clone() else {
            return Err(self.deny(&ctx, rec, "db (the project database's name) is required"));
        };
        let scope = args.scope.clone().unwrap_or_else(|| "session".into());
        rec.dataset = Some(format!("project_db:{scope}/{name}"));
        let create = action == "create";
        if !matches!(
            action.as_str(),
            "create" | "load" | "execute" | "query" | "describe" | "delete"
        ) {
            return Err(self.deny(
                &ctx,
                rec,
                "action must be one of list, create, load, execute, query, describe, delete",
            ));
        }
        let db = match self.project_db_at(&name, &scope, create) {
            Ok(d) => d,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let side = match read_sidecar(&db) {
            Ok(s) => s,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        if action == "delete" {
            return self
                .project_db_delete(&ctx, rec, &db, side, args.approval_id.as_deref())
                .await;
        }
        let mut side = match (side, create) {
            (Some(s), _) => s,
            (None, true) => Sidecar {
                name: name.clone(),
                scope: scope.clone(),
                created: now(),
                description: args.description.clone(),
                hidden: None,
                ..Default::default()
            },
            (None, false) => {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!("there is no {scope} project database {name:?}; create it first"),
                ))
            }
        };
        if side.name != name || side.scope != scope {
            return Err(self.deny(
                &ctx,
                rec,
                "the project database's record names another database; it is not used",
            ));
        }
        if let Some(why) = withheld(&self.state.settings(), &side) {
            return Err(self.deny(
                &ctx,
                rec,
                format!("this project database is withheld: {why}. Ask the person whether to delete it (action delete) and build it again"),
            ));
        }
        if db.file.exists() || !create {
            self.track_db(&db, &side);
        }
        rec.labels = side.labels.iter().cloned().collect();
        match action.as_str() {
            "create" => self.project_db_create(&ctx, rec, db, side).await,
            "load" => self.project_db_load(&ctx, rec, db, &mut side, &args).await,
            "execute" => {
                self.project_db_execute(&ctx, rec, db, &mut side, &args)
                    .await
            }
            "query" => self.project_db_query(&ctx, rec, db, &side, &args).await,
            _ => self.project_db_describe(&ctx, rec, db, &side).await,
        }
    }
}

type Answer = Result<Json<Output<ProjectDbOut>>, String>;

fn answer(
    action: &str,
    db: &Db,
    result: Value,
    labels: &BTreeSet<String>,
    checks: Vec<Check>,
    prov: Option<String>,
) -> Answer {
    Ok(Json(Output {
        result: ProjectDbOut {
            action: action.into(),
            db: Some(db.name.clone()),
            scope: Some(db.scope.clone()),
            status: "done".into(),
            result,
            labels: labels.iter().cloned().collect(),
            approval: None,
        },
        checks,
        provenance: prov,
    }))
}

impl Gateway {
    fn worker_failed(
        &self,
        ctx: &RequestContext<RoleServer>,
        rec: AuditRecord,
        e: String,
    ) -> String {
        match self.audit(
            ctx,
            AuditRecord {
                decision: "error".into(),
                reason: Some(e.clone()),
                ..rec
            },
        ) {
            Ok(()) => e,
            Err(a) => a,
        }
    }

    async fn project_db_create(
        &self,
        ctx: &RequestContext<RoleServer>,
        rec: AuditRecord,
        db: Db,
        mut side: Sidecar,
    ) -> Answer {
        let reply = match self
            .project_worker("project_db_create", self.worker_params(&db))
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.worker_failed(ctx, rec, e)),
        };
        side.history.push(json!({"at": now(), "action": "create"}));
        if let Err(e) = write_sidecar(&db, &side) {
            return Err(self.worker_failed(ctx, rec, e));
        }
        self.track_db(&db, &side);
        self.audit(ctx, rec)?;
        let result = json!({
            "created": reply.get("created"),
            "path": db.file,
            "duckdb": reply.get("duckdb"),
            "spatial_functions": reply.get("spatial"),
            "tables": side.tables.keys().collect::<Vec<_>>(),
        });
        let mut checks = vec![];
        if reply.get("spatial").and_then(Value::as_bool) != Some(true) {
            checks.push(Check {
                id: "project_db.no_spatial".into(),
                severity: Severity::Info,
                message: "DuckDB's spatial extension is not installed beside the engine, so geometry is stored and returned but spatial SQL functions (ST_Intersects, ST_Area…) are not available; use vector_op on query results instead".into(),
                detail: None,
                fix: Some("pip install the duckdb-extension-spatial version matching DuckDB (requirements-gis.txt)".into()),
            });
        }
        answer("create", &db, result, &side.labels, checks, None)
    }

    async fn project_db_load(
        &self,
        ctx: &RequestContext<RoleServer>,
        mut rec: AuditRecord,
        db: Db,
        side: &mut Sidecar,
        args: &ProjectDbArgs,
    ) -> Answer {
        let (Some(handle), Some(table)) = (args.layer.clone(), args.table.clone()) else {
            return Err(self.deny(ctx, rec, "load needs layer (a ws:// handle) and table"));
        };
        let path = match crate::providers::resolve_handle(
            &self.state.session,
            &self.state.sandbox,
            &handle,
        ) {
            Ok(p) => p,
            Err(e) => return Err(self.deny(ctx, rec, e)),
        };
        let meta = match self.state.outputs.get(&path) {
            Some(m) => m,
            None => {
                return Err(self.deny(
                    ctx,
                    rec,
                    format!("{handle} was not written by a Columbus tool in this session, so it cannot be loaded"),
                ))
            }
        };
        match meta.source {
            OutputSource::Engine | OutputSource::Plugin | OutputSource::Script => {}
            OutputSource::Provider => {
                return Err(self.deny(
                    ctx,
                    rec,
                    format!("{handle} came from a connected server; only layers Columbus, an approved plugin or an approved script wrote can be loaded"),
                ))
            }
        }
        if !self.state.outputs.unchanged(&path) {
            return Err(self.deny(
                ctx,
                rec,
                format!("{handle} changed after Columbus wrote it, so it is not loaded"),
            ));
        }
        let lower = path.to_string_lossy().to_ascii_lowercase();
        if !(lower.ends_with(".gpkg") || lower.ends_with(".csv")) {
            return Err(self.deny(ctx, rec, "only GeoPackage and CSV layers can be loaded"));
        }
        let labels = self.state.flow.labels_for(std::slice::from_ref(&path));
        // Labels and what each connection allows, before any data goes in.
        let settings = self.state.settings();
        for l in labels.iter().filter(|l| l.starts_with("connection:")) {
            match connection_fingerprint(&settings, l) {
                Some(f) => {
                    side.fingerprints.entry(l.clone()).or_insert(f);
                }
                None => {
                    return Err(self.deny(
                        ctx,
                        rec,
                        format!(
                        "{handle} holds data from {}, which this session's settings no longer have",
                        l.trim_start_matches("connection:")
                    ),
                    ))
                }
            }
        }
        let hide = settings.policy.hide_fields.clone();
        side.hidden = Some(match side.hidden.take() {
            None => hide.clone(),
            Some(h) => h.into_iter().filter(|f| hide.contains(f)).collect(),
        });
        side.labels.extend(labels.iter().cloned());
        // The record goes first, with the new labels: if anything after this
        // fails, the file is labelled as conservatively as it may now be.
        if let Err(e) = write_sidecar(&db, side) {
            return Err(self.worker_failed(ctx, rec, e));
        }
        self.track_db(&db, side);
        rec.labels = side.labels.iter().cloned().collect();
        let prov = crate::server::new_prov_id();
        let started = Local::now();
        let mut p = self.worker_params(&db);
        p.insert("source".into(), json!(path));
        p.insert("table".into(), json!(table));
        p.insert(
            "mode".into(),
            json!(args.mode.clone().unwrap_or_else(|| "create".into())),
        );
        p.insert(
            "primary_key".into(),
            json!(args.primary_key.clone().unwrap_or_default()),
        );
        p.insert(
            "types".into(),
            json!(args.types.clone().unwrap_or_else(|| "infer".into())),
        );
        p.insert("hide_fields".into(), json!(hide));
        p.insert(
            "comment".into(),
            json!(args.description.clone().unwrap_or_else(|| format!(
                "Loaded from {handle} ({}) on {}",
                meta.made_by,
                Local::now().format("%Y-%m-%d")
            ))),
        );
        let reply = match self.project_worker("project_db_load", p).await {
            Ok(r) => r,
            Err(e) => return Err(self.worker_failed(ctx, rec, e)),
        };
        // Belt and braces: no column the policy hides may have gone in.
        let cols: Vec<String> = reply
            .get("columns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| c.get("name").and_then(Value::as_str).map(String::from))
            .collect();
        let access_hidden = |c: &str| {
            columbus_shared::policy::DatasetRules {
                allow: hide.clone(),
                deny: vec![],
            }
            .permits(c)
            .unwrap_or(false)
        };
        if let Some(c) = cols.iter().find(|c| access_hidden(c)) {
            let _ = self
                .project_worker("project_db_execute", {
                    let mut p = self.worker_params(&db);
                    p.insert(
                        "sql".into(),
                        json!(format!("DROP TABLE \"{}\"", table.replace('"', "\"\""))),
                    );
                    p
                })
                .await;
            return Err(self.worker_failed(
                ctx,
                rec,
                format!("the load brought in {c:?}, a field the organization hides; the table was dropped"),
            ));
        }
        let rows = reply.get("rows").and_then(Value::as_u64).unwrap_or(0);
        let mut checks = vec![Check::info(
            "project_db.loaded",
            format!("{rows} rows loaded into {table}"),
        )];
        if meta.truncated {
            side.truncated = true;
            checks.push(Check {
                id: "input.truncated".into(),
                severity: Severity::Warn,
                message: format!("{handle} was cut off at a row limit when it was made, so {table} holds only part of the data, and so does everything built from this database"),
                detail: None,
                fix: Some("Narrow the original query with where or bbox so it is complete, or raise the limit in policy, then load again with mode replace".into()),
            });
        }
        for f in &meta.inherited {
            if !side.inherited.contains(f) {
                side.inherited.push(f.clone());
            }
        }
        checks.push(Check::info(
            "flow.labels",
            format!(
                "The project database carries data labels: {}",
                side.labels.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        ));
        side.tables.insert(
            table.clone(),
            TableRecord {
                source: handle.clone(),
                made_by: Some(meta.made_by.clone()),
                provenance: meta.provenance.clone(),
                rows,
                crs: reply.get("crs").and_then(Value::as_str).map(String::from),
                geometry_column: reply
                    .get("geometry_column")
                    .and_then(Value::as_str)
                    .map(String::from),
                labels: labels.iter().cloned().collect(),
                loaded: now(),
                truncated: meta.truncated,
            },
        );
        side.history.push(json!({"at": now(), "action": "load", "table": table, "rows": rows, "provenance": prov}));
        trim(&mut side.history);
        if let Err(e) = write_sidecar(&db, side) {
            return Err(self.worker_failed(ctx, rec, e));
        }
        self.provenance(
            ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!([{"handle": handle, "labels": labels, "made_by": meta.made_by, "provenance": meta.provenance, "truncated": meta.truncated}]),
                steps: json!([{"tool": "project_db", "action": "load", "db": db.name, "scope": db.scope, "table": table,
                    "mode": args.mode, "primary_key": args.primary_key, "engine": "duckdb"}]),
                labels: side.labels.iter().cloned().collect(),
                checks: &checks,
                outputs: vec![format!("project_db:{}/{}#{table}", db.scope, db.name)],
            },
        );
        rec.rows = Some(rows);
        rec.prov = Some(prov.clone());
        self.audit(ctx, rec)?;
        answer("load", &db, reply, &side.labels, checks, Some(prov))
    }

    async fn project_db_execute(
        &self,
        ctx: &RequestContext<RoleServer>,
        rec: AuditRecord,
        db: Db,
        side: &mut Sidecar,
        args: &ProjectDbArgs,
    ) -> Answer {
        let Some(sql) = args.sql.clone().filter(|s| !s.trim().is_empty()) else {
            return Err(self.deny(ctx, rec, "execute needs sql"));
        };
        let mut p = self.worker_params(&db);
        p.insert("sql".into(), json!(sql));
        let prov = crate::server::new_prov_id();
        let started = Local::now();
        let reply = match self.project_worker("project_db_execute", p).await {
            Ok(r) => r,
            Err(e) => {
                // Refused or failed: nothing in the batch was kept.
                return Err(self.worker_failed(ctx, rec, e));
            }
        };
        let stmts: Vec<Value> = reply
            .get("statements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // One audit line per statement: its kind, its own hash, the rows it
        // changed. Never the statement's text (it may hold values).
        let n = stmts.len();
        for s in &stmts {
            let text = s.get("sql").and_then(Value::as_str).unwrap_or("");
            self.audit(
                ctx,
                AuditRecord {
                    reason: Some(format!(
                        "statement {} of {n}: {}",
                        s.get("index").and_then(Value::as_u64).unwrap_or(0),
                        s.get("type").and_then(Value::as_str).unwrap_or("?")
                    )),
                    request: Some(request_hash("project_db.statement", &json!(text))),
                    rows: s.get("rows").and_then(Value::as_u64),
                    prov: Some(prov.clone()),
                    ..rec.clone()
                },
            )?;
        }
        // Tables dropped or replaced by name lose their load record.
        let present = self.project_db_tables(&db).await.unwrap_or_default();
        side.tables
            .retain(|t, _| present.iter().any(|p| p.eq_ignore_ascii_case(t)));
        side.history.push(json!({"at": now(), "action": "execute", "statements": stmts.iter().map(|s| s.get("type")).collect::<Vec<_>>(), "provenance": prov}));
        trim(&mut side.history);
        write_sidecar(&db, side)?;
        let checks = vec![Check::info(
            "project_db.statements",
            format!("{n} statement(s) run in one transaction"),
        )];
        self.provenance(
            ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!([{"project_db": db.name, "scope": db.scope}]),
                steps: json!(stmts.iter().map(|s| json!({"tool": "project_db", "action": "execute", "type": s.get("type"), "sql": s.get("sql"), "rows": s.get("rows")})).collect::<Vec<_>>()),
                labels: side.labels.iter().cloned().collect(),
                checks: &checks,
                outputs: vec![format!("project_db:{}/{}", db.scope, db.name)],
            },
        );
        let result = json!({
            "statements": stmts.iter().map(|s| json!({"index": s.get("index"), "type": s.get("type"), "rows": s.get("rows")})).collect::<Vec<_>>(),
            "seconds": reply.get("seconds"),
        });
        answer("execute", &db, result, &side.labels, checks, Some(prov))
    }

    async fn project_db_tables(&self, db: &Db) -> Result<Vec<String>, String> {
        let reply = self
            .project_worker("project_db_describe", self.worker_params(db))
            .await?;
        Ok(reply
            .get("tables")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
            .collect())
    }

    async fn project_db_query(
        &self,
        ctx: &RequestContext<RoleServer>,
        mut rec: AuditRecord,
        db: Db,
        side: &Sidecar,
        args: &ProjectDbArgs,
    ) -> Answer {
        let Some(sql) = args.sql.clone().filter(|s| !s.trim().is_empty()) else {
            return Err(self.deny(ctx, rec, "query needs sql (one SELECT)"));
        };
        // Reading the database is reading what was loaded into it.
        for l in &side.labels {
            self.state.flow.add(l.clone());
        }
        let crss: BTreeSet<String> = side.tables.values().filter_map(|t| t.crs.clone()).collect();
        let crs = match &args.crs {
            Some(c) => {
                let ok = c.strip_prefix("EPSG:").is_some_and(|n| {
                    !n.is_empty() && n.len() <= 6 && n.chars().all(|d| d.is_ascii_digit())
                });
                if !ok {
                    return Err(self.deny(
                        ctx,
                        rec,
                        "crs must be an EPSG code such as \"EPSG:2277\"",
                    ));
                }
                Some(c.clone())
            }
            None if crss.len() == 1 => crss.iter().next().cloned(),
            None => None,
        };
        let read_limit = self.state.policy().read_limits().max_rows();
        let max_rows = args.limit.unwrap_or(read_limit).clamp(1, read_limit);
        let prov = crate::server::new_prov_id();
        let stem = match self
            .state
            .sandbox
            .resolve(&format!("results/{}-{prov}", db.name))
        {
            Ok(s) => s,
            Err(e) => return Err(self.deny(ctx, rec, e)),
        };
        let return_rows = self.state.policy().return_rows.unwrap_or(500).min(500);
        let mut p = self.worker_params(&db);
        p.insert("sql".into(), json!(sql));
        p.insert("max_rows".into(), json!(max_rows));
        p.insert("return_rows".into(), json!(return_rows));
        p.insert("output_stem".into(), json!(stem));
        p.insert("crs".into(), json!(crs));
        let started = Local::now();
        let reply = match self.project_worker("project_db_query", p).await {
            Ok(r) => r,
            Err(e) => {
                let e = if e.contains("no CRS is known") && crss.len() > 1 {
                    format!(
                        "{e}; the tables loaded with geometry are in {}",
                        crss.iter().cloned().collect::<Vec<_>>().join(", ")
                    )
                } else {
                    e
                };
                return Err(self.worker_failed(ctx, rec, e));
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
                return Err(self.worker_failed(
                    ctx,
                    rec,
                    "the engine returned no output in the workspace".into(),
                ))
            }
        };
        let rows_read = reply.get("rows_read").and_then(Value::as_u64).unwrap_or(0);
        let hit = reply
            .get("hit_read_limit")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut checks = vec![Check::info(
            "query.rows",
            format!(
                "{rows_read} rows written, {} returned",
                reply
                    .get("rows_returned")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
            ),
        )];
        if hit {
            checks.push(Check {
                id: "query.read_limit".into(),
                severity: Severity::Warn,
                message: format!(
                    "Stopped at the read limit of {max_rows} rows; the result is incomplete"
                ),
                detail: None,
                fix: Some("Filter or aggregate in the query, or ask for fewer rows".into()),
            });
        }
        if rows_read == 0 {
            checks.push(Check {
                id: "filter.zero_rows".into(),
                severity: Severity::Warn,
                message: "The query returned no rows".into(),
                detail: None,
                fix: Some("Check the join keys and conditions".into()),
            });
        }
        if side.truncated {
            checks.push(Check {
                id: "input.truncated".into(),
                severity: Severity::Warn,
                message: "A table in this project database was loaded from a result cut off at a row limit, so this result may cover only part of the data".into(),
                detail: None,
                fix: Some("Load that table again from a complete result (mode replace)".into()),
            });
        }
        if let Some(c) = &crs {
            if args.crs.is_none() && reply.pointer("/summary/crs").is_some_and(|v| !v.is_null()) {
                checks.push(Check::info(
                    "project_db.crs",
                    format!(
                        "Geometry labelled {c}, the CRS every table loaded with geometry shares"
                    ),
                ));
            }
        }
        checks.extend(checks_from(reply.get("checks")));
        let labels = self.state.flow.labels_for(std::slice::from_ref(&db.file));
        let inherit = crate::providers::Inheritance {
            fails: side.inherited.clone(),
            truncated: side.truncated,
        }
        .with_step_fails(&checks, &prov);
        self.state.flow.record_file(&path, &labels);
        self.state.outputs.record(
            &path,
            OutputMeta {
                made_by: format!("project_db query {}", db.name),
                provenance: Some(prov.clone()),
                rows: Some(rows_read),
                source: OutputSource::Engine,
                truncated: hit || inherit.truncated,
                inherited: inherit.fails,
                sha256: None,
            },
        );
        if !labels.is_empty() {
            checks.push(Check::info(
                "flow.labels",
                format!(
                    "The output carries data labels: {}",
                    labels.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
            ));
        }
        let handle = self.ws_handle(&path).unwrap_or_default();
        let inputs: Vec<Value> = side
            .tables
            .iter()
            .map(|(t, r)| json!({"project_db": db.name, "table": t, "source": r.source, "provenance": r.provenance, "labels": r.labels}))
            .collect();
        self.provenance(
            ctx,
            Prov {
                id: &prov,
                started,
                inputs: json!(inputs),
                steps: json!([{"tool": "project_db", "action": "query", "db": db.name, "scope": db.scope, "sql": sql, "crs": crs, "engine": "duckdb"}]),
                labels: labels.iter().cloned().collect(),
                checks: &checks,
                outputs: vec![handle.clone()],
            },
        );
        rec.rows = Some(rows_read);
        rec.prov = Some(prov.clone());
        rec.labels = labels.iter().cloned().collect();
        rec.request = Some(request_hash(
            "project_db.statement",
            &json!(reply
                .pointer("/statement/sql")
                .and_then(Value::as_str)
                .unwrap_or(&sql)),
        ));
        self.audit(ctx, rec)?;
        let result = json!({
            "columns": reply.get("columns"),
            "rows": reply.get("rows"),
            "rows_read": rows_read,
            "rows_returned": reply.get("rows_returned"),
            "layer": {"handle": handle, "summary": reply.get("summary")},
            "seconds": reply.get("seconds"),
        });
        answer("query", &db, result, &labels, checks, Some(prov))
    }

    async fn project_db_describe(
        &self,
        ctx: &RequestContext<RoleServer>,
        rec: AuditRecord,
        db: Db,
        side: &Sidecar,
    ) -> Answer {
        for l in &side.labels {
            self.state.flow.add(l.clone());
        }
        let mut reply = match self
            .project_worker("project_db_describe", self.worker_params(&db))
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(self.worker_failed(ctx, rec, e)),
        };
        if let Some(o) = reply.as_object_mut() {
            o.insert("loaded".into(), json!(side.tables));
            o.insert("description".into(), json!(side.description));
            o.insert("truncated".into(), json!(side.truncated));
            o.insert("path".into(), json!(db.file));
        }
        self.audit(ctx, rec)?;
        answer("describe", &db, reply, &side.labels, vec![], None)
    }

    async fn project_db_delete(
        &self,
        ctx: &RequestContext<RoleServer>,
        mut rec: AuditRecord,
        db: &Db,
        side: Option<Sidecar>,
        approval_id: Option<&str>,
    ) -> Answer {
        if !db.dir.exists() {
            return Err(self.deny(ctx, rec, "there is no such project database"));
        }
        let bytes = std::fs::metadata(&db.file).map(|m| m.len()).unwrap_or(0);
        let tables: Vec<String> = side
            .as_ref()
            .map(|s| s.tables.keys().cloned().collect())
            .unwrap_or_default();
        let summary = format!(
            "Delete the {} project database {:?} ({} MB{}){}. This cannot be undone.",
            db.scope,
            db.name,
            bytes / 1_000_000,
            if tables.is_empty() {
                String::new()
            } else {
                format!(", tables {}", tables.join(", "))
            },
            if db.scope == "project" {
                ", kept for later sessions"
            } else {
                ""
            },
        );
        let bound = json!({"action": "delete", "db": db.name, "scope": db.scope, "path": db.dir});
        let ask = Ask {
            tool: "project_db",
            label: "project_db delete",
            request: &bound,
            risk: Risk::Edit,
            summary: summary.clone(),
            detail: Some(
                json!({"database": db.name, "scope": db.scope, "path": db.file, "bytes": bytes, "tables": tables}),
            ),
            approval_id,
            every_time: true,
            code: None,
            standing: None,
        };
        match self.seek_approval(ctx, ask).await {
            Ok(Sought::Granted(id)) => rec.approval = Some(id),
            Ok(Sought::Declined(msg)) => return Err(self.deny(ctx, rec, msg)),
            Ok(Sought::Pending(needed)) => {
                rec.approval = Some(needed.id.clone());
                self.audit(
                    ctx,
                    AuditRecord {
                        decision: "approval_required".into(),
                        ..rec
                    },
                )?;
                return Ok(Json(Output {
                    result: ProjectDbOut {
                        action: "delete".into(),
                        db: Some(db.name.clone()),
                        scope: Some(db.scope.clone()),
                        status: "approval_required".into(),
                        result: json!({}),
                        labels: vec![],
                        approval: Some(needed),
                    },
                    checks: vec![],
                    provenance: None,
                }));
            }
            Err(e) => return Err(self.deny(ctx, rec, e)),
        }
        if !remove_db_dir(&db.dir) {
            return Err(self.worker_failed(
                ctx,
                rec,
                format!(
                    "{} could not be deleted (is it open in another program?)",
                    db.dir.display()
                ),
            ));
        }
        self.state
            .project_dbs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&db.dir);
        self.audit(ctx, rec)?;
        answer(
            "delete",
            db,
            json!({"deleted": true}),
            &BTreeSet::new(),
            vec![],
            None,
        )
    }
}

fn trim(h: &mut Vec<Value>) {
    if h.len() > HISTORY {
        let extra = h.len() - HISTORY;
        h.drain(..extra);
    }
}
