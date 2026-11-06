//! Engine worker supervision: worker protocol v2 (docs/worker-protocol.md).
//!
//! Workers are long-lived child processes speaking line-delimited JSON-RPC
//! over stdio. They receive only already-checked requests. Each `run` gets
//! a run id; `progress` and `log` notifications arrive while it works. On a
//! timeout the gateway sends `cancel` and waits up to 10 seconds before
//! killing the worker. A crashed or stuck worker is restarted on next use
//! without taking the gateway down.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// How long a worker has to stop after `cancel` before it is killed.
pub const CANCEL_GRACE: Duration = Duration::from_secs(10);

/// Environment variables a plugin worker may inherit (everything else,
/// including COLUMBUS_* and any secrets, is withheld).
const PLUGIN_ENV_ALLOW: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "LANG",
    "LC_ALL",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    // Windows starts an AppContainer process relative to the person's
    // profile and fails (ERROR_ENVVAR_NOT_FOUND) without these; they are
    // folder names, and the container cannot read those folders anyway.
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
];

/// How to start a worker.
#[derive(Debug, Clone)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// None: inherit the gateway's environment (Columbus's own engine).
    /// Some: start from an empty environment plus the allow list and these.
    pub env: Option<Vec<(String, OsString)>>,
    /// A plugin: its folder, entry file and approved hash, checked against
    /// the files on disk before every start and restart.
    pub contents: Option<(PathBuf, String, String)>,
}

impl Launch {
    /// Columbus's core engine: `python -I -u columbus_worker.py`.
    pub fn core(python: PathBuf, script: PathBuf) -> Self {
        let cwd = script.parent().map(PathBuf::from);
        Launch {
            program: python,
            args: vec!["-I".into(), "-u".into(), script.into_os_string()],
            cwd,
            env: None,
            contents: None,
        }
    }

    /// A plugin: scrubbed environment, the session workspace as its folder
    /// and temp folder.
    pub fn plugin(program: PathBuf, args: Vec<OsString>, workspace: PathBuf, tmp: PathBuf) -> Self {
        let mut env: Vec<(String, OsString)> = PLUGIN_ENV_ALLOW
            .iter()
            .filter_map(|k| std::env::var_os(k).map(|v| (k.to_string(), v)))
            .collect();
        for k in ["TEMP", "TMP", "TMPDIR"] {
            env.push((k.into(), tmp.clone().into_os_string()));
        }
        Launch {
            program,
            args,
            cwd: Some(workspace),
            env: Some(env),
            contents: None,
        }
    }
}

pub struct Worker {
    launch: Launch,
    proc: Mutex<Option<Running>>,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    /// A plugin started as the leader of its own process group (Unix):
    /// everything it started is ended with it. (Windows: the launcher's job
    /// object does this, and this stays None.)
    #[cfg_attr(not(unix), allow(dead_code))]
    group: Option<u32>,
}

/// Every engine process running now (its id, and whether it leads a
/// process group), so a run that must stop at once can end them.
static LIVE: std::sync::Mutex<Vec<(u32, bool)>> = std::sync::Mutex::new(Vec::new());

/// End every engine process this program started (and, for a process
/// group's leader, all it started). For a run stopping at its time limit,
/// which exits without the usual clean-up.
pub fn end_all() {
    let live = LIVE.lock().map(|l| l.clone()).unwrap_or_default();
    for (pid, group) in live {
        if pid <= 1 {
            continue;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(
                if group { -(pid as i32) } else { pid as i32 },
                libc::SIGKILL,
            );
        }
        #[cfg(windows)]
        {
            let _ = group;
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .creation_flags(0x0800_0000)
                .status();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let (Some(pid), Ok(mut l)) = (self.child.id(), LIVE.lock()) {
            l.retain(|(p, _)| *p != pid);
        }
        #[cfg(unix)]
        if let Some(pid) = self.group {
            if pid > 1 {
                // The worker and anything it forked, which kill_on_drop
                // alone would leave running.
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            }
        }
    }
}

#[derive(Debug)]
pub enum WorkerError {
    /// The worker could not start; the message says how to fix it.
    Unavailable(String),
    /// The worker answered with a JSON-RPC error.
    Engine {
        /// JSON-RPC error code (docs/worker-protocol.md); kept for diagnostics.
        #[allow(dead_code)]
        code: i64,
        message: String,
    },
    Timeout(Duration),
    Cancelled,
    Crashed(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::Unavailable(m) => write!(f, "engine worker unavailable: {m}"),
            WorkerError::Engine { message, .. } => write!(f, "{message}"),
            WorkerError::Timeout(d) => write!(
                f,
                "the job took longer than {} s and was stopped",
                d.as_secs()
            ),
            WorkerError::Cancelled => write!(f, "the job was cancelled"),
            WorkerError::Crashed(m) => write!(
                f,
                "engine worker stopped unexpectedly and was restarted: {m}"
            ),
        }
    }
}

