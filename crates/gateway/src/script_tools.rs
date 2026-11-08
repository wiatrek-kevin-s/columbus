//! `run_python` (design section 3, "Scripts"): a Python script the agent
//! wrote, run on workspace layers after the person has read it and said yes.
//!
//! The agent is untrusted, so the script is too. It runs only when
//! organization policy allows scripts, only after the person approves that
//! exact code with those exact inputs (every run, unless policy
//! `scripts.session_approval` lets them allow that code for the session:
//! the engine and governed level-A environments only), and only confined at level A through the `columbus-confine`
//! launcher: its own run folder is the one place it can write, the Python
//! installation the one other place it can read, and it has no network.
//! There is no unconfined fallback, whatever `allow_unconfined_local` says.
//! It gets a cleared environment (no secrets, no COLUMBUS_* settings), copies
//! of its inputs whose content is checked against what Columbus recorded, a
//! time limit after which its whole process tree is ended, and capped
//! output. What it leaves in `out/` comes back as workspace outputs that
//! carry the inputs' data labels and fails, copied out of the run folder
//! without following links.
//!
//! The one exception is a **trusted** environment (D-3, REQ-15), under
//! policy `environments.allow_trusted`: its scripts run unconfined, as the
//! person, and the card says Columbus cannot enforce the connection rules
//! for that run. It is still approved every run (never for the session),
//! still gets the same run folder, copies and time limit (its process tree
//! ended), and what it leaves in `out/` comes back carrying every data
//! label of the session plus `environment:<id>`, so exports through
//! Columbus still follow the flow rules.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::Local;
use columbus_shared::approvals::Risk;
use columbus_shared::audit::AuditRecord;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, Json, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::approval_flow::{Ask, Sought};
use crate::providers::{OutputMeta, OutputSource};
use crate::server::{
    checks_from, client_name, new_prov_id, Check, Gateway, Output, Severity, SERVER_NAME,
};

/// Runs the script (its path the first argument) as `__main__` from its
/// UTF-8 text, compiled from that text: a coding declaration has no effect,
/// so the code that runs is the code the person read. Python run on a file
/// would honour `# coding: utf-7` and decode a comment into code.
const RUN_TEXT: &str = "import sys, types; p = sys.argv[1]; t = open(p, encoding='utf-8', newline='').read(); m = types.ModuleType('__main__'); m.__file__ = p; m.__builtins__ = __builtins__; sys.modules['__main__'] = m; sys.argv = [p]; exec(compile(t, p, 'exec', dont_inherit=True), m.__dict__)";

/// How a personal tool or workflow step whose environment can't be used
/// stops (REQ-18, R-13c): it needs the person. A workflow run goes on with
/// the steps that don't depend on it.
pub(crate) const ENV_NEEDS_YOU: &str =
    "needs you: the environment this tool runs in can't be used:";

/// Largest script, in bytes.
const MAX_CODE: usize = 100 * 1024;
const MAX_INPUTS: usize = 20;
const DEFAULT_TIMEOUT: u64 = 120;
/// Bytes of stdout and of stderr returned; the rest is cut with a note.
const STREAM_CAP: usize = 64 * 1024;
/// Files taken from `out/`, and their sizes.
const MAX_OUTPUTS: usize = 50;
const MAX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// Processes the script may run at once, itself included.
const PROCESSES: u32 = 8;
/// GDAL drivers left out, as in the engine (`_SKIP` in columbus_gis.py):
/// formats whose files can point at other files, programs or URLs.
const GDAL_SKIP: &str = "GDALG OGR_VRT VRT";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunPythonArgs {
    /// Python 3 source, at most 100 KB. It runs as script.py in a run folder
    /// of its own: inputs are in in/<name>.<ext> (inputs.json maps each name
    /// to its path), every regular file it writes in out/ becomes a
    /// workspace output, and what it prints comes back. No network; it can
    /// write only its run folder. geopandas, shapely, pyproj, rasterio and
    /// pandas are there when the engine has them; ArcPy is not.
    pub code: String,
    /// Workspace layers to give the script, as name -> ws:// handle. Names
    /// use letters, digits, `_` and `-`.
    #[serde(default)]
    pub inputs: Option<BTreeMap<String, String>>,
    /// Seconds before the script is stopped (default 120, at most the
    /// organization's limit).
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// An approval the person granted for this exact run.
    #[serde(default)]
    pub approval_id: Option<String>,
    /// A Python environment the person registered (whoami lists them, with
    /// their Python version, packages and arcpy licence), instead of the
    /// engine's Python. A governed one runs confined, with the same
    /// extracts; its own interpreter folder is the only extra folder it can
    /// read. A trusted one (experimental; policy environments.allow_trusted)
    /// runs unconfined as the person, approved every run; its outputs carry
    /// every data label of the session plus environment:<id>. Needs
    /// organization policy environments.allow.
    #[serde(default)]
    pub environment: Option<String>,
}

/// A registered environment a script runs in, checked just now.
pub(crate) struct ScriptEnv {
    pub(crate) env: columbus_shared::environments::Environment,
    /// The confinement level of a governed environment; None for a trusted
    /// one, which runs unconfined (D-3).
    pub(crate) level: Option<columbus_confine::Level>,
    /// The interpreter's prefixes, as confirmed: the folders it may read.
    pub(crate) read_only: Vec<PathBuf>,
}

impl ScriptEnv {
    /// A trusted environment: unconfined, as the person (D-3, REQ-15).
    pub(crate) fn trusted(&self) -> bool {
        self.level.is_none()
    }
    /// Whether "Allow this script for the session" may ever be offered for
    /// a run here (AF-6, REQ-17: governed level A only). Never for trusted:
    /// every trusted run is approved on its own.
    pub(crate) fn session_allow_possible(&self) -> bool {
        self.level == Some(columbus_confine::Level::A)
    }
    /// Personal tools and workflow steps (REQ-18) take governed
    /// environments at level A only; this says why another is refused.
    pub(crate) fn usable_in_tools_and_steps(&self) -> Result<(), String> {
        self.env.usable_in_tools_and_steps()?;
        if self.level != Some(columbus_confine::Level::A) {
            return Err(format!(
                "environment {} runs with network (confinement B); personal tools and workflow steps run at level A only (no network), so they cannot use it. Use an environment at level A",
                self.env.id
            ));
        }
        Ok(())
    }
    /// The confinement profile of a personal tool running here: one per
    /// tool and environment, so on Windows neither the environment's
    /// scripts nor another tool are ever granted this tool's folder.
    pub(crate) fn tool_profile(&self, tool: &str) -> String {
        let h = columbus_shared::canon::sha256_hex(format!("{tool}\0{}", self.env.id).as_bytes());
        format!("tool-env.{}", &h[..16])
    }
    pub(crate) fn python(&self) -> PathBuf {
        PathBuf::from(&self.env.python)
    }
    /// The confinement profile's name: one per environment, so on Windows
    /// a script in one environment is never granted another's folders.
    pub(crate) fn profile(&self) -> String {
        format!("script.env-{}", self.env.id)
    }
    /// The provenance `environment` map (F-9).
    pub(crate) fn provenance(&self) -> Value {
        json!({"id": self.env.id, "trust": self.env.trust.as_str(),
            "python_version": self.env.probe.python_version,
            "packages_sha256": self.env.probe.packages_sha256})
    }
}

