//! The person's registered Python environments (F-4, build brief Part 5).
//!
//! An environment is a Python interpreter that scripts can run in instead
//! of the engine's Python. Shared by the server (which runs scripts in
//! them and lists them in `whoami`) and the terminal commands and app
//! (which register, confirm and remove them at the keyboard), so all of
//! them agree on the files, the probe and the hash.
//!
//! ```text
//! environments/                 beside the connections folder
//!   pro.yaml                    one registration (F-4)
//!   .confirmed.json             id -> what the person confirmed (internal)
//! <Columbus data>/envs/<id>/    environments Columbus creates
//! ```
//!
//! Confirmation records a SHA-256 over the registration's `python`,
//! `trust`, `confinement` and `created_by` with its `probe`, kept apart
//! from the YAML. Any later change to those fields, or to the packages
//! the interpreter sees, marks the environment `changed`, and it does not
//! run until the person confirms it again: editing the YAML can never
//! make an environment trusted. No MCP tool writes here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::canon::{canonical_json, sha256_hex};
use crate::policy::EnvironmentsPolicy;

/// The internal record of confirmations, in the environments folder. Its
/// name can't be an environment's id.
pub const CONFIRMED: &str = ".confirmed.json";
/// The probe's time limit (R-13).
pub const PROBE_SECONDS: u64 = 120;
/// What every refusal of a changed environment says (R-13b).
pub const CONFIRM_AGAIN: &str = "confirm it again in Settings";
/// Most environments listed.
pub const MAX_ENVIRONMENTS: usize = 100;

/// `[a-z0-9-]{1,40}` (F-4).
pub fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The registrations folder: `COLUMBUS_ENVIRONMENTS`, else `environments`
/// beside the connections folder.
pub fn environments_dir() -> PathBuf {
    if let Some(d) = crate::paths::env_path("COLUMBUS_ENVIRONMENTS") {
        return d;
    }
    let conns = crate::paths::connections_dir();
    conns
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(conns)
        .join("environments")
}

/// Where Columbus creates environments: `COLUMBUS_ENVS`, else `envs`
/// beside the session workspaces (as the tools folder), else
/// `<Columbus data>/envs`.
pub fn envs_dir() -> PathBuf {
    if let Some(d) = crate::paths::env_path("COLUMBUS_ENVS") {
        return d;
    }
    if let Some(ws) = crate::paths::env_path("COLUMBUS_WORKSPACES") {
        return ws
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(ws)
            .join("envs");
    }
    crate::paths::platform_dirs().2.join("envs")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trust {
    Governed,
    Trusted,
}

impl Trust {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "governed" => Some(Trust::Governed),
            "trusted" => Some(Trust::Trusted),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Trust::Governed => "governed",
            Trust::Trusted => "trusted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Confinement {
    #[default]
    A,
    B,
}

impl Confinement {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "A" | "a" => Some(Confinement::A),
            "B" | "b" => Some(Confinement::B),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Confinement::A => "A",
            Confinement::B => "B",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CreatedBy {
    Person,
    Columbus,
}

impl CreatedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            CreatedBy::Person => "person",
            CreatedBy::Columbus => "columbus",
        }
    }
}

/// What the probe found, as F-4 stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub python_version: String,
    pub packages_sha256: String,
    pub arcpy: bool,
    pub licence: Option<String>,
    pub probed_at: String,
}

/// One `environments/<id>.yaml` (F-4). Unknown keys are refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub id: String,
    pub python: String,
    pub trust: Trust,
    #[serde(default)]
    pub confinement: Confinement,
    pub created_by: CreatedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub probe: Probe,
    pub confirmed_at: String,
}

impl Environment {
    /// The SHA-256 that confirmation records: the fields that decide how
    /// and where scripts run, with the probe.
    pub fn confirmation_hash(&self) -> String {
        let v = json!({
            "python": self.python,
            "trust": self.trust.as_str(),
            "confinement": self.confinement.as_str(),
            "created_by": self.created_by.as_str(),
            "probe": serde_json::to_value(&self.probe).unwrap_or(Value::Null),
        });
        format!("sha256:{}", sha256_hex(canonical_json(&v).as_bytes()))
    }