/// A progress notification from a running job.
#[derive(Debug, Clone)]
pub struct Progress {
    pub percent: Option<f64>,
    pub message: String,
}

fn error_of(msg: &Value) -> Option<WorkerError> {
    let err = msg.get("error")?;
    let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code == -32004 {
        return Some(WorkerError::Cancelled);
    }
    Some(WorkerError::Engine {
        code,
        message: err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("engine error")
            .to_string(),
    })
}

impl Worker {
    pub fn new(launch: Launch) -> Self {
        Worker {
            launch,
            proc: Mutex::new(None),
        }
    }

    async fn start(&self) -> Result<Running, WorkerError> {
        let l = &self.launch;
        if let Some((dir, entry, want)) = &l.contents {
            let now = std::fs::read_to_string(dir.join("plugin.yaml"))
                .map_err(|e| e.to_string())
                .and_then(|m| crate::plugins::plugin_hash(dir, &m, entry));
            if now.as_deref() != Ok(want.as_str()) {
                return Err(WorkerError::Unavailable(
                    "its files changed since they were approved, so it was not started; it stays off until a person approves it again".into(),
                ));
            }
        }
        if !l.program.exists() && l.program.components().count() > 1 {
            return Err(WorkerError::Unavailable(format!(
                "{} not found",
                l.program.display()
            )));
        }
        // The core engine's script is a path; a plugin's entry is checked by the host.
        for a in l.args.iter().filter(|_| l.env.is_none()) {
            let p = PathBuf::from(a);
            if p.extension().is_some_and(|e| e == "py") && !p.exists() {
                return Err(WorkerError::Unavailable(format!(
                    "worker script not found at {} (set COLUMBUS_WORKER)",
                    p.display()
                )));
            }
        }
        let mut cmd = Command::new(&l.program);
        cmd.args(&l.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(cwd) = &l.cwd {
            cmd.current_dir(cwd);
        }
        // A plugin leads a process group of its own, so stopping it stops
        // everything it started (on Windows the launcher's job object does).
        #[cfg(unix)]
        if l.env.is_some() {
            cmd.process_group(0);
        }
        if let Some(env) = &l.env {
            cmd.env_clear();
            for (k, v) in env {
                cmd.env(k, v);
            }
        }
        let mut child = cmd.spawn().map_err(|e| {
            WorkerError::Unavailable(format!(
                "could not start {} ({e}); check COLUMBUS_PYTHON",
                l.program.display()
            ))
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        let group = if cfg!(unix) && l.env.is_some() {
            child.id()
        } else {
            None
        };
        if let (Some(pid), Ok(mut live)) = (child.id(), LIVE.lock()) {
            live.push((pid, group.is_some()));
        }
        Ok(Running {
            child,
            stdin,
            stdout,
            next_id: 1,
            group,
        })
    }

    async fn send(run: &mut Running, method: &str, params: Value) -> Result<u64, String> {
        let id = run.next_id;
        run.next_id += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            .to_string()
            + "\n";
        run.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        run.stdin.flush().await.map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// Read until the reply to `id`, passing notifications to `on_note`.
    async fn reply_to(
        run: &mut Running,
        id: u64,
        on_note: &mut (dyn FnMut(&Value) + Send),
    ) -> Result<Value, String> {
        loop {
            let line = match run.stdout.next_line().await {
                Ok(Some(l)) if l.trim().is_empty() => continue,
                Ok(Some(l)) => l,
                Ok(None) => return Err("worker closed its output".into()),
                Err(e) => return Err(e.to_string()),
            };
            let msg: Value =
                serde_json::from_str(&line).map_err(|e| format!("unreadable message: {e}"))?;
            match msg.get("id") {
                Some(v) if v.as_u64() == Some(id) => return Ok(msg),
                Some(Value::Null) | None if msg.get("method").is_some() => on_note(&msg),
                // A reply to an earlier request (a cancel ack): skip it.
                _ => {}
            }
        }
    }

    async fn ensure(&self) -> Result<tokio::sync::MutexGuard<'_, Option<Running>>, WorkerError> {
        let mut guard = self.proc.lock().await;
        if guard.is_none() {
            *guard = Some(self.start().await?);
        }
        Ok(guard)
    }

    async fn kill(run: Option<Running>) {
        if let Some(mut r) = run {
            // Its whole group first, while the leader still holds the id.
            #[cfg(unix)]
            if let Some(pid) = r.group.filter(|p| *p > 1) {
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            }
            let _ = r.child.kill().await;
        }
    }

    /// `describe`: the plugin's identity and tools.
    pub async fn describe(&self, timeout: Duration) -> Result<Value, WorkerError> {
        let mut guard = self.ensure().await?;
        let run = guard.as_mut().expect("started");
        let exchange = async {
            let id = Self::send(run, "describe", json!({})).await?;
            Self::reply_to(run, id, &mut |_| {}).await
        };
        match tokio::time::timeout(timeout, exchange).await {
            Err(_) => {
                Self::kill(guard.take()).await;
                Err(WorkerError::Timeout(timeout))
            }
            Ok(Err(e)) => {
                Self::kill(guard.take()).await;
                Err(WorkerError::Crashed(e))
            }
            Ok(Ok(msg)) => match error_of(&msg) {
                Some(e) => Err(e),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            },
        }
    }

    /// `run` one tool. `params` holds the run request (`run`, `tool`,
    /// `params`, `inputs`, `outputs`, …). On timeout the worker is sent
    /// `cancel` and given [`CANCEL_GRACE`] to stop before it is killed.
    pub async fn run(
        &self,
        params: Value,
        timeout: Duration,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<Value, WorkerError> {
        let run_id = params.get("run").cloned().unwrap_or(Value::Null);
        let mut guard = self.ensure().await?;
        let run = guard.as_mut().expect("started");
        let mut on_note = |msg: &Value| {
            if msg.get("method").and_then(Value::as_str) == Some("progress") {
                let p = msg.get("params").cloned().unwrap_or(Value::Null);
                on_progress(Progress {
                    percent: p.get("percent").and_then(Value::as_f64),
                    message: p
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
        };
        let id = match Self::send(run, "run", params).await {
            Ok(id) => id,
            Err(e) => {
                Self::kill(guard.take()).await;
                return Err(WorkerError::Crashed(e));
            }
        };
        let first = tokio::time::timeout(timeout, Self::reply_to(run, id, &mut on_note)).await;
        let msg = match first {
            Ok(Ok(msg)) => msg,
            Ok(Err(e)) => {
                Self::kill(guard.take()).await;
                return Err(WorkerError::Crashed(e));
            }
            Err(_) => {
                // Ask it to stop; kill it if it does not within the grace period.
                let stopped = async {
                    Self::send(run, "cancel", json!({"run": run_id})).await?;
                    Self::reply_to(run, id, &mut on_note).await
                };
                match tokio::time::timeout(CANCEL_GRACE, stopped).await {
                    Ok(Ok(_)) => {}
                    _ => Self::kill(guard.take()).await,
                }
                return Err(WorkerError::Timeout(timeout));
            }
        };
        match error_of(&msg) {
            Some(e) => Err(e),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    /// Run one of Columbus's core engine tools and return its summary.
    pub async fn core(
        &self,
        tool: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, WorkerError> {
        let req = json!({"run": format!("c_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]), "tool": tool, "params": params});
        let r = self.run(req, timeout, &mut |_| {}).await?;
        Ok(r.get("summary").cloned().unwrap_or(Value::Null))
    }
}