fn input_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 40
        && !n.starts_with('-')
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A file extension safe to put on a copy: letters and digits only.
fn safe_ext(p: &Path) -> String {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.len() <= 10 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "dat".into())
}

/// What a stream said, kept up to [`STREAM_CAP`].
pub(crate) struct Captured {
    kept: Vec<u8>,
    total: u64,
}

impl Captured {
    fn truncated(&self) -> bool {
        self.total > self.kept.len() as u64
    }
    pub(crate) fn text(&self) -> String {
        let mut s = String::from_utf8_lossy(&self.kept).into_owned();
        if self.truncated() {
            s.push_str(&format!(
                "\n[cut by Columbus: {} more bytes not shown]",
                self.total - self.kept.len() as u64
            ));
        }
        s
    }
}

/// Read a stream to its end, keeping the first [`STREAM_CAP`] bytes. It is
/// always read to the end so the script never blocks on a full pipe.
async fn capture(mut r: impl AsyncRead + Unpin) -> Captured {
    let mut kept = Vec::new();
    let mut total: u64 = 0;
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                total += n as u64;
                if kept.len() < STREAM_CAP {
                    let take = (STREAM_CAP - kept.len()).min(n);
                    kept.extend_from_slice(&buf[..take]);
                }
            }
        }
    }
    Captured { kept, total }
}

/// Ends the script's process group if the call is dropped part way (the
/// client cancelled): nothing it started is left running. On Windows the
/// launcher's job object does this when the launcher is killed.
/// A direct (trusted) run on Windows has no job object, so its tree is
/// ended with `taskkill /T`.
struct GroupGuard {
    pid: Option<u32>,
    tree: bool,
}

impl GroupGuard {
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            if pid > 1 {
                // The script was started as the leader of its own group.
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            }
        }
        #[cfg(windows)]
        if let (Some(pid), true) = (self.pid, self.tree) {
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        #[cfg(not(windows))]
        let _ = self.tree;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

pub(crate) struct Ran {
    /// None when it was ended by a signal or the time limit.
    pub(crate) exit_code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) stdout: Captured,
    pub(crate) stderr: Captured,
    pub(crate) seconds: f64,
}

/// The environment a script gets: nothing of the gateway's but what a
/// program needs to start.
pub(crate) fn script_env(run: &Path) -> Vec<(String, OsString)> {
    let mut env: Vec<(String, OsString)> = Vec::new();
    if let Some(p) = std::env::var_os("PATH") {
        env.push(("PATH".into(), p));
    }
    if cfg!(windows) {
        if let Some(p) = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("SYSTEMROOT")) {
            env.push(("SYSTEMROOT".into(), p));
        }
        // Windows starts an AppContainer process relative to the person's
        // profile and fails (ERROR_ENVVAR_NOT_FOUND) without these folder
        // names; the container cannot read those folders.
        for k in [
            "SystemDrive",
            "windir",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "ProgramData",
            "ALLUSERSPROFILE",
            "PUBLIC",
            "USERNAME",
            "USERDOMAIN",
            "HOMEDRIVE",
            "HOMEPATH",
            "ProgramFiles",
            "CommonProgramFiles",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ] {
            if let Some(v) = std::env::var_os(k) {
                env.push((k.into(), v));
            }
        }
    }
    let tmp = run.join("tmp").into_os_string();
    for k in ["TEMP", "TMP", "TMPDIR"] {
        env.push((k.into(), tmp.clone()));
    }
    for (k, v) in [
        ("PYTHONIOENCODING", "utf-8"),
        ("PROJ_NETWORK", "OFF"),
        ("GDAL_SKIP", GDAL_SKIP),
        ("OGR_SQLITE_ALLOW_EXTERNAL_ACCESS", "NO"),
    ] {
        env.push((k.into(), v.into()));
    }
    env.push(("COLUMBUS_RUN_DIR".into(), run.as_os_str().to_owned()));
    env
}

/// Copy one file the script left (opened through the safe folder walk: a
/// regular file with one name, never a link, FIFO or device) to a new file
/// of the gateway's, at most `cap` bytes. Returns its SHA-256 and size.
fn take_output(src: std::fs::File, to: &Path, cap: u64) -> Result<(String, u64), String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let m = src.metadata().map_err(|e| e.to_string())?;
    if m.len() > cap {
        return Err(format!("it is larger than {} MB", cap / 1024 / 1024));
    }
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| format!("could not write the copy: {e}"))?;
    let mut h = Sha256::new();
    let mut n: u64 = 0;
    let mut buf = vec![0u8; 1 << 16];
    let mut src = src.take(cap + 1);
    loop {
        let k = src.read(&mut buf).map_err(|e| e.to_string())?;
        if k == 0 {
            break;
        }
        n += k as u64;
        if n > cap {
            drop(dst);
            let _ = std::fs::remove_file(to);
            return Err(format!("it is larger than {} MB", cap / 1024 / 1024));
        }
        h.update(&buf[..k]);
        dst.write_all(&buf[..k]).map_err(|e| e.to_string())?;
    }
    dst.sync_all().map_err(|e| e.to_string())?;
    Ok((format!("{:x}", h.finalize()), n))
}

/// One file taken from `out/`.
struct Taken {
    name: String,
    path: PathBuf,
    sha256: String,
    bytes: u64,
}

/// Every regular file directly in `out/` (below `root`, reached without
/// following links: the script owns the run folder and could have replaced
/// `out` with a link), copied to `<results>/<prefix>-<name>`; the rest
/// (links, folders, odd names, too many or too large) is listed as skipped.
/// Called only once the script's processes have all ended.
fn collect_outputs(
    root: &Path,
    out_dir: &Path,
    results: &Path,
    prefix: &str,
) -> (Vec<Taken>, Vec<String>) {
    let mut taken = Vec::new();
    let mut skipped = Vec::new();
    let dir = match crate::safefs::below(root, out_dir).and_then(|comps| {
        let comps: Vec<&str> = comps.iter().map(String::as_str).collect();
        crate::safefs::Dir::open_beneath(root, &comps, false)
    }) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (taken, skipped),
        Err(e) => {
            skipped.push(format!("out/ ({e}; nothing was taken)"));
            return (taken, skipped);
        }
    };
    let names = match dir.entries() {
        Ok(n) => n,
        Err(_) => return (taken, skipped),
    };
    let mut total: u64 = 0;
    for name in names {
        if !crate::export_tools::valid_name(&name) {
            skipped.push(format!(
                "{name} (names may use letters, digits, space, -, _ and ., at most 80 characters)"
            ));
            continue;
        }
        let src = match dir.open_file(&name) {
            Ok(f) => f,
            Err(e) => {
                skipped.push(format!(
                    "{name} ({e}; only plain files directly in out/ are taken)"
                ));
                continue;
            }
        };
        if taken.len() >= MAX_OUTPUTS {
            skipped.push(format!("{name} (more than {MAX_OUTPUTS} files)"));
            continue;
        }
        let room = MAX_TOTAL_BYTES.saturating_sub(total);
        if let Err(e) = std::fs::create_dir_all(results) {
            skipped.push(format!("{name} (could not make the results folder: {e})"));
            continue;
        }
        // A fresh name, created new (never a file or link already there).
        let to = results.join(format!("{prefix}-{name}"));
        match take_output(src, &to, room.min(MAX_OUTPUT_BYTES)) {
            Ok((sha256, bytes)) => {
                total += bytes;
                taken.push(Taken {
                    name,
                    path: to,
                    sha256,
                    bytes,
                });
            }
            Err(why) => skipped.push(format!("{name} ({why})")),
        }
    }
    (taken, skipped)
}

