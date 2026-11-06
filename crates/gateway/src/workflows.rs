//! Workflows on the server's side: `propose_workflow`, which files a
//! workflow for the person to approve in the Columbus app, and the headless
//! runner, `columbus-mcp workflow run <id>`, which runs an approved one on
//! a schedule without a person waiting.
//!
//! The runner is an ordinary Columbus session, with three differences:
//! - it serves one client, itself, in process (named `columbus-runner` in
//!   the audit log), which calls only the workflow's own steps;
//! - its project scope is the workflow's: only the connections it lists;
//! - it never waits for a person: any call that would need an approval is
//!   refused at once with "needs you", and the run stops there.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::Local;
use columbus_shared::policy::ProjectScope;
use columbus_shared::workflows::{self as wf, Workflow};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, Implementation, InitializeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, ClientHandler, RoleServer, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::{client_name, Gateway, Output, State};

/// What a refused approval says in a run: the step needs the person.
pub(crate) const NEEDS_YOU: &str = "needs you:";

/// The name the runner's calls carry in the audit log.
pub const RUNNER: &str = "columbus-runner";

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeWorkflowArgs {
    /// The whole workflow.yaml, as text. Read columbus_guide
    /// {"name": "designing-workflows"} first: its format, the tools a step
    /// may be, references between steps, the schedule and outputs.
    pub workflow: String,
}

#[tool_router(router = workflow_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        name = "propose_workflow",
        description = "Save a repeated job as one of the person's workflows, so it happens by itself on a schedule (or when they start it) without anyone waiting: for example, check a dataset and export the findings every weekday at 06:00. Give the whole workflow.yaml as text; which tools a step may be, references between steps, the scope, the schedule and outputs are in columbus_guide {\"name\": \"designing-workflows\"}, which you must read first. Columbus checks it and files it for the person, who reads it and approves it in the Columbus app (Workflows) with Touch ID or Windows Hello; nothing happens before that, scripts and edits are never steps, and it never waits for an approval. Needs workflows_allowed in whoami.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn propose_workflow(
        &self,
        Parameters(args): Parameters<ProposeWorkflowArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let text = args.workflow.replace("\r\n", "\n");
        let sha = columbus_shared::canon::sha256_hex(text.as_bytes());
        let request = json!({"workflow_sha256": sha});
        let rec = self.gate(&ctx, "propose_workflow", &request)?;
        let settings = self.state.settings();
        let policy = &settings.policy;
        if !policy.allow_workflows {
            return Err(self.deny(
                &ctx,
                rec,
                "organization policy does not allow workflows (allow_workflows is off), so a workflow cannot be proposed here; the person can turn them on in the Columbus app if their organization lets them",
            ));
        }
        let w = match wf::check(&text) {
            Ok(w) => w,
            Err(e) => {
                return Err(self.deny(
                    &ctx,
                    rec,
                    format!("the workflow was not proposed; fix this and call again: {e}"),
                ))
            }
        };
        let mut problems = Vec::new();
        for s in &w.steps {
            let tool = if wf::personal_tool_id(&s.tool).is_some() {
                "run_tool"
            } else {
                s.tool.as_str()
            };
            if !policy.tool_allowed(tool) {
                problems.push(format!(
                    "step {:?}: organization policy does not allow {tool}",
                    s.id
                ));
            }
        }
        if !w.personal_tools().is_empty() && !policy.allow_personal_tools {
            problems.push(
                "it uses personal tools, and organization policy turns them off (allow_personal_tools)"
                    .into(),
            );
        }
        if let Err(e) = wf::tool_hashes(&w, self.state.tools.root()) {
            problems.push(format!(
                "{e}; install it first (propose_tool, then the person installs it in the Columbus app)"
            ));
        }
        for c in &w.scope.connections {
            if !settings.connections.iter().any(|x| &x.id == c) {
                problems.push(format!(
                    "scope.connections names {c:?}, which is not one of the person's connections (list_connections)"
                ));
            }
        }
        if let Some(f) = &w.outputs.folder {
            if !policy.exports.iter().any(|x| x.id == f.export) {
                problems.push(format!(
                    "outputs.folder export {:?} is not one of the policy's export folders (whoami lists them)",
                    f.export
                ));
            }
        }
        if !problems.is_empty() {
            return Err(self.deny(
                &ctx,
                rec,
                format!(
                    "the workflow was not proposed; fix these and call again:\n- {}",
                    problems.join("\n- ")
                ),
            ));
        }
        let root = wf::workflows_dir();
        let replaces = wf::list_installed(&root, self.state.tools.root())
            .into_iter()
            .find(|x| x.id == w.id)
            .map(|x| json!({"name": x.name, "fingerprint": x.fingerprint}));
        let info = json!({
            "id": w.id,
            "name": w.name,
            "proposed": Local::now().to_rfc3339(),
            "session": self.state.session,
            "user": self.state.user,
            "client": client_name(&ctx),
            "replaces": replaces,
        });
        if let Err(e) = wf::write_proposal(&root, &text, &info) {
            return Err(self.deny(&ctx, rec, format!("the workflow could not be filed: {e}")));
        }
        self.audit(
            &ctx,
            columbus_shared::audit::AuditRecord {
                decision: "allow".into(),
                ..rec
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Json(Output {
            result: json!({
                "status": "waiting_for_person",
                "id": w.id,
                "plain": wf::describe(&w),
                "next": "Tell the person the workflow is waiting in the Columbus app under Workflows, where they read it and approve it. Nothing runs before that. After approving, they can run it once with Run now and read its report.",
            }),
            checks: vec![],
            provenance: None,
        }))
    }
}

