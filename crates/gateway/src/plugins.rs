//! Engine plugins (design change 1, section 7): a folder with a
//! `plugin.yaml` and a worker built on the Columbus plugin SDK, speaking
//! worker protocol v2 (docs/worker-protocol.md).
//!
//! Plugins follow Rule 1 like connected servers: a plugin runs only when a
//! person has approved its exact contents (a hash over `plugin.yaml` and its
//! entry file, recorded in `approved.yaml` beside the plugins), and its
//! `describe` answer must match its `plugin.yaml`. Any change switches it
//! off until it is approved again.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use columbus_broker::manifest::Risk;
use columbus_shared::canon::sha256_hex;
use columbus_shared::policy::OrgPolicy;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::OnceCell;

use crate::worker::{Launch, Worker};

pub const APPROVALS_FILE: &str = "approved.yaml";
/// Ids the built-in engine owns.
const RESERVED: &[&str] = &["columbus", "columbus.core"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Python,
    Rust,
}

/// A field a layer or table input must have, or an output will have.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FieldSpec {
    pub name: String,
    /// text, integer, number, boolean, date or any.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The shapes a text parameter may take when it is not one of a fixed list:
/// enough for field names and labels, never for code, expressions or
/// templates (no brackets, braces, quotes, `@`, `%`, `$`, backslashes or
/// line breaks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextFormat {
    /// A field or column name: letters, digits and `_`, starting with a
    /// letter or `_`, at most 64 characters.
    FieldName,
    /// A short label or code (a route id, a title): letters, digits,
    /// spaces and `-_.,:/#&+` , at most 80 characters.
    Label,
}

