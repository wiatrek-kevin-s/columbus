//! Workflows: saved jobs of steps the person approved once in the Columbus
//! app, run on a schedule or on demand by `columbus-mcp workflow run`,
//! without a person waiting. Shared by the server (which files proposals
//! and runs workflows) and the app (which lists, installs, schedules and
//! removes them), so the two agree on the files and the fingerprint.
//!
//! Layout (`workflows/` beside the tools folder, or `COLUMBUS_WORKFLOWS`):
//!
//! ```text
//! workflows/
//!   approved.yaml                 id -> fingerprint of what the person approved
//!   morning-check/                an installed workflow
//!     workflow.yaml   install.json
//!   proposals/
//!     morning-check/              waiting for the person
//!       workflow.yaml   proposal.json
//!   runs/
//!     morning-check/2026-11-06T06-00-00-000-1a2b/
//!       run.json   report.md
//!   locks/
//!     morning-check.lock          one run at a time
//! ```
//!
//! The fingerprint covers `workflow.yaml` and the installed hash of every
//! personal tool it uses: a workflow runs only while all of those are
//! exactly what the person approved. The assistant has no tool that writes
//! `approved.yaml`; only the app does, after the person confirms.
//!
//! What a workflow can do is bounded by what it names: each step is one of
//! a fixed list of reading and processing tools (or an installed personal
//! tool), every connection it reads is listed in its scope, and a run never
//! waits for a person: any step that would need an approval stops with
//! "needs you".

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::canon::sha256_hex;
use crate::tools::{self, is_dir, plain_chars, read_plain, replace_dir, write_atomic};

pub const FILE: &str = "workflow.yaml";
pub const PROPOSAL: &str = "proposal.json";
pub const INSTALL: &str = "install.json";
pub const APPROVED: &str = "approved.yaml";
pub const PROPOSALS: &str = "proposals";
pub const RUNS: &str = "runs";
pub const LOCKS: &str = "locks";
pub const RUN_FILE: &str = "run.json";
pub const REPORT: &str = "report.md";
/// Largest `workflow.yaml`, in bytes.
pub const MAX_FILE: usize = 64 * 1024;
pub const MAX_STEPS: usize = 20;
/// Largest arguments of one step, as JSON.
pub const MAX_ARGS: usize = 16 * 1024;
pub const MAX_PROPOSALS: usize = 20;
pub const MAX_INSTALLED: usize = 50;
/// Longest run allowed, in minutes; and the default.
pub const MAX_RUN_MINUTES: u32 = 240;
pub const DEFAULT_RUN_MINUTES: u32 = 30;
/// Runs kept per workflow; and the default.
pub const MAX_KEEP_RUNS: u32 = 500;
pub const DEFAULT_KEEP_RUNS: u32 = 100;

/// The tools a step may call: they read and process, and write only to the
/// run's own workspace. Exports are made by `outputs.folder`, scripts and
/// edits are never steps, and installed personal tools are named
/// `personal.<name>.run`.
pub const STEP_TOOLS: &[&str] = &[
    "list_datasets",
    "describe_dataset",
    "sample_rows",
    "query_features",
    "sql_select",
    "vector_op",
    "raster_op",
    "verify",
    "read_output",
];

/// Export formats `outputs.folder` may ask for.
pub const FORMATS: &[&str] = &["gpkg", "geojson", "fgb", "shp", "csv"];

const DAYS: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The person's workflows folder: `COLUMBUS_WORKFLOWS`, else `workflows/`
/// beside the tools folder.
pub fn workflows_dir() -> PathBuf {
    if let Some(d) = crate::paths::env_path("COLUMBUS_WORKFLOWS") {
        return d;
    }
    let t = tools::tools_dir();
    t.parent()
        .map(Path::to_path_buf)
        .unwrap_or(t)
        .join("workflows")
}