    /// The probe in plain words for `whoami` and the terminal: "arcpy: no",
    /// or "arcpy: yes, licence ArcInfo". (T-8 words the licence fully.)
    pub fn arcpy_line(&self) -> String {
        if !self.probe.arcpy {
            return "arcpy: no".into();
        }
        match &self.probe.licence {
            Some(l) => format!("arcpy: yes, licence {l}"),
            None => "arcpy: yes, licence unknown".into(),
        }
    }
}

/// What the person confirmed, per environment (internal; not F-4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmed {
    /// The confirmation hash; absent once a change was seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Why it was marked changed, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<String>,
    /// The cheap freshness signal (see [`fingerprint`]).
    #[serde(default)]
    pub fingerprint: String,
    /// The interpreter's prefixes: the folders a confined script may read.
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// The folders on the interpreter's `sys.path`, listed by the signal.
    #[serde(default)]
    pub site_dirs: Vec<String>,
}

/// An environment's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ready,
    /// Changed since it was confirmed (or never confirmed), and why.
    Changed(String),
    /// Its file doesn't load.
    Invalid(String),
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Ready => "ready",
            Status::Changed(_) => "changed",
            Status::Invalid(_) => "invalid",
        }
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Status::Ready => None,
            Status::Changed(r) | Status::Invalid(r) => Some(r),
        }
    }
}

/// One registered environment and its state.
#[derive(Debug, Clone)]
pub struct View {
    pub id: String,
    pub env: Option<Environment>,
    pub confirmed: Option<Confirmed>,
    pub status: Status,
}

fn file_for(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.yaml"))
}

/// Read and check one registration.
pub fn load(dir: &Path, id: &str) -> Result<Environment, String> {
    if !id_ok(id) {
        return Err(format!(
            "{id:?} is not an environment id (lowercase letters, digits and -, at most 40)"
        ));
    }
    let path = file_for(dir, id);
    let md = std::fs::symlink_metadata(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("there is no environment {id:?}")
        } else {
            format!("{}: {e}", path.display())
        }
    })?;
    if !md.file_type().is_file() || md.len() > 64 * 1024 {
        return Err(format!(
            "{} is not a plain file of at most 64 KB",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let env: Environment =
        serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if env.id != id {
        return Err(format!(
            "{} says id {:?}; it must match its file name",
            path.display(),
            env.id
        ));
    }
    if !Path::new(&env.python).is_absolute() {
        return Err(format!(
            "{}: python must be an absolute path to an interpreter",
            path.display()
        ));
    }
    if env.confinement == Confinement::B && env.trust != Trust::Governed {
        return Err(format!(
            "{}: confinement applies to governed environments only",
            path.display()
        ));
    }
    Ok(env)
}

pub fn read_confirmed(dir: &Path) -> BTreeMap<String, Confirmed> {
    std::fs::read(dir.join(CONFIRMED))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_confirmed(dir: &Path, all: &BTreeMap<String, Confirmed>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    crate::tools::write_atomic(&dir.join(CONFIRMED), &bytes)
}

/// The ids of the registrations in `dir`.
pub fn ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let id = name.strip_suffix(".yaml")?.to_string();
            id_ok(&id).then_some(id)
        })
        .collect();
    ids.sort();
    ids.truncate(MAX_ENVIRONMENTS);
    ids
}

/// One environment with its state: its file must load, its confirmed
/// hash must match, and the cheap freshness signal must be unchanged.
pub fn view(dir: &Path, id: &str) -> View {
    let confirmed = read_confirmed(dir).remove(id);
    let env = match load(dir, id) {
        Ok(e) => e,
        Err(why) => {
            return View {
                id: id.into(),
                env: None,
                confirmed,
                status: Status::Invalid(why),
            }
        }
    };
    let status = match &confirmed {
        None => Status::Changed("it has not been confirmed at the keyboard".into()),
        Some(c) if c.changed.is_some() => Status::Changed(c.changed.clone().unwrap_or_default()),
        Some(c) if c.sha256.as_deref() != Some(env.confirmation_hash().as_str()) => {
            Status::Changed(
                "its registration (python, trust, confinement, created_by or probe) was edited after it was confirmed"
                    .into(),
            )
        }
        Some(c) if fingerprint(Path::new(&env.python), &c.site_dirs, &c.prefixes) != c.fingerprint => {
            Status::Changed(
                "its interpreter or installed packages changed after it was confirmed".into(),
            )
        }
        Some(_) => Status::Ready,
    };
    View {
        id: id.into(),
        env: Some(env),
        confirmed,
        status,
    }
}

