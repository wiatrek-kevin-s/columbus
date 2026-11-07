//! `columbus-mcp share import '<json>'`: file what a shared `.columbus`
//! file holds as proposals, for the Columbus app's "Open a shared file".
//!
//! The JSON is `{"file": "<path>", "sha256": "<what the app showed>",
//! "connections": {"theirs": "mine"}, "export": "<my export folder>"}` (the
//! last two for a workflow). Nothing is installed or run here: each tool and
//! the workflow are filed exactly as the person's own assistant's proposals
//! are, after the same checks, and wait in the app for the person to read
//! and approve with Touch ID or Windows Hello.
//!
//! - A tool must be exactly what `propose_tool` builds from its own files
//!   (every rule on names, descriptions and wording), its code passes the
//!   same lint, and organization policy must allow it and `propose_tool`.
//!   It is not test-run here (no data travels with it); the app says so.
//! - A workflow is rewritten to the person's connections and export folder
//!   (every connection it names, its steps' inputs included) and then
//!   passes the same checks as `propose_workflow`. Its schedule is never
//!   imported.
//! - All or nothing: everything is checked, the proposal limits included,
//!   before anything is filed, and what was filed is taken back if filing
//!   fails part way. Nothing already waiting for the person is replaced.
//! - Every opening is audited (`share_import`), refused ones included, with
//!   the file's hash, who it says it is from, and what was filed.
//!
//! Prints `{"ok": true, "filed": [...]}` or `{"ok": false, "error": "..."}`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use columbus_shared::audit::AuditRecord;
use columbus_shared::share;
use columbus_shared::tools as store;
use columbus_shared::workflows as wf;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::State;

pub const CLIENT: &str = "columbus-app-share";
/// As organization policy names it (`deny_tools: [share_import]`).
pub const TOOL: &str = "share_import";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    file: String,
    /// The hash of the file the person was shown; it must not have changed.
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    connections: BTreeMap<String, String>,
    #[serde(default)]
    export: Option<String>,
}

