//! The four-layer permission model (design section 3).
//!
//! Access is the intersection of organization policy, connection settings,
//! an optional data group and an optional project scope. Each layer can only
//! narrow the one above it; nothing here can widen access.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};

use crate::error::{Denied, LoadError};

/// Read or read-write. Ordered so that `min` gives the narrower mode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Read,
    ReadWrite,
}

/// Row and time limits. Missing values mean "no limit at this layer".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}

impl Limits {
    /// The tighter of two limit sets, field by field.
    pub fn narrow(self, other: Limits) -> Limits {
        fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, None) => a,
                (None, b) => b,
            }
        }
        Limits {
            rows: min_opt(self.rows, other.rows),
            seconds: min_opt(self.seconds, other.seconds),
        }
    }
}

impl Limits {
    /// Rows one read may take: this layer's value, the default where none is
    /// set, never above [`MAX_LIMITS`] and never below 1. Every tool that
    /// reads, writes a result or times a call goes through this (and
    /// [`Limits::max_seconds`]), so they all agree.
    pub fn max_rows(&self) -> u64 {
        self.rows
            .unwrap_or(DEFAULT_ROWS)
            .clamp(1, MAX_LIMITS.rows.unwrap_or(u64::MAX))
    }

    /// Seconds one call may run, as [`Limits::max_rows`] does for rows.
    pub fn max_seconds(&self) -> u64 {
        self.seconds
            .unwrap_or(DEFAULT_SECONDS)
            .clamp(1, MAX_LIMITS.seconds.unwrap_or(u64::MAX))
    }
}

const DEFAULT_ROWS: u64 = 200_000;
const DEFAULT_SECONDS: u64 = 120;

/// Built-in defaults from the design (section 3, "Limits and defaults").
pub const DEFAULT_LIMITS: Limits = Limits {
    rows: Some(DEFAULT_ROWS),
    seconds: Some(DEFAULT_SECONDS),
};

/// The firm ceiling, in code: no policy can raise a limit past it.
pub const MAX_LIMITS: Limits = Limits {
    rows: Some(20_000_000),
    seconds: Some(3600),
};

/// The organization's limits. Below the defaults they always apply; above
/// them only with `allow_raise: true`, and never past [`MAX_LIMITS`].
/// Connections and data groups can then only narrow what this allows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
    /// Honour `rows` and `seconds` above the defaults (200,000 rows, 120 s),
    /// up to the ceiling (20,000,000 rows, 3,600 s). Off: a value above a
    /// default is ignored and the default applies.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_raise: bool,
}

impl OrgLimits {
    /// The defaults, as the organization's limits.
    pub const DEFAULT: OrgLimits = OrgLimits {
        rows: DEFAULT_LIMITS.rows,
        seconds: DEFAULT_LIMITS.seconds,
        allow_raise: false,
    };

    /// The limits in force at the organization layer: the one value every
    /// tool narrows from.
    pub fn effective(&self) -> Limits {
        let set = Limits {
            rows: self.rows,
            seconds: self.seconds,
        };
        let base = if self.allow_raise {
            Limits {
                rows: self.rows.or(DEFAULT_LIMITS.rows),
                seconds: self.seconds.or(DEFAULT_LIMITS.seconds),
            }
        } else {
            DEFAULT_LIMITS.narrow(set)
        };
        let l = base.narrow(MAX_LIMITS);
        Limits {
            rows: Some(l.max_rows()),
            seconds: Some(l.max_seconds()),
        }
    }

    /// Whether the policy asks for more than the defaults and is allowed to.
    pub fn raised(&self) -> bool {
        let e = self.effective();
        e.max_rows() > DEFAULT_ROWS || e.max_seconds() > DEFAULT_SECONDS
    }

    /// A value the policy sets above a default without `allow_raise`, or
    /// above the ceiling: said in `whoami` so an administrator notices.
    pub fn ignored(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (what, set, default, max) in [
            (
                "rows",
                self.rows,
                DEFAULT_ROWS,
                MAX_LIMITS.rows.unwrap_or(u64::MAX),
            ),
            (
                "seconds",
                self.seconds,
                DEFAULT_SECONDS,
                MAX_LIMITS.seconds.unwrap_or(u64::MAX),
            ),
        ] {
            match set {
                Some(v) if v > default && !self.allow_raise => out.push(format!(
                    "limits.{what}: {v} is above the default ({default}) and applies only with limits.allow_raise: true; {default} is in force"
                )),
                Some(v) if v > max => out.push(format!(
                    "limits.{what}: {v} is above the ceiling Columbus allows ({max}); {max} is in force"
                )),
                _ => {}
            }
        }
        out
    }
}
/// Rows returned to the agent per call; larger results stay in the workspace.
pub const DEFAULT_RETURN_ROWS: u64 = 500;

/// Allow and deny lists of dataset names with `*` wildcards, matched
/// case-insensitively (database identifiers usually are).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatasetRules {
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl DatasetRules {
    fn compile(patterns: &[String]) -> Result<Vec<GlobMatcher>, LoadError> {
        patterns
            .iter()
            .map(|p| {
                GlobBuilder::new(p)
                    .case_insensitive(true)
                    .literal_separator(false)
                    .build()
                    .map(|g| g.compile_matcher())
                    .map_err(|e| LoadError::Invalid(format!("bad dataset pattern {p:?}: {e}")))
            })
            .collect()
    }

    /// True when `name` matches an allow pattern and no deny pattern.
    /// An empty allow list allows nothing.
    pub fn permits(&self, name: &str) -> Result<bool, LoadError> {
        let allow = Self::compile(&self.allow)?;
        let deny = Self::compile(&self.deny)?;
        let name = normalize_identifier(name);
        Ok(allow.iter().any(|g| g.is_match(&name)) && !deny.iter().any(|g| g.is_match(&name)))
    }
}