/// Every registered environment with its state.
pub fn list(dir: &Path) -> Vec<View> {
    ids(dir).iter().map(|id| view(dir, id)).collect()
}

/// Mark an environment changed (a check at use found its packages
/// differ): it stays refused until the person confirms it again.
pub fn mark_changed(dir: &Path, id: &str, why: &str) -> Result<(), String> {
    let mut all = read_confirmed(dir);
    if let Some(c) = all.get_mut(id) {
        c.sha256 = None;
        c.changed = Some(why.to_string());
        write_confirmed(dir, &all)?;
    }
    Ok(())
}

// ---------- the probe ----------

/// Run inside the probed interpreter only (never the gateway or engine):
/// its version, prefixes and `sys.path` folders, every distribution
/// `importlib.metadata` sees, the names in `conda-meta/`, and, given
/// `full`, whether `arcpy` imports and `arcpy.ProductInfo()`. The answer
/// is the last line starting `COLUMBUS-PROBE `, whatever else is printed.
pub const PROBE_PY: &str = r#"import sys, os, json
r = {"python_version": "%d.%d.%d" % tuple(sys.version_info[:3]),
     "prefixes": sorted({sys.prefix, sys.base_prefix, sys.exec_prefix, sys.base_exec_prefix}),
     "site_dirs": [p for p in sys.path if p and os.path.isdir(p)]}
try:
    import importlib.metadata as md
    pk = set()
    for d in md.distributions():
        try:
            n = d.metadata["Name"]
        except Exception:
            n = None
        if n:
            pk.add("%s==%s" % (n, d.version))
    r["packages"] = sorted(pk)
    cm = os.path.join(sys.prefix, "conda-meta")
    r["conda_meta"] = sorted(os.listdir(cm)) if os.path.isdir(cm) else None
    r["arcpy"] = False
    r["licence"] = None
    if sys.argv[1:] == ["full"]:
        try:
            import arcpy
            r["arcpy"] = True
            try:
                r["licence"] = str(arcpy.ProductInfo())
            except Exception:
                pass
        except Exception:
            pass
except ImportError:
    r = {"error": "Python 3.8 or later is needed (importlib.metadata)"}
sys.stdout.write("\nCOLUMBUS-PROBE " + json.dumps(r) + "\n")
sys.stdout.flush()
"#;

/// The probe's arguments after the interpreter: isolated (no user site,
/// no PYTHON* settings, no current folder), no bytecode written.
pub fn probe_args(full: bool) -> Vec<String> {
    let mut a: Vec<String> = ["-I", "-B", "-X", "utf8", "-c", PROBE_PY]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if full {
        a.push("full".into());
    }
    a
}

/// What a probe found.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Found {
    pub python_version: String,
    pub prefixes: Vec<String>,
    pub site_dirs: Vec<String>,
    pub packages: Vec<String>,
    pub conda_meta: Option<Vec<String>>,
    pub arcpy: bool,
    pub licence: Option<String>,
}

impl Found {
    /// SHA-256 of the sorted `name==version` lines, then, where
    /// `conda-meta/` exists, its sorted file names.
    pub fn packages_sha256(&self) -> String {
        let mut text = String::new();
        let mut lines = self.packages.clone();
        lines.sort();
        for l in &lines {
            text.push_str(l);
            text.push('\n');
        }
        if let Some(cm) = &self.conda_meta {
            let mut cm = cm.clone();
            cm.sort();
            text.push_str("conda-meta/\n");
            for n in &cm {
                text.push_str(n);
                text.push('\n');
            }
        }
        format!("sha256:{}", sha256_hex(text.as_bytes()))
    }
}

/// Read the probe's answer from what it printed.
pub fn parse_probe(stdout: &str) -> Result<Found, String> {
    let line = stdout
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("COLUMBUS-PROBE "))
        .ok_or("it did not answer the probe")?;
    let v: Value = serde_json::from_str(line).map_err(|e| format!("its answer: {e}"))?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return Err(e.to_string());
    }
    serde_json::from_value(v).map_err(|e| format!("its answer: {e}"))
}