/// Longest line the approval card shows without scrolling.
const MAX_LINE: usize = 200;

/// Code is run exactly as the person reads it: no characters that display
/// differently from how Python reads them. Refused: carriage returns (Python
/// breaks a line at a lone CR that a browser shows as one line), other
/// control characters but tab and newline, bidirectional and invisible
/// formatting characters (Trojan Source), and lines too long to see whole.
fn shown_as_run(code: &str) -> Result<(), String> {
    columbus_shared::tools::coding_line(code)?;
    for (i, line) in code.split('\n').enumerate() {
        let n = i + 1;
        if line.chars().count() > MAX_LINE {
            return Err(format!(
                "line {n} is longer than {MAX_LINE} characters; split it, so the person can read every line of the script"
            ));
        }
        for c in line.chars() {
            let bad = c != '\t' && columbus_shared::canon::hidden_char(c);
            if bad {
                return Err(format!(
                    "line {n} contains the character U+{:04X}, which displays differently from how Python reads it; scripts must be plain text",
                    c as u32
                ));
            }
        }
    }
    Ok(())
}

fn check(id: &str, severity: Severity, message: String, fix: Option<&str>) -> Check {
    Check {
        id: id.into(),
        severity,
        message,
        detail: None,
        fix: fix.map(String::from),
    }
}

/// One input, checked and bound.
pub(crate) struct ScriptInput {
    pub(crate) name: String,
    pub(crate) handle: String,
    pub(crate) path: PathBuf,
    pub(crate) meta: OutputMeta,
    pub(crate) sha256: String,
}

impl Gateway {
    /// A workspace file Columbus recorded in this session, not from a
    /// connected server, unchanged (as `own_output` for exports).
    pub(crate) fn script_input(&self, name: &str, handle: &str) -> Result<ScriptInput, String> {
        let path =
            crate::providers::resolve_handle(&self.state.session, &self.state.sandbox, handle)
                .map_err(|e| format!("input {name}: {e}"))?;
        let meta = self.state.outputs.get(&path).ok_or_else(|| {
            format!("input {name}: {handle} was not written by a Columbus tool in this session, so a script cannot read it")
        })?;
        if meta.source == OutputSource::Provider {
            return Err(format!(
                "input {name}: {handle} came from a connected server; scripts get only files Columbus, an approved engine plugin or an approved script wrote"
            ));
        }
        if !self.state.outputs.unchanged(&path) {
            return Err(format!(
                "input {name}: {handle} changed after Columbus wrote it"
            ));
        }
        let sha256 = meta
            .sha256
            .clone()
            .ok_or_else(|| format!("input {name}: {handle} has no recorded content"))?;
        Ok(ScriptInput {
            name: name.to_string(),
            handle: handle.to_string(),
            path,
            meta,
            sha256,
        })
    }

