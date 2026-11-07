//! `columbus-mcp share import '<json>'`: file what a shared `.columbus`
//! file holds as proposals, for the Columbus app's "Open a shared file".
//!
//! The JSON is `{"file": "<path>", "connections": {"theirs": "mine"},
//! "export": "<my export folder>"}` (the last two for a workflow). Nothing
//! is installed or run here: each tool and the workflow are filed exactly
//! as the person's own assistant's proposals are, after the same checks,
//! and wait in the app for the person to read and approve with Touch ID or
//! Windows Hello.
//!
//! - A tool must be exactly what `propose_tool` builds from its own files
//!   (every rule on names, descriptions and wording), its code passes the
//!   same lint, and organization policy must allow it. It is not test-run
//!   here (no data travels with it); the app says so.
//! - A workflow is rewritten to the person's connections and export folder
//!   and then passes the same checks as `propose_workflow`. Its schedule is
//!   never imported.
//! - All of it is checked before any of it is filed.
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    file: String,
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
    let out = match run(request).await {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": e}),
    };
    println!("{out}");
    Ok(())
}

struct ToolToFile {
    id: String,
    manifest: String,
    code: String,
    info: Value,
}

async fn run(request: &str) -> Result<Value, String> {
    if request.len() > 64 * 1024 {
        return Err("the request is too long".into());
    }
    let req: Request = serde_json::from_str(request).map_err(|e| format!("not a request: {e}"))?;
    let path = Path::new(&req.file);
    let shared = share::read(path)?;
    let file_sha = columbus_shared::canon::sha256_hex(
        &std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
    );
    let state = State::load(crate::config::Paths::from_env()).map_err(|e| e.to_string())?;
    let workspace = state.sandbox.root().to_path_buf();
    let out = check_and_file(&state, &shared, &req, &file_sha, path).await;
    let _ = std::fs::remove_dir(&workspace);
    let rec = |decision: &str, reason: Option<String>| AuditRecord {
        user: state.user.clone(),
        client: CLIENT.into(),
        session: Some(state.session.clone()),
        tool: "share_import".into(),
        connection: None,
        dataset: None,
        rows: None,
        decision: decision.into(),
        reason,
        approval: None,
        prov: None,
        request: Some(file_sha.clone()),
        provider: None,
        provider_version: None,
        tools_hash: None,
        labels: vec![],
        flow_decision: None,
    };
    match &out {
        Ok(v) => {
            state
                .audit
                .append(&rec(
                    "allowed",
                    Some(format!(
                        "filed for review from a file shared by {}: {}",
                        shared.shared_by, v["filed"]
                    )),
                ))
                .map_err(|e| format!("the audit log could not be written ({e})"))?;
        }
        Err(e) => {
            let _ = state.audit.append(&rec("denied", Some(e.clone())));
        }
    }
    out
}

async fn check_and_file(
    state: &State,
    shared: &share::ShareFile,
    req: &Request,
    file_sha: &str,
    path: &Path,
) -> Result<Value, String> {
    let settings = state.settings();
    let policy = &settings.policy;
    if !policy.allow_shared_tools {
        return Err("your organization's settings don't allow opening shared tools and workflows (allow_shared_tools is off). It can be turned on in the Columbus app's Settings if your organization lets you".into());
    }
    if !shared.tools.is_empty() && !policy.allow_personal_tools {
        return Err(
            "it brings personal tools, and personal tools are off (allow_personal_tools)".into(),
        );
    }
    if shared.kind == "workflow" && !policy.allow_workflows {
        return Err("it's a workflow, and workflows are off (allow_workflows)".into());
    }
    let tools_root = state.tools.root();
    let now = chrono::Local::now().to_rfc3339();
    let imported = json!({
        "from": shared.shared_by,
        "shared_at": shared.shared_at,
        "columbus": shared.columbus,
        "file": path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        "file_sha256": file_sha,
    });
    let installed = store::read_approvals(tools_root);

    // Every tool, checked as propose_tool checks one (but not test-run).
    let mut tools_to_file = Vec::new();
    let mut already = Vec::new();
    for t in &shared.tools {
        let label = t.id.clone();
        if installed.get(&t.id) == Some(&t.hash) {
            already.push(t.id.clone());
            continue;
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
                std::time::Duration::from_secs(60),
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
        tools_to_file.push(ToolToFile {
            id: built.id,
            manifest: built.manifest_text,
            code: built.entry,
            info,
        });
    }

    // The workflow, in the person's own names, checked as propose_workflow
    // checks one.
    let mut workflow_to_file = None;
    if let Some(text) = &shared.workflow {
        let w = wf::check(text)?;
        let mine = share::remap(&w, &req.connections, req.export.as_deref())?;
        let w2 = wf::check(&mine)?;
        let brings: BTreeSet<String> = tools_to_file.iter().map(|t| t.id.clone()).collect();
        let problems =
            crate::workflows::workflow_problems(&settings, None, tools_root, &w2, &brings);
        if !problems.is_empty() {
            return Err(format!(
                "the workflow can't be filed here:\n- {}",
                problems.join("\n- ")
            ));
        }
        let root = wf::workflows_dir();
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
        workflow_to_file = Some((root, mine, info, w2.id));
    } else if !req.connections.is_empty() || req.export.is_some() {
        return Err("a shared tool takes no connections or export folder".into());
    }

    // Filed only once everything has passed.
    let mut filed = Vec::new();
    for t in &tools_to_file {
        store::write_proposal(
            tools_root,
            &t.id,
            t.manifest.as_bytes(),
            t.code.as_bytes(),
            &t.info,
        )?;
        filed.push(json!({"kind": "tool", "id": t.id}));
    }
    if let Some((root, text, info, id)) = &workflow_to_file {
        wf::write_proposal(root, text, info)?;
        filed.push(json!({"kind": "workflow", "id": id}));
    }
    Ok(json!({"ok": true, "filed": filed, "already_installed": already}))
}
