//! The worker protocol: [`Plugin`] (tool registry and stdin/stdout loop)
//! and [`Context`] (what a tool sees during one run).

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::extract::{Extract, ExtractError};
use crate::geometry::GeomError;

/// The worker protocol version this SDK speaks.
pub const PROTOCOL_VERSION: u32 = 2;

/// JSON-RPC error codes (docs/worker-protocol.md, "Errors").
pub mod codes {
    /// The line is not JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Valid JSON, but not a JSON-RPC request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// No such method.
    pub const UNKNOWN_METHOD: i64 = -32601;
    /// Missing or malformed parameters, unknown tool or input.
    pub const INVALID_PARAMS: i64 = -32602;
    /// The engine or feature is not available.
    pub const UNSUPPORTED: i64 = -32001;
    /// The tool failed; also "busy" when a run is already active.
    pub const ENGINE_ERROR: i64 = -32002;
    /// The run went past its `timeout_s`.
    pub const TIMEOUT: i64 = -32003;
    /// The run was cancelled.
    pub const CANCELLED: i64 = -32004;
}

/// After stdin closes: how long an active run may take to reply before it
/// is cancelled, and again after that (the gateway's grace period after
/// `cancel`).
const GRACE: Duration = Duration::from_secs(10);

/// Why a tool failed. Each variant becomes one protocol error code; the
/// message is shown to the agent, so keep it plain and free of data.
///
/// Errors from the SDK's own types convert with `?`: [`ExtractError`],
/// [`GeomError`] and [`std::io::Error`] become [`ToolError::Engine`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// A parameter, input or output is missing or has the wrong type
    /// (-32602).
    #[error("{0}")]
    InvalidParams(String),
    /// The engine or a feature the run needs is not available (-32001).
    #[error("{0}")]
    Unsupported(String),
    /// The tool failed (-32002).
    #[error("{0}")]
    Engine(String),
    /// The run went past its deadline (-32003). Returned by
    /// [`Context::progress`].
    #[error("the run took longer than its time limit")]
    Timeout,
    /// The run was cancelled (-32004). Returned by [`Context::progress`].
    #[error("cancelled")]
    Cancelled,
}

impl ToolError {
    /// The JSON-RPC error code this error is reported with.
    pub fn code(&self) -> i64 {
        match self {
            ToolError::InvalidParams(_) => codes::INVALID_PARAMS,
            ToolError::Unsupported(_) => codes::UNSUPPORTED,
            ToolError::Engine(_) => codes::ENGINE_ERROR,
            ToolError::Timeout => codes::TIMEOUT,
            ToolError::Cancelled => codes::CANCELLED,
        }
    }

    /// Shorthand for [`ToolError::Engine`].
    pub fn engine(message: impl Into<String>) -> Self {
        ToolError::Engine(message.into())
    }

    /// Shorthand for [`ToolError::InvalidParams`].
    pub fn invalid(message: impl Into<String>) -> Self {
        ToolError::InvalidParams(message.into())
    }
}

impl From<ExtractError> for ToolError {
    fn from(e: ExtractError) -> Self {
        ToolError::Engine(e.to_string())
    }
}

impl From<GeomError> for ToolError {
    fn from(e: GeomError) -> Self {
        ToolError::Engine(format!("geometry: {e}"))
    }
}

impl From<io::Error> for ToolError {
    fn from(e: io::Error) -> Self {
        ToolError::Engine(format!("I/O error: {e}"))
    }
}

/// A warning's severity (verification report format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Fail,
    Warn,
    Info,
}

/// One entry of a run's `warnings`. Created by [`Context::warn`]; add
/// `detail` and `fix` with the builder methods.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    pub id: String,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Warning {
    /// Attach structured detail (counts, ids, …; never raw data).
    pub fn detail(&mut self, detail: Value) -> &mut Self {
        self.detail = Some(detail);
        self
    }

    /// Say how to fix it.
    pub fn fix(&mut self, fix: impl Into<String>) -> &mut Self {
        self.fix = Some(fix.into());
        self
    }
}

/// The signature every tool handler has. The returned value is the run's
/// `summary` (`null` becomes `{}`).
type Handler = dyn Fn(&mut Context) -> Result<Value, ToolError> + Send + Sync;

struct Tool {
    name: String,
    params: Vec<String>,
    outputs: Vec<String>,
    handler: Arc<Handler>,
}