impl TextFormat {
    pub fn pattern(self) -> &'static str {
        match self {
            TextFormat::FieldName => "^[A-Za-z_][A-Za-z0-9_]{0,63}$",
            TextFormat::Label => "^[A-Za-z0-9 _.,:/#&+-]{0,80}$",
        }
    }

    pub fn max_len(self) -> usize {
        match self {
            TextFormat::FieldName => 64,
            TextFormat::Label => 80,
        }
    }

    /// Whether `s` has this shape.
    pub fn accepts(self, s: &str) -> bool {
        let ok_char = |c: char, first: bool| match self {
            TextFormat::FieldName => {
                c == '_' || c.is_ascii_alphabetic() || (!first && c.is_ascii_digit())
            }
            TextFormat::Label => c.is_ascii_alphanumeric() || " _.,:/#&+-".contains(c),
        };
        let n = s.chars().count();
        let size_ok = match self {
            TextFormat::FieldName => (1..=64).contains(&n),
            TextFormat::Label => n <= 80,
        };
        size_ok && s.chars().enumerate().all(|(i, c)| ok_char(c, i == 0))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ParamSpec {
    Dataset {
        #[serde(default)]
        access: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Number {
        #[serde(default)]
        unit: Option<String>,
        #[serde(default)]
        default: Option<f64>,
        #[serde(default)]
        min: Option<f64>,
        #[serde(default)]
        max: Option<f64>,
        /// No default, and may be left out.
        #[serde(default)]
        optional: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Integer {
        #[serde(default)]
        default: Option<i64>,
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
        /// No default, and may be left out.
        #[serde(default)]
        optional: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    String {
        #[serde(default, rename = "enum")]
        choices: Option<Vec<String>>,
        /// The shape the text must have, when it is not one of `enum`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<TextFormat>,
        #[serde(default)]
        default: Option<String>,
        /// No default, and may be left out.
        #[serde(default)]
        optional: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Boolean {
        #[serde(default)]
        default: Option<bool>,
        /// No default, and may be left out.
        #[serde(default)]
        optional: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// A workspace file (a layer or table handle).
    File {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// layer or table.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// point, line, polygon or any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        geometry: Option<String>,
        /// What CRS it must be in ("EPSG:3081", "projected", "any").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        crs: Option<String>,
        /// Fields it must have.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<FieldSpec>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OutputSpec {
    Layer {
        #[serde(default)]
        geometry: Option<String>,
        /// gpkg or geojson (the default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        crs: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<FieldSpec>,
    },
    Table {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<FieldSpec>,
    },
    File {
        #[serde(default)]
        format: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl OutputSpec {
    pub fn extension(&self) -> String {
        match self {
            OutputSpec::Layer { format, .. } if format.as_deref() == Some("gpkg") => "gpkg".into(),
            OutputSpec::Layer { .. } => "geojson".into(),
            OutputSpec::Table { .. } => "csv".into(),
            OutputSpec::File { format, .. } => format
                .as_deref()
                .filter(|f| f.chars().all(|c| c.is_ascii_alphanumeric()) && !f.is_empty())
                .unwrap_or("bin")
                .to_string(),
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            OutputSpec::Layer { .. } => "layer",
            OutputSpec::Table { .. } => "table",
            OutputSpec::File { .. } => "file",
        }
    }
    pub fn description(&self) -> Option<&str> {
        match self {
            OutputSpec::Layer { description, .. }
            | OutputSpec::Table { description, .. }
            | OutputSpec::File { description, .. } => description.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginTool {
    pub summary: String,
    #[serde(default)]
    pub params: BTreeMap<String, ParamSpec>,
    #[serde(default)]
    pub outputs: BTreeMap<String, OutputSpec>,
    #[serde(default = "default_risk")]
    pub risk: Risk,
    /// Verification checks the tool's results should pass (design section 4).
    #[serde(default)]
    pub checks: Vec<String>,
    /// A short name for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What it does and when to use it, for other assistants (the summary
    /// is its first sentence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When not to use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_for: Option<String>,
    /// What each check in `checks` means.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub check_notes: BTreeMap<String, String>,
    /// Example calls: `{description, arguments}`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<Value>,
}

fn default_risk() -> Risk {
    Risk::Read
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub version: String,
    pub runtime: Runtime,
    pub entry: String,
    #[serde(default)]
    pub engine_env: Option<String>,
    /// Rule 5: the level the plugin runs at. A (no network) when absent; B
    /// (internet) only where organization policy lists it for plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confinement: Option<PluginConfinement>,
    /// F-5, personal tools only: the registered governed environment the
    /// tool runs in (its id, never a path); absent: the engine's Python.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub tools: BTreeMap<String, PluginTool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginConfinement {
    #[serde(default)]
    pub level: Option<String>,
}

impl PluginManifest {
    /// An organization's or the person's engine plugin.
    pub fn parse(text: &str) -> Result<Self, String> {
        let m = Self::parse_any(text)?;
        if m.id.starts_with(columbus_shared::tools::ID_PREFIX) {
            return Err(format!(
                "id {:?}: ids starting with \"personal.\" are the person's own tools, installed from the Columbus app",
                m.id
            ));
        }
        if m.environment.is_some() {
            return Err("environment: only personal tools run in a registered environment; an engine plugin runs in the engine's Python".into());
        }
        Ok(m)
    }

    /// The rules every manifest follows, personal tools' too.
    pub fn parse_any(text: &str) -> Result<Self, String> {
        let m: PluginManifest = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
        let ok = !m.id.is_empty()
            && m.id.len() <= 64
            && m.id.split('.').all(|seg| {
                seg.starts_with(|c: char| c.is_ascii_lowercase())
                    && seg
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            });
        if !ok {
            return Err(format!(
                "id {:?} must be dot-separated lower-case names (a-z, 0-9, '-')",
                m.id
            ));
        }
        if RESERVED.contains(&m.id.as_str()) {
            return Err(format!("id {:?} is reserved", m.id));
        }
        if m.entry.contains("..")
            || Path::new(&m.entry).is_absolute()
            || m.entry.contains(['/', '\\'])
        {
            return Err("entry must be a file name inside the plugin folder".into());
        }
        if m.tools.is_empty() {
            return Err("a plugin needs at least one tool".into());
        }
        for (name, t) in &m.tools {
            if !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            {
                return Err(format!("tool name {name:?} must be lower_snake_case"));
            }
            if let Some(clash) = t.params.keys().find(|p| t.outputs.contains_key(*p)) {
                return Err(format!(
                    "{name}: {clash:?} is both a parameter and an output"
                ));
            }
        }
        Ok(m)
    }

    /// A personal tool's manifest: the rules for every plugin, and those
    /// that keep a tool the assistant wrote small and contained: Python,
    /// `tool.py`, level A, one read-only tool named `run` whose inputs are
    /// workspace files and scalars (never a connection's dataset).
    pub fn parse_personal(text: &str, id: &str) -> Result<Self, String> {
        use columbus_shared::tools;
        let m = Self::parse_any(text)?;
        if m.id != id || !tools::id_ok(&m.id) {
            return Err(format!("{:?} is not the personal tool {id}", m.id));
        }
        if m.runtime != Runtime::Python || m.entry != tools::ENTRY {
            return Err("a personal tool is Python, with tool.py as its entry".into());
        }
        if let Some(l) = m.confinement.as_ref().and_then(|c| c.level.as_deref()) {
            if !l.eq_ignore_ascii_case("A") {
                return Err("a personal tool runs at confinement level A only".into());
            }
        }
        if let Some(e) = &m.environment {
            if !columbus_shared::environments::id_ok(e) {
                return Err(format!(
                    "environment {e:?} is not an environment id (a-z, 0-9 and -, at most 40)"
                ));
            }
        }
        if m.tools.len() != 1 || !m.tools.contains_key(tools::TOOL) {
            return Err("a personal tool has exactly one tool, named run".into());
        }
        let t = &m.tools[tools::TOOL];
        if t.risk != Risk::Read {
            return Err(
                "a personal tool only reads its inputs and writes its outputs (risk: read)".into(),
            );
        }
        if let Some(n) = t
            .params
            .iter()
            .find(|(_, p)| matches!(p, ParamSpec::Dataset { .. }))
            .map(|(n, _)| n)
        {
            return Err(format!(
                "{n}: a personal tool takes workspace layers and tables (type: file), not datasets"
            ));
        }
        // Text reaching an installed tool at run time is a fixed choice or
        // has a narrow shape, whichever rules it was installed under.
        if let Some(n) = t
            .params
            .iter()
            .find(|(_, p)| {
                matches!(
                    p,
                    ParamSpec::String {
                        choices: None,
                        format: None,
                        ..
                    }
                )
            })
            .map(|(n, _)| n)
        {
            return Err(format!(
                "{n}: a personal tool's text parameter needs enum or format; ask your assistant to propose the tool again"
            ));
        }
        if t.outputs.is_empty() {
            return Err("a personal tool has at least one output".into());
        }
        Ok(m)
    }

    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.id.clone())
    }
}

impl ParamSpec {
    /// A scalar that may be left out: optional, with no default.
    pub fn may_be_absent(&self) -> bool {
        match self {
            ParamSpec::Number {
                default, optional, ..
            } => *optional && default.is_none(),
            ParamSpec::Integer {
                default, optional, ..
            } => *optional && default.is_none(),
            ParamSpec::String {
                default, optional, ..
            } => *optional && default.is_none(),
            ParamSpec::Boolean {
                default, optional, ..
            } => *optional && default.is_none(),
            ParamSpec::Dataset { .. } | ParamSpec::File { .. } => false,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            ParamSpec::Dataset { description, .. }
            | ParamSpec::Number { description, .. }
            | ParamSpec::Integer { description, .. }
            | ParamSpec::String { description, .. }
            | ParamSpec::Boolean { description, .. }
            | ParamSpec::File { description, .. } => description.as_deref(),
        }
    }
}

/// Fields in words: `ROUTE_ID (text): the route's id; MEAS (number)`.
pub fn fields_text(fields: &[FieldSpec]) -> String {
    fields
        .iter()
        .map(|f| {
            let mut s = f.name.clone();
            if let Some(k) = &f.kind {
                s.push_str(&format!(" ({k})"));
            }
            if let Some(d) = &f.description {
                s.push_str(&format!(": {d}"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// What a workspace-file input expects, in words.
fn file_text(
    description: &Option<String>,
    kind: &Option<String>,
    geometry: &Option<String>,
    crs: &Option<String>,
    fields: &[FieldSpec],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = description {
        parts.push(d.trim_end_matches('.').to_string());
    }
    parts.push(format!(
        "A workspace {} handle, ws://<session>/<path> (from query_features, vector_op or another tool)",
        kind.as_deref().unwrap_or("file")
    ));
    if let Some(g) = geometry {
        parts.push(format!("geometry: {g}"));
    }
    if let Some(c) = crs {
        parts.push(format!("CRS: {c}"));
    }
    if !fields.is_empty() {
        parts.push(format!("required fields: {}", fields_text(fields)));
    }
    parts.join(". ") + "."
}

/// The schema's description for a scalar: the plugin's own words, then its unit.
fn scalar_text(description: Option<&str>, unit: Option<&str>) -> Option<String> {
    match (description, unit) {
        (Some(d), Some(u)) => Some(format!("{} (in {u}).", d.trim_end_matches('.'))),
        (Some(d), None) => Some(d.to_string()),
        (None, Some(u)) => Some(format!("In {u}")),
        (None, None) => None,
    }
}

/// JSON Schema for a tool's arguments, as the agent sees it.
pub fn input_schema(t: &PluginTool) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, p) in &t.params {
        let (mut schema, req) = match p {
            ParamSpec::Dataset { description, .. } => (
                json!({"description": match description {
                        Some(d) => format!("{} A dataset as \"<connection>:<dataset>\", or {{\"connection\", \"dataset\", \"where\"}}. Columbus reads it through the connection's permissions and row filter.", d.trim_end()),
                        None => "A dataset as \"<connection>:<dataset>\", or {\"connection\", \"dataset\", \"where\"}. Columbus reads it through the connection's permissions and row filter.".to_string(),
                       },
                       "oneOf": [{"type": "string"}, {"type": "object", "properties": {
                           "connection": {"type": "string"}, "dataset": {"type": "string"}, "where": {"type": "string"}},
                           "required": ["connection", "dataset"]}]}),
                true,
            ),
            ParamSpec::Number {
                unit,
                default,
                min,
                max,
                optional,
                description,
            } => {
                let mut s = json!({"type": "number"});
                if let Some(d) = scalar_text(description.as_deref(), unit.as_deref()) {
                    s["description"] = json!(d);
                }
                if let Some(d) = default {
                    s["default"] = json!(d);
                }
                if let Some(m) = min {
                    s["minimum"] = json!(m);
                }
                if let Some(m) = max {
                    s["maximum"] = json!(m);
                }
                (s, default.is_none() && !optional)
            }
            ParamSpec::Integer {
                default,
                min,
                max,
                optional,
                unit,
                description,
            } => {
                let mut s = json!({"type": "integer"});
                if let Some(d) = scalar_text(description.as_deref(), unit.as_deref()) {
                    s["description"] = json!(d);
                }
                if let Some(d) = default {
                    s["default"] = json!(d);
                }
                if let Some(m) = min {
                    s["minimum"] = json!(m);
                }
                if let Some(m) = max {
                    s["maximum"] = json!(m);
                }
                (s, default.is_none() && !optional)
            }
            ParamSpec::String {
                choices,
                format,
                default,
                optional,
                ..
            } => {
                let mut s = json!({"type": "string"});
                if let Some(c) = choices {
                    s["enum"] = json!(c);
                }
                if let Some(f) = format {
                    s["pattern"] = json!(f.pattern());
                    s["maxLength"] = json!(f.max_len());
                }
                if let Some(d) = default {
                    s["default"] = json!(d);
                }
                (s, default.is_none() && !optional)
            }
            ParamSpec::Boolean {
                default, optional, ..
            } => {
                let mut s = json!({"type": "boolean"});
                if let Some(d) = default {
                    s["default"] = json!(d);
                }
                (s, default.is_none() && !optional)
            }
            ParamSpec::File {
                description,
                kind,
                geometry,
                crs,
                fields,
            } => (
                json!({"type": "string", "description": file_text(description, kind, geometry, crs, fields)}),
                true,
            ),
        };
        if matches!(p, ParamSpec::String { .. } | ParamSpec::Boolean { .. }) {
            if let Some(d) = p.description() {
                schema["description"] = json!(d);
            }
        }
        props.insert(name.clone(), schema);
        if req {
            required.push(name.clone());
        }
    }
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// The full description `describe_tool` gives: what it does, when not to
/// use it, its outputs, its checks and examples.
pub fn describe_text(t: &PluginTool) -> String {
    let mut out = t.description.clone().unwrap_or_else(|| t.summary.clone());
    if let Some(n) = &t.not_for {
        out.push_str(&format!("\n\nNot for: {n}"));
    }
    if !t.outputs.is_empty() {
        out.push_str("\n\nOutputs, saved to the workspace:");
        for (n, o) in &t.outputs {
            let mut line = format!("\n- {n} ({})", o.kind());
            if let OutputSpec::Layer { geometry, crs, .. } = o {
                let bits: Vec<String> = [
                    geometry.as_ref().map(|g| format!("geometry {g}")),
                    crs.as_ref().map(|c| format!("CRS {c}")),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !bits.is_empty() {
                    line.push_str(&format!(", {}", bits.join(", ")));
                }
            }
            if let Some(d) = o.description() {
                line.push_str(&format!(": {d}"));
            }
            let fields = match o {
                OutputSpec::Layer { fields, .. } | OutputSpec::Table { fields, .. } => {
                    fields.as_slice()
                }
                OutputSpec::File { .. } => &[],
            };
            if !fields.is_empty() {
                line.push_str(&format!(" Fields: {}.", fields_text(fields)));
            }
            out.push_str(&line);
        }
    }
    if !t.checks.is_empty() {
        out.push_str("\n\nChecks it reports:");
        for c in &t.checks {
            match t.check_notes.get(c) {
                Some(n) => out.push_str(&format!("\n- {c}: {n}")),
                None => out.push_str(&format!("\n- {c}")),
            }
        }
    }
    for e in &t.examples {
        let d = e.get("description").and_then(Value::as_str).unwrap_or("");
        let a = e.get("arguments").cloned().unwrap_or(Value::Null);
        out.push_str(&format!("\n\nExample: {d}\n{a}"));
    }
    out
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PluginStatus {
    Ready,
    /// Installed but never approved.
    Unapproved {
        hash: String,
    },
    /// Its files changed since approval.
    Changed {
        hash: String,
    },
    Blocked {
        reason: String,
    },
    Error {
        reason: String,
    },
}

pub struct Plugin {
    pub manifest: PluginManifest,
    pub dir: PathBuf,
    pub hash: String,
    pub personal: bool,
    /// A personal tool: proposed by an assistant with `propose_tool` and
    /// installed by the person in the Columbus app (see personal_tools.rs).
    pub personal_tool: bool,
    /// A tool in the personal-tool format that the organization installed
    /// for everyone (its tools folder, approved by its administrator).
    pub org_tool: bool,
    /// The name of its own folder in the workspace (`plugins/<folder>`):
    /// its id, or for a proposed tool's test run an id of its own, so a test
    /// never shares a folder with the installed tool.
    folder: String,
    pub(crate) gate: Option<PluginStatus>,
    /// The launcher and level, or None for an unconfined pilot.
    confine: Option<(PathBuf, columbus_confine::Level)>,
    worker: OnceCell<Result<Arc<Worker>, String>>,
    /// A personal tool's describe check, made once with a worker that is
    /// stopped straight after (personal tools keep no process between runs).
    checked: OnceCell<Result<(), String>>,
}

impl Plugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Its own folder's name under `plugins/` in the workspace.
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// "A", "B", or "unconfined" (a pilot on a machine that cannot confine).
    pub fn confinement(&self) -> String {
        self.confine
            .as_ref()
            .map(|(_, l)| l.as_str().to_string())
            .unwrap_or_else(|| "unconfined".into())
    }
}

/// The registered environment a personal tool runs in (F-5), as checked
/// for this run: its interpreter, the folders it may read (its prefixes, as
/// confirmed), its confinement level and the confinement profile's name.
pub(crate) struct ToolEnv {
    pub python: PathBuf,
    pub read_only: Vec<PathBuf>,
    pub level: Option<columbus_confine::Level>,
    pub profile: String,
}

/// The confinement a plugin runs at (Rule 5): its declared level (A when
/// none), if policy allows it for plugins and this machine can provide it.
/// `Ok(None)` only for an unconfined pilot.
fn plugin_confinement(
    policy: &OrgPolicy,
    m: &PluginManifest,
) -> Result<Option<(PathBuf, columbus_confine::Level)>, String> {
    let declared = m
        .confinement
        .as_ref()
        .and_then(|c| c.level.clone())
        .unwrap_or_else(|| "A".into());
    let level = columbus_confine::Level::parse(&declared)
        .ok_or_else(|| format!("confinement level {declared:?} is not A, B or C"))?;
    if level == columbus_confine::Level::C {
        return Err("engine plugins cannot run at confinement level C".into());
    }
    if !policy
        .plugins
        .levels
        .iter()
        .any(|l| l.eq_ignore_ascii_case(level.as_str()))
    {
        return Err(format!(
            "confinement level {} is not allowed for plugins by organization policy",
            level.as_str()
        ));
    }
    match columbus_confine::ready(level) {
        Ok(launcher) => Ok(Some((launcher, level))),
        Err(_) if policy.servers.allow_unconfined_local => Ok(None),
        Err(e) => Err(format!(
            "plugins run confined (level {}), and {e}",
            level.as_str()
        )),
    }
}

/// An engine plugin's own folder in the session workspace: the only place
/// it may write; each run's inputs and outputs live under `runs/<run>`.
pub fn plugin_dir(workspace: &Path, id: &str) -> PathBuf {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    workspace.join("plugins").join(safe)
}

/// The folders a Python interpreter loads from (its own and, for a virtual
/// environment, the base installation), which a confined plugin must be
/// able to read. Asked once per interpreter.
pub(crate) fn python_dirs(python: &Path) -> Vec<PathBuf> {
    static CACHE: std::sync::Mutex<BTreeMap<PathBuf, Vec<PathBuf>>> =
        std::sync::Mutex::new(BTreeMap::new());
    if let Some(v) = CACHE.lock().expect("python dirs").get(python) {
        return v.clone();
    }
    let out = std::process::Command::new(python)
        .args([
            "-I",
            "-c",
            "import sys, json; print(json.dumps(sorted({sys.base_prefix, sys.prefix, sys.exec_prefix, sys.base_exec_prefix})))",
        ])
        .output();
    let dirs: Vec<PathBuf> = out
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice::<Vec<String>>(&o.stdout).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .collect();
    CACHE
        .lock()
        .expect("python dirs")
        .insert(python.to_path_buf(), dirs.clone());
    dirs
}

/// Hash over `plugin.yaml` and the entry file.
pub fn plugin_hash(dir: &Path, manifest_text: &str, entry: &str) -> Result<String, String> {
    let mut entry_path = dir.join(entry);
    if !entry_path.exists() && cfg!(windows) {
        entry_path.set_extension("exe");
    }
    let bytes = std::fs::read(&entry_path)
        .map_err(|e| format!("cannot read entry {}: {e}", entry_path.display()))?;
    let mut all = Vec::with_capacity(manifest_text.len() + bytes.len() + 1);
    all.extend_from_slice(manifest_text.as_bytes());
    all.push(0);
    all.extend_from_slice(&bytes);
    Ok(format!("sha256:{}", sha256_hex(&all)))
}

pub fn read_approvals(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(dir.join(APPROVALS_FILE))
        .ok()
        .and_then(|t| serde_yaml::from_str(&t).ok())
        .unwrap_or_default()
}

pub struct Plugins {
    plugins: Vec<Arc<Plugin>>,
    python: PathBuf,
    host_script: PathBuf,
    workspace: PathBuf,
    memory_mb: u64,
    pub notes: Vec<String>,
}

impl Plugins {
    pub fn load(
        policy: &OrgPolicy,
        org_dir: &Path,
        personal_dir: &Path,
        python: PathBuf,
        host_script: PathBuf,
        workspace: PathBuf,
        taken: &BTreeSet<String>,
    ) -> Self {
        let mut notes = Vec::new();
        let mut plugins: Vec<Arc<Plugin>> = Vec::new();
        let mut dirs = vec![(org_dir.to_path_buf(), false)];
        if policy.plugins.allow_personal {
            dirs.push((personal_dir.to_path_buf(), true));
        } else if personal_dir.exists()
            && std::fs::read_dir(personal_dir)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
        {
            notes.push("Personal plugins ignored: organization policy allows only organization-installed plugins.".into());
        }
        for (root, personal) in dirs {
            let approvals = read_approvals(&root);
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            let mut folders: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect();
            folders.sort();
            for dir in folders {
                let yaml = dir.join("plugin.yaml");
                let Ok(text) = std::fs::read_to_string(&yaml) else {
                    continue;
                };
                let manifest = match PluginManifest::parse(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        notes.push(format!("Plugin in {} not loaded: {e}", dir.display()));
                        continue;
                    }
                };
                if taken.contains(&manifest.id) || plugins.iter().any(|p| p.id() == manifest.id) {
                    notes.push(format!(
                        "Plugin {:?} not loaded: another provider uses that id.",
                        manifest.id
                    ));
                    continue;
                }
                let (confine, confine_gate) = match plugin_confinement(policy, &manifest) {
                    Ok(Some(c)) => (Some(c), None),
                    Ok(None) => {
                        notes.push(format!(
                            "Plugin {} runs unconfined: this machine cannot confine it and organization policy allows unconfined pilots.",
                            manifest.id
                        ));
                        (None, None)
                    }
                    Err(reason) => (None, Some(PluginStatus::Blocked { reason })),
                };
                let (hash, gate) = match plugin_hash(&dir, &text, &manifest.entry) {
                    Err(e) => (String::new(), Some(PluginStatus::Error { reason: e })),
                    Ok(h) => {
                        let gate = if !policy.plugins.allow.is_empty()
                            && !policy.plugins.allow.contains(&manifest.id)
                        {
                            Some(PluginStatus::Blocked {
                                reason: "not on the organization's plugin allow list".into(),
                            })
                        } else {
                            match approvals.get(&manifest.id) {
                                None => Some(PluginStatus::Unapproved { hash: h.clone() }),
                                Some(a) if *a != h => {
                                    Some(PluginStatus::Changed { hash: h.clone() })
                                }
                                Some(_) => None,
                            }
                        };
                        (h, gate)
                    }
                };
                let gate = confine_gate.or(gate);
                let folder = manifest.id.clone();
                plugins.push(Arc::new(Plugin {
                    manifest,
                    dir,
                    hash,
                    personal,
                    personal_tool: false,
                    org_tool: false,
                    folder,
                    gate,
                    confine,
                    worker: OnceCell::new(),
                    checked: OnceCell::new(),
                }));
            }
        }
        Plugins {
            plugins,
            python,
            host_script,
            workspace,
            memory_mb: policy.plugins.memory_mb,
            notes,
        }
    }

    /// The person's own tools (personal tools): each folder in `root` named
    /// for its id, with the plugin.yaml and tool.py the person installed
    /// from the Columbus app, on only while `approved.yaml` holds the hash
    /// of exactly those files. Always confined at level A, never unconfined.
    /// `reuse`: tools already loaded, kept (with their running workers)
    /// when nothing about them changed.
    #[allow(clippy::too_many_arguments)]
    pub fn load_personal_tools(
        policy: &OrgPolicy,
        root: &Path,
        python: PathBuf,
        host_script: PathBuf,
        workspace: PathBuf,
        taken: &BTreeSet<String>,
        reuse: &[Arc<Plugin>],
        org: bool,
    ) -> Self {
        let what = if org {
            "Organization tool"
        } else {
            "Personal tool"
        };
        use columbus_shared::tools;
        let mut notes = Vec::new();
        let mut plugins: Vec<Arc<Plugin>> = Vec::new();
        let host = |plugins, notes| Plugins {
            plugins,
            python: python.clone(),
            host_script: host_script.clone(),
            workspace: workspace.clone(),
            memory_mb: policy.plugins.memory_mb,
            notes,
        };
        if (org && !policy.allow_org_tools) || (!org && !policy.allow_personal_tools) {
            return host(plugins, notes);
        }
        if org {
            if !root.exists() {
                return host(plugins, notes);
            }
            if let Err(e) = tools::org_tools_trusted(root) {
                notes.push(format!("Organization tools not loaded: {e}"));
                return host(plugins, notes);
            }
        }
        let approvals = tools::read_approvals(root);
        let confine = match columbus_confine::ready(columbus_confine::Level::A) {
            Ok(l) if policy.plugins.levels.iter().any(|x| x.eq_ignore_ascii_case("A")) => {
                Ok((l, columbus_confine::Level::A))
            }
            Ok(_) => Err("organization policy allows no confinement level A for plugins, and personal tools run only at A".to_string()),
            Err(e) => Err(format!("personal tools run only confined (level A: no network, writes only their own folder), and {e}")),
        };
        for v in tools::list_installed(root) {
            let dir = root.join(&v.id);
            let read = std::fs::read(dir.join(tools::MANIFEST))
                .and_then(|m| std::fs::read(dir.join(tools::ENTRY)).map(|e| (m, e)));
            let Ok((mtext, entry)) = read else {
                notes.push(format!(
                    "{what} {} not loaded: its files cannot be read.",
                    v.id
                ));
                continue;
            };
            let manifest = match tools::check_files(&v.id, &mtext, &entry)
                .and_then(|_| String::from_utf8(mtext.clone()).map_err(|e| e.to_string()))
                .and_then(|t| PluginManifest::parse_personal(&t, &v.id))
            {
                Ok(m) => m,
                Err(e) => {
                    notes.push(format!("{what} {} not loaded: {e}", v.id));
                    continue;
                }
            };
            if taken.contains(&manifest.id) {
                notes.push(format!(
                    "{what} {:?} not loaded: another provider uses that id.",
                    manifest.id
                ));
                continue;
            }
            let hash = tools::content_hash(&mtext, &entry);
            let gate = match (&confine, approvals.get(&manifest.id)) {
                (Err(reason), _) => Some(PluginStatus::Blocked {
                    reason: reason.clone(),
                }),
                (_, None) => Some(PluginStatus::Unapproved { hash: hash.clone() }),
                (_, Some(a)) if *a != hash => Some(PluginStatus::Changed { hash: hash.clone() }),
                _ => None,
            };
            let same = reuse.iter().find(|p| {
                p.id() == manifest.id
                    && p.hash == hash
                    && p.dir == dir
                    && p.gate.is_none()
                    && gate.is_none()
            });
            if let Some(p) = same {
                plugins.push(p.clone());
                continue;
            }
            plugins.push(Arc::new(Plugin {
                folder: manifest.id.clone(),
                manifest,
                dir,
                hash,
                personal: !org,
                personal_tool: true,
                org_tool: org,
                gate,
                confine: confine.as_ref().ok().cloned(),
                worker: OnceCell::new(),
                checked: OnceCell::new(),
            }));
        }
        host(plugins, notes)
    }

    /// One proposed tool, for its test run only: its files are in `dir`
    /// (outside every folder a plugin may write), it is confined at level
    /// A, and the person approved this run.
    pub fn for_test(
        policy: &OrgPolicy,
        manifest: PluginManifest,
        dir: PathBuf,
        hash: String,
        python: PathBuf,
        host_script: PathBuf,
        workspace: PathBuf,
    ) -> Result<(Self, Arc<Plugin>), String> {
        if !policy
            .plugins
            .levels
            .iter()
            .any(|l| l.eq_ignore_ascii_case("A"))
        {
            return Err("organization policy allows no confinement level A for plugins, and tools run only at A".into());
        }
        let launcher = columbus_confine::ready(columbus_confine::Level::A).map_err(|e| {
            format!("tools run only confined (level A: no network, writes only their own folder), and {e}; Columbus never runs one unconfined")
        })?;
        let p = Arc::new(Plugin {
            // A folder of its own: the test shares nothing with the
            // installed tool of the same id, or with another test.
            folder: format!(
                "{}.test-{}",
                manifest.id,
                &uuid::Uuid::new_v4().simple().to_string()[..12]
            ),
            manifest,
            dir,
            hash,
            personal: true,
            personal_tool: true,
            org_tool: false,
            gate: None,
            confine: Some((launcher, columbus_confine::Level::A)),
            worker: OnceCell::new(),
            checked: OnceCell::new(),
        });
        Ok((
            Plugins {
                plugins: vec![p.clone()],
                python,
                host_script,
                workspace,
                memory_mb: policy.plugins.memory_mb,
                notes: vec![],
            },
            p,
        ))
    }

    pub fn all(&self) -> &[Arc<Plugin>] {
        &self.plugins
    }

    pub fn get(&self, id: &str) -> Option<&Arc<Plugin>> {
        self.plugins.iter().find(|p| p.id() == id)
    }

    pub fn status(&self, p: &Plugin) -> PluginStatus {
        if let Some(g) = &p.gate {
            return g.clone();
        }
        match (p.worker.get(), p.checked.get()) {
            (Some(Err(e)), _) | (_, Some(Err(e))) => PluginStatus::Error { reason: e.clone() },
            _ => PluginStatus::Ready,
        }
    }

    fn launch(&self, p: &Plugin) -> Launch {
        self.launch_from(p, &p.dir, &p.folder, None)
    }

    /// Start the plugin from the files in `dir` (its own folder, or a copy
    /// Columbus made of exactly the files it checked), with `folder` (under
    /// `plugins/` in the workspace) as the one place it may write. A
    /// personal tool in a registered environment (F-5) runs that
    /// environment's interpreter, reading its folders instead of the
    /// engine's, under a profile of its own.
    fn launch_from(&self, p: &Plugin, dir: &Path, folder: &str, env: Option<&ToolEnv>) -> Launch {
        let own = plugin_dir(&self.workspace, folder);
        let tmp = own.join("tmp");
        let _ = std::fs::create_dir_all(&tmp);
        let python = env.map_or_else(|| self.python.clone(), |e| e.python.clone());
        let python_read = match env {
            Some(e) => e.read_only.clone(),
            None => python_dirs(&self.python),
        };
        let (program, args, extra_read) = match p.manifest.runtime {
            Runtime::Python => (
                python,
                vec![
                    OsString::from("-I"),
                    OsString::from("-u"),
                    self.host_script.clone().into_os_string(),
                    dir.as_os_str().to_owned(),
                    OsString::from(&p.manifest.entry),
                ],
                // The interpreter and its libraries, and the SDK beside the
                // plugin host.
                python_read
                    .into_iter()
                    .chain(self.host_script.parent().map(PathBuf::from))
                    .collect::<Vec<_>>(),
            ),
            Runtime::Rust => {
                let mut exe = dir.join(&p.manifest.entry);
                if cfg!(windows) && exe.extension().is_none() {
                    exe.set_extension("exe");
                }
                (exe, vec![], vec![])
            }
        };
        let mut launch = Launch::plugin(program, args, own.clone(), tmp);
        // Every start (and every restart after a crash) checks the files are
        // still the ones approved, just before the process is started.
        launch.contents = Some((dir.to_path_buf(), p.manifest.entry.clone(), p.hash.clone()));
        if p.personal_tool {
            // As for scripts: no PROJ downloads, no GDAL formats that can
            // point at other files or URLs.
            if let Some(env) = launch.env.as_mut() {
                for (k, v) in [
                    ("PYTHONIOENCODING", "utf-8"),
                    ("PROJ_NETWORK", "OFF"),
                    ("GDAL_SKIP", "GDALG OGR_VRT VRT"),
                    ("OGR_SQLITE_ALLOW_EXTERNAL_ACCESS", "NO"),
                ] {
                    env.push((k.into(), v.into()));
                }
            }
        }
        if let Some((launcher, level)) = &p.confine {
            let spec = columbus_confine::Spec {
                level: env.and_then(|e| e.level).unwrap_or(*level),
                name: env.map_or_else(
                    || format!("plugin.{}", p.manifest.id),
                    |e| e.profile.clone(),
                ),
                // Its own folder only, never the workspace.
                read_write: vec![own.clone()],
                read_only: std::iter::once(dir.to_path_buf())
                    .chain(extra_read)
                    .collect(),
                memory_mb: Some(self.memory_mb),
                processes: Some(16),
            };
            let args: Vec<String> = launch
                .args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            launch.args = spec
                .launcher_args(&launch.program.to_string_lossy(), &args)
                .into_iter()
                .map(OsString::from)
                .collect();
            launch.program = launcher.clone();
        }
        launch
    }

    /// Start a plugin that is not yet approved and check its `describe`,
    /// for the person reviewing it. Never used to run tools.
    pub async fn worker_unchecked(&self, p: &Plugin) -> Result<(), String> {
        let w = Worker::new(self.launch(p));
        let d = w
            .describe(Duration::from_secs(30))
            .await
            .map_err(|e| e.to_string())?;
        check_describe(&p.manifest, &d)
    }

    /// Start (once) and check that `describe` matches `plugin.yaml`. Not
    /// for personal tools, which get a new worker for every run.
    pub async fn worker(&self, p: &Plugin) -> Result<Arc<Worker>, String> {
        if let Some(g) = &p.gate {
            return Err(describe_status(p.id(), g));
        }
        if p.personal_tool {
            return Err(format!(
                "{} runs in a worker of its own for each run",
                p.id()
            ));
        }
        p.worker
            .get_or_init(|| async {
                let w = Worker::new(self.launch(p));
                let d = w
                    .describe(Duration::from_secs(30))
                    .await
                    .map_err(|e| e.to_string())?;
                check_describe(&p.manifest, &d)?;
                Ok(Arc::new(w))
            })
            .await
            .clone()
    }
}

impl Plugins {
    /// Whether the plugin is ready: approved, and its worker's describe
    /// matches its manifest. A personal tool is checked once with a worker
    /// that is stopped at once, from a checked copy of its files.
    pub async fn check(&self, p: &Plugin) -> Result<(), String> {
        if let Some(g) = &p.gate {
            return Err(describe_status(p.id(), g));
        }
        if !p.personal_tool {
            return self.worker(p).await.map(|_| ());
        }
        p.checked
            .get_or_init(|| async {
                // In folders of its own, never the tool's: a check can come
                // while the tool runs, and must not touch that run's files.
                let tag = uuid::Uuid::new_v4().simple().to_string();
                let code = self.workspace.join("tool-code").join(&tag);
                let folder = format!("{}.check-{}", p.folder, &tag[..12]);
                let result = async {
                    self.stage_code(p, &code)?;
                    // A tool that runs in an environment may import what
                    // only that environment has: its files are checked
                    // here, and its describe at every run, in the
                    // environment (which is checked then too).
                    if p.manifest.environment.is_some() {
                        return Ok(());
                    }
                    self.fresh_worker_in(p, &code, &folder, None)
                        .await
                        .map(|_| ())
                }
                .await;
                let _ = std::fs::remove_dir_all(&code);
                // The worker was just stopped: on Windows its folder may be
                // held a moment longer.
                remove_dir_retrying(&plugin_dir(&self.workspace, &folder));
                result
            })
            .await
            .clone()
    }

    /// Copy a personal tool's two files into `into` (a new folder no plugin
    /// can write), from exactly the bytes whose hash is checked here against
    /// the one approved: what runs is what was installed.
    pub fn stage_code(&self, p: &Plugin, into: &Path) -> Result<(), String> {
        use columbus_shared::tools;
        let read = |f: &str| {
            let path = p.dir.join(f);
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_file() => {
                    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
                }
                _ => Err(format!("{} is not a plain file", path.display())),
            }
        };
        let manifest = read(tools::MANIFEST)?;
        let entry = read(&p.manifest.entry)?;
        if tools::content_hash(&manifest, &entry) != p.hash {
            return Err(format!(
                "{} changed since the person installed it, so it is off; propose it again with propose_tool and the person installs it again",
                p.id()
            ));
        }
        std::fs::create_dir_all(into).map_err(|e| format!("{}: {e}", into.display()))?;
        for (name, bytes) in [
            (tools::MANIFEST, &manifest),
            (p.manifest.entry.as_str(), &entry),
        ] {
            crate::export_tools::write_new(&into.join(name), bytes)
                .map_err(|e| format!("could not stage {name}: {e}"))?;
        }
        Ok(())
    }

    /// A worker of its own for one run of a personal tool, started from
    /// `code` (see [`stage_code`](Self::stage_code)) with an empty folder.
    /// It ends when the run's last reference to it goes.
    /// `env`: the registered environment it runs in (checked just now), or
    /// None for the engine's Python.
    pub async fn fresh_worker(
        &self,
        p: &Plugin,
        code: &Path,
        env: Option<&ToolEnv>,
    ) -> Result<Worker, String> {
        self.fresh_worker_in(p, code, &p.folder, env).await
    }

    async fn fresh_worker_in(
        &self,
        p: &Plugin,
        code: &Path,
        folder: &str,
        env: Option<&ToolEnv>,
    ) -> Result<Worker, String> {
        if let Some(g) = &p.gate {
            return Err(describe_status(p.id(), g));
        }
        // Not started on a folder that could not be emptied: an earlier run's
        // files would still be there.
        let own = plugin_dir(&self.workspace, folder);
        match std::fs::remove_dir_all(&own) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "{} was not started: its folder from an earlier run could not be emptied ({e})",
                    p.id()
                ))
            }
        }
        let w = Worker::new(self.launch_from(p, code, folder, env));
        let d = w
            .describe(Duration::from_secs(30))
            .await
            .map_err(|e| e.to_string())?;
        check_describe(&p.manifest, &d)?;
        Ok(w)
    }

    /// Empty a plugin's own folder: nothing one run left is there for the next.
    pub fn wipe_own(&self, p: &Plugin) {
        // The next run also refuses to start on a folder left over.
        remove_dir_retrying(&plugin_dir(&self.workspace, &p.folder));
    }
}

/// Remove a folder a stopped worker used. On Windows a folder can't be
/// removed while that worker's process is still letting go of it, or a
/// virus scanner holds a file in it: retry, waiting longer between tries,
/// for up to about ten seconds in all.
fn remove_dir_retrying(dir: &Path) {
    for i in 0..60u64 {
        match std::fs::remove_dir_all(dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                std::thread::sleep(Duration::from_millis(50 + (i * 5).min(250)))
            }
            _ => return,
        }
    }
}

/// The worker's own account of itself must match its manifest exactly.
pub fn check_describe(m: &PluginManifest, d: &Value) -> Result<(), String> {
    if d.get("protocol").and_then(Value::as_i64) != Some(2) {
        return Err("worker does not speak protocol 2".into());
    }
    if d.pointer("/plugin/id").and_then(Value::as_str) != Some(m.id.as_str()) {
        return Err("worker reports a different plugin id than plugin.yaml".into());
    }
    let names = |v: Option<&Value>| -> BTreeSet<String> {
        v.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let tools = d
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let listed: BTreeSet<String> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
        .collect();
    let declared: BTreeSet<String> = m.tools.keys().cloned().collect();
    if listed != declared {
        return Err(format!(
            "worker tools {listed:?} do not match plugin.yaml {declared:?}"
        ));
    }
    for t in &tools {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("");
        let spec = &m.tools[name];
        if names(t.get("params")) != spec.params.keys().cloned().collect() {
            return Err(format!(
                "{name}: worker parameters do not match plugin.yaml"
            ));
        }
        if names(t.get("outputs")) != spec.outputs.keys().cloned().collect() {
            return Err(format!("{name}: worker outputs do not match plugin.yaml"));
        }
    }
    Ok(())
}

pub fn describe_status(id: &str, s: &PluginStatus) -> String {
    if id.starts_with(columbus_shared::tools::ID_PREFIX) {
        match s {
            PluginStatus::Unapproved { .. } => return format!("{id} is not installed; the person installs it from Workflows › Tools in the Columbus app"),
            PluginStatus::Changed { .. } => return format!("{id} changed since the person installed it, so it is off; propose it again with propose_tool and the person installs it again"),
            _ => {}
        }
    }
    match s {
        PluginStatus::Ready => format!("{id} is ready"),
        PluginStatus::Unapproved { .. } => format!("{id} has not been approved; a person approves it with `columbus-mcp plugins approve {id}`"),
        PluginStatus::Changed { .. } => format!("{id} changed since it was approved; it stays off until a person approves it again"),
        PluginStatus::Blocked { reason } => format!("{id} is blocked: {reason}"),
        PluginStatus::Error { reason } => format!("{id} is unavailable: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LRS: &str = r#"
id: contoso.lrs-check
name: LRS event checker
version: 1.2.0
runtime: python
entry: worker.py
engine_env: columbus-py
tools:
  check_events:
    summary: Check route events for gaps, overlaps and out-of-range measures.
    params:
      events:   { type: dataset, access: read }
      routes:   { type: dataset, access: read }
      tolerance: { type: number, unit: mile, default: 0.001 }
    outputs:
      findings: { type: layer, geometry: point }
      report:   { type: table }
    risk: read
    checks: [geometry.valid, lrs.measures]
"#;

    #[test]
    fn the_change_documents_plugin_parses() {
        let m = PluginManifest::parse(LRS).unwrap();
        let t = &m.tools["check_events"];
        assert!(matches!(t.params["events"], ParamSpec::Dataset { .. }));
        assert_eq!(t.outputs["findings"].extension(), "geojson");
        let schema = input_schema(t);
        assert_eq!(schema["required"], json!(["events", "routes"]));
        assert_eq!(schema["properties"]["tolerance"]["default"], json!(0.001));
    }

    #[test]
    fn bad_manifests_are_refused() {
        for (from, to) in [
            ("id: contoso.lrs-check", "id: Contoso"),
            ("id: contoso.lrs-check", "id: columbus.core"),
            ("entry: worker.py", "entry: ../worker.py"),
            ("check_events:", "Check-Events:"),
        ] {
            assert!(
                PluginManifest::parse(&LRS.replace(from, to)).is_err(),
                "{to}"
            );
        }
    }

    #[test]
    fn describe_must_match() {
        let m = PluginManifest::parse(LRS).unwrap();
        let good = json!({"protocol": 2, "plugin": {"id": "contoso.lrs-check"}, "tools": [
            {"name": "check_events", "params": ["routes", "events", "tolerance"], "outputs": ["report", "findings"]}]});
        assert!(check_describe(&m, &good).is_ok());
        let mut extra = good.clone();
        extra["tools"][0]["params"] = json!(["events", "routes", "tolerance", "upload_to"]);
        assert!(check_describe(&m, &extra).is_err());
        let mut other = good.clone();
        other["plugin"]["id"] = json!("evil");
        assert!(check_describe(&m, &other).is_err());
    }

    #[test]
    fn approval_and_change_detection() {
        let d = tempfile::tempdir().unwrap();
        let pd = d.path().join("lrs");
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("plugin.yaml"), LRS).unwrap();
        std::fs::write(pd.join("worker.py"), "print('v1')").unwrap();
        let load = || {
            Plugins::load(
                &OrgPolicy::default(),
                d.path(),
                &d.path().join("none"),
                "python3".into(),
                "host.py".into(),
                d.path().into(),
                &BTreeSet::new(),
            )
        };
        let ps = load();
        let p = ps.get("contoso.lrs-check").unwrap();
        let hash = match ps.status(p) {
            PluginStatus::Unapproved { hash } => hash,
            s => panic!("{s:?}"),
        };
        std::fs::write(
            d.path().join(APPROVALS_FILE),
            format!("contoso.lrs-check: {hash:?}\n"),
        )
        .unwrap();
        assert!(matches!(
            load().status(load().get("contoso.lrs-check").unwrap()),
            PluginStatus::Ready
        ));
        std::fs::write(pd.join("worker.py"), "print('v2')").unwrap();
        assert!(matches!(
            load().status(load().get("contoso.lrs-check").unwrap()),
            PluginStatus::Changed { .. }
        ));
    }
}

#[cfg(test)]
mod text_format_tests {
    use super::TextFormat;

    #[test]
    fn text_formats_accept_only_their_shape() {
        for ok in ["RTE_ID", "_x", "route2"] {
            assert!(TextFormat::FieldName.accepts(ok), "{ok}");
        }
        for bad in [
            "",
            "2x",
            "a b",
            "a.b",
            "os.system",
            "x'",
            "a>1",
            &"a".repeat(65),
            "é",
        ] {
            assert!(!TextFormat::FieldName.accepts(bad), "{bad}");
        }
        for ok in [
            "IH0035",
            "US 290 W",
            "District 14: north/south #2 & 3 - final",
            "",
        ] {
            assert!(TextFormat::Label.accepts(ok), "{ok}");
        }
        for bad in [
            "{0.x}",
            "a@b",
            "a > 1",
            "x'y",
            "a\nb",
            "100%",
            "$x",
            "a\\b",
            "(x)",
            &"a".repeat(81),
        ] {
            assert!(!TextFormat::Label.accepts(bad), "{bad}");
        }
    }
}