    /// The environment `id`, ready for a governed script to run in, or why
    /// not (R-13b: a changed one says "confirm it again in Settings"). The
    /// hook personal tools and workflow steps (T-9) use too.
    pub(crate) fn script_environment(
        &self,
        id: &str,
        policy: &columbus_shared::policy::OrgPolicy,
    ) -> Result<ScriptEnv, String> {
        use columbus_shared::environments::{self as envs, Confinement, Status, Trust};
        if !policy.environments.allow {
            return Err("organization policy does not allow environments (environments.allow is off), so the environment argument is refused; leave it out to run in the engine's Python".into());
        }
        let v = envs::view(&self.state.paths.environments, id);
        let env = match (&v.status, v.env) {
            (Status::Ready, Some(e)) => e,
            (Status::Changed(why), _) => {
                return Err(format!(
                    "environment {id} changed since the person confirmed it ({why}), so it does not run until they {} (or run `columbus-mcp env confirm {id}` in a terminal)",
                    envs::CONFIRM_AGAIN
                ))
            }
            (Status::Invalid(why), _) => {
                return Err(format!(
                    "environment {id} cannot be used: {why}. The person registers environments at the keyboard (`columbus-mcp env add`); whoami lists them"
                ))
            }
            (Status::Ready, None) => return Err(format!("environment {id} cannot be used")),
        };
        let level = if env.trust == Trust::Trusted {
            // Checked at every run, so turning the switch off stops them.
            if !policy.environments.allow_trusted {
                return Err(format!("environment {id} is trusted, and organization policy does not allow trusted environments (environments.allow_trusted is off)"));
            }
            None
        } else {
            Some(match env.confinement {
                Confinement::A => columbus_confine::Level::A,
                Confinement::B => {
                    if !policy.environments.allow_network {
                        return Err(format!("environment {id} runs with network (confinement B), and organization policy does not allow that (environments.allow_network is off)"));
                    }
                    columbus_confine::Level::B
                }
            })
        };
        let read_only: Vec<PathBuf> = v
            .confirmed
            .map(|c| c.prefixes)
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir())
            .collect();
        if read_only.is_empty() {
            return Err(format!(
                "environment {id}'s interpreter folders are gone; the person must {}",
                envs::CONFIRM_AGAIN
            ));
        }
        Ok(ScriptEnv {
            env,
            level,
            read_only,
        })
    }

    /// The environment `id`, ready for a personal tool or workflow step to
    /// run in (F-5, F-6, REQ-18), or why not, in words that start with
    /// [`ENV_NEEDS_YOU`]: missing, `changed`, trusted, at level B, or no
    /// longer allowed by policy. A trusted one is refused for what it is,
    /// whatever the policy says.
    pub(crate) fn tool_environment(
        &self,
        id: &str,
        policy: &columbus_shared::policy::OrgPolicy,
    ) -> Result<ScriptEnv, String> {
        use columbus_shared::environments as envs;
        let needs = |why: String| format!("{ENV_NEEDS_YOU} {why}");
        if let Some(e) = envs::view(&self.state.paths.environments, id).env {
            e.usable_in_tools_and_steps().map_err(needs)?;
        }
        let se = self.script_environment(id, policy).map_err(needs)?;
        se.usable_in_tools_and_steps().map_err(needs)?;
        Ok(se)
    }

    /// [`tool_environment`](Self::tool_environment), then the interpreter
    /// checked confined (as `check_environment`), with the profile the
    /// tool runs under.
    pub(crate) async fn tool_environment_checked(
        &self,
        tool: &str,
        id: &str,
        policy: &columbus_shared::policy::OrgPolicy,
    ) -> Result<(ScriptEnv, crate::plugins::ToolEnv), String> {
        let se = self.tool_environment(id, policy)?;
        let launcher = columbus_confine::ready(columbus_confine::Level::A).map_err(|e| {
            format!("tools run only confined (level A: no network, writes only their own folder), and {e}; Columbus never runs one unconfined")
        })?;
        self.check_environment(Some(&launcher), &se, policy.plugins.memory_mb)
            .await
            .map_err(|why| format!("{ENV_NEEDS_YOU} {why}"))?;
        let te = crate::plugins::ToolEnv {
            python: se.python(),
            read_only: se.read_only.clone(),
            level: se.level,
            profile: se.tool_profile(tool),
        };
        Ok((se, te))
    }

    /// Check, confined exactly as its scripts will be, that the
    /// environment's interpreter starts and still sees the packages the
    /// person confirmed (R-13b). An interpreter that cannot start confined
    /// here is refused, never run unconfined (A-1's fallback). A trusted
    /// environment is checked as it runs: unconfined (`launcher` None).
    pub(crate) async fn check_environment(
        &self,
        launcher: Option<&Path>,
        se: &ScriptEnv,
        memory_mb: u64,
    ) -> Result<(), String> {
        use columbus_shared::environments as envs;
        let id = &se.env.id;
        let runs = self
            .state
            .sandbox
            .resolve("runs")
            .map_err(|e| e.to_string())?;
        let run = runs.join(format!("probe-{}", new_prov_id()));
        std::fs::create_dir_all(&runs)
            .and_then(|_| std::fs::create_dir(&run))
            .and_then(|_| std::fs::create_dir(run.join("tmp")))
            .map_err(|e| format!("could not make the probe folder: {e}"))?;
        let spec = columbus_confine::Spec {
            level: columbus_confine::Level::A,
            name: se.profile(),
            read_write: vec![run.clone()],
            read_only: se.read_only.clone(),
            memory_mb: Some(memory_mb),
            processes: Some(PROCESSES),
        };
        let python = se.python().to_string_lossy().into_owned();
        let ran = match launcher {
            Some(launcher) => {
                run_under(
                    launcher,
                    &spec,
                    &run,
                    &python,
                    &envs::probe_args(false),
                    script_env(&run),
                    envs::probe_limit(),
                )
                .await
            }
            None => {
                run_direct(
                    &run,
                    &python,
                    &envs::probe_args(false),
                    trusted_env(&run),
                    envs::probe_limit(),
                )
                .await
            }
        };
        let _ = std::fs::remove_dir_all(&run);
        let ran = ran?;
        let trusted = launcher.is_none();
        let unconfinable = |why: String| {
            if trusted {
                format!("environment {id}'s interpreter did not start ({why}), so nothing was run; the person can check it and {}", envs::CONFIRM_AGAIN)
            } else {
                format!(
                    "environment {id} cannot run confined on this system ({why}), so its scripts are refused here; Columbus never runs a governed environment unconfined. Leave environment out to use the engine's Python"
                )
            }
        };
        if ran.timed_out {
            return Err(unconfinable(format!(
                "its interpreter did not answer within {} s and was stopped",
                envs::probe_limit().as_secs()
            )));
        }
        let found = match envs::parse_probe(&ran.stdout.text()) {
            Ok(f) => f,
            Err(_) => {
                let err = ran.stderr.text();
                let tail: String = err
                    .trim()
                    .lines()
                    .last()
                    .unwrap_or("")
                    .chars()
                    .take(300)
                    .collect();
                return Err(unconfinable(if tail.is_empty() {
                    format!("its interpreter exited with {:?}", ran.exit_code)
                } else {
                    tail
                }));
            }
        };
        let was = &se.env.probe;
        if found.packages_sha256() != was.packages_sha256
            || found.python_version != was.python_version
        {
            let why = format!(
                "its packages or Python version changed since it was confirmed (now Python {}, {} packages)",
                found.python_version,
                found.packages.len()
            );
            let _ = envs::mark_changed(&self.state.paths.environments, id, &why);
            return Err(format!(
                "environment {id} changed since the person confirmed it ({why}), so it does not run until they {} (or run `columbus-mcp env confirm {id}` in a terminal)",
                envs::CONFIRM_AGAIN
            ));
        }
        Ok(())
    }

    /// Start the script under the launcher (or, for a trusted environment,
    /// `launcher` None, directly as the person), wait for it within
    /// `timeout`, and end everything it started.
    async fn run_script(
        &self,
        launcher: Option<&Path>,
        spec: &columbus_confine::Spec,
        run: &Path,
        timeout: Duration,
        python: &Path,
    ) -> Result<Ran, String> {
        let python = python.to_string_lossy().into_owned();
        let script = run.join("script.py").display().to_string();
        // -X utf8 because -I ignores PYTHONIOENCODING. The script is run
        // from its UTF-8 text (RUN_TEXT), never decoded by Python itself.
        let args: Vec<String> = ["-I", "-B", "-X", "utf8", "-c", RUN_TEXT, &script]
            .iter()
            .map(|s| s.to_string())
            .collect();
        match launcher {
            Some(launcher) => {
                run_under(
                    launcher,
                    spec,
                    run,
                    &python,
                    &args,
                    script_env(run),
                    timeout,
                )
                .await
            }
            None => run_direct(run, &python, &args, trusted_env(run), timeout).await,
        }
    }
}

/// The environment a trusted script gets: the person's own (arcpy's
/// licence check reads their profile, and for Named User licences the
/// network), without any `COLUMBUS_*` setting of the gateway's, with its
/// temporary files in the run folder.
pub(crate) fn trusted_env(run: &Path) -> Vec<(String, OsString)> {
    let mut env: Vec<(String, OsString)> = std::env::vars_os()
        .filter_map(|(k, v)| {
            let name = k.to_str()?.to_string();
            let upper = name.to_ascii_uppercase();
            let dropped = upper.starts_with("COLUMBUS_")
                || ["TEMP", "TMP", "TMPDIR", "PYTHONIOENCODING"].contains(&upper.as_str());
            (!dropped).then_some((name, v))
        })
        .collect();
    let tmp = run.join("tmp").into_os_string();
    for k in ["TEMP", "TMP", "TMPDIR"] {
        env.push((k.into(), tmp.clone()));
    }
    env.push(("PYTHONIOENCODING".into(), "utf-8".into()));
    env.push(("COLUMBUS_RUN_DIR".into(), run.as_os_str().to_owned()));
    env
}