/// The interpreter's file name must look like one (`python`, `python3`,
/// `python3.12`, `python.exe`), so registration never runs some other
/// program with the probe's arguments.
pub fn looks_like_python(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    stem.strip_prefix("python")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// Check an interpreter path before anything runs it.
pub fn check_interpreter(python: &Path) -> Result<(), String> {
    let shown = python.display();
    if !python.is_absolute() {
        return Err(format!("{shown} is not an absolute path"));
    }
    if !looks_like_python(python) {
        return Err(format!(
            "{shown} is not a Python interpreter (its file name must be python, python3, python3.N or python.exe)"
        ));
    }
    if !std::fs::metadata(python).is_ok_and(|m| m.is_file()) {
        return Err(format!(
            "{shown} is not a Python interpreter (no such file)"
        ));
    }
    Ok(())
}

/// Run the full probe in the interpreter, as the person, at the keyboard
/// (registration and confirmation): `-I` so nothing of theirs but the
/// interpreter's own folders is on its path; the environment without any
/// `COLUMBUS_*` setting (arcpy's licence check needs the rest); stopped,
/// with what it started, after `limit`.
pub fn probe(python: &Path, limit: Duration) -> Result<Found, String> {
    check_interpreter(python)?;
    let shown = python.display().to_string();
    let scratch =
        std::env::temp_dir().join(format!("columbus-probe-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let mut cmd = std::process::Command::new(python);
    cmd.args(probe_args(true))
        .current_dir(&scratch)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("COLUMBUS_")
        {
            cmd.env_remove(&k);
        }
    }
    let ran = run_limited(cmd, limit);
    let _ = std::fs::remove_dir_all(&scratch);
    let (stdout, stderr, ok) = match ran {
        Ran::Done {
            stdout,
            stderr,
            success,
        } => (stdout, stderr, success),
        Ran::TimedOut => {
            return Err(format!(
                "the probe of {shown} ran longer than {} s and was stopped",
                limit.as_secs()
            ))
        }
        Ran::Failed(e) => return Err(format!("{shown} is not a Python interpreter: {e}")),
    };
    parse_probe(&stdout).map_err(|why| {
        let tail: String = stderr
            .lines()
            .last()
            .unwrap_or("")
            .chars()
            .take(300)
            .collect();
        if ok {
            format!("{shown} is not a Python interpreter ({why})")
        } else {
            format!("{shown} is not a Python interpreter ({why}; {tail})")
        }
    })
}

enum Ran {
    Done {
        stdout: String,
        stderr: String,
        success: bool,
    },
    TimedOut,
    Failed(String),
}

/// Run a command to its end or `limit`, ending it and what it started on
/// time-out (its process group on Unix, its process tree on Windows).
fn run_limited(mut cmd: std::process::Command, limit: Duration) -> Ran {
    use std::io::Read;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Ran::Failed(e.to_string()),
    };
    let read = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.by_ref().take(16 * 1024 * 1024).read_to_end(&mut buf);
                // Read the rest so the program never blocks on a full pipe.
                let _ = std::io::copy(&mut r, &mut std::io::sink());
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() >= limit => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Ran::Failed(e.to_string()),
        }
    };
    let Some(status) = status else {
        kill_tree(&mut child);
        let _ = child.wait();
        return Ran::TimedOut;
    };
    #[cfg(unix)]
    kill_tree(&mut child);
    Ran::Done {
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        success: status.success(),
    }
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        if pid > 1 {
            // It leads its own group.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// The cheap freshness signal: the interpreter file's size and time, and
/// the names directly in each `sys.path` folder and in `conda-meta/`
/// (bytecode caches left out), hashed. Installing, removing or upgrading a
/// package changes a name there (pip and conda name folders by version).
pub fn fingerprint(python: &Path, site_dirs: &[String], prefixes: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut feed = |b: &[u8]| {
        h.update((b.len() as u64).to_le_bytes());
        h.update(b);
    };
    match std::fs::metadata(python) {
        Ok(m) => {
            feed(&m.len().to_le_bytes());
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            feed(&t.to_le_bytes());
        }
        Err(e) => feed(format!("python-error:{:?}", e.kind()).as_bytes()),
    }
    let mut dirs: Vec<PathBuf> = site_dirs.iter().map(PathBuf::from).collect();
    for p in prefixes {
        dirs.push(Path::new(p).join("conda-meta"));
    }
    for d in dirs {
        feed(d.to_string_lossy().as_bytes());
        match std::fs::read_dir(&d) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n != "__pycache__" && !n.ends_with(".pyc"))
                    .collect();
                names.sort();
                for n in names {
                    feed(n.as_bytes());
                }
            }
            Err(e) => feed(format!("error:{:?}", e.kind()).as_bytes()),
        }
    }
    format!("sha256:{:x}", h.finalize())
}