/// Strip quoting so `"ROADS"."ROADWAY"` and `[ROADS].[ROADWAY]` compare as `ROADS.ROADWAY`.
pub fn normalize_identifier(name: &str) -> String {
    name.split('.')
        .map(|part| {
            part.trim()
                .trim_matches(|c| c == '"' || c == '[' || c == ']' || c == '`')
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Layer 1: organization policy, written only by administrators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgPolicy {
    /// Connection types allowed at all (e.g. `oracle`, `postgis`, `gpkg`).
    #[serde(default)]
    pub connection_types: Vec<String>,
    /// Hosts any connection may reach. `*` wildcards allowed.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Tools allowed; empty means all built-in tools.
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub deny_tools: Vec<String>,
    #[serde(default)]
    pub allow_scripts: bool,
    #[serde(default)]
    pub allow_edits: bool,
    /// The person's own tools: scripts the assistant packaged with
    /// `propose_tool` and the person installed in the Columbus app. Off by
    /// default; off hides every installed one at once.
    #[serde(default)]
    pub allow_personal_tools: bool,
    /// The person's workflows: saved jobs the assistant proposed with
    /// `propose_workflow` and the person approved in the Columbus app, run
    /// on a schedule without a person waiting. Off by default; off stops
    /// every installed one at once.
    #[serde(default)]
    pub allow_workflows: bool,
    /// Tools and workflows a coworker shared as a file (Share in the
    /// Columbus app): when on, the person can open one, and what it holds
    /// is filed as proposals they review and approve themselves, exactly as
    /// their own assistant's. Off by default.
    #[serde(default)]
    pub allow_shared_tools: bool,
    /// Whether `map_snapshot` may label features with a field's values
    /// (values the assistant can already read from the layer). On by
    /// default; `false` keeps snapshots to geometry only.
    #[serde(default = "yes")]
    pub snapshot_labels: bool,
    /// Sharing keys the organization vouches for, by name: a shared file
    /// signed by one shows as from that person for everyone.
    #[serde(default)]
    pub share_signers: Vec<crate::share::TrustedSender>,
    /// Open only shared files signed by a key the person or the
    /// organization trusts. Off by default.
    #[serde(default)]
    pub require_signed_shares: bool,
    /// Tools the organization installed for everyone (in its tools folder,
    /// approved by whoever administers it). On by default; off hides them.
    #[serde(default = "yes")]
    pub allow_org_tools: bool,
    /// Rows read and seconds per call; see [`OrgLimits`]. Use
    /// [`OrgPolicy::read_limits`] for the values in force.
    #[serde(default)]
    pub limits: OrgLimits,
    /// The person's local project databases (`project_db`).
    #[serde(default)]
    pub project_db: ProjectDbPolicy,
    /// Rows returned to the AI provider per call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_rows: Option<u64>,
    /// Field names (wildcards allowed) never returned to the agent.
    #[serde(default)]
    pub hide_fields: Vec<String>,
    /// Connected MCP servers (design change 1, section 4).
    #[serde(default)]
    pub servers: ServerPolicy,
    /// Organization data-flow rules between connections and providers
    /// (design change 1, Rule 3). Most specific rule wins; deny beats
    /// approve at equal specificity.
    #[serde(default)]
    pub flows: Vec<FlowRule>,
    /// Engine plugins (design change 1, section 7).
    #[serde(default)]
    pub plugins: PluginPolicy,
    /// How people are asked to approve.
    #[serde(default)]
    pub approvals: ApprovalPolicy,
    /// Folders outside the workspace that `export_output` may write to.
    /// Nothing can be exported anywhere else.
    #[serde(default)]
    pub exports: Vec<ExportFolder>,
    /// Hosts `registry_fetch` may download from without asking the person
    /// (`*.arcgis.com`, `hazards.fema.gov`). Any other host needs approval.
    #[serde(default)]
    pub fetch_hosts: Vec<String>,
    /// The companion window (the Columbus app and the local page).
    #[serde(default)]
    pub companion: CompanionPolicy,
    /// Notes Columbus drafts from a connection's database.
    #[serde(default)]
    pub notes: NotesPolicy,
}

/// Project databases: local DuckDB files the assistant builds from governed
/// results and queries with SQL. They write only to their own file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDbPolicy {
    /// Off: `project_db` refuses every action.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Most memory the database engine uses per call, in MiB; beyond it,
    /// work spills to disk in the database's own folder.
    #[serde(default = "project_db_memory")]
    pub memory_mb: u64,
    /// Most disk one project database may take, in MiB: its file and the
    /// engine's spill folder each. Over it, loads and changes other than
    /// DROP, DELETE and VACUUM are refused.
    #[serde(default = "project_db_max")]
    pub max_mb: u64,
}

fn project_db_memory() -> u64 {
    2048
}

fn project_db_max() -> u64 {
    20480
}

impl Default for ProjectDbPolicy {
    fn default() -> Self {
        ProjectDbPolicy {
            enabled: true,
            memory_mb: project_db_memory(),
            max_mb: project_db_max(),
        }
    }
}

/// What drafted notes may hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotesPolicy {
    /// List the codes short fields hold (up to 30 per field), as code lists
    /// to label. Off: fields, types, keys and comments only.
    #[serde(default = "yes")]
    pub values: bool,
    /// Ask the person about every change the assistant makes to their notes.
    /// Off (the default): one approval covers a whole batch of changes, and
    /// the person may allow note changes for the rest of the session.
    #[serde(default)]
    pub ask_each_change: bool,
}

impl Default for NotesPolicy {
    fn default() -> Self {
        NotesPolicy {
            values: true,
            ask_each_change: false,
        }
    }
}

/// What the companion window may do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanionPolicy {
    /// Whether the person may turn on a basemap from OpenFreeMap. It is off
    /// until they do: tile requests tell that host which area is in view.
    #[serde(default = "yes")]
    pub basemap: bool,
}