/// Run `program args` directly, unconfined, as the person (trusted
/// environments only), in `cwd` with only `env`; wait for it within
/// `timeout`, and end everything it started: its process group on Unix,
/// its process tree on Windows.
pub(crate) async fn run_direct(
    cwd: &Path,
    program: &str,
    args: &[String],
    env: Vec<(String, OsString)>,
    timeout: Duration,
) -> Result<Ran, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    wait_for(cmd, timeout, true).await
}

/// Run `program args` under the confinement launcher, in `cwd` with only
/// `env`; wait for it within `timeout`, and end everything it started.
pub(crate) async fn run_under(
    launcher: &Path,
    spec: &columbus_confine::Spec,
    cwd: &Path,
    program: &str,
    args: &[String],
    env: Vec<(String, OsString)>,
    timeout: Duration,
) -> Result<Ran, String> {
    let mut cmd = tokio::process::Command::new(launcher);
    cmd.args(spec.launcher_args(program, args))
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    wait_for(cmd, timeout, false).await
}

/// Start `cmd`, wait for it within `timeout`, and end everything it
/// started. `tree`: also end its process tree on Windows (a direct run has
/// no launcher job object to do it).
async fn wait_for(
    mut cmd: tokio::process::Command,
    timeout: Duration,
    tree: bool,
) -> Result<Ran, String> {
    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| {
        if tree {
            format!("could not start the script: {e}")
        } else {
            format!("could not start the confined script: {e}")
        }
    })?;
    let mut group = GroupGuard {
        pid: child.id(),
        tree,
    };
    let out = tokio::spawn(capture(child.stdout.take().expect("piped stdout")));
    let err = tokio::spawn(capture(child.stderr.take().expect("piped stderr")));
    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(s)) => (Some(s), false),
        Ok(Err(e)) => {
            group.kill();
            let _ = child.start_kill();
            return Err(format!("lost track of the script: {e}"));
        }
        Err(_) => {
            // The whole tree: the group on Linux, the job (closed with
            // the launcher) on Windows.
            group.kill();
            let _ = child.start_kill();
            (child.wait().await.ok(), true)
        }
    };
    // Anything it left running in its group goes too. (On Windows the tree
    // was ended above on a time-out; a process that has exited has no tree
    // left to find by its id.)
    #[cfg(unix)]
    group.kill();
    group.pid = None;
    let seconds = started.elapsed().as_secs_f64();
    let grace = Duration::from_secs(5);
    let empty = || Captured {
        kept: Vec::new(),
        total: 0,
    };
    let stdout = match tokio::time::timeout(grace, out).await {
        Ok(Ok(c)) => c,
        _ => empty(),
    };
    let stderr = match tokio::time::timeout(grace, err).await {
        Ok(Ok(c)) => c,
        _ => empty(),
    };
    Ok(Ran {
        exit_code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
        timed_out,
        stdout,
        stderr,
        seconds,
    })
}