// ---------- registering, confirming, removing (at the keyboard only) ----------

/// What the person asks to register.
#[derive(Debug, Clone)]
pub struct Registration {
    pub id: String,
    pub python: PathBuf,
    pub trust: Trust,
    pub confinement: Confinement,
    pub created_by: CreatedBy,
    pub description: Option<String>,
}

/// The rules every registration and confirmation meets, under `policy`.
fn check_rules(
    policy: &EnvironmentsPolicy,
    envs: &Path,
    id: &str,
    python: &Path,
    trust: Trust,
    confinement: Confinement,
    created_by: CreatedBy,
) -> Result<(), String> {
    if !policy.allow {
        return Err("organization policy does not allow environments (environments.allow is off), so none can be registered or confirmed".into());
    }
    if !id_ok(id) {
        return Err(format!(
            "{id:?} is not an environment id (lowercase letters, digits and -, at most 40)"
        ));
    }
    check_interpreter(python)?;
    if trust == Trust::Trusted && !policy.allow_trusted {
        return Err("organization policy does not allow trusted environments (environments.allow_trusted is off); register it as governed".into());
    }
    if confinement == Confinement::B {
        if trust != Trust::Governed {
            return Err("confinement applies to governed environments only".into());
        }
        if !policy.allow_network {
            return Err("organization policy does not allow governed environments network access (environments.allow_network is off); use confinement A".into());
        }
    }
    if created_by == CreatedBy::Columbus && columbus_folder(envs, id, python).is_none() {
        return Err(format!(
            "an environment Columbus created must have its interpreter under {}",
            envs.join(id).display()
        ));
    }
    Ok(())
}

/// `<envs>/<id>`, really there (not a link) and really under `<envs>`,
/// when `python` really lies inside it.
fn columbus_folder(envs: &Path, id: &str, python: &Path) -> Option<PathBuf> {
    let folder = envs.join(id);
    let md = std::fs::symlink_metadata(&folder).ok()?;
    if !md.file_type().is_dir() {
        return None;
    }
    let real_envs = std::fs::canonicalize(envs).ok()?;
    let real = std::fs::canonicalize(&folder).ok()?;
    if real.parent() != Some(real_envs.as_path()) {
        return None;
    }
    // The interpreter's own path, without following its last link (a venv's
    // python is a link to the base interpreter).
    let py_dir = std::fs::canonicalize(python.parent()?).ok()?;
    py_dir.starts_with(&real).then_some(real)
}

fn store(dir: &Path, env: &Environment, found: &Found, python: &Path) -> Result<(), String> {
    let yaml = serde_yaml::to_string(env).map_err(|e| e.to_string())?;
    let text = format!(
        "# Registered at the keyboard. Editing python, trust, confinement, created_by or probe\n# marks it changed until it is confirmed again.\n{yaml}"
    );
    crate::tools::write_atomic(&file_for(dir, &env.id), text.as_bytes())?;
    let mut all = read_confirmed(dir);
    all.insert(
        env.id.clone(),
        Confirmed {
            sha256: Some(env.confirmation_hash()),
            changed: None,
            fingerprint: fingerprint(python, &found.site_dirs, &found.prefixes),
            prefixes: found.prefixes.clone(),
            site_dirs: found.site_dirs.clone(),
        },
    );
    write_confirmed(dir, &all)
}

/// The probe as F-4 stores it.
pub fn probe_record(found: &Found, now: &str) -> Probe {
    Probe {
        python_version: found.python_version.clone(),
        packages_sha256: found.packages_sha256(),
        arcpy: found.arcpy,
        licence: found.licence.clone(),
        probed_at: now.to_string(),
    }
}