impl Default for CompanionPolicy {
    fn default() -> Self {
        CompanionPolicy { basemap: true }
    }
}

/// A folder `export_output` may write to, named by `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFolder {
    pub id: String,
    pub path: std::path::PathBuf,
    /// Exports here need no approval (when every check passes and no flow
    /// rule says otherwise). Default: each export is approved by a person.
    #[serde(default)]
    pub granted: bool,
}

/// How approvals reach the person when their AI client cannot ask them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    /// Open Columbus's local approvals page in the person's browser.
    #[serde(default = "yes")]
    pub page: bool,
    /// How long a tool call waits for the person's answer on the page
    /// before returning `approval_required`. At most 300.
    #[serde(default = "default_wait")]
    pub wait_seconds: u64,
}

fn yes() -> bool {
    true
}

fn default_wait() -> u64 {
    45
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        ApprovalPolicy {
            page: true,
            wait_seconds: default_wait(),
        }
    }
}

/// What engine plugins may be used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginPolicy {
    /// Plugins the person installs under their own profile. Off by default.
    #[serde(default)]
    pub allow_personal: bool,
    /// Plugin ids allowed at all; empty means any organization-installed plugin.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Confinement levels plugins may run at (Rule 5). Default: A only (no
    /// network); add B for plugins that call public services themselves.
    /// Level C is never offered to plugins.
    #[serde(default = "plugin_levels")]
    pub levels: Vec<String>,
    /// Most memory a plugin run may use, in MiB.
    #[serde(default = "plugin_memory")]
    pub memory_mb: u64,
}

impl Default for PluginPolicy {
    fn default() -> Self {
        PluginPolicy {
            allow_personal: false,
            allow: vec![],
            levels: plugin_levels(),
            memory_mb: plugin_memory(),
        }
    }
}

fn plugin_levels() -> Vec<String> {
    vec!["A".into()]
}
fn plugin_memory() -> u64 {
    4096
}
fn server_levels() -> Vec<String> {
    vec!["A".into(), "B".into(), "C".into()]
}
fn server_memory() -> u64 {
    2048
}

/// What connected MCP servers may be used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerPolicy {
    /// Manifests the person adds under their own profile. Off by default:
    /// only organization-managed manifests are loaded.
    #[serde(default)]
    pub allow_personal: bool,
    /// Server ids allowed at all; empty means any organization-managed manifest.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Start local servers and plugins unconfined where this machine cannot
    /// confine them (an operating system without a confinement
    /// implementation, or the launcher missing). Where confinement is
    /// available it is always used. For development and pilots only; off by
    /// default.
    #[serde(default)]
    pub allow_unconfined_local: bool,
    /// Confinement levels local servers may run at (Rule 5); a manifest
    /// asking for another is blocked. Level C also needs `trust: trusted` or
    /// an allow-list entry.
    #[serde(default = "server_levels")]
    pub levels: Vec<String>,
    /// Most memory a local server may use, in MiB.
    #[serde(default = "server_memory")]
    pub memory_mb: u64,
}

impl Default for ServerPolicy {
    fn default() -> Self {
        ServerPolicy {
            allow_personal: false,
            allow: vec![],
            allow_unconfined_local: false,
            levels: server_levels(),
            memory_mb: server_memory(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FlowAction {
    Deny,
    Approve,
}

/// `from` holds labels (`connection:roads-prod`, `connection:*`); `to`
/// holds destinations (`server:drive`, `server:*`, export folders as
/// `folder:<id>` or `folder:*`, a workflow's AI review as `ai:anthropic` or
/// `ai:*`, and `*` for every destination). A rule for `server:*` covers the
/// AI review too: Anthropic is an outside service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowRule {
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub action: FlowAction,
}

impl Default for OrgPolicy {
    /// The policy in force when no file exists: read-only, no scripts, no
    /// edits, only local file connection types (GeoPackage, SQLite,
    /// shapefiles and file geodatabases).
    fn default() -> Self {
        OrgPolicy {
            connection_types: vec![
                "gpkg".into(),
                "sqlite".into(),
                "file".into(),
                "shapefile".into(),
                "filegdb".into(),
            ],
            hosts: vec![],
            tools: vec![],
            deny_tools: vec![],
            allow_scripts: false,
            allow_edits: false,
            allow_personal_tools: false,
            allow_shared_tools: false,
            snapshot_labels: true,
            share_signers: vec![],
            require_signed_shares: false,
            allow_org_tools: true,
            allow_workflows: false,
            limits: OrgLimits::DEFAULT,
            project_db: ProjectDbPolicy::default(),
            return_rows: Some(DEFAULT_RETURN_ROWS),
            hide_fields: vec![],
            servers: ServerPolicy::default(),
            flows: vec![],
            plugins: PluginPolicy::default(),
            approvals: ApprovalPolicy::default(),
            exports: vec![],
            fetch_hosts: vec![],
            companion: CompanionPolicy::default(),
            notes: NotesPolicy::default(),
        }
    }
}

impl OrgPolicy {
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let text = std::fs::read_to_string(path).map_err(|e| LoadError::Io(path.into(), e))?;
        Self::parse(&text, path)
    }

    /// The policy in `text`, as read from `path` (named in errors).
    pub fn parse(text: &str, path: &Path) -> Result<Self, LoadError> {
        serde_yaml::from_str(text).map_err(|e| LoadError::Parse(path.into(), e.to_string()))
    }

    /// Rows and seconds in force at the organization layer (see
    /// [`OrgLimits::effective`]); a connection narrows from here.
    pub fn read_limits(&self) -> Limits {
        self.limits.effective()
    }

    pub fn tool_allowed(&self, tool: &str) -> bool {
        let listed = self.tools.is_empty() || self.tools.iter().any(|t| t == tool);
        listed && !self.deny_tools.iter().any(|t| t == tool)
    }

    /// May `registry_fetch` download from this host without approval?
    pub fn fetch_host_allowed(&self, host: &str) -> bool {
        let rules = DatasetRules {
            allow: self.fetch_hosts.clone(),
            deny: vec![],
        };
        rules.permits(host).unwrap_or(false)
    }

    pub fn host_allowed(&self, host: &str) -> bool {
        let rules = DatasetRules {
            allow: self.hosts.clone(),
            deny: vec![],
        };
        rules.permits(host).unwrap_or(false)
    }
}

/// Layer 2: a connection, set by the person who adds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// Data source name, file path or service URL. Never a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Database name (PostgreSQL, SQL Server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Login name. The password is never here: it lives in the operating
    /// system's credential store under this connection's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// PostgreSQL `sslmode` (`disable`, `prefer`, `require`, `verify-full`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sslmode: Option<String>,
    /// Schemas searched for unqualified table names. Defaults to `public`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_path: Option<String>,
    #[serde(default = "default_mode")]
    pub mode: Mode,
    pub datasets: DatasetRules,
    /// SQL boolean expression added to every query on this connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_filter: Option<String>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_mode() -> Mode {
    Mode::Read
}

impl Connection {
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let text = std::fs::read_to_string(path).map_err(|e| LoadError::Io(path.into(), e))?;
        Self::parse(&text, path)
    }