/// A workflow's id: 3 to 40 lowercase letters, digits and single hyphens,
/// starting with a letter.
pub fn id_ok(id: &str) -> bool {
    // Not the names of the folder's own parts.
    ![RUNS, LOCKS, PROPOSALS].contains(&id)
        && (3..=40).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A step's id: a letter, then up to 30 lowercase letters, digits and `_`.
pub fn step_id_ok(id: &str) -> bool {
    (1..=31).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn connection_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub purpose: String,
    /// When it runs by itself; without one, only when the person starts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
    #[serde(default)]
    pub scope: Scope,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub outputs: Outputs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    /// `daily`, `weekdays`, or a list of `mon` … `sun`.
    pub days: Days,
    /// `HH:MM`, 24-hour, this computer's time.
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Days {
    Named(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    /// Every connection a step reads; nothing else is read.
    #[serde(default)]
    pub connections: Vec<String>,
    /// Longest the whole run may take (default 30, at most 240).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_minutes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub tool: String,
    /// The tool's arguments. A text value `${step}` is the layer an earlier
    /// step made; `${step.a.b}` is a field of its result.
    #[serde(default)]
    pub args: Value,
    /// Go on to the next step when this one has a failing check (by
    /// default the run stops there).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub continue_on_fail: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outputs {
    /// Export the layers of these steps to one of the policy's export folders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderOut>,
    /// Runs kept (default 100, at most 500).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_runs: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderOut {
    /// The export folder's id, as whoami lists them.
    pub export: String,
    /// The steps whose layers are exported.
    pub steps: Vec<String>,
    /// gpkg (default), geojson, fgb, shp or csv.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

impl Workflow {
    pub fn run_minutes(&self) -> u32 {
        self.scope.run_minutes.unwrap_or(DEFAULT_RUN_MINUTES)
    }

    pub fn keep_runs(&self) -> u32 {
        self.outputs.keep_runs.unwrap_or(DEFAULT_KEEP_RUNS)
    }

    /// The days it runs, as `mon` … `sun`.
    pub fn days(&self) -> Vec<&'static str> {
        match self.schedule.as_ref().map(|s| &s.days) {
            None => vec![],
            Some(Days::Named(n)) if n == "daily" => DAYS.to_vec(),
            Some(Days::Named(_)) => DAYS[..5].to_vec(),
            Some(Days::List(l)) => DAYS
                .iter()
                .copied()
                .filter(|d| l.iter().any(|x| x == d))
                .collect(),
        }
    }

    /// The schedule's time as (hour, minute).
    pub fn at(&self) -> Option<(u32, u32)> {
        let at = &self.schedule.as_ref()?.at;
        let (h, m) = at.split_once(':')?;
        Some((h.parse().ok()?, m.parse().ok()?))
    }

    /// The personal tools it uses, by id (`personal.<name>`).
    pub fn personal_tools(&self) -> BTreeSet<String> {
        self.steps
            .iter()
            .filter_map(|s| personal_tool_id(&s.tool))
            .collect()
    }
}

/// `personal.<name>.run` -> `personal.<name>`.
pub fn personal_tool_id(tool: &str) -> Option<String> {
    let id = tool.strip_suffix(&format!(".{}", tools::TOOL))?;
    tools::id_ok(id).then(|| id.to_string())
}

/// A reference in a step's arguments: `${step}` or `${step.a.b}`.
pub fn reference(v: &str) -> Option<(&str, Vec<&str>)> {
    let inner = v.strip_prefix("${")?.strip_suffix('}')?;
    let mut parts = inner.split('.');
    let step = parts.next()?;
    Some((step, parts.collect()))
}

/// Read and check a workflow's text: everything the server and the app can
/// know without running it.
pub fn check(text: &str) -> Result<Workflow, String> {
    if text.len() > MAX_FILE {
        return Err(format!(
            "a workflow is at most {} KB of text",
            MAX_FILE / 1024
        ));
    }
    if text.starts_with('\u{FEFF}') || text.contains('\r') {
        return Err("save the workflow as plain UTF-8 text with \\n line ends".into());
    }
    plain_chars(text).map_err(|e| format!("workflow.yaml: {e}"))?;
    let wf: Workflow =
        serde_yaml::from_str(text).map_err(|e| format!("workflow.yaml does not read: {e}"))?;
    // What the person is shown is what was read: no character hidden in
    // an escape (\u202e, a line break in a name) either.
    let parsed = serde_json::to_value(&wf).map_err(|e| e.to_string())?;
    plain_values(&parsed).map_err(|e| format!("workflow.yaml: {e}"))?;
    if !id_ok(&wf.id) {
        return Err(format!(
            "{:?} is not a workflow id: 3 to 40 lowercase letters, digits and single hyphens, starting with a letter",
            wf.id
        ));
    }
    let one_line = |what: &str, t: &str, max: usize| -> Result<(), String> {
        if t.chars().count() > max || t.contains(['"', '\n']) || t.trim().is_empty() {
            return Err(format!(
                "the workflow's {what} must be one plain line of at most {max} characters, without double quotes"
            ));
        }
        Ok(())
    };
    one_line("name", &wf.name, 80)?;
    if wf.purpose.chars().count() > 600 {
        return Err("the purpose is at most 600 characters".into());
    }
    if let Some(s) = &wf.schedule {
        match &s.days {
            Days::Named(n) if n == "daily" || n == "weekdays" => {}
            Days::Named(n) => {
                return Err(format!(
                    "schedule days {n:?}: use daily, weekdays, or a list of mon, tue, wed, thu, fri, sat, sun"
                ))
            }
            Days::List(l) => {
                let set: BTreeSet<&String> = l.iter().collect();
                if l.is_empty()
                    || set.len() != l.len()
                    || !l.iter().all(|d| DAYS.contains(&d.as_str()))
                {
                    return Err(
                        "schedule days: a list of different days from mon, tue, wed, thu, fri, sat, sun"
                            .into(),
                    );
                }
            }
        }
        let ok = s.at.len() == 5
            && s.at.as_bytes()[2] == b':'
            && wf.at().is_some_and(|(h, m)| h < 24 && m < 60)
            && s.at
                .chars()
                .enumerate()
                .all(|(i, c)| i == 2 || c.is_ascii_digit());
        if !ok {
            return Err(format!(
                "schedule at {:?}: a time as HH:MM, 24-hour (06:00)",
                s.at
            ));
        }
    }
    if let Some(m) = wf.scope.run_minutes {
        if !(1..=MAX_RUN_MINUTES).contains(&m) {
            return Err(format!("scope run_minutes is 1 to {MAX_RUN_MINUTES}"));
        }
    }
    for c in &wf.scope.connections {
        if !connection_id_ok(c) {
            return Err(format!("{c:?} is not a connection id"));
        }
    }
    if wf.steps.is_empty() || wf.steps.len() > MAX_STEPS {
        return Err(format!("a workflow has 1 to {MAX_STEPS} steps"));
    }
    let mut seen: Vec<&str> = Vec::new();
    for s in &wf.steps {
        let at = format!("step {:?}", s.id);
        if !step_id_ok(&s.id) {
            return Err(format!(
                "{at}: a step id is a lowercase letter, then letters, digits and _ (at most 31)"
            ));
        }
        if seen.contains(&s.id.as_str()) {
            return Err(format!("{at}: two steps have this id"));
        }
        if !STEP_TOOLS.contains(&s.tool.as_str()) && personal_tool_id(&s.tool).is_none() {
            return Err(format!(
                "{at}: {:?} cannot be a step. A step is one of {} or an installed personal tool (personal.<name>.run); scripts, edits and exports are never steps (results go to a folder through outputs.folder)",
                s.tool,
                STEP_TOOLS.join(", ")
            ));
        }
        match &s.args {
            Value::Null => {}
            Value::Object(_) => {}
            _ => return Err(format!("{at}: args is a mapping of the tool's arguments")),
        }
        let size = s.args.to_string().len();
        if size > MAX_ARGS {
            return Err(format!(
                "{at}: its arguments are larger than {} KB",
                MAX_ARGS / 1024
            ));
        }
        check_args(&s.args, &seen, &at, 0, None)?;
        if let Some(c) = s.args.get("connection") {
            let c = c
                .as_str()
                .ok_or_else(|| format!("{at}: connection is a connection id"))?;
            if !wf.scope.connections.iter().any(|x| x == c) {
                return Err(format!(
                    "{at}: it reads connection {c:?}, which is not listed in scope.connections"
                ));
            }
        }
        seen.push(&s.id);
    }
    if let Some(f) = &wf.outputs.folder {
        if !connection_id_ok(&f.export) {
            return Err(format!("{:?} is not an export folder id", f.export));
        }
        if f.steps.is_empty() {
            return Err("outputs.folder lists the steps whose layers are exported".into());
        }
        for s in &f.steps {
            if !seen.contains(&s.as_str()) {
                return Err(format!(
                    "outputs.folder names step {s:?}, which is not a step"
                ));
            }
        }
        if let Some(fmt) = &f.format {
            if !FORMATS.contains(&fmt.as_str()) {
                return Err(format!(
                    "outputs.folder format {fmt:?}: one of {}",
                    FORMATS.join(", ")
                ));
            }
        }
    }
    if let Some(k) = wf.outputs.keep_runs {
        if !(1..=MAX_KEEP_RUNS).contains(&k) {
            return Err(format!("outputs keep_runs is 1 to {MAX_KEEP_RUNS}"));
        }
    }
    Ok(wf)
}

/// Arguments: no approval ids (a run never spends one), references only to
/// earlier steps, not too deep.
/// Arguments a reference may never fill: what is read, how, and where
/// results go are fixed when the person approves the workflow, not chosen
/// by data a run reads.
pub const FIXED_ARGS: &[&str] = &[
    "sql",
    "where",
    "dataset",
    "connection",
    "folder",
    "name",
    "op",
];

fn check_args(
    v: &Value,
    earlier: &[&str],
    at: &str,
    depth: usize,
    key: Option<&str>,
) -> Result<(), String> {
    if depth > 8 {
        return Err(format!("{at}: its arguments are nested too deeply"));
    }
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "approval_id" {
                    return Err(format!(
                        "{at}: a workflow never carries an approval; a run does not wait for a person"
                    ));
                }
                check_args(x, earlier, at, depth + 1, Some(k))?;
            }
        }
        Value::Array(a) => {
            for x in a {
                check_args(x, earlier, at, depth + 1, key)?;
            }
        }
        Value::String(s) if s.contains("${") => {
            if let Some(k) = key.filter(|k| FIXED_ARGS.contains(k)) {
                return Err(format!(
                    "{at}: {k} is fixed when the workflow is approved; it cannot be a reference ({s:?})"
                ));
            }
            let Some((step, path)) = reference(s) else {
                return Err(format!(
                    "{at}: {s:?}: a reference is a whole value, ${{step}} or ${{step.field}}"
                ));
            };
            if !earlier.contains(&step) {
                return Err(format!(
                    "{at}: {s:?} refers to {step:?}, which is not an earlier step"
                ));
            }
            if path.len() > 6
                || path.iter().any(|p| {
                    p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                })
            {
                return Err(format!("{at}: {s:?} is not a reference to a result field"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Every text value and key of the parsed workflow is plain (see
/// [`plain_chars`]), and the one-line ones have no line break.
fn plain_values(v: &Value) -> Result<(), String> {
    match v {
        Value::String(s) => plain_chars(s),
        Value::Array(a) => a.iter().try_for_each(plain_values),
        Value::Object(m) => m.iter().try_for_each(|(k, x)| {
            if k.contains('\n') {
                return Err(format!("the key {k:?} has a line break"));
            }
            plain_chars(k)?;
            plain_values(x)
        }),
        _ => Ok(()),
    }
}

/// The fingerprint a person approves: `workflow.yaml`'s bytes, a zero byte,
/// then `id=hash` of every personal tool it uses as installed.
pub fn fingerprint(file: &[u8], tool_hashes: &BTreeMap<String, String>) -> String {
    let mut all = file.to_vec();
    all.push(0);
    for (id, h) in tool_hashes {
        all.extend_from_slice(format!("{id}={h}\n").as_bytes());
    }
    format!("sha256:{}", sha256_hex(&all))
}

/// The installed hash of each personal tool `wf` uses, or why one is not
/// ready.
pub fn tool_hashes(wf: &Workflow, tools_root: &Path) -> Result<BTreeMap<String, String>, String> {
    let installed = tools::list_installed(tools_root);
    let mut out = BTreeMap::new();
    for id in wf.personal_tools() {
        match installed.iter().find(|t| t.id == id) {
            Some(t) if t.state.as_deref() == Some("ready") => {
                out.insert(id, t.hash.clone());
            }
            Some(_) => {
                return Err(format!(
                    "it uses {id}, which is not ready (changed or off since it was installed)"
                ))
            }
            None => return Err(format!("it uses {id}, which is not installed")),
        }
    }
    Ok(out)
}

/// In plain words, for the person approving it: when it runs, each step,
/// where results go.
pub fn describe(wf: &Workflow) -> Vec<String> {
    let mut out = vec![];
    match (&wf.schedule, wf.at()) {
        (Some(s), Some((h, m))) => {
            let days = match &s.days {
                Days::Named(n) if n == "daily" => "every day".to_string(),
                Days::Named(_) => "every weekday (Monday to Friday)".to_string(),
                Days::List(l) => format!("on {}", l.join(", ")),
            };
            out.push(format!(
                "Runs by itself {days} at {h:02}:{m:02} while you are signed in to this computer (once on waking if it was asleep), and whenever you start it."
            ));
        }
        _ => out.push("Runs only when you start it.".into()),
    }
    if wf.scope.connections.is_empty() {
        out.push("Reads no connection.".into());
    } else {
        out.push(format!(
            "Reads only: {} (under their rules: row filters, hidden fields and limits apply).",
            wf.scope.connections.join(", ")
        ));
    }
    for (i, s) in wf.steps.iter().enumerate() {
        let mut args = if s.args.is_null() {
            String::new()
        } else {
            s.args.to_string()
        };
        if args.chars().count() > 240 {
            args = format!("{}…", args.chars().take(240).collect::<String>());
        }
        out.push(format!(
            "Step {} ({}): {}{}{}",
            i + 1,
            s.id,
            s.tool,
            if args.is_empty() { "" } else { " " },
            args
        ));
    }
    match &wf.outputs.folder {
        Some(f) => out.push(format!(
            "Exports the layers of {} to the export folder {} as {}, with provenance beside each file. If that needs your approval, the run says so and exports nothing.",
            f.steps.join(", "),
            f.export,
            f.format.as_deref().unwrap_or("gpkg")
        )),
        None => out.push("Exports nothing; results stay in the run's report and workspace.".into()),
    }
    out.push(format!(
        "Stops after {} minutes; never waits for a person: a step that needs an approval stops the run with \"needs you\".",
        wf.run_minutes()
    ));
    out
}

/// What the app shows of a workflow.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowView {
    pub id: String,
    pub name: String,
    pub purpose: String,
    pub fingerprint: String,
    /// The plain-words account (see [`describe`]).
    pub plain: Vec<String>,
    /// The schedule, for the app's scheduler: days `mon` … `sun` and the time.
    pub days: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub tools: Vec<String>,
    pub connections: Vec<String>,
    /// Installed: `ready`, `changed` (its file differs from what was
    /// approved), `tools_changed` (a personal tool it uses changed),
    /// `invalid`, `not_installed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    /// Proposals: what the server recorded, the full text, and the
    /// installed workflow it would replace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// Installed: the latest run's record, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<Value>,
}

fn view(id: &str, dir: &Path, tools_root: &Path) -> WorkflowView {
    let mut v = WorkflowView {
        id: id.to_string(),
        name: id.to_string(),
        purpose: String::new(),
        fingerprint: String::new(),
        plain: vec![],
        days: vec![],
        at: None,
        tools: vec![],
        connections: vec![],
        state: None,
        problem: None,
        installed: None,
        proposal: None,
        text: None,
        replaces: None,
        last_run: None,
    };
    let bytes = match read_plain(&dir.join(FILE), MAX_FILE) {
        Ok(b) => b,
        Err(e) => {
            v.problem = Some(e);
            return v;
        }
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    v.text = Some(text.clone());
    let wf = match check(&text) {
        Ok(w) if w.id == id => w,
        Ok(w) => {
            v.problem = Some(format!("workflow.yaml names another id ({})", w.id));
            return v;
        }
        Err(e) => {
            v.problem = Some(e);
            return v;
        }
    };
    v.name = wf.name.clone();
    v.purpose = wf.purpose.clone();
    v.plain = describe(&wf);
    v.days = wf.days().into_iter().map(String::from).collect();
    v.at = wf.at().map(|(h, m)| format!("{h:02}:{m:02}"));
    v.tools = wf.steps.iter().map(|s| s.tool.clone()).collect();
    v.connections = wf.scope.connections.clone();
    match tool_hashes(&wf, tools_root) {
        Ok(h) => v.fingerprint = fingerprint(&bytes, &h),
        Err(e) => v.problem = Some(e),
    }
    v
}

/// `approved.yaml`: id -> fingerprint. A file that does not read approves nothing.
pub fn read_approvals(root: &Path) -> BTreeMap<String, String> {
    read_plain(&root.join(APPROVED), 1024 * 1024)
        .ok()
        .and_then(|b| serde_yaml::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_approvals(root: &Path, map: &BTreeMap<String, String>) -> Result<(), String> {
    let text = format!(
        "# Workflows the person approved in the Columbus app, by the fingerprint of\n# their workflow.yaml and the personal tools they use. Any other is off.\n{}",
        serde_yaml::to_string(map).map_err(|e| e.to_string())?
    );
    write_atomic(&root.join(APPROVED), text.as_bytes())
}

fn folder_ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| is_dir(&e.path()))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| id_ok(n))
        .collect();
    ids.sort();
    ids
}

/// The installed workflows, with whether each may run.
pub fn list_installed(root: &Path, tools_root: &Path) -> Vec<WorkflowView> {
    let approvals = read_approvals(root);
    folder_ids(root)
        .into_iter()
        // `runs`, `locks` and `proposals` are not ids of installed ones.
        .filter(|id| ![RUNS, LOCKS, PROPOSALS].contains(&id.as_str()))
        .take(MAX_INSTALLED)
        .map(|id| {
            let dir = root.join(&id);
            let mut v = view(&id, &dir, tools_root);
            v.state = Some(
                match approvals.get(&id) {
                    None => "not_installed",
                    Some(_)
                        if v.problem
                            .as_deref()
                            .is_some_and(|p| p.starts_with("it uses")) =>
                    {
                        "tools_changed"
                    }
                    Some(_) if v.problem.is_some() => "invalid",
                    Some(f) if *f == v.fingerprint => "ready",
                    Some(_) => {
                        // Its own file is what was approved, so a tool changed.
                        let file_same = read_install(&dir)
                            .and_then(|j| j["file_hash"].as_str().map(String::from))
                            .is_some_and(|h| {
                                read_plain(&dir.join(FILE), MAX_FILE)
                                    .is_ok_and(|b| format!("sha256:{}", sha256_hex(&b)) == h)
                            });
                        if file_same {
                            "tools_changed"
                        } else {
                            "changed"
                        }
                    }
                }
                .into(),
            );
            v.installed =
                read_install(&dir).and_then(|j| j["installed"].as_str().map(String::from));
            v.last_run = list_runs(root, &id, 1).into_iter().next();
            v
        })
        .collect()
}

fn read_install(dir: &Path) -> Option<Value> {
    read_plain(&dir.join(INSTALL), 64 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// The proposals waiting for the person, oldest first, with their text.
pub fn list_proposals(root: &Path, tools_root: &Path) -> Vec<WorkflowView> {
    let approvals = read_approvals(root);
    let mut out: Vec<WorkflowView> = folder_ids(&root.join(PROPOSALS))
        .into_iter()
        .map(|id| proposal_view(root, tools_root, &id, &approvals))
        .collect();
    out.sort_by(|a, b| {
        let at = |v: &WorkflowView| {
            v.proposal
                .as_ref()
                .and_then(|p| p["proposed"].as_str().map(String::from))
                .unwrap_or_default()
        };
        at(a).cmp(&at(b))
    });
    out
}

fn proposal_view(
    root: &Path,
    tools_root: &Path,
    id: &str,
    approvals: &BTreeMap<String, String>,
) -> WorkflowView {
    let dir = root.join(PROPOSALS).join(id);
    let mut v = view(id, &dir, tools_root);
    v.proposal = read_plain(&dir.join(PROPOSAL), 1024 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if is_dir(&root.join(id)) {
        v.replaces = Some(approvals.get(id).cloned().unwrap_or_default());
    }
    v
}

/// One waiting proposal, read now.
pub fn read_proposal(root: &Path, tools_root: &Path, id: &str) -> Result<WorkflowView, String> {
    if !id_ok(id) || !is_dir(&root.join(PROPOSALS).join(id)) {
        return Err(format!("there is no workflow {id:?} waiting"));
    }
    let v = proposal_view(root, tools_root, id, &read_approvals(root));
    match &v.problem {
        Some(p) => Err(format!("{id} cannot be installed: {p}")),
        None => Ok(v),
    }
}

/// File a proposal (the server): it replaces a waiting proposal of the
/// same id. Refused when too many others are waiting.
pub fn write_proposal(root: &Path, text: &str, info: &Value) -> Result<PathBuf, String> {
    let wf = check(text)?;
    let id = wf.id.as_str();
    let proposals = root.join(PROPOSALS);
    let others = folder_ids(&proposals)
        .into_iter()
        .filter(|p| p != id)
        .count();
    if others >= MAX_PROPOSALS {
        return Err(format!(
            "{others} workflows are already waiting for the person to review in the Columbus app; ask them to install or decline those first"
        ));
    }
    std::fs::create_dir_all(&proposals).map_err(|e| format!("{}: {e}", proposals.display()))?;
    let staging = proposals.join(format!(".{id}.{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let written = (|| {
        write_atomic(&staging.join(FILE), text.as_bytes())?;
        let j = serde_json::to_vec_pretty(info).map_err(|e| e.to_string())?;
        write_atomic(&staging.join(PROPOSAL), &j)?;
        replace_dir(&staging, &proposals.join(id))
    })();
    if written.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    written.map(|_| proposals.join(id))
}

/// Install a waiting proposal: exactly what has fingerprint `want` (what
/// the person was shown), checked again here. Called only by the app,
/// after the person confirmed.
pub fn install(
    root: &Path,
    tools_root: &Path,
    id: &str,
    want: &str,
    now: &str,
) -> Result<WorkflowView, String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a workflow id"));
    }
    let from = root.join(PROPOSALS).join(id);
    if !is_dir(&from) {
        return Err(format!("there is no workflow {id:?} waiting"));
    }
    let bytes = read_plain(&from.join(FILE), MAX_FILE)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "workflow.yaml is not UTF-8 text")?;
    let wf = check(text)?;
    if wf.id != id {
        return Err(format!("workflow.yaml names another id than {id}"));
    }
    let hashes = tool_hashes(&wf, tools_root)?;
    let fp = fingerprint(&bytes, &hashes);
    if fp != want {
        return Err(
            "the workflow (or a tool it uses) changed after you looked at it; nothing was installed. Look again".into(),
        );
    }
    let proposal: Value = read_plain(&from.join(PROPOSAL), 1024 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let staging = root.join(format!(".install-{id}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let placed = (|| {
        write_atomic(&staging.join(FILE), &bytes)?;
        let info = json!({
            "installed": now,
            "fingerprint": fp,
            "file_hash": format!("sha256:{}", sha256_hex(&bytes)),
            "tools": hashes,
            "proposed": proposal["proposed"],
            "session": proposal["session"],
            "client": proposal["client"],
        });
        write_atomic(
            &staging.join(INSTALL),
            &serde_json::to_vec_pretty(&info).map_err(|e| e.to_string())?,
        )?;
        replace_dir(&staging, &root.join(id))
    })();
    if let Err(e) = placed {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let mut approvals = read_approvals(root);
    approvals.insert(id.to_string(), fp);
    write_approvals(root, &approvals)?;
    let _ = std::fs::remove_dir_all(&from);
    list_installed(root, tools_root)
        .into_iter()
        .find(|w| w.id == id)
        .ok_or_else(|| format!("{id} was installed but does not list"))
}

/// Decline a waiting proposal: it is deleted.
pub fn decline(root: &Path, id: &str) -> Result<(), String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a workflow id"));
    }
    let dir = root.join(PROPOSALS).join(id);
    if !is_dir(&dir) {
        return Err(format!("there is no workflow {id:?} waiting"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Remove an installed workflow: off at once (its approval goes first),
/// then its folder is deleted. Its runs are kept.
pub fn remove(root: &Path, id: &str) -> Result<(), String> {
    if !id_ok(id) || [RUNS, LOCKS, PROPOSALS].contains(&id) {
        return Err(format!("{id:?} is not a workflow id"));
    }
    let mut approvals = read_approvals(root);
    let had = approvals.remove(id).is_some();
    let dir = root.join(id);
    if !had && !is_dir(&dir) {
        return Err(format!("there is no workflow {id:?} installed"));
    }
    if had {
        write_approvals(root, &approvals)?;
    }
    if is_dir(&dir) {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

/// The workflow that may run now: installed, approved with this exact
/// fingerprint, its tools as approved. Otherwise, why not ("needs
/// re-approval: …").
pub fn ready(root: &Path, tools_root: &Path, id: &str) -> Result<(Workflow, String), String> {
    if !id_ok(id) || [RUNS, LOCKS, PROPOSALS].contains(&id) {
        return Err(format!("{id:?} is not a workflow id"));
    }
    let dir = root.join(id);
    if !is_dir(&dir) {
        return Err(format!("there is no workflow {id:?} installed"));
    }
    let Some(approved) = read_approvals(root).get(id).cloned() else {
        return Err(format!("{id} is not approved"));
    };
    let bytes = read_plain(&dir.join(FILE), MAX_FILE)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "workflow.yaml is not UTF-8 text")?;
    let wf = check(text).map_err(|e| format!("needs re-approval: {e}"))?;
    if wf.id != id {
        return Err("needs re-approval: workflow.yaml names another id".into());
    }
    let hashes = tool_hashes(&wf, tools_root).map_err(|e| format!("needs re-approval: {e}"))?;
    if fingerprint(&bytes, &hashes) != approved {
        return Err(
            "needs re-approval: the workflow or a personal tool it uses changed since you approved it"
                .into(),
        );
    }
    Ok((wf, approved))
}

/// `runs/<id>/`.
pub fn runs_dir(root: &Path, id: &str) -> PathBuf {
    root.join(RUNS).join(id)
}

/// A new run's id: the local time, then a few random characters.
pub fn new_run_id() -> String {
    format!(
        "{}-{}",
        chrono::Local::now().format("%Y-%m-%dT%H-%M-%S-%3f"),
        &uuid::Uuid::new_v4().simple().to_string()[..4]
    )
}

pub fn run_id_ok(run: &str) -> bool {
    run.len() == 28
        && run
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == 'T')
}

/// The latest runs' records, newest first.
pub fn list_runs(root: &Path, id: &str, limit: usize) -> Vec<Value> {
    let dir = runs_dir(root, id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut runs: Vec<String> = entries
        .flatten()
        .filter(|e| is_dir(&e.path()))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| run_id_ok(n))
        .collect();
    runs.sort();
    runs.reverse();
    runs.into_iter()
        .take(limit)
        .map(|r| {
            let rec = read_plain(&dir.join(&r).join(RUN_FILE), 1024 * 1024)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let mut v = rec.unwrap_or_else(|| json!({"status": "running"}));
            // The summary only: steps are read with the run.
            if let Some(m) = v.as_object_mut() {
                m.remove("steps");
                m.insert("run".into(), json!(r));
            }
            v
        })
        .collect()
}

/// One run's record and report.
pub fn read_run(root: &Path, id: &str, run: &str) -> Result<(Value, String), String> {
    if !id_ok(id) || !run_id_ok(run) {
        return Err("no such run".into());
    }
    let dir = runs_dir(root, id).join(run);
    let rec = read_plain(&dir.join(RUN_FILE), 1024 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or_else(|| json!({"status": "running"}));
    let report = read_plain(&dir.join(REPORT), 2 * 1024 * 1024)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    Ok((rec, report))
}

/// Delete all but the newest `keep` runs, with each one's session
/// workspace (its copies of the data) when that is a folder directly in
/// `workspaces`.
pub fn prune_runs(root: &Path, id: &str, keep: usize, workspaces: Option<&Path>) {
    let dir = runs_dir(root, id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut runs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_dir(p))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(run_id_ok)
        })
        .collect();
    runs.sort();
    let n = runs.len().saturating_sub(keep);
    let ws_root = workspaces.and_then(|w| w.canonicalize().ok());
    for p in runs.into_iter().take(n) {
        if let Some(ws_root) = &ws_root {
            let ws = read_plain(&p.join(RUN_FILE), 1024 * 1024)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| v["workspace"].as_str().map(PathBuf::from))
                .and_then(|w| {
                    std::fs::symlink_metadata(&w)
                        .ok()
                        .filter(|m| m.is_dir())
                        .map(|_| w)
                })
                .and_then(|w| w.canonicalize().ok());
            if let Some(ws) = ws.filter(|w| w.parent() == Some(ws_root.as_path())) {
                let _ = std::fs::remove_dir_all(ws);
            }
        }
        let _ = std::fs::remove_dir_all(p);
    }
}

/// One run at a time per workflow: held while the run lasts, removed when
/// dropped (only if it is still this run's). A lock is stale when the
/// process that took it is gone, or it is older than that workflow's time
/// limit and ten minutes: a killed run does not block the next one.
pub struct Lock {
    path: PathBuf,
    mine: String,
}

impl Drop for Lock {
    fn drop(&mut self) {
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t == self.mine) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
fn process_gone(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only asks whether the process exists.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn process_gone(_pid: u32) -> bool {
    false
}

pub fn lock(root: &Path, id: &str, run_minutes: u32) -> Result<Lock, String> {
    use std::io::Write;
    let dir = root.join(LOCKS);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let p = dir.join(format!("{id}.lock"));
    let mine = format!("{} {}", std::process::id(), uuid::Uuid::new_v4().simple());
    let make = || -> std::io::Result<Lock> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)?;
        f.write_all(mine.as_bytes())?;
        Ok(Lock {
            path: p.clone(),
            mine: mine.clone(),
        })
    };
    match make() {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let md = std::fs::symlink_metadata(&p)
                .ok()
                .filter(|m| m.file_type().is_file());
            let old = md
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > u64::from(run_minutes) * 60 + 600);
            let gone = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| {
                    t.split_whitespace()
                        .next()
                        .and_then(|n| n.parse::<u32>().ok())
                })
                .is_some_and(process_gone);
            if old || gone {
                let _ = std::fs::remove_file(&p);
                make().map_err(|e| e.to_string())
            } else {
                Err(format!("{id} is already running"))
            }
        }
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WF: &str = "id: morning-check\nname: Morning check\npurpose: Count the tracts every weekday.\nschedule: { days: weekdays, at: \"06:00\" }\nscope: { connections: [tiger], run_minutes: 10 }\nsteps:\n  - id: tracts\n    tool: query_features\n    args: { connection: tiger, dataset: tracts }\n  - id: check\n    tool: verify\n    args: { layer: \"${tracts}\" }\noutputs:\n  folder: { export: outbox, steps: [tracts] }\n";

    #[test]
    fn a_workflow_reads_and_says_what_it_does() {
        let wf = check(WF).unwrap();
        assert_eq!(wf.days(), vec!["mon", "tue", "wed", "thu", "fri"]);
        assert_eq!(wf.at(), Some((6, 0)));
        assert_eq!(wf.run_minutes(), 10);
        let plain = describe(&wf).join("\n");
        assert!(plain.contains("every weekday"), "{plain}");
        assert!(plain.contains("Reads only: tiger"), "{plain}");
        assert!(plain.contains("export folder outbox"), "{plain}");
    }

    #[test]
    fn what_a_workflow_may_not_do() {
        let bad = |from: &str, to: &str| {
            let t = WF.replace(from, to);
            assert_ne!(t, WF, "{from}");
            check(&t).expect_err(&t)
        };
        assert!(bad("tool: verify", "tool: run_python").contains("cannot be a step"));
        assert!(bad("tool: verify", "tool: export_output").contains("cannot be a step"));
        assert!(bad("tool: verify", "tool: apply_edits").contains("cannot be a step"));
        assert!(bad("connections: [tiger]", "connections: [other]").contains("scope.connections"));
        assert!(bad("${tracts}", "${later}").contains("not an earlier step"));
        assert!(bad("${tracts}", "x ${tracts}").contains("whole value"));
        assert!(bad("${tracts}", "${tracts.a b}").contains("not a reference"));
        assert!(bad("{ layer:", "{ approval_id: x, layer:").contains("never carries an approval"));
        assert!(bad("at: \"06:00\"", "at: \"6:00\"").contains("HH:MM"));
        assert!(bad("at: \"06:00\"", "at: \"24:00\"").contains("HH:MM"));
        assert!(bad("days: weekdays", "days: sometimes").contains("schedule days"));
        assert!(bad("days: weekdays", "days: [mon, mon]").contains("schedule days"));
        assert!(bad("run_minutes: 10", "run_minutes: 999").contains("run_minutes"));
        assert!(bad("steps: [tracts]", "steps: [nope]").contains("not a step"));
        assert!(bad("id: morning-check", "id: Morning").contains("not a workflow id"));
        assert!(bad("name: Morning check", "name: \"say \\\"hi\\\"\"").contains("one plain line"));
        assert!(bad("purpose:", "surprise: 1\npurpose:").contains("does not read"));
        assert!(bad("Count", "Count\u{202E}").contains("displays differently"));
        assert!(bad("id: check", "id: tracts").contains("two steps"));
        assert!(bad(
            "{ layer: \"${tracts}\" }",
            "{ layer: \"${tracts}\", where: \"${tracts.a}\" }"
        )
        .contains("fixed when"));
        assert!(bad("name: Morning check", "name: \"Morning\\u202echeck\"")
            .contains("displays differently"));
        assert!(bad("name: Morning check", "name: \"Morning\\ncheck\"").contains("one plain line"));
        assert!(bad("id: morning-check", "id: runs").contains("not a workflow id"));
        // A personal tool is a step by its full tool name.
        let t = WF.replace("tool: verify", "tool: personal.count-features.run");
        let wf = check(&t).unwrap();
        assert_eq!(
            wf.personal_tools().into_iter().collect::<Vec<_>>(),
            vec!["personal.count-features".to_string()]
        );
    }

    #[test]
    fn proposed_installed_run_only_as_approved() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("workflows");
        let tools = d.path().join("tools");
        let info = json!({"proposed": "2026-11-06T06:00:00-06:00"});
        write_proposal(&root, WF, &info).unwrap();
        let p = read_proposal(&root, &tools, "morning-check").unwrap();
        assert!(p.problem.is_none(), "{:?}", p.problem);
        assert!(
            ready(&root, &tools, "morning-check").is_err(),
            "not before it is installed"
        );
        assert!(install(&root, &tools, "morning-check", "sha256:other", "now").is_err());
        let v = install(&root, &tools, "morning-check", &p.fingerprint, "now").unwrap();
        assert_eq!(v.state.as_deref(), Some("ready"));
        let (wf, fp) = ready(&root, &tools, "morning-check").unwrap();
        assert_eq!(
            (wf.id.as_str(), fp.as_str()),
            ("morning-check", p.fingerprint.as_str())
        );
        // A changed file is off until approved again.
        std::fs::write(
            root.join("morning-check").join(FILE),
            WF.replace("06:00", "07:00"),
        )
        .unwrap();
        assert!(ready(&root, &tools, "morning-check")
            .unwrap_err()
            .contains("needs re-approval"));
        assert_eq!(
            list_installed(&root, &tools)[0].state.as_deref(),
            Some("changed")
        );
        // One run at a time.
        std::fs::write(root.join("morning-check").join(FILE), WF).unwrap();
        let held = lock(&root, "morning-check", 10).unwrap();
        assert!(lock(&root, "morning-check", 10)
            .err()
            .unwrap()
            .contains("already running"));
        drop(held);
        lock(&root, "morning-check", 10).unwrap();
        // A lock whose process is gone is stale.
        std::fs::write(root.join(LOCKS).join("morning-check.lock"), "999999999 x").unwrap();
        #[cfg(unix)]
        lock(&root, "morning-check", 10).unwrap();
        remove(&root, "morning-check").unwrap();
        assert!(ready(&root, &tools, "morning-check").is_err());
        assert!(remove(&root, "runs").is_err());
        assert!(!id_ok("runs") && !id_ok("proposals") && !id_ok("locks"));
    }

    #[test]
    fn a_workflow_using_a_personal_tool_is_bound_to_that_tools_hash() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("workflows");
        let tools_root = d.path().join("tools");
        let yaml = "id: personal.count-features\nname: Count\nversion: 1.0.0\nruntime: python\nentry: tool.py\ntools:\n  run:\n    summary: Count.\n    description: Count features.\n    risk: read\n";
        let code = "def run(ctx):\n    return {}\n";
        tools::write_proposal(
            &tools_root,
            "personal.count-features",
            yaml.as_bytes(),
            code.as_bytes(),
            &json!({}),
        )
        .unwrap();
        let h = tools::content_hash(yaml.as_bytes(), code.as_bytes());
        tools::install(&tools_root, "personal.count-features", &h, "now").unwrap();
        let t = WF.replace("tool: verify", "tool: personal.count-features.run");
        write_proposal(&root, &t, &json!({})).unwrap();
        let p = read_proposal(&root, &tools_root, "morning-check").unwrap();
        install(&root, &tools_root, "morning-check", &p.fingerprint, "now").unwrap();
        ready(&root, &tools_root, "morning-check").unwrap();
        // The tool installed again with other code: the workflow is off.
        let code2 = "def run(ctx):\n    return {'n': 1}\n";
        tools::write_proposal(
            &tools_root,
            "personal.count-features",
            yaml.as_bytes(),
            code2.as_bytes(),
            &json!({}),
        )
        .unwrap();
        tools::install(
            &tools_root,
            "personal.count-features",
            &tools::content_hash(yaml.as_bytes(), code2.as_bytes()),
            "now",
        )
        .unwrap();
        assert!(ready(&root, &tools_root, "morning-check")
            .unwrap_err()
            .contains("needs re-approval"));
        assert_eq!(
            list_installed(&root, &tools_root)[0].state.as_deref(),
            Some("tools_changed")
        );
    }
}