/// Serializes every stdout line: replies from the reader thread and
/// notifications and replies from the run thread never interleave.
struct Out(Mutex<Box<dyn Write + Send>>);

impl Out {
    fn send(&self, msg: &Value) {
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(e) = w.write_all(line.as_bytes()).and_then(|()| w.flush()) {
            eprintln!("columbus-plugin: could not write to stdout: {e}");
        }
    }

    fn reply(&self, id: &Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn error(&self, id: &Value, code: i64, message: &str) {
        self.send(
            &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        );
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

/// The run in progress, shared by the reader thread (which may cancel it)
/// and the run thread (which clears it when done).
struct Active {
    run: String,
    cancel: Arc<AtomicBool>,
}

type Slot = Arc<Mutex<Option<Active>>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An engine plugin: its identity and tools. Build it, then call
/// [`serve`](Plugin::serve) from `main`.
///
/// ```no_run
/// use columbus_plugin::{Context, Plugin, ToolError};
///
/// fn count(ctx: &mut Context) -> Result<serde_json::Value, ToolError> {
///     let ex = ctx.dataset("layer")?;
///     Ok(serde_json::json!({"rows": ex.row_count()}))
/// }
///
/// fn main() {
///     Plugin::new("contoso.counter", env!("CARGO_PKG_VERSION"))
///         .tool("count", &["layer"], &[], count)
///         .serve();
/// }
/// ```
pub struct Plugin {
    id: String,
    version: String,
    tools: Vec<Tool>,
}

impl Plugin {
    /// A plugin with the `id` and `version` of its `plugin.yaml`.
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Plugin {
            id: id.into(),
            version: version.into(),
            tools: Vec::new(),
        }
    }

    /// Register a tool: its name, parameter names and output names (these
    /// must match `plugin.yaml` exactly, or the gateway disables the
    /// plugin) and its handler.
    ///
    /// # Panics
    ///
    /// When a tool with this name is already registered.
    pub fn tool<F>(mut self, name: &str, params: &[&str], outputs: &[&str], handler: F) -> Self
    where
        F: Fn(&mut Context) -> Result<Value, ToolError> + Send + Sync + 'static,
    {
        assert!(
            self.tools.iter().all(|t| t.name != name),
            "tool {name:?} is registered twice"
        );
        self.tools.push(Tool {
            name: name.to_string(),
            params: params.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            handler: Arc::new(handler),
        });
        self
    }

    /// The `describe` result: protocol version, plugin identity, tools.
    pub fn describe(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "params": t.params, "outputs": t.outputs}))
            .collect();
        json!({
            "protocol": PROTOCOL_VERSION,
            "plugin": {"id": self.id, "version": self.version},
            "tools": tools,
        })
    }

    /// Speak the worker protocol on stdin/stdout until stdin closes, then
    /// return. Log with [`Context::log`] or `eprintln!`; never print to
    /// stdout, which carries the protocol.
    pub fn serve(self) {
        let stdin = io::stdin();
        if let Err(e) = self.serve_io(stdin.lock(), io::stdout()) {
            eprintln!("columbus-plugin: stopped reading stdin: {e}");
        }
    }

    /// [`serve`](Plugin::serve) over any reader and writer, for tests.
    ///
    /// Requests are read on the calling thread; each `run` executes on its
    /// own thread so `cancel` can arrive while it works. At end of input it
    /// gives an active run 10 seconds to reply, then cancels it and waits
    /// up to 10 seconds more, and returns.
    pub fn serve_io<R, W>(self, mut input: R, output: W) -> io::Result<()>
    where
        R: BufRead,
        W: Write + Send + 'static,
    {
        let out = Arc::new(Out(Mutex::new(Box::new(output))));
        let slot: Slot = Arc::new(Mutex::new(None));
        let mut worker: Option<JoinHandle<()>> = None;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if input.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            let line = match std::str::from_utf8(&buf) {
                Ok(s) => s.trim(),
                Err(_) => {
                    out.error(&Value::Null, codes::PARSE_ERROR, "parse error: not UTF-8");
                    continue;
                }
            };
            if line.is_empty() {
                continue;
            }
            if let Some(h) = self.handle(line, &out, &slot) {
                if let Some(old) = worker.replace(h) {
                    // The previous run has already cleared the slot.
                    let _ = old.join();
                }
            }
        }
        // End of input: nobody can send `cancel` any more. Give an active
        // run GRACE to reply, then cancel it and give it GRACE again.
        if let Some(h) = worker {
            let wait = |limit: Instant| {
                while !h.is_finished() && Instant::now() < limit {
                    thread::sleep(Duration::from_millis(20));
                }
            };
            wait(Instant::now() + GRACE);
            if !h.is_finished() {
                if let Some(a) = lock(&slot).as_ref() {
                    a.cancel.store(true, Ordering::SeqCst);
                }
                wait(Instant::now() + GRACE);
            }
            if h.is_finished() {
                let _ = h.join();
            } else {
                eprintln!("columbus-plugin: the active run did not stop; exiting without it");
            }
        }
        Ok(())
    }

    /// Handle one request line. Returns the thread of a run it started.
    fn handle(&self, line: &str, out: &Arc<Out>, slot: &Slot) -> Option<JoinHandle<()>> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                out.error(
                    &Value::Null,
                    codes::PARSE_ERROR,
                    &format!("parse error: {e}"),
                );
                return None;
            }
        };
        let Some(obj) = msg.as_object() else {
            out.error(
                &Value::Null,
                codes::INVALID_REQUEST,
                "invalid request: expected one JSON object",
            );
            return None;
        };
        // A message without `id` is a notification: act on it, never reply.
        let id = obj.get("id").cloned();
        let reply_id = id.clone().unwrap_or(Value::Null);
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            if id.is_some() {
                out.error(
                    &reply_id,
                    codes::INVALID_REQUEST,
                    "invalid request: no method",
                );
            }
            return None;
        };
        let empty = Map::new();
        let params = match obj.get("params") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(m)) => m,
            Some(_) => {
                if id.is_some() {
                    out.error(&reply_id, codes::INVALID_PARAMS, "params must be an object");
                }
                return None;
            }
        };
        match method {
            "describe" => {
                if id.is_some() {
                    out.reply(&reply_id, self.describe());
                }
                None
            }
            "cancel" => {
                let Some(run) = params.get("run").and_then(Value::as_str) else {
                    if id.is_some() {
                        out.error(
                            &reply_id,
                            codes::INVALID_PARAMS,
                            "cancel needs the run id in params.run",
                        );
                    }
                    return None;
                };
                if let Some(a) = lock(slot).as_ref().filter(|a| a.run == run) {
                    a.cancel.store(true, Ordering::SeqCst);
                }
                // Acknowledged at once, whether or not that run is active.
                if id.is_some() {
                    out.reply(&reply_id, json!({}));
                }
                None
            }
            "run" => {
                // A run nobody can receive the result of is not started.
                let id = id?;
                self.start_run(id, params, out, slot)
            }
            _ => {
                if id.is_some() {
                    out.error(
                        &reply_id,
                        codes::UNKNOWN_METHOD,
                        &format!("unknown method {method:?}"),
                    );
                }
                None
            }
        }
    }

    fn start_run(
        &self,
        id: Value,
        params: &Map<String, Value>,
        out: &Arc<Out>,
        slot: &Slot,
    ) -> Option<JoinHandle<()>> {
        let mut active = lock(slot);
        if active.is_some() {
            out.error(&id, codes::ENGINE_ERROR, "busy: a run is already active");
            return None;
        }
        let invalid = |m: &str| {
            out.error(&id, codes::INVALID_PARAMS, m);
            None
        };
        let Some(run) = params
            .get("run")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        else {
            return invalid("run needs a run id in params.run");
        };
        let Some(tool_name) = params.get("tool").and_then(Value::as_str) else {
            return invalid("run needs a tool name in params.tool");
        };
        let Some(tool) = self.tools.iter().find(|t| t.name == tool_name) else {
            return invalid(&format!("no tool named {tool_name:?}"));
        };
        let object = |key: &str| -> Result<Map<String, Value>, String> {
            match params.get(key) {
                None | Some(Value::Null) => Ok(Map::new()),
                Some(Value::Object(m)) => Ok(m.clone()),
                Some(_) => Err(format!("params.{key} must be an object")),
            }
        };
        let (p, inputs, outputs) = match (object("params"), object("inputs"), object("outputs")) {
            (Ok(p), Ok(i), Ok(o)) => (p, i, o),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return invalid(&e),
        };
        let deadline = params
            .get("timeout_s")
            .and_then(Value::as_f64)
            .filter(|t| t.is_finite() && *t > 0.0)
            .and_then(|t| Instant::now().checked_add(Duration::from_secs_f64(t.min(1e9))));

        let cancel = Arc::new(AtomicBool::new(false));
        *active = Some(Active {
            run: run.to_string(),
            cancel: Arc::clone(&cancel),
        });
        drop(active);

        let mut ctx = Context {
            run: run.to_string(),
            tool: tool_name.to_string(),
            params: p,
            inputs,
            outputs,
            workspace: params
                .get("workspace")
                .and_then(Value::as_str)
                .map(PathBuf::from),
            deadline,
            cancel,
            warnings: Vec::new(),
            counts: BTreeMap::new(),
            out: Arc::clone(out),
        };
        let handler = Arc::clone(&tool.handler);
        let (run_out, run_slot, run_id) = (Arc::clone(out), Arc::clone(slot), id.clone());
        let spawned = thread::Builder::new()
            .name(format!("columbus-run-{run}"))
            .spawn(move || {
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| handler(&mut ctx)))
                    .unwrap_or_else(|p| {
                        let what = p
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| p.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown panic".into());
                        Err(ToolError::Engine(format!("the tool panicked: {what}")))
                    });
                // Decide the outcome and free the slot together, so a cancel
                // either lands before (and wins) or finds no run.
                let cancelled = {
                    let mut a = lock(&run_slot);
                    *a = None;
                    ctx.cancel.load(Ordering::SeqCst)
                };
                match outcome {
                    _ if cancelled => run_out.error(&run_id, codes::CANCELLED, "cancelled"),
                    Ok(summary) => run_out.reply(&run_id, ctx.into_result(summary)),
                    Err(e) => run_out.error(&run_id, e.code(), &e.to_string()),
                }
            });
        match spawned {
            Ok(h) => Some(h),
            Err(e) => {
                lock(slot).take();
                out.error(
                    &id,
                    codes::ENGINE_ERROR,
                    &format!("could not start the run: {e}"),
                );
                None
            }
        }
    }
}