    /// The connection in `text`, as read from `path` (named in errors).
    pub fn parse(text: &str, path: &Path) -> Result<Self, LoadError> {
        let c: Connection =
            serde_yaml::from_str(text).map_err(|e| LoadError::Parse(path.into(), e.to_string()))?;
        c.validate(path)?;
        Ok(c)
    }

    /// What `load` checks beyond parsing: an id, and dataset patterns that
    /// compile.
    pub fn validate(&self, path: &Path) -> Result<(), LoadError> {
        let c = self;
        if c.id.is_empty() {
            return Err(LoadError::Invalid(format!(
                "{}: id is empty",
                path.display()
            )));
        }
        // Fail early on bad patterns rather than at query time.
        c.datasets.permits("")?;
        Ok(())
    }

    /// Where a connection points, and as whom: everything that decides
    /// which data a stored password opens.
    pub fn location(&self) -> serde_json::Value {
        serde_json::json!([
            self.kind.to_ascii_lowercase(),
            self.dsn,
            self.path,
            self.host,
            self.port,
            self.database,
            self.user,
            self.sslmode,
            self.search_path
        ])
    }

    /// Load every `*.yaml` / `*.yml` in a folder, sorted by id.
    pub fn load_dir(dir: &Path) -> Result<Vec<Self>, LoadError> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        let entries = std::fs::read_dir(dir).map_err(|e| LoadError::Io(dir.into(), e))?;
        for entry in entries {
            let path = entry.map_err(|e| LoadError::Io(dir.into(), e))?.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "yaml" || ext == "yml" {
                out.push(Connection::load(&path)?);
            }
        }
        Self::check_ids(&mut out)?;
        Ok(out)
    }

    /// Sort by id and refuse duplicate ids, as `load_dir` does.
    pub fn check_ids(out: &mut [Self]) -> Result<(), LoadError> {
        out.sort_by(|a, b| a.id.cmp(&b.id));
        let mut seen = BTreeSet::new();
        for c in out.iter() {
            if !seen.insert(c.id.clone()) {
                return Err(LoadError::Invalid(format!(
                    "duplicate connection id {:?}",
                    c.id
                )));
            }
        }
        Ok(())
    }
}

/// Layer 3: a named subset of datasets on one connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataGroup {
    pub id: String,
    pub connection: String,
    pub datasets: Vec<GroupDataset>,
    #[serde(default = "default_mode")]
    pub mode: Mode,
}

/// One dataset in a data group, by physical name, with an optional filter.
///
/// Context packs name datasets by context id (section 5); the context
/// module resolves those to physical names before building a `DataGroup`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupDataset {
    pub physical: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// Layer 4: what one project or session may use.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectScope {
    /// Connection ids; empty means every connection the person has.
    #[serde(default)]
    pub connections: Vec<String>,
    /// When set, the session may only touch datasets in these groups.
    #[serde(default)]
    pub data_groups: Vec<DataGroup>,
}

/// The access in force for one connection after all four layers.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveAccess {
    pub connection: String,
    pub kind: String,
    pub mode: Mode,
    pub limits: Limits,
    pub return_rows: u64,
    pub row_filter: Option<String>,
    #[serde(skip)]
    datasets: DatasetRules,
    /// Per-dataset filters from data groups, keyed by normalized name.
    #[serde(skip)]
    group_filters: Vec<(String, Option<String>)>,
    /// When a data group applies, only these datasets are reachable.
    #[serde(skip)]
    group_only: bool,
    pub hide_fields: Vec<String>,
}

