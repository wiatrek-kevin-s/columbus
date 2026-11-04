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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ParamSpec {
    Dataset {
        #[serde(default)]
        access: Option<String>,
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
    },
    String {
        #[serde(default, rename = "enum")]
        choices: Option<Vec<String>>,
        #[serde(default)]
        default: Option<String>,
        /// No default, and may be left out.
        #[serde(default)]
        optional: bool,
    },
    Boolean {
        #[serde(default)]
        default: Option<bool>,
        /// No default, and may be left out.
        #[serde(default)]
        optional: bool,
    },
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum OutputSpec {
    Layer {
        #[serde(default)]
        geometry: Option<String>,
    },
    Table,
    File {
        #[serde(default)]
        format: Option<String>,
    },
}

impl OutputSpec {
    pub fn extension(&self) -> String {
        match self {
            OutputSpec::Layer { .. } => "geojson".into(),
            OutputSpec::Table => "csv".into(),
            OutputSpec::File { format } => format
                .as_deref()
                .filter(|f| f.chars().all(|c| c.is_ascii_alphanumeric()) && !f.is_empty())
                .unwrap_or("bin")
                .to_string(),
        }
    }
    pub fn kind(&self) -> &'static str {
        match self {
            OutputSpec::Layer { .. } => "layer",
            OutputSpec::Table => "table",
            OutputSpec::File { .. } => "file",
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
    pub tools: BTreeMap<String, PluginTool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginConfinement {
    #[serde(default)]
    pub level: Option<String>,
}

impl PluginManifest {
    pub fn parse(text: &str) -> Result<Self, String> {
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
            ParamSpec::Boolean { default, optional } => *optional && default.is_none(),
            ParamSpec::Dataset { .. } | ParamSpec::File => false,
        }
    }
}

/// JSON Schema for a tool's arguments, as the agent sees it.
pub fn input_schema(t: &PluginTool) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, p) in &t.params {
        let (schema, req) = match p {
            ParamSpec::Dataset { .. } => (
                json!({"description": "A dataset as \"<connection>:<dataset>\", or {\"connection\", \"dataset\", \"where\"}. Columbus reads it through the connection's permissions and row filter.",
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
            } => {
                let mut s = json!({"type": "number"});
                if let Some(u) = unit {
                    s["description"] = json!(format!("In {u}"));
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
            } => {
                let mut s = json!({"type": "integer"});
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
                default,
                optional,
            } => {
                let mut s = json!({"type": "string"});
                if let Some(c) = choices {
                    s["enum"] = json!(c);
                }
                if let Some(d) = default {
                    s["default"] = json!(d);
                }
                (s, default.is_none() && !optional)
            }
            ParamSpec::Boolean { default, optional } => {
                let mut s = json!({"type": "boolean"});
                if let Some(d) = default {
                    s["default"] = json!(d);
                }
                (s, default.is_none() && !optional)
            }
            ParamSpec::File => (
                json!({"type": "string", "description": "A workspace file handle, ws://<session>/<path>"}),
                true,
            ),
        };
        props.insert(name.clone(), schema);
        if req {
            required.push(name.clone());
        }
    }
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
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
    gate: Option<PluginStatus>,
    /// The launcher and level, or None for an unconfined pilot.
    confine: Option<(PathBuf, columbus_confine::Level)>,
    worker: OnceCell<Result<Arc<Worker>, String>>,
}

impl Plugin {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    /// "A", "B", or "unconfined" (a pilot on a machine that cannot confine).
    pub fn confinement(&self) -> String {
        self.confine
            .as_ref()
            .map(|(_, l)| l.as_str().to_string())
            .unwrap_or_else(|| "unconfined".into())
    }
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
                plugins.push(Arc::new(Plugin {
                    manifest,
                    dir,
                    hash,
                    personal,
                    gate,
                    confine,
                    worker: OnceCell::new(),
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
        match p.worker.get() {
            Some(Err(e)) => PluginStatus::Error { reason: e.clone() },
            _ => PluginStatus::Ready,
        }
    }

    fn launch(&self, p: &Plugin) -> Launch {
        let own = plugin_dir(&self.workspace, &p.manifest.id);
        let tmp = own.join("tmp");
        let _ = std::fs::create_dir_all(&tmp);
        let (program, args, extra_read) = match p.manifest.runtime {
            Runtime::Python => (
                self.python.clone(),
                vec![
                    OsString::from("-I"),
                    OsString::from("-u"),
                    self.host_script.clone().into_os_string(),
                    p.dir.clone().into_os_string(),
                    OsString::from(&p.manifest.entry),
                ],
                // The interpreter and its libraries, and the SDK beside the
                // plugin host.
                python_dirs(&self.python)
                    .into_iter()
                    .chain(self.host_script.parent().map(PathBuf::from))
                    .collect::<Vec<_>>(),
            ),
            Runtime::Rust => {
                let mut exe = p.dir.join(&p.manifest.entry);
                if cfg!(windows) && exe.extension().is_none() {
                    exe.set_extension("exe");
                }
                (exe, vec![], vec![])
            }
        };
        let mut launch = Launch::plugin(program, args, own.clone(), tmp);
        if let Some((launcher, level)) = &p.confine {
            let spec = columbus_confine::Spec {
                level: *level,
                name: format!("plugin.{}", p.manifest.id),
                // Its own folder only, never the workspace.
                read_write: vec![own.clone()],
                read_only: std::iter::once(p.dir.clone()).chain(extra_read).collect(),
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

    /// Start (once) and check that `describe` matches `plugin.yaml`.
    pub async fn worker(&self, p: &Plugin) -> Result<Arc<Worker>, String> {
        if let Some(g) = &p.gate {
            return Err(describe_status(p.id(), g));
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
    #[cfg_attr(
        target_os = "macos",
        ignore = "no sandbox on macOS yet, so every plugin is blocked before approval is looked at"
    )]
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