/// Register a new environment, after the person typed its id (`typed`) to
/// confirm what they were shown (`found`, the probe they saw). Called only
/// from the keyboard: the terminal's `columbus-mcp env add` and the app.
pub fn register(
    dir: &Path,
    envs: &Path,
    policy: &EnvironmentsPolicy,
    reg: &Registration,
    found: &Found,
    typed: &str,
    now: &str,
) -> Result<Environment, String> {
    check_rules(
        policy,
        envs,
        &reg.id,
        &reg.python,
        reg.trust,
        reg.confinement,
        reg.created_by,
    )?;
    if std::fs::symlink_metadata(file_for(dir, &reg.id)).is_ok() {
        return Err(format!(
            "an environment {:?} is already registered; remove it first, or confirm it again",
            reg.id
        ));
    }
    if typed.trim() != reg.id {
        return Err("not confirmed; nothing changed".into());
    }
    let env = Environment {
        id: reg.id.clone(),
        python: reg.python.display().to_string(),
        trust: reg.trust,
        confinement: reg.confinement,
        created_by: reg.created_by,
        description: reg.description.clone(),
        probe: probe_record(found, now),
        confirmed_at: now.to_string(),
    };
    store(dir, &env, found, &reg.python)?;
    Ok(env)
}

/// The registration as it is in its file now, for the person to look at
/// before confirming it again (even when it is `changed`).
pub fn pending(dir: &Path, id: &str) -> Result<Environment, String> {
    load(dir, id)
}

/// Confirm an environment again, as its file stands now, with a fresh
/// probe the person was shown, after they typed its id.
pub fn confirm(
    dir: &Path,
    envs: &Path,
    policy: &EnvironmentsPolicy,
    id: &str,
    found: &Found,
    typed: &str,
    now: &str,
) -> Result<Environment, String> {
    let mut env = load(dir, id)?;
    let python = PathBuf::from(&env.python);
    check_rules(
        policy,
        envs,
        id,
        &python,
        env.trust,
        env.confinement,
        env.created_by,
    )?;
    if typed.trim() != id {
        return Err("not confirmed; nothing changed".into());
    }
    env.probe = probe_record(found, now);
    env.confirmed_at = now.to_string();
    store(dir, &env, found, &python)?;
    Ok(env)
}

/// What removing an environment deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub yaml: PathBuf,
    /// The interpreter folder too: only for an environment whose confirmed
    /// registration says `created_by: columbus` and whose folder's real path
    /// lies under `<Columbus data>/envs/`. Never a folder the person
    /// registered.
    pub folder: Option<PathBuf>,
}

/// What [`remove`] would delete, to show the person first.
pub fn removal(dir: &Path, envs: &Path, id: &str) -> Result<Removal, String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not an environment id"));
    }
    let yaml = file_for(dir, id);
    if std::fs::symlink_metadata(&yaml).is_err() {
        return Err(format!("there is no environment {id:?}"));
    }
    let v = view(dir, id);
    // The folder goes only if created_by: columbus is in what was confirmed:
    // the confirmed hash still matches the file.
    let confirmed_columbus = v.env.as_ref().filter(|e| {
        e.created_by == CreatedBy::Columbus
            && v.confirmed
                .as_ref()
                .and_then(|c| c.sha256.as_deref())
                .is_some_and(|h| h == e.confirmation_hash())
    });
    let folder = confirmed_columbus.and_then(|e| columbus_folder(envs, id, Path::new(&e.python)));
    Ok(Removal { yaml, folder })
}

