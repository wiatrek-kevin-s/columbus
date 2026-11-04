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

/// Built-in defaults from the design (section 3, "Limits and defaults").
pub const DEFAULT_LIMITS: Limits = Limits {
    rows: Some(200_000),
    seconds: Some(120),
};
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
    #[serde(default)]
    pub limits: Limits,
    /// Rows returned to the AI provider per call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_rows: Option<u64>,
    /// Field names (wildcards allowed) never returned to the agent.
    #[serde(default)]
    pub hide_fields: Vec<String>,
}

impl Default for OrgPolicy {
    /// The policy in force when no file exists: read-only, no scripts, no
    /// edits, only local file connection types.
    fn default() -> Self {
        OrgPolicy {
            connection_types: vec!["gpkg".into(), "sqlite".into(), "file".into()],
            hosts: vec![],
            tools: vec![],
            deny_tools: vec![],
            allow_scripts: false,
            allow_edits: false,
            limits: DEFAULT_LIMITS,
            return_rows: Some(DEFAULT_RETURN_ROWS),
            hide_fields: vec![],
        }
    }
}

impl OrgPolicy {
    pub fn load(path: &Path) -> Result<Self, LoadError> {
        let text = std::fs::read_to_string(path).map_err(|e| LoadError::Io(path.into(), e))?;
        serde_yaml::from_str(&text).map_err(|e| LoadError::Parse(path.into(), e.to_string()))
    }

    pub fn tool_allowed(&self, tool: &str) -> bool {
        let listed = self.tools.is_empty() || self.tools.iter().any(|t| t == tool);
        listed && !self.deny_tools.iter().any(|t| t == tool)
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
        let c: Connection = serde_yaml::from_str(&text)
            .map_err(|e| LoadError::Parse(path.into(), e.to_string()))?;
        if c.id.is_empty() {
            return Err(LoadError::Invalid(format!(
                "{}: id is empty",
                path.display()
            )));
        }
        // Fail early on bad patterns rather than at query time.
        c.datasets.permits("")?;
        Ok(c)
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
        out.sort_by(|a, b| a.id.cmp(&b.id));
        let mut seen = BTreeSet::new();
        for c in &out {
            if !seen.insert(c.id.clone()) {
                return Err(LoadError::Invalid(format!(
                    "duplicate connection id {:?}",
                    c.id
                )));
            }
        }
        Ok(out)
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
            if !org.host_allowed(host) {
                return Err(Denied::Host(host.clone()));
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
            limits: DEFAULT_LIMITS.narrow(org.limits).narrow(conn.limits),
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

    /// Whether a dataset (physical name) may be read at all.
    pub fn dataset_allowed(&self, name: &str) -> bool {
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
    /// row filter and any data-group filters for that dataset.
    pub fn filters_for(&self, name: &str) -> Vec<String> {
        let key = normalize_identifier(name).to_uppercase();
        let mut out: Vec<String> = self.row_filter.iter().cloned().collect();
        out.extend(
            self.group_filters
                .iter()
                .filter(|(n, _)| *n == key)
                .filter_map(|(_, f)| f.clone()),
        );
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
        o.limits = Limits {
            rows: Some(100_000),
            seconds: Some(30),
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