impl EffectiveAccess {
    /// Combine the layers for one connection at time `now`.
    pub fn resolve(
        org: &OrgPolicy,
        conn: &Connection,
        scope: &ProjectScope,
        now: DateTime<Utc>,
    ) -> Result<Self, Denied> {
        if !org
            .connection_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&conn.kind))
        {
            return Err(Denied::ConnectionType(conn.kind.clone()));
        }
        if let Some(host) = &conn.host {
            let k = conn.kind.to_ascii_lowercase();
            let sql_server = matches!(
                k.as_str(),
                "sqlserver" | "mssql" | "sde-sqlserver" | "azure-sql"
            );
            match plain_host(host, sql_server) {
                Some(name) if org.host_allowed(name) => {}
                _ => return Err(Denied::Host(host.clone())),
            }
        }
        // An Oracle service named by `dsn` is checked like a host: an
        // EZConnect string by the host in it, a descriptor not at all (it can
        // name any address). A bare alias is resolved by the organization's
        // own tnsnames.ora.
        if let Some(dsn) = &conn.dsn {
            let k = conn.kind.to_ascii_lowercase();
            if matches!(k.as_str(), "oracle" | "sde-oracle") {
                match dsn_host(dsn) {
                    DsnHost::Alias => {}
                    DsnHost::Host(h)
                        if plain_host(h, false).is_some_and(|n| org.host_allowed(n)) => {}
                    _ => return Err(Denied::Host(dsn.clone())),
                }
            }
        }
        if let Some(exp) = conn.expires {
            if now.date_naive() > exp {
                return Err(Denied::Expired(conn.id.clone(), exp));
            }
        }
        if !scope.connections.is_empty() && !scope.connections.contains(&conn.id) {
            return Err(Denied::NotInProject(conn.id.clone()));
        }

        let mut mode = conn.mode;
        if !org.allow_edits {
            mode = Mode::Read;
        }

        let groups: Vec<&DataGroup> = scope
            .data_groups
            .iter()
            .filter(|g| g.connection == conn.id)
            .collect();
        let mut group_filters = Vec::new();
        for g in &groups {
            mode = mode.min(g.mode);
            for d in &g.datasets {
                group_filters.push((
                    normalize_identifier(&d.physical).to_uppercase(),
                    d.filter.clone(),
                ));
            }
        }
        // A project that names data groups, but none for this connection,
        // gets no datasets from it.
        let group_only = !scope.data_groups.is_empty();

        Ok(EffectiveAccess {
            connection: conn.id.clone(),
            kind: conn.kind.clone(),
            mode,
            limits: org.read_limits().narrow(conn.limits),
            return_rows: org
                .return_rows
                .unwrap_or(DEFAULT_RETURN_ROWS)
                .min(DEFAULT_RETURN_ROWS),
            row_filter: conn.row_filter.clone(),
            datasets: conn.datasets.clone(),
            group_filters,
            group_only,
            hide_fields: org.hide_fields.clone(),
        })
    }

    /// Whether a dataset (physical name) may be read at all. System
    /// catalogs are never allowed, whatever the allow list says, so a
    /// broad pattern like `*` cannot expose logins, settings or other
    /// schemas' metadata.
    pub fn dataset_allowed(&self, name: &str) -> bool {
        if is_system_catalog(name, &self.kind) {
            return false;
        }
        if !self.datasets.permits(name).unwrap_or(false) {
            return false;
        }
        if self.group_only {
            let key = normalize_identifier(name).to_uppercase();
            return self.group_filters.iter().any(|(n, _)| *n == key);
        }
        true
    }

    /// All filters that must be ANDed onto reads of `name`: the connection's
    /// row filter and, under data groups, the groups' filter for that
    /// dataset. Groups add up (a dataset in two groups shows the rows either
    /// keeps, as its datasets add up in `dataset_allowed`), so their filters
    /// for one dataset are ORed, and a group listing it without a filter
    /// keeps every row; the connection's own filter always applies.
    pub fn filters_for(&self, name: &str) -> Vec<String> {
        let key = normalize_identifier(name).to_uppercase();
        let mut out: Vec<String> = self.row_filter.iter().cloned().collect();
        let entries: Vec<&Option<String>> = self
            .group_filters
            .iter()
            .filter(|(n, _)| *n == key)
            .map(|(_, f)| f)
            .collect();
        if entries.is_empty() || entries.iter().any(|f| f.is_none()) {
            return out;
        }
        let mut each: Vec<&str> = entries.iter().filter_map(|f| f.as_deref()).collect();
        each.sort_unstable();
        each.dedup();
        match each.as_slice() {
            [one] => out.push(one.to_string()),
            many => out.push(
                many.iter()
                    .map(|f| format!("({f})"))
                    .collect::<Vec<_>>()
                    .join(" OR "),
            ),
        }
        out
    }

    pub fn field_hidden(&self, field: &str) -> bool {
        DatasetRules {
            allow: self.hide_fields.clone(),
            deny: vec![],
        }
        .permits(field)
        .unwrap_or(false)
    }
}

/// Schemas that hold database internals on the supported engines.
const SYSTEM_SCHEMAS: &[&str] = &[
    "PG_CATALOG",
    "INFORMATION_SCHEMA",
    "PG_TOAST",
    "SYS",
    "SYSTEM",
    "MDSYS",
    "CTXSYS",
    "XDB",
    "LBACSYS",
    "DVSYS",
    "AUDSYS",
    "OUTLN",
    "SYSIBM",
    "MASTER",
    "MSDB",
    "TEMPDB",
    "MODEL",
];