/// Remove an environment after the person typed its id: its YAML and its
/// confirmation, and the folder [`removal`] names, if any. Past audit lines
/// and provenance stay as they are.
pub fn remove(dir: &Path, envs: &Path, id: &str, typed: &str) -> Result<Removal, String> {
    let plan = removal(dir, envs, id)?;
    if typed.trim() != id {
        return Err("not confirmed; nothing changed".into());
    }
    std::fs::remove_file(&plan.yaml).map_err(|e| format!("{}: {e}", plan.yaml.display()))?;
    let mut all = read_confirmed(dir);
    if all.remove(id).is_some() {
        write_confirmed(dir, &all)?;
    }
    if let Some(f) = &plan.folder {
        // Checked again just before: still a real folder under envs.
        let again = std::fs::symlink_metadata(f).is_ok_and(|m| m.file_type().is_dir());
        let real_envs = std::fs::canonicalize(envs).ok();
        if again && real_envs.as_deref() == f.parent() {
            std::fs::remove_dir_all(f).map_err(|e| format!("{}: {e}", f.display()))?;
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found() -> Found {
        Found {
            python_version: "3.12.1".into(),
            prefixes: vec![],
            site_dirs: vec![],
            packages: vec!["b==2".into(), "a==1".into()],
            conda_meta: None,
            arcpy: false,
            licence: None,
        }
    }

    #[test]
    fn ids_and_interpreter_names() {
        for ok in ["pro", "a-1", &"a".repeat(40)] {
            assert!(id_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "Pro",
            "a_b",
            "a.b",
            "../x",
            &"a".repeat(41),
            ".confirmed",
        ] {
            assert!(!id_ok(bad), "{bad}");
        }
        for ok in [
            "python",
            "python3",
            "python3.12",
            "python.exe",
            "PYTHON.EXE",
        ] {
            assert!(looks_like_python(Path::new(ok)), "{ok}");
        }
        for bad in ["rm", "pythonw-evil", "sh", "python3-config", "ipython"] {
            assert!(!looks_like_python(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn the_packages_hash_covers_names_versions_and_conda_meta() {
        let a = found();
        let mut b = found();
        b.packages.reverse();
        assert_eq!(a.packages_sha256(), b.packages_sha256());
        b.packages.push("c==3".into());
        assert_ne!(a.packages_sha256(), b.packages_sha256());
        let mut c = found();
        c.conda_meta = Some(vec!["x-1.0.json".into()]);
        assert_ne!(a.packages_sha256(), c.packages_sha256());
    }

    #[test]
    fn the_probe_answer_is_found_among_noise() {
        let out = "hello from a .pth\nCOLUMBUS-PROBE {\"python_version\": \"3.11.10\", \"prefixes\": [], \"site_dirs\": [], \"packages\": [\"x==1\"], \"conda_meta\": null, \"arcpy\": true, \"licence\": \"ArcInfo\"}\n";
        let f = parse_probe(out).unwrap();
        assert!(f.arcpy && f.licence.as_deref() == Some("ArcInfo"));
        assert!(parse_probe("nothing").is_err());
        assert!(parse_probe("COLUMBUS-PROBE {\"error\": \"old\"}").is_err());
    }

    #[test]
    fn every_hashed_field_changes_the_hash_and_unknown_keys_are_refused() {
        let env = Environment {
            id: "pro".into(),
            python: "/opt/py/bin/python".into(),
            trust: Trust::Governed,
            confinement: Confinement::A,
            created_by: CreatedBy::Person,
            description: None,
            probe: probe_record(&found(), "2026-11-08T10:00:00-06:00"),
            confirmed_at: "2026-11-08T10:00:00-06:00".into(),
        };
        let h = env.confirmation_hash();
        let mut e = env.clone();
        e.trust = Trust::Trusted;
        assert_ne!(e.confirmation_hash(), h);
        let mut e = env.clone();
        e.python = "/other/python".into();
        assert_ne!(e.confirmation_hash(), h);
        let mut e = env.clone();
        e.confinement = Confinement::B;
        assert_ne!(e.confirmation_hash(), h);
        let mut e = env.clone();
        e.created_by = CreatedBy::Columbus;
        assert_ne!(e.confirmation_hash(), h);
        let mut e = env.clone();
        e.probe.packages_sha256 = "sha256:00".into();
        assert_ne!(e.confirmation_hash(), h);
        // The description is free text, not hashed.
        let mut e = env.clone();
        e.description = Some("notes".into());
        assert_eq!(e.confirmation_hash(), h);
        let text = serde_yaml::to_string(&env).unwrap();
        assert_eq!(serde_yaml::from_str::<Environment>(&text).unwrap(), env);
        let bad = format!("{text}allow_everything: true\n");
        assert!(serde_yaml::from_str::<Environment>(&bad).is_err());
    }
}