/// The runner's client: itself, in process.
struct RunnerClient;

impl ClientHandler for RunnerClient {
    fn get_info(&self) -> InitializeRequestParams {
        InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new(RUNNER, env!("CARGO_PKG_VERSION")),
        )
    }
}

/// How a run ended, as `run.json`'s `status`.
fn exit_code(status: &str) -> i32 {
    match status {
        "succeeded" | "succeeded_with_warnings" => 0,
        "needs_you" => 2,
        "needs_reapproval" | "off" => 3,
        _ => 1,
    }
}

/// `columbus-mcp workflow run <id>`: run an approved workflow once, write
/// its record and report, and return the exit code (0 done, 1 failed,
/// 2 needs the person, 3 not approved or off).
pub async fn run_cli(id: &str, scheduled: bool) -> anyhow::Result<i32> {
    run_cli_with(id, scheduled, None).await
}

/// As [`run_cli`], with the engine's Python named (as `COLUMBUS_PYTHON`
/// would): a full path to a file.
pub async fn run_cli_with(id: &str, scheduled: bool, python: Option<&str>) -> anyhow::Result<i32> {
    let root = wf::workflows_dir();
    let mut paths = crate::config::Paths::from_env();
    if let Some(p) = python {
        let p = Path::new(p);
        if !p.is_absolute() || !p.is_file() {
            anyhow::bail!("--python names the engine's Python by its full path");
        }
        paths.python = p.to_path_buf();
    }
    let tools_root = paths.tools.clone();
    let ws_root = paths.workspaces.clone();
    let prune = |keep: u32| wf::prune_runs(&root, id, keep as usize, Some(&ws_root));
    let started = Local::now();
    let run = wf::new_run_id();
    let run_dir = wf::runs_dir(&root, id).join(&run);
    let mut record = json!({
        "workflow": id,
        "run": run,
        "started": started.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        "scheduled": scheduled,
        "status": "running",
        "steps": [],
        "deliveries": [],
    });
    let finish = |record: &mut Value, status: &str, why: Option<String>| -> i32 {
        record["status"] = json!(status);
        record["finished"] =
            json!(Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
        if let Some(w) = why {
            record["reason"] = json!(w);
        }
        exit_code(status)
    };

    if !wf::id_ok(id) {
        anyhow::bail!("{id:?} is not a workflow id");
    }
    // Checked first: an unapproved or changed workflow does nothing else.
    let ready = wf::ready(&root, &tools_root, id);
    std::fs::create_dir_all(&run_dir)?;
    let (workflow, fingerprint) = match ready {
        Ok(x) => x,
        Err(e) => {
            let code = finish(&mut record, "needs_reapproval", Some(e));
            write_run(&run_dir, &record, None)?;
            prune(wf::DEFAULT_KEEP_RUNS);
            return Ok(code);
        }
    };
    // A schedule entry left behind for a workflow that no longer has one
    // runs nothing.
    if scheduled && workflow.schedule.is_none() {
        let code = finish(
            &mut record,
            "skipped",
            Some(
                "it has no schedule now (a schedule entry was left behind); run it from the app"
                    .into(),
            ),
        );
        write_run(&run_dir, &record, Some(&workflow))?;
        prune(workflow.keep_runs());
        return Ok(code);
    }
    // The personal tools it uses, as approved: checked again before each
    // of their steps, since a tool may be reinstalled while a run goes on.
    let tools_now = wf::tool_hashes(&workflow, &tools_root).unwrap_or_default();
    record["fingerprint"] = json!(fingerprint);
    record["name"] = json!(workflow.name);
    let _lock = match wf::lock(&root, id) {
        Ok(l) => l,
        Err(e) => {
            let code = finish(&mut record, "skipped", Some(e));
            write_run(&run_dir, &record, Some(&workflow))?;
            prune(workflow.keep_runs());
            return Ok(code);
        }
    };
    write_run(&run_dir, &record, Some(&workflow))?;

    let mut state = match State::load(paths) {
        Ok(s) => s,
        Err(e) => {
            let code = finish(
                &mut record,
                "failed",
                Some(format!("Columbus could not start: {e}")),
            );
            write_run(&run_dir, &record, Some(&workflow))?;
            return Ok(code);
        }
    };
    if !state.policy().allow_workflows {
        let code = finish(
            &mut record,
            "off",
            Some("organization policy turns workflows off (allow_workflows)".into()),
        );
        write_run(&run_dir, &record, Some(&workflow))?;
        prune(workflow.keep_runs());
        return Ok(code);
    }
    state.unattended = true;
    // Only the connections the workflow lists (none: a name no connection has).
    state.scope = ProjectScope {
        connections: if workflow.scope.connections.is_empty() {
            vec!["\u{0}none".into()]
        } else {
            workflow.scope.connections.clone()
        },
        data_groups: vec![],
    };
    record["session"] = json!(state.session);
    record["workspace"] = json!(state.sandbox.root().display().to_string());
    let audit = state.audit.clone();
    let who = (state.user.clone(), state.session.clone());
    let return_rows = state.policy().return_rows.unwrap_or(500).clamp(1, 500);

    let deadline = Instant::now() + Duration::from_secs(u64::from(workflow.run_minutes()) * 60);
    let gateway = Gateway::new(state);
    let (server_io, client_io) = tokio::io::duplex(8 << 20);
    let server = tokio::spawn(async move {
        if let Ok(s) = gateway.serve(server_io).await {
            let _ = s.waiting().await;
        }
    });
    let client = RunnerClient.serve(client_io).await?;

    let status = run_steps(
        &client,
        &workflow,
        &mut record,
        deadline,
        &tools_root,
        &tools_now,
    )
    .await;
    let status = match status {
        Ok(s) if s == "succeeded" || s == "succeeded_with_warnings" => {
            // The AI review, if any: a review that cannot run leaves the
            // run's results as they are, marked.
            let s = match &workflow.review {
                Some(r) => {
                    let ok =
                        review(&client, r, &mut record, &audit, &who, return_rows, deadline).await;
                    if ok {
                        s
                    } else {
                        s.max_with("succeeded_with_warnings".into())
                    }
                }
                None => s,
            };
            deliver(&client, &workflow, &run, &mut record, deadline)
                .await
                .unwrap_or_else(|s| s)
                .max_with(s)
        }
        Ok(s) => s,
        Err(s) => s,
    };
    let _ = client.cancel().await;
    server.abort();
    let code = finish(&mut record, &status, None);
    write_run(&run_dir, &record, Some(&workflow))?;
    prune(workflow.keep_runs());
    eprintln!("{id}: {status} (run {run})");
    Ok(code)
}

trait MaxWith {
    fn max_with(self, other: String) -> String;
}

impl MaxWith for String {
    /// The worse of two endings.
    fn max_with(self, other: String) -> String {
        let rank = |s: &str| match s {
            "succeeded" => 0,
            "succeeded_with_warnings" => 1,
            "needs_you" => 3,
            _ => 2,
        };
        if rank(&self) >= rank(&other) {
            self
        } else {
            other
        }
    }
}

/// A step's arguments with its references filled in from earlier results.
fn resolve(v: &Value, results: &BTreeMap<String, Value>) -> Result<Value, String> {
    Ok(match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| Ok((k.clone(), resolve(x, results)?)))
                .collect::<Result<_, String>>()?,
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| resolve(x, results))
                .collect::<Result<_, String>>()?,
        ),
        Value::String(s) => match wf::reference(s) {
            Some((step, path)) => {
                let r = results
                    .get(step)
                    .ok_or_else(|| format!("{s}: step {step} has no result"))?;
                if path.is_empty() {
                    json!(main_handle(r)
                        .ok_or_else(|| format!("{s}: step {step} made no layer or file"))?)
                } else {
                    let mut cur = r;
                    for p in &path {
                        cur = match cur {
                            Value::Array(a) => p.parse::<usize>().ok().and_then(|i| a.get(i)),
                            _ => cur.get(*p),
                        }
                        .ok_or_else(|| format!("{s}: step {step}'s result has no {p}"))?;
                    }
                    // A number, a yes/no or a workspace handle: never text
                    // from the data, which could steer what a step does.
                    match cur {
                        Value::Number(_) | Value::Bool(_) | Value::Null => cur.clone(),
                        Value::String(h) if h.starts_with("ws://") && !h.chars().any(char::is_whitespace) => cur.clone(),
                        _ => {
                            return Err(format!(
                                "{s}: a reference to a result field may be a number, true/false or a layer handle, not text or a list"
                            ))
                        }
                    }
                }
            }
            None => v.clone(),
        },
        _ => v.clone(),
    })
}