/// True for system catalog tables and views on the connection's engine,
/// including the unqualified names that resolve to them (PostgreSQL always
/// searches `pg_catalog` first; Oracle's `DBA_`/`ALL_`/`USER_`/`V$` views
/// are public synonyms; SQL Server's `sys*` compatibility views).
pub fn is_system_catalog(name: &str, kind: &str) -> bool {
    let norm = normalize_identifier(name).to_ascii_uppercase();
    let parts: Vec<&str> = norm.split('.').collect();
    let last = *parts.last().unwrap_or(&"");
    if parts.len() >= 2
        && parts[..parts.len() - 1]
            .iter()
            .any(|p| SYSTEM_SCHEMAS.contains(p))
    {
        return true;
    }
    match kind.to_ascii_lowercase().as_str() {
        "postgis" | "postgres" | "postgresql" | "duckdb" | "geoparquet" => {
            parts.len() == 1 && (last.starts_with("PG_") || last.starts_with("DUCKDB_"))
        }
        "oracle" | "sde-oracle" => {
            parts.len() == 1
                && ["DBA_", "ALL_", "USER_", "CDB_", "V$", "GV$", "V_$", "GV_$"]
                    .iter()
                    .any(|p| last.starts_with(p))
        }
        "sqlserver" | "mssql" | "azure-sql" | "sde-sqlserver" => {
            parts.len() == 1 && last.starts_with("SYS")
        }
        "gpkg" | "geopackage" | "sqlite" | "spatialite" | "shapefile" => {
            last.starts_with("SQLITE_")
        }
        // A file geodatabase's own tables (GDB_Items and the like).
        "filegdb" => last.starts_with("SQLITE_") || last.starts_with("GDB_"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn roads() -> Connection {
        serde_yaml::from_str(
            r#"
id: roads-prod
type: oracle
dsn: ORAPROD
host: oraprod.corp.example
mode: read-write
datasets:
  allow: ["ROADS.ROADWAY_*", "ROADS.CONTROL_SECTION"]
  deny:  ["ROADS.*_AUDIT"]
row_filter: "DISTRICT_NBR = 14"
limits: { rows: 50000 }
expires: 2027-06-30
"#,
        )
        .unwrap()
    }

    fn org() -> OrgPolicy {
        OrgPolicy {
            connection_types: vec!["oracle".into()],
            hosts: vec!["*.corp.example".into()],
            ..OrgPolicy::default()
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap()
    }

    #[test]
    fn allow_and_deny_lists() {
        let a =
            EffectiveAccess::resolve(&org(), &roads(), &ProjectScope::default(), now()).unwrap();
        assert!(a.dataset_allowed("ROADS.ROADWAY_INVENTORY"));
        assert!(a.dataset_allowed("roads.roadway_inventory"));
        assert!(a.dataset_allowed("\"ROADS\".\"CONTROL_SECTION\""));
        assert!(!a.dataset_allowed("ROADS.ROADWAY_AUDIT"));
        assert!(!a.dataset_allowed("HR.EMPLOYEES"));
    }

    #[test]
    fn edits_need_org_permission() {
        let a =
            EffectiveAccess::resolve(&org(), &roads(), &ProjectScope::default(), now()).unwrap();
        assert_eq!(a.mode, Mode::Read);
        let mut o = org();
        o.allow_edits = true;
        let a = EffectiveAccess::resolve(&o, &roads(), &ProjectScope::default(), now()).unwrap();
        assert_eq!(a.mode, Mode::ReadWrite);
    }

    #[test]
    fn limits_take_the_tightest() {
        let mut o = org();
        o.limits = OrgLimits {
            rows: Some(100_000),
            seconds: Some(30),
            allow_raise: false,
        };
        let a = EffectiveAccess::resolve(&o, &roads(), &ProjectScope::default(), now()).unwrap();
        assert_eq!(
            a.limits,
            Limits {
                rows: Some(50_000),
                seconds: Some(30)
            }
        );
    }

    #[test]
    fn raising_limits_needs_the_opt_in_and_stops_at_the_ceiling() {
        let open = |rows, seconds, allow_raise| {
            let mut o = org();
            o.limits = OrgLimits {
                rows,
                seconds,
                allow_raise,
            };
            let mut c = roads();
            c.limits = Limits::default();
            EffectiveAccess::resolve(&o, &c, &ProjectScope::default(), now())
                .unwrap()
                .limits
        };
        // Above the default without the opt-in: the default applies.
        let l = open(Some(2_000_000), Some(900), false);
        assert_eq!((l.max_rows(), l.max_seconds()), (200_000, 120));
        // With it: honoured.
        let l = open(Some(2_000_000), Some(900), true);
        assert_eq!((l.max_rows(), l.max_seconds()), (2_000_000, 900));
        // Never past the ceiling in code.
        let l = open(Some(u64::MAX), Some(1_000_000), true);
        assert_eq!((l.max_rows(), l.max_seconds()), (20_000_000, 3600));
        // The opt-in alone raises nothing.
        let l = open(None, None, true);
        assert_eq!((l.max_rows(), l.max_seconds()), (200_000, 120));
        // Lowering needs no opt-in.
        let l = open(Some(10), Some(5), false);
        assert_eq!((l.max_rows(), l.max_seconds()), (10, 5));
        // A connection still only narrows a raised limit.
        let mut o = org();
        o.limits = OrgLimits {
            rows: Some(5_000_000),
            seconds: None,
            allow_raise: true,
        };
        let a = EffectiveAccess::resolve(&o, &roads(), &ProjectScope::default(), now()).unwrap();
        assert_eq!(a.limits.max_rows(), 50_000);
        let ignored = OrgLimits {
            rows: Some(300_000),
            seconds: None,
            allow_raise: false,
        }
        .ignored();
        assert!(ignored[0].contains("allow_raise"), "{ignored:?}");
        let p: OrgPolicy =
            serde_yaml::from_str("limits: { rows: 2000000, seconds: 900, allow_raise: true }\n")
                .unwrap();
        assert_eq!(p.read_limits().max_rows(), 2_000_000);
        assert!(p.limits.raised());
        // Zero means one, never "no limit".
        assert_eq!(
            Limits {
                rows: Some(0),
                seconds: Some(0)
            }
            .max_rows(),
            1
        );
    }

    #[test]
    fn org_policy_blocks_types_hosts_and_expiry() {
        let mut o = org();
        o.connection_types = vec!["postgis".into()];
        assert!(matches!(
            EffectiveAccess::resolve(&o, &roads(), &ProjectScope::default(), now()),
            Err(Denied::ConnectionType(_))
        ));
        let mut o = org();
        o.hosts = vec!["*.other.example".into()];
        assert!(matches!(
            EffectiveAccess::resolve(&o, &roads(), &ProjectScope::default(), now()),
            Err(Denied::Host(_))
        ));
        let later = Utc.with_ymd_and_hms(2027, 7, 1, 0, 0, 0).unwrap();
        assert!(matches!(
            EffectiveAccess::resolve(&org(), &roads(), &ProjectScope::default(), later),
            Err(Denied::Expired(..))
        ));
    }

    #[test]
    fn data_groups_narrow_and_add_filters() {
        let scope = ProjectScope {
            connections: vec!["roads-prod".into()],
            data_groups: vec![DataGroup {
                id: "live".into(),
                connection: "roads-prod".into(),
                mode: Mode::Read,
                datasets: vec![
                    GroupDataset {
                        physical: "ROADS.ROADWAY_INVENTORY".into(),
                        filter: None,
                    },
                    GroupDataset {
                        physical: "ROADS.ROADWAY_CENTERLINE".into(),
                        filter: Some("STATUS <> 'R'".into()),
                    },
                    // Listed in the group but denied by the connection: stays denied.
                    GroupDataset {
                        physical: "ROADS.ROADWAY_AUDIT".into(),
                        filter: None,
                    },
                ],
            }],
        };
        let a = EffectiveAccess::resolve(&org(), &roads(), &scope, now()).unwrap();
        assert!(a.dataset_allowed("ROADS.ROADWAY_INVENTORY"));
        assert!(!a.dataset_allowed("ROADS.CONTROL_SECTION"));
        assert!(!a.dataset_allowed("ROADS.ROADWAY_AUDIT"));
        assert_eq!(
            a.filters_for("roads.roadway_centerline"),
            vec!["DISTRICT_NBR = 14".to_string(), "STATUS <> 'R'".to_string()]
        );
    }

    #[test]
    fn groups_sharing_a_dataset_add_up() {
        let g = |id: &str, filter: Option<&str>| DataGroup {
            id: id.into(),
            connection: "roads-prod".into(),
            mode: Mode::Read,
            datasets: vec![GroupDataset {
                physical: "ROADS.ROADWAY_CENTERLINE".into(),
                filter: filter.map(String::from),
            }],
        };
        let scope = |groups| ProjectScope {
            connections: vec![],
            data_groups: groups,
        };
        let a = EffectiveAccess::resolve(
            &org(),
            &roads(),
            &scope(vec![g("a", Some("COUNTY = 1")), g("b", Some("COUNTY = 2"))]),
            now(),
        )
        .unwrap();
        // Either group's rows, always under the connection's own filter.
        assert_eq!(
            a.filters_for("ROADS.ROADWAY_CENTERLINE"),
            vec![
                "DISTRICT_NBR = 14".to_string(),
                "(COUNTY = 1) OR (COUNTY = 2)".to_string()
            ]
        );
        // A group that lists it without a filter keeps all of its rows.
        let a = EffectiveAccess::resolve(
            &org(),
            &roads(),
            &scope(vec![g("a", Some("COUNTY = 1")), g("b", None)]),
            now(),
        )
        .unwrap();
        assert_eq!(
            a.filters_for("ROADS.ROADWAY_CENTERLINE"),
            vec!["DISTRICT_NBR = 14".to_string()]
        );
        // The same filter twice is one filter.
        let a = EffectiveAccess::resolve(
            &org(),
            &roads(),
            &scope(vec![g("a", Some("COUNTY = 1")), g("b", Some("COUNTY = 1"))]),
            now(),
        )
        .unwrap();
        assert_eq!(
            a.filters_for("ROADS.ROADWAY_CENTERLINE"),
            vec!["DISTRICT_NBR = 14".to_string(), "COUNTY = 1".to_string()]
        );
    }

    #[test]
    fn system_catalogs_never_allowed() {
        let mut conn = roads();
        conn.datasets.allow = vec!["*".into()];
        conn.datasets.deny = vec![];
        for (kind, name, system) in [
            ("postgis", "pg_catalog.pg_authid", true),
            ("postgis", "pg_roles", true),
            ("postgis", "information_schema.tables", true),
            ("postgis", "\"PG_CATALOG\".\"PG_SETTINGS\"", true),
            ("postgis", "public.roads", false),
            ("postgis", "roads.pg_style_lookup", false),
            ("oracle", "SYS.USER$", true),
            ("oracle", "DBA_USERS", true),
            ("oracle", "V$SESSION", true),
            ("oracle", "ROADS.ALL_SEGMENTS", false),
            ("sqlserver", "sys.sql_logins", true),
            ("sqlserver", "sysobjects", true),
            ("sqlserver", "master.dbo.spt_values", true),
            ("sqlserver", "dbo.roads", false),
            ("gpkg", "sqlite_master", true),
            ("gpkg", "roads", false),
        ] {
            assert_eq!(is_system_catalog(name, kind), system, "{kind} {name}");
            conn.kind = kind.into();
            let mut o = org();
            o.connection_types = vec![kind.into()];
            let a = EffectiveAccess::resolve(&o, &conn, &ProjectScope::default(), now()).unwrap();
            assert_eq!(
                a.dataset_allowed(name),
                !system,
                "{kind} {name} under allow *"
            );
        }
    }

    #[test]
    fn project_scope_limits_connections() {
        let scope = ProjectScope {
            connections: vec!["other".into()],
            data_groups: vec![],
        };
        assert!(matches!(
            EffectiveAccess::resolve(&org(), &roads(), &scope, now()),
            Err(Denied::NotInProject(_))
        ));
    }

    #[test]
    fn return_rows_never_exceed_default() {
        let mut o = org();
        o.return_rows = Some(10_000);
        let a = EffectiveAccess::resolve(&o, &roads(), &ProjectScope::default(), now()).unwrap();
        assert_eq!(a.return_rows, 500);
    }

    #[test]
    fn hidden_fields() {
        let mut o = org();
        o.hide_fields = vec!["*SSN*".into(), "OWNER_NAME".into()];
        let a = EffectiveAccess::resolve(&o, &roads(), &ProjectScope::default(), now()).unwrap();
        assert!(a.field_hidden("owner_name"));
        assert!(a.field_hidden("EMP_SSN_LAST4"));
        assert!(!a.field_hidden("ROUTE_ID"));
    }
}

/// The host name a `host` setting reaches, if it is exactly one host: no
/// list (`a,b`), path, option string or credentials, which could reach a
/// host the allow-list never saw. SQL Server may add one named instance
/// (`db01\\SQLEXPRESS`); the allow-list is checked against the host part.
pub fn plain_host(host: &str, sql_server_instance: bool) -> Option<&str> {
    let (name, instance) = match host.split_once('\\') {
        Some((n, i)) if sql_server_instance => (n, Some(i)),
        Some(_) => return None,
        None => (host, None),
    };
    let bad = |c: char| c.is_whitespace() || c.is_control() || ",;/\\?#@=()'\"".contains(c);
    if name.is_empty() || name.chars().any(bad) {
        return None;
    }
    if let Some(i) = instance {
        if i.is_empty()
            || i.len() > 64
            || !i
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        {
            return None;
        }
    }
    Some(name)
}

enum DsnHost<'a> {
    /// A TNS alias (letters, digits, `_` and `-`, no dots).
    Alias,
    /// The host of an EZConnect string.
    Host(&'a str),
    /// Anything else (a connect descriptor, an empty or odd string).
    Other,
}

/// Strictly `[tcp://|tcps://][//]host[:port]/service[:server][/instance]`
/// (EZConnect, one address); a name with no `/service` is looked up in
/// tnsnames.ora by the driver, so it is an alias; anything else, a second
/// address after `;` included, is `Other`.
fn dsn_host(dsn: &str) -> DsnHost<'_> {
    let word = |t: &str| {
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    };
    let d = dsn.trim();
    if !d.is_empty()
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return DsnHost::Alias;
    }
    let lower = d.to_ascii_lowercase();
    let rest = ["tcps://", "tcp://"]
        .iter()
        .find_map(|p| lower.starts_with(p).then(|| &d[p.len()..]))
        .unwrap_or(d);
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (host, after) = if let Some(r) = rest.strip_prefix('[') {
        match r.split_once(']') {
            Some((h, a))
                if !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit() || c == ':') =>
            {
                (h, a)
            }
            _ => return DsnHost::Other,
        }
    } else {
        let end = rest.find([':', '/']).unwrap_or(rest.len());
        let h = &rest[..end];
        if !word(h) {
            return DsnHost::Other;
        }
        (h, &rest[end..])
    };
    let (port, path) = match after.strip_prefix(':') {
        Some(a) => {
            let end = a.find('/').unwrap_or(a.len());
            (Some(&a[..end]), &a[end..])
        }
        None => (None, after),
    };
    if port.is_some_and(|p| p.is_empty() || p.len() > 5 || !p.chars().all(|c| c.is_ascii_digit())) {
        return DsnHost::Other;
    }
    // With no service it is not EZConnect: the driver looks the whole
    // string up in tnsnames.ora, like a bare alias.
    if path.is_empty() {
        return DsnHost::Alias;
    }
    {
        let Some(p) = path.strip_prefix('/') else {
            return DsnHost::Other;
        };
        let mut parts = p.splitn(2, '/');
        let service = parts.next().unwrap_or("");
        let (name, server) = match service.split_once(':') {
            Some((n, s)) => (n, Some(s)),
            None => (service, None),
        };
        if !word(name) || server.is_some_and(|s| !word(s)) {
            return DsnHost::Other;
        }
        if let Some(instance) = parts.next() {
            if !word(instance) {
                return DsnHost::Other;
            }
        }
    }
    DsnHost::Host(host)
}