/// Everything a tool sees during one run: its parameters, inputs and output
/// paths, and the channel for progress, warnings and logs.
pub struct Context {
    run: String,
    tool: String,
    params: Map<String, Value>,
    inputs: Map<String, Value>,
    outputs: Map<String, Value>,
    workspace: Option<PathBuf>,
    deadline: Option<Instant>,
    cancel: Arc<AtomicBool>,
    warnings: Vec<Warning>,
    counts: BTreeMap<String, Map<String, Value>>,
    out: Arc<Out>,
}

impl Context {
    /// The run id the gateway assigned.
    pub fn run_id(&self) -> &str {
        &self.run
    }

    /// The tool being run.
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The session workspace folder, when the gateway sent one.
    pub fn workspace(&self) -> Option<&std::path::Path> {
        self.workspace.as_deref()
    }

    /// A raw scalar parameter, `None` when absent.
    pub fn param(&self, name: &str) -> Option<&Value> {
        self.params.get(name)
    }

    fn required(&self, name: &str) -> Result<&Value, ToolError> {
        match self.params.get(name) {
            None | Some(Value::Null) => {
                Err(ToolError::invalid(format!("missing parameter {name:?}")))
            }
            Some(v) => Ok(v),
        }
    }

    fn wrong(name: &str, expected: &str) -> ToolError {
        ToolError::invalid(format!("parameter {name:?} must be {expected}"))
    }