/// The layer or file a step made: its handle.
fn main_handle(result: &Value) -> Option<String> {
    for p in [
        "/layer/handle",
        "/handle",
        "/output/handle",
        "/saved/0/handle",
    ] {
        if let Some(h) = result.pointer(p).and_then(Value::as_str) {
            return Some(h.to_string());
        }
    }
    None
}

type Client = rmcp::service::RunningService<rmcp::RoleClient, RunnerClient>;

/// One call: (ok, structured output or error text).
async fn call(client: &Client, tool: &str, args: Value, left: Duration) -> (bool, Value, String) {
    let mut p = CallToolRequestParams::new(tool.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    match tokio::time::timeout(left, client.call_tool(p)).await {
        Err(_) => (false, Value::Null, "stopped at the run's time limit".into()),
        Ok(Err(e)) => (false, Value::Null, e.to_string()),
        Ok(Ok(r)) => {
            let text = r
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect::<Vec<_>>()
                .join("\n");
            if r.is_error == Some(true) {
                (false, Value::Null, text)
            } else {
                let v = r
                    .structured_content
                    .or_else(|| serde_json::from_str(&text).ok())
                    .unwrap_or(Value::Null);
                (true, v, text)
            }
        }
    }
}

fn checks_of(out: &Value) -> (usize, usize, Vec<Value>) {
    let checks: Vec<Value> = out["checks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|c| json!({"id": c["id"], "severity": c["severity"], "message": c["message"]}))
        .collect();
    let n = |s: &str| checks.iter().filter(|c| c["severity"] == s).count();
    (n("fail"), n("warn"), checks)
}

/// A short account of a result, for the report: rows, CRS, layer.
fn summary_of(out: &Value) -> String {
    let r = &out["result"];
    let mut parts = vec![];
    for (k, label) in [
        ("rows", "rows"),
        ("count", "features"),
        ("returned", "returned"),
    ] {
        if let Some(n) = r.get(k).and_then(Value::as_u64) {
            parts.push(format!("{n} {label}"));
        }
    }
    for p in ["/layer/rows", "/layer/summary/rows"] {
        if let Some(n) = r.pointer(p).and_then(Value::as_u64) {
            parts.push(format!("{n} rows"));
        }
    }
    if let Some(c) = r
        .pointer("/layer/crs")
        .or_else(|| r.pointer("/layer/summary/crs"))
        .or_else(|| r.get("crs"))
        .and_then(Value::as_str)
    {
        parts.push(c.to_string());
    }
    if let Some(s) = r.get("status").and_then(Value::as_str) {
        parts.push(s.to_string());
    }
    parts.dedup();
    parts.join(", ")
}

/// Run the steps in order; Ok(status) when every step ran, Err(status)
/// when the run stopped.
async fn run_steps(
    client: &Client,
    workflow: &Workflow,
    record: &mut Value,
    deadline: Instant,
    tools_root: &Path,
    tools_approved: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut results: BTreeMap<String, Value> = BTreeMap::new();
    let mut warned = false;
    for s in &workflow.steps {
        let started = Local::now();
        let mut step = json!({"id": s.id, "tool": s.tool, "started": started.to_rfc3339()});
        let push = |record: &mut Value, step: Value| {
            if let Some(a) = record["steps"].as_array_mut() {
                a.push(step);
            }
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            step["status"] = json!("stopped");
            step["error"] = json!("the run's time limit was reached");
            push(record, step);
            return Err("stopped_at_limit".into());
        }
        // Checked again: only the steps a workflow may have.
        let (tool, args) = match wf::personal_tool_id(&s.tool) {
            Some(tid) => {
                let now = wf::tool_hashes(workflow, tools_root).unwrap_or_default();
                if !now.contains_key(&tid) || now.get(&tid) != tools_approved.get(&tid) {
                    step["status"] = json!("failed");
                    step["error"] = json!(format!(
                        "{tid} changed since this run started (or since you approved the workflow); it was not run"
                    ));
                    push(record, step);
                    return Err("needs_reapproval".into());
                }
                let args = match resolve(&s.args, &results) {
                    Ok(a) => a,
                    Err(e) => {
                        step["status"] = json!("failed");
                        step["error"] = json!(e);
                        push(record, step);
                        return Err("failed".into());
                    }
                };
                (
                    "run_tool".to_string(),
                    json!({"name": s.tool, "args": if args.is_null() { json!({}) } else { args }}),
                )
            }
            None if wf::STEP_TOOLS.contains(&s.tool.as_str()) => match resolve(&s.args, &results) {
                Ok(a) => (s.tool.clone(), if a.is_null() { json!({}) } else { a }),
                Err(e) => {
                    step["status"] = json!("failed");
                    step["error"] = json!(e);
                    push(record, step);
                    return Err("failed".into());
                }
            },
            None => {
                step["status"] = json!("failed");
                step["error"] = json!(format!("{} cannot be a step", s.tool));
                push(record, step);
                return Err("failed".into());
            }
        };
        if let Some(c) = args.get("connection").and_then(Value::as_str) {
            if !workflow.scope.connections.iter().any(|x| x == c) {
                step["status"] = json!("failed");
                step["error"] = json!(format!("connection {c:?} is not in the workflow's scope"));
                push(record, step);
                return Err("failed".into());
            }
        }
        let (ok, out, text) = call(client, &tool, args, left).await;
        step["finished"] = json!(Local::now().to_rfc3339());
        if !ok {
            let needs_you = text.contains(NEEDS_YOU);
            step["status"] = json!(if needs_you { "needs_you" } else { "failed" });
            step["error"] = json!(text.chars().take(2000).collect::<String>());
            push(record, step);
            return Err(if text.contains("time limit") {
                "stopped_at_limit".into()
            } else if needs_you {
                "needs_you".into()
            } else {
                "failed".into()
            });
        }
        let (fails, warns, checks) = checks_of(&out);
        step["summary"] = json!(summary_of(&out));
        step["checks"] = json!(checks);
        step["provenance"] = out["provenance"].clone();
        if let Some(h) = main_handle(&out["result"]) {
            step["handle"] = json!(h);
        }
        results.insert(s.id.clone(), out["result"].clone());
        warned |= warns > 0;
        if fails > 0 && !s.continue_on_fail {
            step["status"] = json!("failed_check");
            push(record, step);
            return Err("failed".into());
        }
        step["status"] = json!("done");
        warned |= fails > 0;
        push(record, step);
    }
    record["results"] = json!(results
        .iter()
        .filter_map(|(k, v)| main_handle(v).map(|h| (k.clone(), h)))
        .collect::<BTreeMap<_, _>>());
    Ok(if warned {
        "succeeded_with_warnings".into()
    } else {
        "succeeded".into()
    })
}

/// The API key the person saved in the Columbus app (its credential store
/// entry). Test builds may give one in the environment.
fn api_key() -> Result<String, String> {
    #[cfg(debug_assertions)]
    if let Ok(k) = std::env::var("COLUMBUS_WORKFLOW_API_KEY") {
        return Ok(k);
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        keyring_core::Entry::new("columbus-app", "anthropic-api-key")
            .and_then(|e| e.get_password())
            .map_err(|e| format!("the API key saved in the Columbus app could not be read here ({e}); on a Mac, choose Always Allow when asked during Run now"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Err("no API key: the Columbus app keeps it in the system's credential store, which this system does not have".into())
}

fn api_base() -> String {
    #[cfg(debug_assertions)]
    if let Ok(b) = std::env::var("COLUMBUS_WORKFLOW_API_BASE") {
        return b;
    }
    "https://api.anthropic.com".into()
}

const REVIEW_SYSTEM: &str = "You review the results of a GIS workflow for the person who set it up, against their criteria. \
You are given rows of its results as tables. You cannot see anything else and cannot act; do not ask questions. \
Answer with one JSON object only, no other text: {\"summary\": \"one to three sentences\", \"flagged\": [{\"layer\": \"<step id>\", \"row\": <row number as given>, \"why\": \"a short reason\"}]}. \
Flag only rows the criteria say to flag. If the rows given may not be all of them, say so in the summary.";

/// The AI review: the listed layers' rows (read through the session, so
/// every rule applies and the reads are audited), the criteria, one model
/// call with no tools, and the notes in the run's record. False when it
/// did not run (the reason is recorded).
async fn review(
    client: &Client,
    r: &wf::Review,
    record: &mut Value,
    audit: &std::sync::Arc<columbus_shared::audit::AuditLog>,
    who: &(String, String),
    return_rows: u64,
    deadline: Instant,
) -> bool {
    let model = r.model().to_string();
    let mut out = json!({"model": model, "status": "did_not_run"});
    let result: Result<(), String> = async {
        let mut left = u64::from(r.rows()).min(return_rows);
        let mut tables = String::new();
        let mut sent = 0u64;
        for step in &r.reads {
            let Some(h) = record["results"].get(step).and_then(Value::as_str).map(String::from) else {
                return Err(format!("step {step} made no layer to review"));
            };
            if left == 0 {
                break;
            }
            let mut args = json!({"handle": h, "limit": left});
            if !r.fields.is_empty() {
                args["fields"] = json!(r.fields);
            }
            let time = deadline.saturating_duration_since(Instant::now());
            let (ok, v, text) = call(client, "read_output", args, time).await;
            if !ok {
                return Err(format!("its rows of {step} could not be read: {}", text.chars().take(300).collect::<String>()));
            }
            let res = &v["result"];
            let cols: Vec<String> = res["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| c.as_str().unwrap_or("").to_string())
                .collect();
            let rows = res["rows"].as_array().cloned().unwrap_or_default();
            let total = res["total"].as_u64().unwrap_or(rows.len() as u64);
            tables.push_str(&format!(
                "Layer {step}: {} of {total} rows shown.\nrow\t{}\n",
                rows.len(),
                cols.join("\t")
            ));
            for (i, row) in rows.iter().enumerate() {
                let cells: Vec<String> = row
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| match c {
                        Value::String(s) => s.replace(['\t', '\n'], " "),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .collect();
                tables.push_str(&format!("{}\t{}\n", i + 1, cells.join("\t")));
            }
            tables.push('\n');
            sent += rows.len() as u64;
            left = left.saturating_sub(rows.len() as u64);
        }
        out["rows_sent"] = json!(sent);
        let key = api_key()?;
        let mut body = json!({
            "model": model,
            "max_tokens": 8000,
            "system": REVIEW_SYSTEM,
            "messages": [{"role": "user", "content": format!("Criteria:\n{}\n\nResults:\n{tables}", r.criteria.trim())}],
        });
        if let Some(e) = &r.effort {
            body["output_config"] = json!({"effort": e});
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1)))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = http
            .post(format!("{}/v1/messages", api_base().trim_end_matches('/')))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|_| "the AI service could not be reached".to_string())?;
        let code = resp.status();
        let bytes = resp.bytes().await.map_err(|_| "the reply was cut off".to_string())?;
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !code.is_success() {
            return Err(format!(
                "the AI service answered {}: {}",
                code.as_u16(),
                v["error"]["message"].as_str().unwrap_or("no reason given")
            ));
        }
        out["tokens"] = json!({"input": v["usage"]["input_tokens"], "output": v["usage"]["output_tokens"]});
        let text: String = v["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = text
            .find('{')
            .zip(text.rfind('}'))
            .and_then(|(a, b)| serde_json::from_str::<Value>(&text[a..=b]).ok());
        let clip = |s: &str, n: usize| s.chars().take(n).collect::<String>();
        match parsed {
            Some(p) => {
                out["summary"] = json!(clip(p["summary"].as_str().unwrap_or(""), 1500));
                let flagged: Vec<Value> = p["flagged"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(500)
                    .map(|f| json!({
                        "layer": clip(f["layer"].as_str().unwrap_or(""), 40),
                        "row": f["row"].as_u64(),
                        "why": clip(f["why"].as_str().unwrap_or(""), 300),
                    }))
                    .collect();
                out["flagged"] = json!(flagged);
            }
            None => out["summary"] = json!(clip(&text, 1500)),
        }
        out["status"] = json!("done");
        Ok(())
    }
    .await;
    if let Err(e) = &result {
        out["reason"] = json!(e);
    }
    // The model call in the audit log: who, which model, how many rows, never the rows.
    let _ = audit.append(&columbus_shared::audit::AuditRecord {
        user: who.0.clone(),
        client: RUNNER.into(),
        session: Some(who.1.clone()),
        tool: "workflow_review".into(),
        rows: out["rows_sent"].as_u64(),
        decision: if result.is_ok() {
            "allow".into()
        } else {
            "error".into()
        },
        reason: Some(match &result {
            Ok(()) => format!(
                "model {model}; tokens {} in, {} out",
                out["tokens"]["input"], out["tokens"]["output"]
            ),
            Err(e) => e.clone(),
        }),
        provider: Some("anthropic".into()),
        ..Default::default()
    });
    record["review"] = out;
    result.is_ok()
}

/// Export the chosen steps' layers to the workflow's folder.
async fn deliver(
    client: &Client,
    workflow: &Workflow,
    run: &str,
    record: &mut Value,
    deadline: Instant,
) -> Result<String, String> {
    let Some(f) = &workflow.outputs.folder else {
        return Ok("succeeded".into());
    };
    let handles = record["results"].clone();
    // The run's id (its date and time, to the millisecond, and a random
    // part): every run's files have their own names.
    let when = run;
    let mut worst = "succeeded".to_string();
    for step in &f.steps {
        let Some(h) = handles.get(step).and_then(Value::as_str) else {
            worst = worst.max_with("failed".into());
            push_delivery(
                record,
                json!({"step": step, "status": "failed", "error": "the step made no layer"}),
            );
            continue;
        };
        let base = format!("{}-{}", workflow.id, step);
        let base: String = base.chars().take(80 - 1 - when.len()).collect();
        let name = format!("{base}-{when}");
        let mut args = json!({"handle": h, "folder": f.export, "name": name});
        if let Some(fmt) = &f.format {
            args["format"] = json!(fmt);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let (ok, out, text) = call(client, "export_output", args, left).await;
        if ok {
            push_delivery(
                record,
                json!({"step": step, "status": "done", "folder": f.export, "name": name, "result": out["result"]}),
            );
        } else {
            let needs_you = text.contains(NEEDS_YOU);
            worst = worst.max_with(if needs_you { "needs_you" } else { "failed" }.into());
            push_delivery(
                record,
                json!({"step": step, "status": if needs_you { "needs_you" } else { "failed" }, "folder": f.export, "error": text.chars().take(2000).collect::<String>()}),
            );
        }
    }
    if worst == "succeeded" {
        Ok(worst)
    } else {
        Err(worst)
    }
}

fn push_delivery(record: &mut Value, d: Value) {
    if let Some(a) = record["deliveries"].as_array_mut() {
        a.push(d);
    }
}

/// `run.json` and `report.md`, written whole.
fn write_run(dir: &Path, record: &Value, workflow: Option<&Workflow>) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(record)?;
    columbus_shared::tools::write_atomic(&dir.join(wf::RUN_FILE), &bytes)
        .map_err(anyhow::Error::msg)?;
    columbus_shared::tools::write_atomic(
        &dir.join(wf::REPORT),
        report(record, workflow).as_bytes(),
    )
    .map_err(anyhow::Error::msg)?;
    Ok(())
}

fn status_words(s: &str) -> &'static str {
    match s {
        "succeeded" => "Succeeded",
        "succeeded_with_warnings" => "Succeeded, with findings to review",
        "needs_you" => "Needs you: a step needs your approval, so the run stopped there",
        "needs_reapproval" => "Not run: needs re-approval",
        "off" => "Not run: workflows are off",
        "skipped" => "Not run: it was already running",
        "stopped_at_limit" => "Stopped at the run's time limit",
        "running" => "Running",
        _ => "Failed",
    }
}

/// The run's report, as Markdown: what ran, what it found, where results went.
pub(crate) fn report(record: &Value, workflow: Option<&Workflow>) -> String {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut out = String::new();
    let name = workflow
        .map(|w| w.name.clone())
        .unwrap_or_else(|| s(&record["workflow"]));
    out.push_str(&format!("# {name}\n\n"));
    let started = chrono::DateTime::parse_from_rfc3339(&s(&record["started"]))
        .map(|d| d.format("%A %-d %B %Y, %H:%M").to_string())
        .unwrap_or_else(|_| s(&record["started"]));
    out.push_str(&format!(
        "**{}.** Run {} started {}{}.\n\n",
        status_words(record["status"].as_str().unwrap_or("")),
        s(&record["run"]),
        started,
        if record["scheduled"] == true {
            " on its schedule"
        } else {
            ""
        }
    ));
    if let Some(r) = record["reason"].as_str() {
        out.push_str(&format!("{r}\n\n"));
    }
    if let Some(steps) = record["steps"].as_array().filter(|a| !a.is_empty()) {
        out.push_str("## Steps\n\n| Step | Tool | Result | Checks |\n|---|---|---|---|\n");
        for st in steps {
            let checks = st["checks"].as_array().cloned().unwrap_or_default();
            let n = |sev: &str| checks.iter().filter(|c| c["severity"] == sev).count();
            let result = match st["status"].as_str() {
                Some("done") => s(&st["summary"]),
                Some("failed_check") => format!("{} (a check failed)", s(&st["summary"])),
                Some("needs_you") => "needs your approval".into(),
                Some(x) => x.replace('_', " "),
                None => String::new(),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} failed, {} to review |\n",
                cell(&s(&st["id"])),
                cell(&s(&st["tool"])),
                cell(&result),
                n("fail"),
                n("warn")
            ));
        }
        out.push('\n');
        for st in steps {
            if let Some(e) = st["error"].as_str() {
                out.push_str(&format!("**{}:** {}\n\n", s(&st["id"]), one_para(e)));
            }
            for c in st["checks"].as_array().into_iter().flatten() {
                if c["severity"] != "info" {
                    out.push_str(&format!(
                        "- {} `{}` ({}): {}\n",
                        s(&st["id"]),
                        s(&c["id"]),
                        s(&c["severity"]),
                        one_para(&s(&c["message"]))
                    ));
                }
            }
        }
        out.push('\n');
    }
    if let Some(r) = record.get("review").filter(|r| r.is_object()) {
        if r["status"] == "done" {
            out.push_str(&format!(
                "## AI review\n\nThe AI's own notes ({}, {} rows read), not checked by Columbus:\n\n",
                s(&r["model"]),
                r["rows_sent"].as_u64().unwrap_or(0)
            ));
            let summary = one_para(&s(&r["summary"]));
            if !summary.is_empty() {
                out.push_str(&format!("{summary}\n\n"));
            }
            let flagged = r["flagged"].as_array().cloned().unwrap_or_default();
            if flagged.is_empty() {
                out.push_str("It flagged nothing.\n\n");
            } else {
                out.push_str("| Layer | Row | Why |\n|---|---|---|\n");
                for f in &flagged {
                    out.push_str(&format!(
                        "| {} | {} | {} |\n",
                        cell(&s(&f["layer"])),
                        f["row"].as_u64().map(|n| n.to_string()).unwrap_or_default(),
                        cell(&s(&f["why"]))
                    ));
                }
                out.push('\n');
            }
        } else {
            out.push_str(&format!(
                "## AI review\n\n**AI review did not run:** {}. The results above are the rules' alone.\n\n",
                one_para(&s(&r["reason"]))
            ));
        }
    }
    if let Some(d) = record["deliveries"].as_array().filter(|a| !a.is_empty()) {
        out.push_str("## Results delivered\n\n");
        for x in d {
            match x["status"].as_str() {
                Some("done") => out.push_str(&format!(
                    "- {} exported to the {} folder as {}\n",
                    s(&x["step"]),
                    s(&x["folder"]),
                    s(&x["name"])
                )),
                _ => out.push_str(&format!(
                    "- {} not exported: {}\n",
                    s(&x["step"]),
                    one_para(&s(&x["error"]))
                )),
            }
        }
        out.push('\n');
    }
    let provs: Vec<String> = record["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|st| st["provenance"].as_str().map(String::from))
        .collect();
    if !provs.is_empty() {
        out.push_str(&format!("Provenance: {}\n", provs.join(", ")));
    }
    out
}

fn one_para(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cell(t: &str) -> String {
    one_para(t).replace('|', "/")
}

/// `columbus-mcp workflow validate <file>`: check a workflow file as
/// propose_workflow would (the parts that need no session).
pub fn validate_cli(file: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(file)?;
    let w = wf::check(&text).map_err(anyhow::Error::msg)?;
    println!("{} is a valid workflow:", w.id);
    for l in wf::describe(&w) {
        println!("- {l}");
    }
    Ok(())
}

/// `columbus-mcp workflow list`.
pub fn list_cli() {
    let root = wf::workflows_dir();
    let tools = crate::config::Paths::from_env().tools;
    for v in wf::list_installed(&root, &tools) {
        let last = v
            .last_run
            .as_ref()
            .and_then(|r| r["status"].as_str())
            .unwrap_or("never run");
        println!(
            "{}  {}  [{}]  last run: {last}",
            v.id,
            v.name,
            v.state.as_deref().unwrap_or("?")
        );
    }
    for v in wf::list_proposals(&root, &tools) {
        println!("{}  {}  [waiting for you to approve]", v.id, v.name);
    }
}
