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

/// The AI review, as policy names it (`deny_tools`) and as a data-flow
/// destination (`flows`: `to: [ai:anthropic]`).
pub const REVIEW_TOOL: &str = "workflow_review";
pub const REVIEW_DESTINATION: &str = "ai:anthropic";
/// Anthropic is also an outside service: rules an organization wrote for
/// every connected server (`to: [server:*]`) cover the review too.
const REVIEW_AS_SERVER: &str = "server:anthropic";

/// The flow rules' answer for sending data with these labels to the AI
/// review: the stricter of the answers for `ai:anthropic` and for
/// `server:anthropic`.
fn review_flow(
    rules: &[columbus_shared::policy::FlowRule],
    labels: &std::collections::BTreeSet<String>,
) -> columbus_flow::FlowDecision {
    use columbus_flow::FlowDecision as D;
    let rank = |d: &D| match d {
        D::Deny { .. } => 3,
        D::Approve { rule: Some(_), .. } => 2,
        D::Approve { rule: None, .. } => 1,
        D::Allow => 0,
    };
    let a = columbus_flow::evaluate_to(rules, labels, REVIEW_DESTINATION);
    let b = columbus_flow::evaluate_to(rules, labels, REVIEW_AS_SERVER);
    if rank(&b) > rank(&a) {
        b
    } else {
        a
    }
}

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
        let problems = workflow_problems(
            &settings,
            self.state.project.as_ref(),
            self.state.tools.root(),
            &w,
            &std::collections::BTreeSet::new(),
        );
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
                "warnings": wf::environment_warnings(&w, self.state.tools.root(), &self.state.paths.environments),
                "next": "Tell the person the workflow is waiting in the Columbus app under Workflows, where they read it and approve it. Nothing runs before that. After approving, they can run it once with Run now and read its report.",
            }),
            checks: vec![],
            provenance: None,
        }))
    }
}