    /// A `number` parameter.
    pub fn param_f64(&self, name: &str) -> Result<f64, ToolError> {
        self.required(name)?
            .as_f64()
            .ok_or_else(|| Self::wrong(name, "a number"))
    }

    /// An `integer` parameter. A JSON number with no fractional part (such
    /// as `3.0`) is accepted.
    pub fn param_i64(&self, name: &str) -> Result<i64, ToolError> {
        let v = self.required(name)?;
        v.as_i64()
            .or_else(|| {
                v.as_f64()
                    .filter(|f| f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15)
                    .map(|f| f as i64)
            })
            .ok_or_else(|| Self::wrong(name, "an integer"))
    }

    /// A `string` parameter.
    pub fn param_str(&self, name: &str) -> Result<String, ToolError> {
        self.required(name)?
            .as_str()
            .map(String::from)
            .ok_or_else(|| Self::wrong(name, "a string"))
    }

    /// A `boolean` parameter.
    pub fn param_bool(&self, name: &str) -> Result<bool, ToolError> {
        self.required(name)?
            .as_bool()
            .ok_or_else(|| Self::wrong(name, "true or false"))
    }

    fn input(&self, name: &str, kind: &str) -> Result<PathBuf, ToolError> {
        let spec = self
            .inputs
            .get(name)
            .ok_or_else(|| ToolError::invalid(format!("no input named {name:?}")))?;
        if spec.get("kind").and_then(Value::as_str) != Some(kind) {
            return Err(ToolError::invalid(format!(
                "input {name:?} is not a {kind}"
            )));
        }
        spec.get("path")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| ToolError::invalid(format!("input {name:?} has no path")))
    }

    /// Open a `dataset` parameter's extract (read-only).
    pub fn dataset(&self, name: &str) -> Result<Extract, ToolError> {
        let path = self.input(name, "dataset")?;
        Ok(Extract::open(path)?)
    }

    /// The path of a `file` parameter (a workspace file).
    pub fn input_file(&self, name: &str) -> Result<PathBuf, ToolError> {
        self.input(name, "file")
    }

    /// Where to write an output. Write nowhere else: other paths are a
    /// protocol violation.
    pub fn output_path(&self, name: &str) -> Result<PathBuf, ToolError> {
        self.outputs
            .get(name)
            .and_then(|o| o.get("path"))
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| ToolError::invalid(format!("no output named {name:?}")))
    }

    /// Record a count for an output, reported as
    /// `result.outputs[name][key]`. The gateway reads `"rows"` for tables
    /// and `"features"` for layers.
    pub fn set_output_count(&mut self, name: &str, key: &str, n: u64) {
        self.counts
            .entry(name.to_string())
            .or_default()
            .insert(key.to_string(), Value::from(n));
    }

    /// Report progress (`percent` 0–100, or `None` when unknown).
    ///
    /// This is the cancellation point: it returns
    /// [`ToolError::Cancelled`] once the run was cancelled and
    /// [`ToolError::Timeout`] once it is past its `timeout_s`, so call it
    /// with `?` in every long loop.
    pub fn progress(
        &self,
        percent: Option<f64>,
        message: impl AsRef<str>,
    ) -> Result<(), ToolError> {
        self.check()?;
        let percent = percent
            .filter(|p| p.is_finite())
            .map(|p| p.clamp(0.0, 100.0));
        self.out.notify(
            "progress",
            json!({"run": self.run, "percent": percent, "message": message.as_ref()}),
        );
        Ok(())
    }

    /// `Err` when the run should stop (cancelled or out of time), without
    /// sending progress. For loops that report progress rarely.
    pub fn check(&self) -> Result<(), ToolError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(ToolError::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(ToolError::Timeout);
        }
        Ok(())
    }

    /// True once the gateway cancelled the run.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// The time left before `timeout_s`, when the gateway set one.
    pub fn time_left(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Add a warning to the run's result. Returns it so `detail` and `fix`
    /// can be added:
    ///
    /// ```no_run
    /// # fn f(ctx: &mut columbus_plugin::Context) {
    /// use columbus_plugin::Severity;
    /// ctx.warn("geometry.invalid", Severity::Warn, "3 geometries are invalid")
    ///     .detail(serde_json::json!({"count": 3}))
    ///     .fix("Repair the geometries in the source dataset.");
    /// # }
    /// ```
    pub fn warn(
        &mut self,
        id: &str,
        severity: Severity,
        message: impl Into<String>,
    ) -> &mut Warning {
        self.warnings.push(Warning {
            id: id.to_string(),
            severity,
            message: message.into(),
            detail: None,
            fix: None,
        });
        self.warnings.last_mut().expect("just pushed")
    }

    /// Send a diagnostic line to the gateway (level `info`). Never log data.
    pub fn log(&self, message: impl AsRef<str>) {
        self.log_at("info", message);
    }

    /// Send a diagnostic line with a level: `debug`, `info`, `warn` or
    /// `error`.
    pub fn log_at(&self, level: &str, message: impl AsRef<str>) {
        self.out
            .notify("log", json!({"level": level, "message": message.as_ref()}));
    }

    fn into_result(self, summary: Value) -> Value {
        let summary = if summary.is_null() {
            json!({})
        } else {
            summary
        };
        json!({"outputs": self.counts, "warnings": self.warnings, "summary": summary})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn plugin() -> Plugin {
        Plugin::new("columbus.test", "0.0.1")
            .tool("echo", &["n", "s", "b"], &["t"], |ctx| {
                let n = ctx.param_i64("n")?;
                let s = ctx.param_str("s")?;
                let b = ctx.param_bool("b")?;
                ctx.progress(Some(250.0), "clamped")?;
                ctx.log("hello");
                ctx.set_output_count("t", "rows", n as u64);
                ctx.warn("echo.note", Severity::Info, "noted")
                    .fix("none needed");
                Ok(json!({"n": n, "s": s, "b": b, "f": ctx.param_f64("n")?}))
            })
            .tool("nothing", &[], &[], |_| Ok(Value::Null))
            .tool("boom", &[], &[], |_| panic!("kaboom"))
            .tool("fail", &["x"], &[], |ctx| {
                ctx.param_f64("x")?;
                Err(ToolError::Unsupported("no GDAL here".into()))
            })
            .tool("late", &[], &[], |ctx| {
                thread::sleep(Duration::from_millis(30));
                ctx.check()?;
                Ok(Value::Null)
            })
    }

    /// Each line in its own session: a session waits for its run at end of
    /// input, so runs never overlap (overlap is tested end to end).
    fn exchange(lines: &[&str]) -> Vec<Value> {
        let cap = Capture::default();
        for line in lines {
            let input = format!("{line}\n");
            plugin().serve_io(input.as_bytes(), cap.clone()).unwrap();
        }
        let text = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn reply(msgs: &[Value], id: i64) -> &Value {
        msgs.iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("no reply {id}: {msgs:?}"))
    }

    #[test]
    fn describe_lists_tools_in_order() {
        let msgs = exchange(&[r#"{"jsonrpc":"2.0","id":1,"method":"describe","params":{}}"#]);
        let r = &reply(&msgs, 1)["result"];
        assert_eq!(r["protocol"], 2);
        assert_eq!(
            r["plugin"],
            json!({"id": "columbus.test", "version": "0.0.1"})
        );
        assert_eq!(
            r["tools"][0],
            json!({"name": "echo", "params": ["n", "s", "b"], "outputs": ["t"]})
        );
        assert_eq!(r["tools"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn run_reports_outputs_warnings_progress_and_logs() {
        let msgs = exchange(&[
            r#"{"jsonrpc":"2.0","id":"a","method":"run","params":{"run":"r1","tool":"echo","params":{"n":3,"s":"x","b":true},"outputs":{"t":{"type":"table","path":"t.csv"}},"timeout_s":60}}"#,
        ]);
        let r = msgs.iter().find(|m| m["id"] == "a").unwrap();
        assert_eq!(r["result"]["outputs"], json!({"t": {"rows": 3}}));
        assert_eq!(
            r["result"]["summary"],
            json!({"n": 3, "s": "x", "b": true, "f": 3.0})
        );
        assert_eq!(
            r["result"]["warnings"],
            json!([{"id": "echo.note", "severity": "info", "message": "noted", "fix": "none needed"}])
        );
        let progress = msgs.iter().find(|m| m["method"] == "progress").unwrap();
        assert_eq!(
            progress["params"],
            json!({"run": "r1", "percent": 100.0, "message": "clamped"})
        );
        assert!(progress.get("id").is_none());
        let log = msgs.iter().find(|m| m["method"] == "log").unwrap();
        assert_eq!(log["params"], json!({"level": "info", "message": "hello"}));
    }

    #[test]
    fn null_summary_becomes_empty_object() {
        let msgs = exchange(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"run","params":{"run":"r","tool":"nothing"}}"#,
        ]);
        assert_eq!(
            reply(&msgs, 1)["result"],
            json!({"outputs": {}, "warnings": [], "summary": {}})
        );
    }

    #[test]
    fn errors_map_to_codes() {
        let msgs = exchange(&[
            "not json",
            "[1,2]",
            r#"{"jsonrpc":"2.0","id":1,"method":"frobnicate"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"run","params":{"run":"r","tool":"nope"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"run","params":{"run":"r","tool":"echo","params":{"n":"three","s":"x","b":true}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"run","params":{"run":"r","tool":"fail","params":{"x":1}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"run","params":{"run":"r","tool":"fail"}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"run","params":{"run":"r","tool":"boom"}}"#,
            r#"{"jsonrpc":"2.0","id":7,"method":"run","params":{"tool":"echo"}}"#,
            r#"{"jsonrpc":"2.0","id":8,"method":"run","params":[1]}"#,
            r#"{"jsonrpc":"2.0","id":9,"method":"cancel","params":{"run":"not-active"}}"#,
            r#"{"jsonrpc":"2.0","id":10,"method":"run","params":{"run":"r","tool":"late","timeout_s":0.001}}"#,
            r#"{"jsonrpc":"2.0","method":"frobnicate"}"#,
        ]);
        let code = |id: i64| reply(&msgs, id)["error"]["code"].as_i64().unwrap();
        let nulls: Vec<i64> = msgs
            .iter()
            .filter(|m| m["id"].is_null())
            .map(|m| m["error"]["code"].as_i64().unwrap())
            .collect();
        assert_eq!(nulls, [codes::PARSE_ERROR, codes::INVALID_REQUEST]);
        assert_eq!(code(1), codes::UNKNOWN_METHOD);
        assert_eq!(code(2), codes::INVALID_PARAMS);
        assert_eq!(code(3), codes::INVALID_PARAMS);
        assert!(reply(&msgs, 3)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("\"n\""));
        assert_eq!(code(4), codes::UNSUPPORTED);
        assert_eq!(reply(&msgs, 4)["error"]["message"], "no GDAL here");
        assert_eq!(code(5), codes::INVALID_PARAMS);
        assert_eq!(code(6), codes::ENGINE_ERROR);
        assert!(reply(&msgs, 6)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("kaboom"));
        assert_eq!(code(7), codes::INVALID_PARAMS);
        assert_eq!(code(8), codes::INVALID_PARAMS);
        assert_eq!(reply(&msgs, 9)["result"], json!({}));
        assert_eq!(code(10), codes::TIMEOUT);
        // The notification got no reply: one message per request above.
        assert_eq!(msgs.len(), 12);
    }

    #[test]
    fn context_accessors() {
        let mut ctx = Context {
            run: "r".into(),
            tool: "t".into(),
            params: json!({"f": 1.5, "i": 2.0, "bad": 2.5})
                .as_object()
                .unwrap()
                .clone(),
            inputs: json!({
                "d": {"kind": "dataset", "path": "missing.sqlite", "rows": 1},
                "f": {"kind": "file", "path": "notes.txt"}
            })
            .as_object()
            .unwrap()
            .clone(),
            outputs: json!({"o": {"type": "layer", "path": "o.geojson"}})
                .as_object()
                .unwrap()
                .clone(),
            workspace: None,
            deadline: None,
            cancel: Arc::new(AtomicBool::new(false)),
            warnings: vec![],
            counts: BTreeMap::new(),
            out: Arc::new(Out(Mutex::new(Box::new(io::sink())))),
        };
        assert_eq!(ctx.param_f64("f"), Ok(1.5));
        assert_eq!(ctx.param_i64("i"), Ok(2));
        assert!(matches!(
            ctx.param_i64("bad"),
            Err(ToolError::InvalidParams(_))
        ));
        assert!(matches!(
            ctx.param_str("missing"),
            Err(ToolError::InvalidParams(_))
        ));
        assert_eq!(ctx.input_file("f"), Ok(PathBuf::from("notes.txt")));
        assert!(matches!(
            ctx.input_file("d"),
            Err(ToolError::InvalidParams(_))
        ));
        assert!(matches!(ctx.dataset("f"), Err(ToolError::InvalidParams(_))));
        assert!(matches!(ctx.dataset("d"), Err(ToolError::Engine(_))));
        assert_eq!(ctx.output_path("o"), Ok(PathBuf::from("o.geojson")));
        assert!(ctx.output_path("x").is_err());
        assert!(ctx.time_left().is_none());
        ctx.set_output_count("o", "features", 4);
        assert!(ctx.progress(None, "fine").is_ok());
        ctx.cancel.store(true, Ordering::SeqCst);
        assert!(ctx.is_cancelled());
        assert_eq!(ctx.progress(Some(1.0), "stop"), Err(ToolError::Cancelled));
        let r = ctx.into_result(json!({"k": 1}));
        assert_eq!(r["outputs"], json!({"o": {"features": 4}}));
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_tools_panic() {
        let _ = Plugin::new("a", "1")
            .tool("t", &[], &[], |_| Ok(Value::Null))
            .tool("t", &[], &[], |_| Ok(Value::Null));
    }
}
