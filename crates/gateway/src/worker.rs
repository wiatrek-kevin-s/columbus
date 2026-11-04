//! Engine worker supervision (design section 2, "Process model").
//!
//! Workers are long-lived child processes speaking line-delimited JSON-RPC
//! over stdio. They receive only already-checked requests. A crashed or
//! stuck worker is killed and restarted without taking the gateway down.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

pub struct Worker {
    python: PathBuf,
    script: PathBuf,
    proc: Mutex<Option<Running>>,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

#[derive(Debug)]
pub enum WorkerError {
    /// The worker could not start; the message says how to fix it.
    Unavailable(String),
    /// The worker answered with a JSON-RPC error.
    Engine {
        /// JSON-RPC error code; kept for the audit detail and future mapping.
        #[allow(dead_code)]
        code: i64,
        message: String,
    },
    Timeout(Duration),
    Crashed(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::Unavailable(m) => write!(f, "engine worker unavailable: {m}"),
            WorkerError::Engine { message, .. } => write!(f, "{message}"),
            WorkerError::Timeout(d) => write!(
                f,
                "engine worker took longer than {} s and was restarted",
                d.as_secs()
            ),
            WorkerError::Crashed(m) => write!(
                f,
                "engine worker stopped unexpectedly and was restarted: {m}"
            ),
        }
    }
}

impl Worker {
    pub fn new(python: PathBuf, script: PathBuf) -> Self {
        Worker {
            python,
            script,
            proc: Mutex::new(None),
        }
    }

    async fn start(&self) -> Result<Running, WorkerError> {
        if !self.script.exists() {
            return Err(WorkerError::Unavailable(format!(
                "worker script not found at {} (set COLUMBUS_WORKER)",
                self.script.display()
            )));
        }
        let mut cmd = Command::new(&self.python);
        // -I: isolated mode, ignores PYTHON* env vars and the user site dir.
        cmd.arg("-I")
            .arg("-u")
            .arg(&self.script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(dir) = self.script.parent() {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| {
            WorkerError::Unavailable(format!(
                "could not start {} ({e}); set COLUMBUS_PYTHON",
                self.python.display()
            ))
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        Ok(Running {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// Send one request and wait for its response, restarting the worker
    /// if it is not running. Requests are serialized per worker.
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, WorkerError> {
        let mut guard = self.proc.lock().await;
        if guard.is_none() {
            *guard = Some(self.start().await?);
        }
        let run = guard.as_mut().expect("just started");
        let id = run.next_id;
        run.next_id += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            .to_string()
            + "\n";

        let exchange = async {
            run.stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            run.stdin.flush().await.map_err(|e| e.to_string())?;
            loop {
                match run.stdout.next_line().await {
                    Ok(Some(l)) if l.trim().is_empty() => continue,
                    Ok(Some(l)) => return Ok(l),
                    Ok(None) => return Err("worker closed its output".to_string()),
                    Err(e) => return Err(e.to_string()),
                }
            }
        };

        let outcome = tokio::time::timeout(timeout, exchange).await;
        let reply = match outcome {
            Err(_) => {
                Self::kill(guard.take()).await;
                return Err(WorkerError::Timeout(timeout));
            }
            Ok(Err(e)) => {
                Self::kill(guard.take()).await;
                return Err(WorkerError::Crashed(e));
            }
            Ok(Ok(line)) => line,
        };

        let msg: Value = match serde_json::from_str(&reply) {
            Ok(v) => v,
            Err(e) => {
                Self::kill(guard.take()).await;
                return Err(WorkerError::Crashed(format!("unreadable reply: {e}")));
            }
        };
        if msg.get("id").and_then(Value::as_u64) != Some(id) {
            Self::kill(guard.take()).await;
            return Err(WorkerError::Crashed(
                "reply id did not match the request".into(),
            ));
        }
        if let Some(err) = msg.get("error") {
            return Err(WorkerError::Engine {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("engine error")
                    .to_string(),
            });
        }
        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn kill(run: Option<Running>) {
        if let Some(mut r) = run {
            let _ = r.child.kill().await;
        }
    }
}
