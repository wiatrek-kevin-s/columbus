//! Sharing a tool or workflow with a coworker as a `.columbus` file
//! (columbus_shared::share), and opening one a coworker shared.
//!
//! Share writes the file here, from the installed files (no server needed:
//! nothing in it is data). Opening one is read here to show what it holds
//! and which of the person's connections each name needs; filing it runs
//! Columbus's server (`columbus-mcp share import`), which checks it as it
//! checks the person's own assistant's proposals and files it to wait,
//! like those, for the person's review and Touch ID or Windows Hello.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use columbus_shared::{paths, share, tools, workflows as wf};
use serde_json::{json, Value};
use tauri::AppHandle;

fn who() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "a coworker".into())
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect()
}

/// Save one installed tool or workflow as a shared file, where the person
/// chooses. Returns the path saved, or None when they cancel.
#[tauri::command]
pub async fn share_export(
    app: AppHandle,
    kind: String,
    id: String,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let file = tauri::async_runtime::spawn_blocking({
        let (kind, id) = (kind.clone(), id.clone());
        move || {
            let now = tools::now();
            let v = env!("CARGO_PKG_VERSION");
            let s = match kind.as_str() {
                "tool" => share::export_tool(&tools::tools_dir(), &id, &who(), &now, v)?,
                "workflow" => share::export_workflow(
                    &wf::workflows_dir(),
                    &tools::tools_dir(),
                    &id,
                    &who(),
                    &now,
                    v,
                )?,
                _ => return Err("only a tool or a workflow can be shared".to_string()),
            };
            share::to_json(&s)
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    let name = format!(
        "{}.{}",
        id.trim_start_matches(tools::ID_PREFIX),
        share::EXTENSION
    );
    let picked = app
        .dialog()
        .file()
        .set_title("Save the shared file")
        .set_file_name(&name)
        .add_filter("Columbus shared file", &[share::EXTENSION])
        .blocking_save_file();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let path = if path.extension().is_some_and(|e| e == share::EXTENSION) {
        path
    } else {
        path.with_extension(share::EXTENSION)
    };
    std::fs::write(&path, file).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(path.display().to_string()))
}

/// Choose a shared file and read it: what it holds, who shared it, and
/// what it needs from the person (their connections and export folder for
/// each name a workflow uses). Nothing is filed yet.
#[tauri::command]
pub async fn share_open(app: AppHandle) -> Result<Option<Value>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Open a shared tool or workflow")
        .add_filter("Columbus shared file", &[share::EXTENSION])
        .blocking_pick_file();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || describe(&path).map(Some))
        .await
        .map_err(|e| e.to_string())?
}

fn describe(path: &std::path::Path) -> Result<Value, String> {
    let s = share::read(path)?;
    let installed = tools::read_approvals(&tools::tools_dir());
    let tool_list: Vec<Value> = s
        .tools
        .iter()
        .map(|t| {
            let y: serde_yaml::Value = serde_yaml::from_str(&t.manifest).unwrap_or_default();
            let run = y.get("tools").and_then(|x| x.get(tools::TOOL));
            let text = |v: Option<&serde_yaml::Value>| {
                v.and_then(|x| x.as_str()).unwrap_or("").to_string()
            };
            json!({
                "id": t.id,
                "title": text(y.get("name")),
                "version": text(y.get("version")),
                "description": text(run.and_then(|r| r.get("description"))),
                "lines": t.code.lines().count(),
                "already_installed": installed.get(&t.id) == Some(&t.hash),
                "replaces": installed.contains_key(&t.id) && installed.get(&t.id) != Some(&t.hash),
            })
        })
        .collect();
    let workflow = match &s.workflow {
        Some(text) => {
            let w = wf::check(text)?;
            let needs = share::needs(&w);
            json!({"id": w.id, "name": w.name, "purpose": w.purpose, "plain": wf::describe(&w),
                "review": w.review.as_ref().map(|r| r.criteria.clone()), "needs": needs})
        }
        None => Value::Null,
    };
    let policy =
        columbus_shared::policy::OrgPolicy::load(&paths::policy_file()).unwrap_or_default();
    let connections: Vec<String> = crate::settings::connection_ids();
    Ok(json!({
        "path": path.display().to_string(),
        "kind": s.kind,
        "shared_by": s.shared_by,
        "shared_at": s.shared_at,
        "columbus": s.columbus,
        "tools": tool_list,
        "workflow": workflow,
        "allowed": policy.allow_shared_tools,
        "mine": {"connections": connections, "exports": policy.exports.iter().map(|e| e.id.clone()).collect::<Vec<_>>()},
    }))
}

/// File what a shared file holds, as proposals the person then reviews:
/// `connections` maps each name the workflow uses to one of theirs.
#[tauri::command]
pub async fn share_import(
    app: AppHandle,
    path: String,
    connections: BTreeMap<String, String>,
    export: Option<String>,
) -> Result<Value, String> {
    let (server, engine) = crate::registry::program(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let req =
            json!({"file": PathBuf::from(&path), "connections": connections, "export": export})
                .to_string();
        let text = crate::registry::run(
            &server,
            engine.as_deref(),
            &["share", "import", &req],
            Duration::from_secs(120),
        )?;
        let v: Value = serde_json::from_str(text.trim())
            .map_err(|_| "Columbus's answer could not be read".to_string())?;
        if v["ok"] == true {
            Ok(v)
        } else {
            Err(v["error"]
                .as_str()
                .unwrap_or("the shared file could not be opened")
                .to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