#[tool_router(router = script_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        description = "Run a Python script on workspace layers, for what the GIS tools cannot do. The person sees the full code and approves every run (where organization policy allows it, they may allow that exact code for the session: the same code with new inputs then runs without asking, any change asks again). It runs confined: no network, it can write only its own run folder, and it is stopped at the time limit. Inputs (name -> ws:// handle) are copied to in/<name>.<ext> and listed in inputs.json; every file it writes in out/ comes back as a workspace output carrying the inputs' data labels; stdout and stderr come back (64 KB each). Needs organization policy to allow scripts (whoami: scripts_allowed). By default it runs in the engine's Python, which has no ArcPy; pass environment to run in one the person registered (whoami: environments), under the same rules. A trusted environment (experimental; where policy allows it, for example for ArcPy) runs unconfined as the person, still approved every run, and its outputs carry the session's data labels plus environment:<id>.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn run_python(
        &self,
        Parameters(args): Parameters<RunPythonArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let code_sha256 = columbus_shared::canon::sha256_hex(args.code.as_bytes());
        let request = json!({"code_sha256": code_sha256, "code_bytes": args.code.len(),
            "inputs": args.inputs, "timeout_seconds": args.timeout_seconds,
            "environment": args.environment});
        let mut rec = self.gate(&ctx, "run_python", &request)?;
        // F-8: the environment's id, or null for the engine's Python.
        rec.environment = Some(args.environment.clone());
        let started = Local::now();
        let prov = new_prov_id();
        let policy = &self.state.policy();
        if !policy.allow_scripts {
            return Err(self.deny(
                &ctx,
                rec,
                "organization policy does not allow scripts (allow_scripts is off), so run_python cannot run code here; use the GIS tools (query_features, vector_op, raster_op, verify) instead",
            ));
        }
        if args.code.trim().is_empty() {
            return Err(self.deny(&ctx, rec, "code is empty"));
        }
        if args.code.len() > MAX_CODE {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "code is {} bytes; scripts may be at most {} KB",
                    args.code.len(),
                    MAX_CODE / 1024
                ),
            ));
        }
        if let Err(why) = shown_as_run(&args.code) {
            return Err(self.deny(&ctx, rec, why));
        }
        // A registered environment as confirmed, or the engine.
        let senv = match args.environment.as_deref() {
            None => None,
            Some(id) => match self.script_environment(id, policy) {
                Ok(e) => Some(e),
                Err(e) => return Err(self.deny(&ctx, rec, e)),
            },
        };
        // A trusted environment runs unconfined (D-3); everything else at
        // its level or not at all.
        let trusted = senv.as_ref().is_some_and(ScriptEnv::trusted);
        let level = senv
            .as_ref()
            .map_or(Some(columbus_confine::Level::A), |e| e.level);
        let network = level == Some(columbus_confine::Level::B);
        let launcher = match level {
            None => None,
            Some(level) => match columbus_confine::ready(level) {
                Ok(l) => Some(l),
                Err(e) => {
                    return Err(self.deny(
                        &ctx,
                        rec,
                        format!(
                            "scripts run only confined (level {}: {}, writes only to their own folder), and {e}; Columbus never runs a script unconfined",
                            level.as_str(),
                            if network { "network" } else { "no network" }
                        ),
                    ))
                }
            },
        };
        if let Some(se) = &senv {
            if let Err(e) = self
                .check_environment(launcher.as_deref(), se, policy.plugins.memory_mb)
                .await
            {
                return Err(self.deny(&ctx, rec, e));
            }
        }
        let cap = policy.read_limits().max_seconds();
        let timeout = args
            .timeout_seconds
            .unwrap_or(DEFAULT_TIMEOUT)
            .clamp(1, cap);

        // Inputs: Columbus's own files, unchanged, bound by content.
        let wanted = args.inputs.clone().unwrap_or_default();
        if wanted.len() > MAX_INPUTS {
            return Err(self.deny(
                &ctx,
                rec,
                format!("a script may take at most {MAX_INPUTS} inputs"),
            ));
        }
        let mut inputs: Vec<ScriptInput> = Vec::new();
        for (name, handle) in &wanted {
            if !input_name_ok(name) {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!("input name {name:?} may use letters, digits, _ and - (at most 40 characters)"),
                ));
            }
            match self.script_input(name, handle) {
                Ok(i) => inputs.push(i),
                Err(e) => return Err(self.deny(&ctx, rec, e)),
            }
        }
        let files: Vec<PathBuf> = inputs.iter().map(|i| i.path.clone()).collect();
        let mut labels: BTreeSet<String> = self.state.flow.labels_for(&files);
        labels.extend(self.state.flow.session_labels());
        let from: Vec<String> = labels
            .iter()
            .map(|l| l.trim_start_matches("connection:").to_string())
            .collect();
        // A trusted run's outputs carry every label of the session and its
        // inputs, plus the environment's (REQ-15), so exports through
        // Columbus still follow the flow rules.
        if let Some(se) = senv.as_ref().filter(|e| e.trusted()) {
            labels.insert(columbus_shared::environments::environment_label(&se.env.id));
        }
        rec.labels = labels.iter().cloned().collect();

        // The person reads the code and says yes to this run.
        let lines = args.code.lines().count();
        let digest = columbus_shared::canon::sha256_hex(args.code.as_bytes());
        let where_runs = match &senv {
            None => String::new(),
            Some(se) if se.trusted() => format!(
                " in trusted environment {} {} (Python {}, unconfined, as you{})",
                se.env.id,
                columbus_shared::environments::EXPERIMENTAL,
                se.env.probe.python_version,
                se.env
                    .licence_line()
                    .map(|l| format!("; {l}"))
                    .unwrap_or_default()
            ),
            Some(se) => format!(
                " in environment {} (Python {}, governed, level {})",
                se.env.id,
                se.env.probe.python_version,
                se.level.map_or("?", |l| l.as_str())
            ),
        };
        let reach = if trusted {
            format!(
                "{} Its outputs that come back through Columbus carry the session's data labels plus {}",
                columbus_shared::environments::TRUSTED_CARD,
                columbus_shared::environments::environment_label(
                    senv.as_ref().map_or("", |e| e.env.id.as_str())
                )
            )
        } else if network {
            "the script can reach the internet; can write only its own run folder".to_string()
        } else {
            "no network; can write only its own run folder".to_string()
        };
        // REQ-17, R-15: under policy scripts.session_approval, the engine
        // and governed level-A environments may be allowed for the session,
        // bound to this exact code and environment (as confirmed) in this
        // Columbus session; never a trusted or level-B one.
        let session_ok = policy.scripts.session_approval
            && senv.as_ref().is_none_or(ScriptEnv::session_allow_possible);
        let session_scope = [json!({"session_script": code_sha256,
            "environment": senv.as_ref().map(|e| json!({"id": e.env.id, "confirmed": e.env.confirmation_hash()}))})];
        let session_words = format!(
            "\"Allow this script for the session\" runs this exact code (sha256 {}){} again with any inputs, without asking, until this chat or AI app connection ends; any change to the code asks again",
            &digest[..12],
            senv.as_ref()
                .map(|e| format!(" in environment {}", e.env.id))
                .unwrap_or_else(|| " in the engine's Python".into())
        );
        let summary = format!(
            "Run a Python script ({lines} line{}, {} bytes, sha256 {}){where_runs} on {}; {reach}; stopped after {timeout} s.{} {}",
            if lines == 1 { "" } else { "s" },
            args.code.len(),
            &digest[..12],
            if inputs.is_empty() {
                "no inputs".to_string()
            } else {
                format!(
                    "input{} {}",
                    if inputs.len() == 1 { "" } else { "s" },
                    inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")
                )
            },
            if from.is_empty() {
                String::new()
            } else {
                format!(" Data in play came from: {}.", from.join(", "))
            },
            if session_ok {
                "You can allow this script for the session."
            } else {
                "Scripts are approved one run at a time."
            },
        );
        let bound_inputs: Vec<Value> = inputs
            .iter()
            .map(|i| json!({"name": i.name, "handle": i.handle, "sha256": i.sha256}))
            .collect();
        // Bound to the environment as confirmed, too.
        let bound = json!({"code_sha256": code_sha256, "inputs": bound_inputs, "timeout_seconds": timeout,
            "environment": senv.as_ref().map(|e| json!({"id": e.env.id, "confirmed": e.env.confirmation_hash()}))});
        let detail = json!({
            "code": args.code,
            "code_sha256": code_sha256,
            "lines": lines,
            "files": inputs.iter().map(|i| json!({"name": format!("{} ({})", i.name, i.handle), "rows": i.meta.rows})).collect::<Vec<_>>(),
            "labels": from,
            "arguments": {"timeout": format!("{timeout} s"),
                "network": if trusted { "anything you can reach (unconfined)" } else if network { "internet: the script can reach the internet" } else { "none" },
                "writes": if trusted { "anywhere you can (unconfined)" } else { "its own run folder only" },
                "environment": senv.as_ref().map_or_else(|| "the engine's Python".to_string(), |e| format!("{} (Python {}, {}, {})", e.env.id, e.env.probe.python_version, e.env.trust_line(), e.env.arcpy_line()))},
            "warning": trusted.then_some(columbus_shared::environments::TRUSTED_CARD),
        });
        let ask = Ask {
            tool: "run_python",
            label: "a Python script",
            request: &bound,
            risk: Risk::Script,
            summary,
            detail: Some(detail),
            approval_id: args.approval_id.as_deref(),
            // Every run, unless policy allows the session approval here
            // (`ScriptEnv::session_allow_possible`, or the engine); a
            // trusted run is always asked on its own.
            every_time: !session_ok,
            code: Some(&args.code),
            standing: session_ok.then_some(&session_scope[..]),
            always_covers: session_ok.then_some(session_words),
            script_session: session_ok,
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
                    result: json!({"status": "approval_required", "approval": needed, "code_sha256": code_sha256}),
                    checks: vec![],
                    provenance: None,
                }));
            }
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        }

        // One script at a time in a session.
        let _one = self.state.scripts.lock().await;

        // A fresh run folder, with copies of the inputs checked byte for
        // byte against what the person approved.
        let runs = match self.state.sandbox.resolve("runs") {
            Ok(p) => p,
            Err(e) => return Err(self.deny(&ctx, rec, e)),
        };
        let run = runs.join(format!("script-{prov}"));
        let made = std::fs::create_dir_all(&runs)
            .and_then(|_| std::fs::create_dir(&run))
            .and_then(|_| std::fs::create_dir(run.join("in")))
            .and_then(|_| std::fs::create_dir(run.join("out")))
            .and_then(|_| std::fs::create_dir(run.join("tmp")));
        if let Err(e) = made {
            return Err(self.deny(&ctx, rec, format!("could not make the run folder: {e}")));
        }
        let cleanup = |run: &Path| {
            let _ = std::fs::remove_dir_all(run);
        };
        let mut listed = serde_json::Map::new();
        let mut input_records = Vec::new();
        for i in &inputs {
            let rel = format!("in/{}.{}", i.name, safe_ext(&i.path));
            match crate::providers::copy_hashed(&i.path, &run.join(&rel)) {
                Ok(h) if h == i.sha256 => {}
                Ok(_) => {
                    cleanup(&run);
                    return Err(self.deny(
                        &ctx,
                        rec,
                        format!(
                            "input {}: {} changed after Columbus wrote it; nothing was run",
                            i.name, i.handle
                        ),
                    ));
                }
                Err(e) => {
                    cleanup(&run);
                    return Err(self.deny(
                        &ctx,
                        rec,
                        format!("could not copy input {}: {e}", i.name),
                    ));
                }
            }
            listed.insert(i.name.clone(), json!(rel));
            input_records.push(
                json!({"input": i.name, "handle": i.handle, "sha256": i.sha256,
                "labels": self.state.flow.file_labels(&i.path), "made_by": i.meta.made_by,
                "truncated": i.meta.truncated, "provenance": i.meta.provenance}),
            );
        }
        let written = crate::export_tools::write_new(
            &run.join("inputs.json"),
            serde_json::to_string_pretty(&Value::Object(listed))
                .unwrap_or_default()
                .as_bytes(),
        )
        .and_then(|_| crate::export_tools::write_new(&run.join("script.py"), args.code.as_bytes()));
        if let Err(e) = written {
            cleanup(&run);
            return Err(self.deny(&ctx, rec, format!("could not write the script: {e}")));
        }

        // The environment's own folders, or the engine's; nothing else.
        let (python, python_dirs, profile) = match &senv {
            Some(se) => (se.python(), se.read_only.clone(), se.profile()),
            None => (
                self.state.paths.python.clone(),
                crate::plugins::python_dirs(&self.state.paths.python),
                "script".to_string(),
            ),
        };
        let spec = columbus_confine::Spec {
            level: level.unwrap_or(columbus_confine::Level::A),
            name: profile,
            read_write: vec![run.clone()],
            read_only: python_dirs.clone(),
            memory_mb: Some(policy.plugins.memory_mb),
            processes: Some(PROCESSES),
        };

        // Recorded before it starts: a run that cannot be audited does not happen.
        if let Err(e) = self.audit(
            &ctx,
            AuditRecord {
                decision: "running".into(),
                prov: Some(prov.clone()),
                ..rec.clone()
            },
        ) {
            cleanup(&run);
            return Err(e);
        }
        let ran = match self
            .run_script(
                launcher.as_deref(),
                &spec,
                &run,
                Duration::from_secs(timeout),
                &python,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                cleanup(&run);
                return Err(self.deny(&ctx, rec, e));
            }
        };
        let stderr_text = ran.stderr.text();
        if !trusted
            && ran.exit_code == Some(columbus_confine::EXIT_CONFINE_FAILED)
            && stderr_text.starts_with("columbus-confine:")
        {
            cleanup(&run);
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "the script could not be started confined, so it did not run: {}",
                    stderr_text.trim()
                ),
            ));
        }

        // Its own checks.
        let mut checks: Vec<Check> = Vec::new();
        if ran.timed_out {
            checks.push(check(
                "script.timeout",
                Severity::Fail,
                format!("The script ran longer than {timeout} s and was stopped, with everything it started"),
                Some("Make it faster (work on fewer rows, simplify first), or allow more time within the organization's limit"),
            ));
        } else if ran.exit_code != Some(0) {
            checks.push(check(
                "script.exit_code",
                Severity::Fail,
                match ran.exit_code {
                    Some(c) => format!("The script exited with code {c}"),
                    None => "The script was ended by a signal (for example, out of memory)".into(),
                },
                Some("Read stderr, fix the code and run it again"),
            ));
        }
        for (which, c) in [("stdout", &ran.stdout), ("stderr", &ran.stderr)] {
            if c.truncated() {
                checks.push(check(
                    "script.stdout_truncated",
                    Severity::Info,
                    format!(
                        "{which} was {} bytes; the first {} KB are returned",
                        c.total,
                        STREAM_CAP / 1024
                    ),
                    Some("Write large results to out/ instead of printing them"),
                ));
            }
        }

        // Outputs: copied out of the run folder, without following links,
        // into results/, which nothing confined can write.
        let results = match self.state.sandbox.resolve("results") {
            Ok(p) => p,
            Err(e) => {
                cleanup(&run);
                return Err(self.deny(&ctx, rec, e));
            }
        };
        let (taken, skipped) = collect_outputs(
            self.state.sandbox.root(),
            &run.join("out"),
            &results,
            &format!("script-{prov}"),
        );
        cleanup(&run);
        if !skipped.is_empty() {
            checks.push(Check {
                id: "script.output_skipped".into(),
                severity: Severity::Warn,
                message: format!(
                    "{} item(s) in out/ were not taken: {}",
                    skipped.len(),
                    skipped.join("; ")
                ),
                detail: Some(json!({"skipped": skipped})),
                fix: Some("Write plain files directly in out/".into()),
            });
        }
        let base = self.state.outputs.inherit(&files);
        let run_fails = base.clone().with_step_fails(&checks, &prov);
        let mut outputs = Vec::new();
        let mut output_records = Vec::new();
        for t in &taken {
            let ext = safe_ext(&t.path);
            let mut own: Vec<Check> = Vec::new();
            let mut summary = None;
            if ext == "gpkg" || ext == "csv" {
                match self
                    .state
                    .worker
                    .core(
                        "verify_layer",
                        json!({"path": t.path}),
                        Duration::from_secs(120),
                    )
                    .await
                {
                    Ok(r) => {
                        for mut c in checks_from(r.get("checks")) {
                            c.message = format!("{}: {}", t.name, c.message);
                            own.push(c);
                        }
                        summary = r.get("summary").cloned();
                    }
                    Err(e) => own.push(check(
                        "script.output_unreadable",
                        Severity::Warn,
                        format!("{} could not be read as a layer: {e}", t.name),
                        Some("Write GeoPackage with a layer, or CSV with a header row"),
                    )),
                }
            }
            let inherit = run_fails.clone().with_step_fails(&own, &prov);
            let rows = summary
                .as_ref()
                .and_then(|s| s.get("rows"))
                .and_then(Value::as_u64);
            self.state.flow.record_file(&t.path, &labels);
            // Recorded with the hash of the bytes as they were copied, never
            // by reading the file again: anything changed since is refused.
            self.state.outputs.record_with_hash(
                &t.path,
                OutputMeta {
                    made_by: "run_python".into(),
                    provenance: Some(prov.clone()),
                    rows,
                    source: OutputSource::Script,
                    truncated: inherit.truncated,
                    inherited: inherit.fails,
                    sha256: None,
                },
                t.sha256.clone(),
            );
            let handle = self.ws_handle(&t.path);
            outputs.push(
                json!({"handle": handle, "name": t.name, "bytes": t.bytes, "summary": summary}),
            );
            output_records.push(json!({"handle": handle, "path": t.path, "sha256": t.sha256, "bytes": t.bytes, "rows": rows}));
            checks.extend(own);
        }
        if base.truncated && !taken.is_empty() {
            checks.push(check(
                "input.truncated",
                Severity::Warn,
                "An input was cut off at a row limit when it was made, so these outputs cover only part of the data".into(),
                Some("Narrow the original query so it is complete"),
            ));
        }
        if !labels.is_empty() && !taken.is_empty() {
            checks.push(check(
                "flow.labels",
                Severity::Info,
                format!("Outputs carry data labels: {}", rec.labels.join(", ")),
                None,
            ));
        }

        let done = !ran.timed_out && ran.exit_code == Some(0);
        let record = json!({
            "id": prov,
            "created": started.to_rfc3339(),
            "finished": Local::now().to_rfc3339(),
            "user": self.state.user,
            "client": client_name(&ctx),
            "inputs": input_records,
            "steps": [{"tool": "run_python", "engine": "python-script",
                "script": {"code": args.code, "code_sha256": code_sha256, "lines": lines},
                "timeout_seconds": timeout, "seconds": ran.seconds,
                "exit_code": ran.exit_code, "timed_out": ran.timed_out,
                "approval": rec.approval,
                "environment": senv.as_ref().map(ScriptEnv::provenance),
                "confinement": if trusted {
                    json!({"level": "none", "unconfined": true, "network": "as the person", "read_write": "as the person"})
                } else {
                    json!({"level": level.map_or("A", |l| l.as_str()), "network": if network { "internet" } else { "none" }, "read_write": ["<run folder>"],
                    "read_only": python_dirs, "memory_mb": policy.plugins.memory_mb, "processes": PROCESSES})
                },
                "stdout_bytes": ran.stdout.total, "stderr_bytes": ran.stderr.total}],
            "labels": labels,
            "checks": checks,
            "outputs": output_records,
            "server": format!("{SERVER_NAME} {}", env!("CARGO_PKG_VERSION")),
        });
        let _ = self.write_provenance(&prov, &record);
        rec.prov = Some(prov.clone());
        rec.rows = Some(taken.len() as u64);
        if !done {
            rec.decision = "error".into();
            rec.reason = Some(if ran.timed_out {
                format!("script stopped after {timeout} s")
            } else {
                format!("script exited with {:?}", ran.exit_code)
            });
        }
        self.audit(&ctx, rec)?;
        Ok(Json(Output {
            result: json!({
                "status": if done { "done" } else { "failed" },
                "exit_code": ran.exit_code,
                "stdout": ran.stdout.text(),
                "stderr": stderr_text,
                "outputs": outputs,
                "code_sha256": code_sha256,
            }),
            checks,
            provenance: Some(prov),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_names_and_extensions_stay_plain() {
        for ok in ["roads", "a_b-2", "X"] {
            assert!(input_name_ok(ok), "{ok}");
        }
        for bad in ["", "../x", "a/b", "a.b", "-x", "a b", &"a".repeat(41)] {
            assert!(!input_name_ok(bad), "{bad}");
        }
        assert_eq!(safe_ext(Path::new("/w/r.GPKG")), "gpkg");
        assert_eq!(safe_ext(Path::new("/w/r")), "dat");
        assert_eq!(safe_ext(Path::new("/w/r.a$b")), "dat");
    }

    #[test]
    fn the_environment_carries_nothing_of_the_gateways() {
        let run = Path::new("/ws/runs/script-p_1");
        let env = script_env(run);
        let keys: BTreeSet<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys
            .iter()
            .all(|k| !k.starts_with("COLUMBUS_") || *k == "COLUMBUS_RUN_DIR"));
        for k in [
            "TEMP",
            "TMP",
            "TMPDIR",
            "PROJ_NETWORK",
            "GDAL_SKIP",
            "COLUMBUS_RUN_DIR",
        ] {
            assert!(keys.contains(k), "{k}");
        }
        assert!(!keys.contains("HOME"));
        // On Windows the profile folder names are needed to start an
        // AppContainer process; nowhere else.
        assert_eq!(keys.contains("USERPROFILE"), cfg!(windows));
    }

    #[cfg(unix)]
    #[test]
    fn outputs_are_plain_files_only() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("a.csv"), "x\n1\n").unwrap();
        std::fs::write(d.path().join("secret.txt"), "s").unwrap();
        std::os::unix::fs::symlink(d.path().join("secret.txt"), out.join("link.csv")).unwrap();
        std::fs::hard_link(d.path().join("secret.txt"), out.join("hard.csv")).unwrap();
        std::fs::create_dir(out.join("sub")).unwrap();
        let dest = d.path().join("dest");
        let root = d.path();
        let (taken, skipped) = collect_outputs(root, &out, &dest, "script-p_1");
        assert_eq!(taken.len(), 1, "{skipped:?}");
        assert_eq!(taken[0].name, "a.csv");
        assert_eq!(std::fs::read_to_string(&taken[0].path).unwrap(), "x\n1\n");
        assert_eq!(taken[0].path, dest.join("script-p_1-a.csv"));
        assert_eq!(skipped.len(), 3, "{skipped:?}");
        assert!(!dest.join("script-p_1-link.csv").exists());
        assert!(!dest.join("script-p_1-hard.csv").exists());
        // A name already taken in results is never written through.
        let (taken, skipped) = collect_outputs(root, &out, &dest, "script-p_1");
        assert!(taken.is_empty() && skipped.len() == 4, "{skipped:?}");
        // out/ itself replaced by a link: nothing is taken through it.
        std::fs::rename(&out, d.path().join("real-out")).unwrap();
        std::os::unix::fs::symlink(d.path().join("real-out"), &out).unwrap();
        let (taken, skipped) = collect_outputs(root, &out, &d.path().join("dest2"), "script-p_2");
        assert!(taken.is_empty(), "{skipped:?}");
    }

    /// The runner reads the script's text: a coding line that would turn a
    /// comment into code when Python decodes the file does nothing.
    #[test]
    fn scripts_run_from_their_text() {
        let Some(py) = ["python3", "python"].into_iter().find(|p| {
            std::process::Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        }) else {
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("script.py");
        std::fs::write(
            &script,
            "# coding: utf-7\n# +AAo-print(\"evil\")\nimport sys\nprint(\"good\", __name__, sys.argv[0] == __file__)\n",
        )
        .unwrap();
        let out = std::process::Command::new(py)
            .args(["-I", "-B", "-X", "utf8", "-c", RUN_TEXT])
            .arg(&script)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            text.trim(),
            "good __main__ True",
            "{text} {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn code_must_read_as_it_runs() {
        use super::shown_as_run;
        assert!(shown_as_run("import os\nprint(1)\n\tpass").is_ok());
        for bad in [
            "# tidy\r__import__('os').system('x')",
            "x = 1 # \u{202E}gnirts",
            "# coding: utf-7\n# +AAo-print(1)\n",
            "\u{FEFF}print(1)",
            "a\u{200B}b = 1",
            "\u{1b}[2J",
            &"x".repeat(201),
        ] {
            assert!(shown_as_run(bad).is_err(), "{bad:?}");
        }
    }
}