pub async fn import(request: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let out = match State::load(crate::config::Paths::from_env()) {
        Ok(state) => {
            let workspace = state.sandbox.root().to_path_buf();
            let out = run(&state, request).await;
            let _ = std::fs::remove_dir(&workspace);
            match out {
                Ok(v) => v,
                Err(e) => json!({"ok": false, "error": e}),
            }
        }
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    println!("{out}");
    Ok(())
}

/// What one opening is recorded with.
#[derive(Default)]
struct Seen {
    file_sha: Option<String>,
    from: Option<String>,
    hashes: Vec<String>,
}

fn record(state: &State, seen: &Seen, decision: &str, reason: String) -> Result<(), String> {
    let mut why = reason;
    if let Some(f) = &seen.from {
        why = format!("{why} (the file says it is from {f})");
    }
    if !seen.hashes.is_empty() {
        why = format!("{why}; contents {}", seen.hashes.join(", "));
    }
    state
        .audit
        .append(&AuditRecord {
            user: state.user.clone(),
            client: CLIENT.into(),
            session: Some(state.session.clone()),
            tool: TOOL.into(),
            connection: None,
            dataset: None,
            rows: None,
            decision: decision.into(),
            reason: Some(why),
            approval: None,
            prov: None,
            request: seen.file_sha.clone(),
            provider: None,
            provider_version: None,
            tools_hash: None,
            labels: vec![],
            flow_decision: None,
        })
        .map(|_| ())
        .map_err(|e| format!("refused: the audit log could not be written ({e})"))
}

async fn run(state: &State, request: &str) -> Result<Value, String> {
    let mut seen = Seen::default();
    let plan = match check(state, request, &mut seen).await {
        Ok(p) => p,
        Err(e) => {
            record(state, &seen, "denied", e.clone())?;
            return Err(e);
        }
    };
    if plan.tools.is_empty() && plan.workflow.is_none() {
        record(
            state,
            &seen,
            "allowed",
            "nothing new to file: all of it is installed already".into(),
        )?;
        return Ok(json!({"ok": true, "filed": [], "already_installed": plan.already}));
    }
    let what: Vec<String> = plan
        .tools
        .iter()
        .map(|t| format!("tool {}", t.id))
        .chain(plan.workflow.as_ref().map(|w| format!("workflow {}", w.id)))
        .collect();
    // Recorded before anything is filed: no proposal without its line.
    record(
        state,
        &seen,
        "allowed",
        format!("filing for review: {}", what.join(", ")),
    )?;
    match file(state, &plan) {
        Ok(filed) => Ok(json!({"ok": true, "filed": filed, "already_installed": plan.already})),
        Err(e) => {
            let _ = record(
                state,
                &seen,
                "error",
                format!("filing stopped, nothing kept: {e}"),
            );
            Err(e)
        }
    }
}

struct ToolToFile {
    id: String,
    manifest: String,
    code: String,
    info: Value,
}

struct WorkflowToFile {
    id: String,
    text: String,
    info: Value,
}

struct Plan {
    tools: Vec<ToolToFile>,
    workflow: Option<WorkflowToFile>,
    already: Vec<String>,
}

/// File every part; if one fails, take back the ones filed. Before each
/// write the id is checked again: something filed for the person since the
/// checks (their own assistant's proposal, say) is never replaced, and only
/// a proposal that is still exactly ours is taken back.
fn file(state: &State, plan: &Plan) -> Result<Vec<Value>, String> {
    let tools_root = state.tools.root();
    let wroot = wf::workflows_dir();
    let mut filed: Vec<Value> = Vec::new();
    let undo = |filed: &[Value]| {
        for f in filed {
            let id = f["id"].as_str().unwrap_or("");
            let ours = f["mark"].as_str().unwrap_or("");
            if f["kind"] == "tool" {
                if store::read_proposal(tools_root, id).is_ok_and(|p| p.hash == ours) {
                    let _ = store::decline(tools_root, id);
                }
            } else if std::fs::read(wroot.join(wf::PROPOSALS).join(id).join(wf::FILE))
                .is_ok_and(|b| columbus_shared::canon::sha256_hex(&b) == ours)
            {
                let _ = wf::decline(&wroot, id);
            }
        }
    };
    for t in &plan.tools {
        if store::list_proposals(tools_root)
            .iter()
            .any(|p| p.id == t.id)
        {
            undo(&filed);
            return Err(format!(
                "{} was filed for your review by something else just now; review it first, then open this file again",
                t.id
            ));
        }
        if let Err(e) = store::write_proposal(
            tools_root,
            &t.id,
            t.manifest.as_bytes(),
            t.code.as_bytes(),
            &t.info,
        ) {
            undo(&filed);
            return Err(e);
        }
        let hash = store::content_hash(t.manifest.as_bytes(), t.code.as_bytes());
        filed.push(json!({"kind": "tool", "id": t.id, "mark": hash}));
    }
    if let Some(w) = &plan.workflow {
        if wf::list_proposals(&wroot, tools_root)
            .iter()
            .any(|p| p.id == w.id)
        {
            undo(&filed);
            return Err(format!(
                "a workflow {} was filed for your review by something else just now; review it first, then open this file again",
                w.id
            ));
        }
        if let Err(e) = wf::write_proposal(&wroot, &w.text, &w.info) {
            undo(&filed);
            return Err(e);
        }
        let mark = columbus_shared::canon::sha256_hex(w.text.as_bytes());
        filed.push(json!({"kind": "workflow", "id": w.id, "mark": mark}));
    }
    Ok(filed
        .into_iter()
        .map(|f| json!({"kind": f["kind"], "id": f["id"]}))
        .collect())
}

/// Whether the person has this exact tool installed, and ready.
fn installed_ready(tools_root: &Path, id: &str, hash: &str) -> bool {
    store::list_installed(tools_root)
        .iter()
        .any(|t| t.id == id && t.hash == hash && t.state.as_deref() == Some("ready"))
}

async fn check(state: &State, request: &str, seen: &mut Seen) -> Result<Plan, String> {
    if request.len() > 64 * 1024 {
        return Err("the request is too long".into());
    }
    let req: Request = serde_json::from_str(request).map_err(|e| format!("not a request: {e}"))?;
    let path = Path::new(&req.file);
    let bytes = share::read_bytes(path)?;
    let file_sha = columbus_shared::canon::sha256_hex(&bytes);
    seen.file_sha = Some(file_sha.clone());
    if req.sha256.as_deref().is_some_and(|s| s != file_sha) {
        return Err("the file changed after you opened it; open it again".into());
    }
    let shared = share::parse_bytes(&bytes)?;
    seen.from = Some(shared.shared_by.clone());
    seen.hashes = shared
        .tools
        .iter()
        .map(|t| format!("{} {}", t.id, t.hash))
        .chain(
            shared
                .workflow_sha256
                .iter()
                .map(|h| format!("workflow {h}")),
        )
        .collect();

    let settings = state.settings();
    let policy = &settings.policy;
    if !policy.allow_shared_tools {
        return Err("your organization's settings don't allow opening shared tools and workflows (allow_shared_tools is off). It can be turned on in the Columbus app if your organization lets you".into());
    }
    if !policy.tool_allowed(TOOL) {
        return Err(format!("organization policy does not allow {TOOL}"));
    }
    if !shared.tools.is_empty() {
        if !policy.allow_personal_tools {
            return Err(
                "it brings personal tools, and personal tools are off (allow_personal_tools)"
                    .into(),
            );
        }
        if !policy.tool_allowed("propose_tool") {
            return Err("it brings personal tools, and organization policy does not allow new ones (propose_tool)".into());
        }
    }
    if shared.kind == "workflow" {
        if !policy.allow_workflows {
            return Err("it's a workflow, and workflows are off (allow_workflows)".into());
        }
        if !policy.tool_allowed("propose_workflow") {
            return Err("it's a workflow, and organization policy does not allow new ones (propose_workflow)".into());
        }
    }
    let tools_root = state.tools.root();
    let now = chrono::Local::now().to_rfc3339();
    let imported = json!({
        "from": shared.shared_by,
        "shared_at": shared.shared_at,
        "columbus": shared.columbus,
        "file": path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .chars()
            .filter(|c| store::plain_chars(&c.to_string()).is_ok())
            .take(120)
            .collect::<String>(),
        "file_sha256": file_sha,
    });
    let waiting_tools: BTreeSet<String> = store::list_proposals(tools_root)
        .into_iter()
        .map(|t| t.id)
        .collect();

    // Every tool, checked as propose_tool checks one (but not test-run).
    let mut tools = Vec::new();
    let mut already = Vec::new();
    for t in &shared.tools {
        let label = t.id.clone();
        if installed_ready(tools_root, &t.id, &t.hash) {
            already.push(t.id.clone());
            continue;
        }
        if waiting_tools.contains(&t.id) {
            return Err(format!(
                "{label} is already waiting for your review under Tools: install or decline it first, then open this file again"
            ));
        }
        let (args, built) = crate::propose_tool::rebuild(&t.id, &t.manifest, &t.code)
            .map_err(|e| format!("{label}: {e}"))?;
        let tool = format!("{}.{}", built.id, store::TOOL);
        if !policy.tool_allowed(&tool) {
            return Err(format!(
                "organization policy does not allow the tool {tool}"
            ));
        }
        if state.tools.taken().contains(&built.id) {
            return Err(format!(
                "{} is the id of one of your organization's plugins or connected servers",
                built.id
            ));
        }
        let lint = state
            .worker
            .core(
                "lint_tool",
                json!({"code": args.code}),
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(|e| format!("{label}: its code could not be checked: {e}"))?;
        let line = |e: &Value| {
            format!(
                "line {}: {}",
                e["line"].as_u64().unwrap_or(0),
                e["message"].as_str().unwrap_or("")
            )
        };
        let mut problems: Vec<String> = lint["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .map(line)
            .collect();
        let mut warnings: Vec<String> = lint["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(line)
            .collect();
        crate::propose_tool::compare_uses(&args, &lint, &mut problems, &mut warnings);
        if !problems.is_empty() {
            return Err(format!(
                "{label}: its code breaks Columbus's rules for tools:\n- {}",
                problems.join("\n- ")
            ));
        }
        let replaces = store::list_installed(tools_root)
            .into_iter()
            .find(|x| x.id == built.id)
            .map(|x| json!({"version": x.version, "hash": x.hash}));
        let info = json!({
            "id": built.id,
            "tool": tool,
            "title": built.manifest.display_name(),
            "version": built.manifest.version,
            "hash": built.hash,
            "proposed": now,
            "session": state.session,
            "user": state.user,
            "client": CLIENT,
            "approval": null,
            "imported": imported,
            "test": null,
            "warnings": warnings,
            "replaces": replaces,
        });
        tools.push(ToolToFile {
            id: built.id,
            manifest: built.manifest_text,
            code: built.entry,
            info,
        });
    }
    if waiting_tools.len() + tools.len() > store::MAX_PROPOSALS {
        return Err(format!(
            "it would put more than {} tools waiting for your review; install or decline some under Tools first",
            store::MAX_PROPOSALS
        ));
    }

    // The workflow, in the person's own names, checked as propose_workflow
    // checks one.
    let mut workflow = None;
    if let Some(text) = &shared.workflow {
        let w = wf::check(text)?;
        let mine = share::remap(&w, &req.connections, req.export.as_deref())?;
        let w2 = wf::check(&mine)?;
        let brings: BTreeSet<String> = tools.iter().map(|t| t.id.clone()).collect();
        let problems =
            crate::workflows::workflow_problems(&settings, None, tools_root, &w2, &brings);
        if !problems.is_empty() {
            return Err(format!(
                "the workflow can't be filed here:\n- {}",
                problems.join("\n- ")
            ));
        }
        let root = wf::workflows_dir();
        let waiting = wf::list_proposals(&root, tools_root);
        if waiting.iter().any(|x| x.id == w2.id) {
            return Err(format!(
                "a workflow {} is already waiting for your review under Workflows: approve or decline it first, then open this file again",
                w2.id
            ));
        }
        if waiting.len() + 1 > wf::MAX_PROPOSALS {
            return Err(format!(
                "{} workflows are already waiting for your review; approve or decline some first",
                waiting.len()
            ));
        }
        let replaces = wf::list_installed(&root, tools_root)
            .into_iter()
            .find(|x| x.id == w2.id)
            .map(|x| json!({"name": x.name, "fingerprint": x.fingerprint}));
        let info = json!({
            "id": w2.id,
            "name": w2.name,
            "proposed": now,
            "session": state.session,
            "user": state.user,
            "client": CLIENT,
            "imported": imported,
            "mapped": {"connections": req.connections, "export": req.export},
            "replaces": replaces,
        });
        workflow = Some(WorkflowToFile {
            id: w2.id,
            text: mine,
            info,
        });
    } else if !req.connections.is_empty() || req.export.is_some() {
        return Err("a shared tool takes no connections or export folder".into());
    }
    Ok(Plan {
        tools,
        workflow,
        already,
    })
}