#[cfg(test)]
mod host_tests {
    use super::*;

    #[test]
    fn only_one_plain_host_is_a_host() {
        assert_eq!(
            plain_host("db.corp.example", false),
            Some("db.corp.example")
        );
        assert_eq!(plain_host("::1", false), Some("::1"));
        assert_eq!(plain_host("db01\\SQLEXPRESS", true), Some("db01"));
        for bad in [
            "evil.com,db.corp.example",
            "db/x",
            "a b",
            "u@db",
            "db01\\SQLEXPRESS",
            "",
        ] {
            assert_eq!(plain_host(bad, false), None, "{bad}");
        }
        assert_eq!(plain_host("db01\\bad name", true), None);
        assert_eq!(plain_host("db01\\a\\b", true), None);
    }

    #[test]
    fn oracle_dsns_are_checked_by_their_host() {
        assert!(matches!(dsn_host("PRODDB"), DsnHost::Alias));
        assert!(matches!(
            dsn_host("db.corp.example:1521/svc"),
            DsnHost::Host("db.corp.example")
        ));
        assert!(matches!(
            dsn_host("//db.corp.example/svc"),
            DsnHost::Host("db.corp.example")
        ));
        assert!(matches!(
            dsn_host("tcps://db.corp.example:2484/svc"),
            DsnHost::Host("db.corp.example")
        ));
        assert!(matches!(dsn_host("[::1]:1521/svc"), DsnHost::Host("::1")));
        assert!(matches!(
            dsn_host("(DESCRIPTION=(ADDRESS=(HOST=evil)))"),
            DsnHost::Other
        ));
        // A second address, options or odd characters: refused.
        for bad in [
            "db.corp.example:1521;evil.example:1521/svc",
            "db.corp.example:;evil.example/svc",
            "db.corp.example:1521/svc;x",
            "db.corp.example:15x1/svc",
            "db.corp.example/svc?x=1",
            "db.corp.example/svc/inst/more",
        ] {
            assert!(matches!(dsn_host(bad), DsnHost::Other), "{bad}");
        }
        assert!(matches!(
            dsn_host("db.corp.example:1521/svc:dedicated/inst"),
            DsnHost::Host("db.corp.example")
        ));
        // With no service the driver reads it as a tnsnames alias, dotted
        // or not.
        assert!(matches!(dsn_host("PROD.WORLD"), DsnHost::Alias));
        assert!(matches!(dsn_host("db.corp:1521"), DsnHost::Alias));
    }
}