/// What keeps a workflow from being filed for the person: the same checks
/// whether their assistant proposed it or a coworker shared it. `brings`
/// are personal tools coming with it (as proposals) rather than installed.
pub(crate) fn workflow_problems(
    settings: &crate::config::Settings,
    project: Option<&columbus_shared::projects::Project>,
    tools_root: &Path,
    w: &Workflow,
    brings: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let policy = &settings.policy;
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
    if w.review.is_some() {
        if !policy.tool_allowed(REVIEW_TOOL) {
            problems.push(format!(
                "it has an AI review, and organization policy does not allow one ({REVIEW_TOOL})"
            ));
        }
        for c in &w.scope.connections {
            let labels = std::iter::once(columbus_flow::connection_label(c)).collect();
            match review_flow(&policy.flows, &labels) {
                columbus_flow::FlowDecision::Deny { .. } => problems.push(format!(
                    "it has an AI review, and a flow rule forbids sending data from {c} to {REVIEW_DESTINATION}"
                )),
                columbus_flow::FlowDecision::Approve { rule: Some(_), .. } => problems.push(format!(
                    "it has an AI review, and a flow rule needs a person to approve each sending of data from {c} to {REVIEW_DESTINATION}, which a run cannot wait for"
                )),
                _ => {}
            }
        }
    }
    if !w.personal_tools().is_empty() && !policy.allow_personal_tools {
        problems.push(
            "it uses personal tools, and organization policy turns them off (allow_personal_tools)"
                .into(),
        );
    }
    // Personal tools installed, or coming with it (a shared workflow
    // brings its tools as proposals, installed before it can be).
    for t in w.personal_tools().difference(brings) {
        // Installed, approved and unchanged (its files hash to the approval).
        let installed = columbus_shared::tools::list_installed(tools_root)
            .iter()
            .any(|x| &x.id == t && x.state.as_deref() == Some("ready"));
        if !installed {
            problems.push(format!(
                "it uses the personal tool {t}, which is not installed; install it first (propose_tool, then the person installs it in the Columbus app)"
            ));
        }
    }
    // F-6, REQ-18: a step's environment (its own, or its tool's) is a
    // registered governed one at level A; trusted ones never run in a
    // workflow. One that is missing or changed now is a warning when the
    // person approves it, and stops the step with "needs you" if still so.
    for s in &w.steps {
        let Some(e) = wf::step_environment(s, tools_root) else {
            continue;
        };
        if !policy.environments.allow {
            problems.push(format!(
                "step {:?} runs in environment {e}, and organization policy does not allow environments (environments.allow)",
                s.id
            ));
            continue;
        }
        let dir = columbus_shared::environments::environments_dir();
        if let Some(env) = columbus_shared::environments::view(&dir, &e).env {
            if let Err(why) = env.usable_in_tools_and_steps() {
                problems.push(format!("step {:?}: {why}", s.id));
            } else if env.confinement == columbus_shared::environments::Confinement::B {
                problems.push(format!(
                    "step {:?}: environment {e} runs with network (confinement B); workflow steps run at level A only (no network)",
                    s.id
                ));
            }
        }
    }
    for c in &w.scope.connections {
        if !settings.connections.iter().any(|x| &x.id == c) {
            problems.push(format!(
                "scope.connections names {c:?}, which is not one of the person's connections (list_connections)"
            ));
        }
    }
    // In a project, a workflow reaches no further than the project.
    if project.is_some_and(|p| !p.data_groups.is_empty()) {
        problems.push("this project uses data groups, and a workflow can't keep to them yet: propose it from a chat outside the project".into());
    }
    if let Some(p) = project {
        if !p.connections.is_empty() {
            for c in w
                .scope
                .connections
                .iter()
                .filter(|c| !p.connections.contains(c))
            {
                problems.push(format!(
                    "scope.connections names {c:?}, which this project ({}) does not use; propose it outside the project, or ask the person to add the connection to the project",
                    p.name
                ));
            }
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
    if let Some(e) = &w.outputs.email {
        match &policy.email {
            None => problems.push("outputs.email: the organization hasn't set up email for workflows (policy email), so a workflow can't send one".into()),
            Some(p) => {
                if let Err(why) = email_policy_ok(p) {
                    problems.push(format!("outputs.email: {why}"));
                }
                for a in e.to.iter().filter(|a| !p.allows(a)) {
                    problems.push(format!(
                        "outputs.email: {a} is not in a domain the organization allows ({})",
                        p.allowed_domains.join(", ")
                    ));
                }
                // The data it reads, going to each domain: as the
                // organization's flow rules decide (a run can't ask).
                let labels: std::collections::BTreeSet<String> = w
                    .scope
                    .connections
                    .iter()
                    .map(|c| columbus_flow::connection_label(c))
                    .collect();
                for d in email_domains(&e.to) {
                    if let Err(why) = email_flow(&policy.flows, &labels, &d) {
                        problems.push(format!("outputs.email: {why}"));
                    }
                }
            }
        }
    }
    problems
}

/// Secret id of the mail server's password in the credential store (a dot
/// no connection id has, so no connection's password can be this one).
pub const EMAIL_SECRET: &str = "columbus.email-smtp";

/// What the policy's email section must be for anything to be sent.
fn email_policy_ok(p: &columbus_shared::policy::EmailPolicy) -> Result<(), String> {
    if !matches!(p.security.as_str(), "starttls" | "tls") {
        return Err(format!(
            "the policy's email security is {:?}; it must be starttls or tls",
            p.security
        ));
    }
    if !wf::address_ok(&p.from) {
        return Err("the policy's email from is not a plain address".into());
    }
    if p.smtp_host.is_empty()
        || p.smtp_host
            .contains(|c: char| c.is_whitespace() || c.is_control())
    {
        return Err("the policy's email smtp_host is not a host name".into());
    }
    Ok(())
}

fn email_domains(to: &[String]) -> std::collections::BTreeSet<String> {
    to.iter()
        .filter_map(|a| a.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()))
        .collect()
}

/// Labelled data emailed to a domain (`email:<domain>`), under the flow
/// rules: a rule that denies it, or asks for approval each time (which a
/// run can't wait for), stops it. Approving the workflow, which names the
/// addresses, is the person's approval otherwise.
fn email_flow(
    rules: &[columbus_shared::policy::FlowRule],
    labels: &std::collections::BTreeSet<String>,
    domain: &str,
) -> Result<String, String> {
    match columbus_flow::evaluate_to(rules, labels, &format!("email:{domain}")) {
        columbus_flow::FlowDecision::Deny { labels, rule } => Err(format!(
            "organization flow rule {} forbids emailing data from {} to {domain}",
            rule + 1,
            labels.join(", ")
        )),
        columbus_flow::FlowDecision::Approve { rule: Some(r), labels } => Err(format!(
            "organization flow rule {} asks for approval each time data from {} is emailed to {domain}, which a workflow run can't wait for",
            r + 1,
            labels.join(", ")
        )),
        columbus_flow::FlowDecision::Approve { rule: None, .. } => {
            Ok("approved with the workflow".into())
        }
        columbus_flow::FlowDecision::Allow => Ok("allow".into()),
    }
}

/// Email the run's report, as the workflow says and the policy in force now
/// allows: to addresses in allowed domains only, the report as text, and
/// the files the run exported only if the organization allows attachments
/// (within its size cap). What happened, for the run's record.
async fn email_report(state: &State, workflow: &Workflow, record: &Value) -> Option<Value> {
    let e = workflow.outputs.email.as_ref()?;
    let status = record["status"].as_str().unwrap_or("");
    if e.when.as_deref() == Some("problems") && status == "succeeded" {
        return None;
    }
    let policy = state.policy().clone();
    let audit = |decision: &str, reason: String| {
        let _ = state.audit.append(&columbus_shared::audit::AuditRecord {
            user: state.user.clone(),
            client: RUNNER.into(),
            session: Some(state.session.clone()),
            tool: "email_report".into(),
            decision: decision.into(),
            reason: Some(reason),
            ..Default::default()
        });
    };
    let Some(p) = policy.email.as_ref() else {
        let why =
            "the organization no longer sets up email for workflows (policy email)".to_string();
        audit("denied", why.clone());
        return Some(json!({"status": "failed", "error": why}));
    };
    if let Err(why) = email_policy_ok(p) {
        audit("denied", why.clone());
        return Some(json!({"status": "failed", "error": why}));
    }
    let to: Vec<String> =
        e.to.iter()
            .filter(|a| p.allows(a) && wf::address_ok(a))
            .cloned()
            .collect();
    let refused: Vec<&String> = e.to.iter().filter(|a| !to.contains(a)).collect();
    if to.is_empty() {
        let why = "none of its addresses is in a domain the organization allows now".to_string();
        audit("denied", why.clone());
        return Some(json!({"status": "failed", "error": why}));
    }
    let mut attachments = Vec::new();
    let mut left_out = Vec::new();
    if e.attach {
        let cap = u64::from(p.max_attachment_mb) * 1024 * 1024;
        let mut total = 0u64;
        let files: Vec<String> = record["deliveries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["status"] == "done")
            .flat_map(|d| d["result"]["files"].as_array().cloned().unwrap_or_default())
            .filter_map(|f| f["path"].as_str().map(String::from))
            .collect();
        for f in files {
            let size = std::fs::symlink_metadata(&f)
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len());
            match size {
                Some(n) if p.attachments && total + n <= cap => {
                    total += n;
                    attachments.push(f);
                }
                _ => left_out.push(
                    Path::new(&f)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string(),
                ),
            }
        }
    }
    let password = match &p.username {
        Some(_) => match state.secrets.get(EMAIL_SECRET) {
            Ok(Some(pw)) => Some(pw),
            Ok(None) => {
                let why = "the mail server's password isn't saved (Workflows, Email, in the Columbus app)".to_string();
                audit("denied", why.clone());
                return Some(json!({"status": "failed", "error": why}));
            }
            Err(err) => {
                let why = format!("the mail server's password could not be read: {err}");
                audit("denied", why.clone());
                return Some(json!({"status": "failed", "error": why}));
            }
        },
        None => None,
    };
    // What the run read goes with its report: every domain must be one the
    // flow rules let that data go to.
    let labels = state.flow.session_labels();
    let mut decisions = Vec::new();
    for d in email_domains(&to) {
        match email_flow(&policy.flows, &labels, &d) {
            Ok(how) => decisions.push(format!("email:{d} {how}")),
            Err(why) => {
                let _ = state.audit.append(&columbus_shared::audit::AuditRecord {
                    user: state.user.clone(),
                    client: RUNNER.into(),
                    session: Some(state.session.clone()),
                    tool: "email_report".into(),
                    decision: "denied".into(),
                    reason: Some(why.clone()),
                    labels: labels.iter().cloned().collect(),
                    flow_decision: Some("deny".into()),
                    ..Default::default()
                });
                return Some(json!({"status": "failed", "error": why}));
            }
        }
    }
    let mut body = report(record, Some(workflow));
    if !left_out.is_empty() {
        body.push_str(&format!(
            "\n\nNot attached ({}): {}.\n",
            if p.attachments {
                "over the size your organization allows"
            } else {
                "your organization doesn't allow attachments"
            },
            left_out.join(", ")
        ));
    }
    let subject = format!("Columbus: {} ({})", workflow.name, status_words(status));
    let names: Vec<String> = attachments
        .iter()
        .map(|f: &String| {
            Path::new(f)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        })
        .collect();
    // Recorded before it is sent: no email without its line.
    if let Err(err) = state.audit.append(&columbus_shared::audit::AuditRecord {
        user: state.user.clone(),
        client: RUNNER.into(),
        session: Some(state.session.clone()),
        tool: "email_report".into(),
        decision: "allowed".into(),
        reason: Some(format!(
            "sending the report of {} run {} to {}; attachments: {}; {}",
            workflow.id,
            record["run"].as_str().unwrap_or(""),
            to.join(", "),
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            },
            decisions.join("; ")
        )),
        request: Some(columbus_shared::canon::sha256_hex(body.as_bytes())),
        labels: labels.iter().cloned().collect(),
        flow_decision: Some("allow".into()),
        ..Default::default()
    }) {
        return Some(
            json!({"status": "failed", "error": format!("not sent: the audit log could not be written ({err})")}),
        );
    }
    let sent = state
        .worker
        .core(
            "send_email",
            json!({"host": p.smtp_host, "port": p.smtp_port, "security": p.security, "from": p.from,
                "to": to, "subject": subject, "body": body, "username": p.username, "password": password,
                "attachments": attachments}),
            Duration::from_secs(90),
        )
        .await;
    match sent {
        Ok(r) => {
            // Only those the mail server took.
            let took: Vec<String> = r["sent"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(String::from))
                .filter(|a| to.contains(a))
                .collect();
            let server_refused: Vec<String> =
                to.iter().filter(|a| !took.contains(a)).cloned().collect();
            let mut not_sent: Vec<String> = refused.iter().map(|s| s.to_string()).collect();
            audit(
                if took.is_empty() { "error" } else { "allowed" },
                format!(
                    "emailed the report of {} run {} to {} ({} attachment(s)); not to: {}",
                    workflow.id,
                    record["run"].as_str().unwrap_or(""),
                    if took.is_empty() {
                        "no one".to_string()
                    } else {
                        took.join(", ")
                    },
                    attachments.len(),
                    if not_sent.is_empty() && server_refused.is_empty() {
                        "none".to_string()
                    } else {
                        format!(
                            "{} (domain not allowed) {} (refused by the mail server)",
                            not_sent.join(", "),
                            server_refused.join(", ")
                        )
                    }
                ),
            );
            if took.is_empty() {
                return Some(
                    json!({"status": "failed", "error": format!("the mail server refused every address ({})", to.join(", "))}),
                );
            }
            not_sent.extend(server_refused);
            Some(
                json!({"status": "sent", "to": took, "attachments": attachments.len(), "not_sent_to": not_sent, "not_attached": left_out}),
            )
        }
        Err(err) => {
            let why = err.to_string();
            audit(
                "error",
                format!("emailing the report of {} failed: {why}", workflow.id),
            );
            Some(json!({"status": "failed", "error": why}))
        }
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
    // What an opened run's chat may read (REQ-27).
    record["scope"] = json!(workflow.scope.connections);
    let state = std::sync::Arc::new(state);
    // Whatever blocks, the run ends: at its time limit and two minutes, it
    // records that it stopped there and exits (its lock goes with it).
    // Its engine processes are ended too.
    progress(&record);
    {
        let dir = run_dir.clone();
        let w = workflow.clone();
        let limit = Duration::from_secs(u64::from(workflow.run_minutes()) * 60 + 120);
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            let Ok(latest) = PROGRESS.lock() else {
                std::process::exit(1)
            };
            // The run's own record is already written: nothing to do.
            if FINISHED.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let mut r = latest.clone();
            r["status"] = json!("stopped_at_limit");
            r["finished"] = json!(Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false));
            r["reason"] = json!("the run did not finish in its time limit, so it was stopped");
            let _ = write_run(&dir, &r, Some(&w));
            crate::worker::end_all();
            std::process::exit(1);
        });
    }

    let deadline = Instant::now() + Duration::from_secs(u64::from(workflow.run_minutes()) * 60);
    let gateway = Gateway::with_state(state.clone());
    let (server_io, client_io) = tokio::io::duplex(8 << 20);
    let server = tokio::spawn(async move {
        if let Ok(s) = gateway.serve(server_io).await {
            let _ = s.waiting().await;
        }
    });
    let client = RunnerClient.serve(client_io).await?;

    let status = run_steps(
        &client,
        &state,
        &workflow,
        &mut record,
        deadline,
        &tools_root,
        &tools_now,
    )
    .await;
    progress(&record);
    let status = match status {
        Ok(s) if s == "succeeded" || s == "succeeded_with_warnings" => {
            // The AI review, if any: a review that cannot run leaves the
            // run's results as they are, marked.
            let s = match &workflow.review {
                Some(r) => {
                    let ok = review(&client, r, &mut record, &state, deadline).await;
                    if ok {
                        s
                    } else {
                        s.max_with("succeeded_with_warnings".into())
                    }
                }
                None => s,
            };
            progress(&record);
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
    // Each layer the run made, fingerprinted, so it can be opened in a
    // chat exactly as the run left it (REQ-27).
    crate::open_run::record_outputs(&state, &mut record);
    let code = finish(&mut record, &status, None);
    // Emailed once the run's ending is known; how that went is in the
    // record, and never changes how the run ended.
    if let Some(sent) = email_report(&state, &workflow, &record).await {
        record["email"] = sent;
    }
    {
        // Held while the record is written, so the time-limit watch either
        // writes first or not at all.
        let _held = PROGRESS.lock();
        FINISHED.store(true, std::sync::atomic::Ordering::SeqCst);
        write_run(&run_dir, &record, Some(&workflow))?;
    }
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

/// The first of `steps` whose result `v` refers to (`${step}` or
/// `${step.a.b}`), if any.
fn uses_any(v: &Value, steps: &[String]) -> Option<String> {
    match v {
        Value::Object(m) => m.values().find_map(|x| uses_any(x, steps)),
        Value::Array(a) => a.iter().find_map(|x| uses_any(x, steps)),
        Value::String(s) => wf::reference(s)
            .map(|(step, _)| step)
            .filter(|step| steps.iter().any(|x| x == step))
            .map(String::from),
        _ => None,
    }
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
/// The run's record as far as it has got, for the time-limit watch.
static PROGRESS: std::sync::Mutex<Value> = std::sync::Mutex::new(Value::Null);
static FINISHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn progress(record: &Value) {
    if let Ok(mut p) = PROGRESS.lock() {
        *p = record.clone();
    }
}

/// R-19, REQ-26: before a step uses a signed-in `arcgis` connection, its
/// refresh token is exchanged for a new one if fewer than 7 days of its
/// life remain (or that is not known). Err: the step stops with this
/// message ("needs you: sign in again (…)" for a lapsed sign-in). Ok: the
/// checks to add to the step's (a renewal that could not be made now).
async fn keep_signed_in(state: &State, step: &wf::Step) -> Result<Vec<Value>, String> {
    let settings = state.settings();
    let mut checks = Vec::new();
    for c in wf::step_connections(step) {
        let Some(conn) = settings
            .connections
            .iter()
            .find(|x| x.id == c && crate::arcgis::is_arcgis(&x.kind))
        else {
            continue;
        };
        let kept = crate::arcgis_auth::keep_alive(
            &state.secrets,
            conn,
            &state.policy(),
            &state.paths.connections,
        )
        .await?;
        match kept {
            crate::arcgis_auth::Kept::AsIs => {}
            crate::arcgis_auth::Kept::Exchanged => checks.push(json!({
                "id": "arcgis.signin_renewed",
                "severity": "info",
                "message": format!("{c}'s ArcGIS sign-in was renewed for another period (fewer than {} days were left)", crate::arcgis_auth::EXCHANGE_BELOW_DAYS),
            })),
            crate::arcgis_auth::Kept::NotNow(why) => checks.push(json!({
                "id": "arcgis.signin_not_renewed",
                "severity": "warn",
                "message": format!("{c}'s ArcGIS sign-in ends within {} days and could not be renewed now ({why}); the next run tries again, or sign in again: {}", crate::arcgis_auth::EXCHANGE_BELOW_DAYS, crate::arcgis_auth::signin_command(&c)),
            })),
        }
    }
    Ok(checks)
}

#[allow(clippy::too_many_arguments)]
async fn run_steps(
    client: &Client,
    state: &State,
    workflow: &Workflow,
    record: &mut Value,
    deadline: Instant,
    tools_root: &Path,
    tools_approved: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut results: BTreeMap<String, Value> = BTreeMap::new();
    let mut warned = false;
    // R-13c: steps that stopped because their environment can't be used;
    // the run goes on with every step that doesn't use their results.
    let mut stopped: Vec<String> = Vec::new();
    for s in &workflow.steps {
        let started = Local::now();
        let mut step = json!({"id": s.id, "tool": s.tool, "started": started.to_rfc3339()});
        let push = |record: &mut Value, step: Value| {
            if let Some(a) = record["steps"].as_array_mut() {
                a.push(step);
            }
        };
        if let Some(on) = uses_any(&s.args, &stopped) {
            step["status"] = json!("skipped");
            step["error"] = json!(format!(
                "it uses the result of step {on}, which needs you, so it was not run"
            ));
            stopped.push(s.id.clone());
            push(record, step);
            continue;
        }
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
                let mut call_args =
                    json!({"name": s.tool, "args": if args.is_null() { json!({}) } else { args }});
                // F-6: the step's environment, else the tool's own.
                if let Some(e) = &s.environment {
                    call_args["environment"] = json!(e);
                }
                ("run_tool".to_string(), call_args)
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
        // A signed-in ArcGIS connection: kept alive, or the step needs you.
        let sign_in_checks = match keep_signed_in(state, s).await {
            Ok(c) => c,
            Err(e) => {
                let needs_you = e.starts_with(NEEDS_YOU);
                step["status"] = json!(if needs_you { "needs_you" } else { "failed" });
                step["error"] = json!(e);
                push(record, step);
                if needs_you {
                    record["reason"] = json!(format!("Step {} stopped: {e}", s.id));
                    return Err("needs_you".into());
                }
                return Err("failed".into());
            }
        };
        {
            // If the run is stopped at its limit during this step, its
            // record says so.
            let mut r = record.clone();
            let mut running = step.clone();
            running["status"] = json!("stopped");
            running["error"] = json!("it was still running at the run's time limit");
            push(&mut r, running);
            progress(&r);
        }
        let (ok, out, text) = call(client, &tool, args, left).await;
        step["finished"] = json!(Local::now().to_rfc3339());
        // A sign-in that lapsed during the step (its access token could
        // not be refreshed) needs the person too.
        let text = match wf::step_connections(s)
            .iter()
            .find(|c| !ok && text.contains(&crate::arcgis_auth::sign_in_again(c)))
        {
            Some(c) => {
                let e = crate::arcgis_auth::needs_sign_in(c);
                record["reason"] = json!(format!("Step {} stopped: {e}", s.id));
                e
            }
            None => text,
        };
        if !ok {
            let needs_you = text.contains(NEEDS_YOU);
            step["status"] = json!(if needs_you { "needs_you" } else { "failed" });
            step["error"] = json!(text.chars().take(2000).collect::<String>());
            if text.contains(crate::script_tools::ENV_NEEDS_YOU) {
                push(record, step);
                stopped.push(s.id.clone());
                continue;
            }
            push(record, step);
            return Err(if text.contains("time limit") {
                "stopped_at_limit".into()
            } else if needs_you {
                "needs_you".into()
            } else {
                "failed".into()
            });
        }
        let (fails, mut warns, mut checks) = checks_of(&out);
        warns += sign_in_checks
            .iter()
            .filter(|c| c["severity"] == "warn")
            .count();
        checks.extend(sign_in_checks);
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
    if !stopped.is_empty() {
        // Nothing is exported or reviewed from a run that is missing steps.
        return Err("needs_you".into());
    }
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
    state: &std::sync::Arc<State>,
    deadline: Instant,
) -> bool {
    let model = r.model().to_string();
    let mut out = json!({"model": model, "status": "did_not_run"});
    let policy = state.policy();
    let return_rows = policy.return_rows.unwrap_or(500).clamp(1, 500);
    let mut labels = std::collections::BTreeSet::new();
    let mut flow_decision = None;
    let mut refused = false;
    let result: Result<(), String> = async {
        if !policy.tool_allowed(REVIEW_TOOL) {
            refused = true;
            return Err(format!("organization policy does not allow an AI review ({REVIEW_TOOL})"));
        }
        // The data's labels, and the organization's rules on sending it to the AI.
        for c in state.scope.connections.iter().filter(|c| !c.starts_with('\u{0}')) {
            labels.insert(columbus_flow::connection_label(c));
        }
        for step in &r.reads {
            if let Some(h) = record["results"].get(step).and_then(Value::as_str) {
                if let Ok(p) = crate::providers::resolve_handle(&state.session, &state.sandbox, h) {
                    labels.extend(state.flow.file_labels(&p));
                }
            }
        }
        // With no rule of the organization's, labelled data leaving needs
        // the person's approval, and approving the workflow was that: what
        // it sends, and to whom, is in what they approved. A rule that asks
        // for an approval each time cannot be met by a run.
        match review_flow(&policy.flows, &labels) {
            columbus_flow::FlowDecision::Allow => flow_decision = Some("allow"),
            columbus_flow::FlowDecision::Approve { rule: None, .. } => {
                flow_decision = Some("approved_with_workflow")
            }
            columbus_flow::FlowDecision::Approve { .. } => {
                flow_decision = Some("approve");
                refused = true;
                return Err(format!("a flow rule needs a person's approval to send this data to {REVIEW_DESTINATION}, and a run never waits for one"));
            }
            columbus_flow::FlowDecision::Deny { .. } => {
                flow_decision = Some("deny");
                refused = true;
                return Err(format!("a flow rule forbids sending this data to {REVIEW_DESTINATION}"));
            }
        }
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
        // The credential store may ask the person first (on a Mac); a run
        // does not wait long for that.
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(120));
        let key = tokio::time::timeout(wait, tokio::task::spawn_blocking(api_key))
            .await
            .map_err(|_| "the API key could not be read without you (on a Mac, start the workflow once with Run now and choose Always Allow)".to_string())?
            .map_err(|e| e.to_string())??;
        let max_tokens = match r.effort.as_deref() {
            Some("high" | "xhigh" | "max") => 32_000,
            _ => 8_000,
        };
        // Streamed: bytes keep coming while the model thinks, so a long
        // review is not cut off by a proxy waiting on an idle connection.
        let mut body = json!({
            "stream": true,
            "model": model,
            "max_tokens": max_tokens,
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
        let streamed = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));
        let v: Value = if code.is_success() && streamed {
            columbus_chat::read_stream(resp)
                .await
                .map_err(|e| e.to_string())?
        } else {
            // At most 8 MB of a reply that is not streamed.
            let mut resp = resp;
            let mut bytes = Vec::new();
            while let Some(c) = resp.chunk().await.map_err(|_| "the reply was cut off".to_string())? {
                bytes.extend_from_slice(&c);
                if bytes.len() > 8 << 20 {
                    return Err("the AI service's reply was larger than Columbus takes".into());
                }
            }
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
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
        // Only a whole answer in the form asked for counts; anything else is
        // a review that did not run, never an all-clear.
        if v["stop_reason"] != "end_turn" {
            return Err(format!(
                "the AI's answer was not complete ({})",
                v["stop_reason"].as_str().unwrap_or("no stop reason")
            ));
        }
        let parsed = text
            .find('{')
            .zip(text.rfind('}'))
            .and_then(|(a, b)| serde_json::from_str::<Value>(&text[a..=b]).ok())
            .filter(|p| p["flagged"].is_array() && p["summary"].is_string());
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
            None => {
                return Err("the AI's answer was not in the form asked for, so it was not used".into())
            }
        }
        out["status"] = json!("done");
        Ok(())
    }
    .await;
    if let Err(e) = &result {
        out["reason"] = json!(e);
    }
    // The model call in the audit log: who, which model, how many rows, never the rows.
    let _ = state.audit.append(&columbus_shared::audit::AuditRecord {
        user: state.user.clone(),
        client: RUNNER.into(),
        session: Some(state.session.clone()),
        labels: labels.iter().cloned().collect(),
        flow_decision: flow_decision.map(String::from),
        tool: "workflow_review".into(),
        rows: out["rows_sent"].as_u64(),
        decision: if result.is_ok() {
            "allowed".into()
        } else if refused {
            "denied".into()
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
        let mut name = format!("{base}-{when}");
        // F-11: a file geodatabase's feature class is named after the
        // export, and its name is a letter then letters, digits and _.
        if f.format.as_deref() == Some("gdb") {
            name = name.replace(|c: char| !c.is_ascii_alphanumeric(), "_");
        }
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
    // A run stopped at its limit after this still says what was delivered.
    progress(record);
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
        "needs_you" => "Needs you: a step needs you (an approval, a sign-in or an environment), so the run stopped there",
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
                Some("needs_you") if s(&st["error"]).starts_with("needs you: sign in again") => {
                    "needs you: sign in again".into()
                }
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
    match record["email"]["status"].as_str() {
        Some("sent") => out.push_str(&format!(
            "Emailed to {}.\n\n",
            record["email"]["to"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Some(_) => out.push_str(&format!(
            "Not emailed: {}\n\n",
            one_para(&s(&record["email"]["error"]))
        )),
        None => {}
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

#[cfg(test)]
mod email_tests {
    use super::*;

    #[test]
    fn email_follows_the_flow_rules() {
        let rules: Vec<columbus_shared::policy::FlowRule> = serde_yaml::from_str(
            "- {from: [\"connection:crashes\"], to: [\"*\"], action: deny}\n- {from: [\"connection:roads\"], to: [\"email:*\"], action: approve}\n",
        )
        .unwrap();
        let l = |c: &str| std::collections::BTreeSet::from([format!("connection:{c}")]);
        assert!(email_flow(&rules, &l("crashes"), "agency.gov").is_err());
        assert!(
            email_flow(&rules, &l("roads"), "agency.gov").is_err(),
            "asks each time"
        );
        assert!(email_flow(&rules, &l("parcels"), "agency.gov").is_ok());
        assert!(email_flow(&rules, &Default::default(), "agency.gov").is_ok());
        assert_eq!(
            email_domains(&["a@Agency.gov".into(), "b@agency.gov".into()]).len(),
            1
        );
    }
}
