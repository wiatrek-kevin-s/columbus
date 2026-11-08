//! Documents > Import: a Word, Excel, PDF, CSV, Markdown or text file the
//! person chooses becomes a draft entry in their notes, through Columbus's
//! server (`columbus-mcp notes import`): previewed first (nothing written),
//! then added with the original kept beside it.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::AppHandle;

const KINDS: &[&str] = &["docx", "xlsx", "pdf", "csv", "md", "markdown", "txt"];

/// The document last chosen in the file dialog and previewed: the only one
/// that can be added (the page never names a file itself).
static PICKED: std::sync::Mutex<Option<(std::path::PathBuf, String)>> = std::sync::Mutex::new(None);

fn ask(app: &AppHandle, req: Value) -> Result<Value, String> {
    let (server, engine) = crate::registry::program(app)?;
    let text = crate::registry::run(
        &server,
        engine.as_deref(),
        &["notes", "import", &req.to_string()],
        Duration::from_secs(180),
    )?;
    let v: Value = serde_json::from_str(text.trim())
        .map_err(|_| "Columbus's answer could not be read".to_string())?;
    if v["ok"] == true {
        Ok(v)
    } else {
        Err(v["error"]
            .as_str()
            .unwrap_or("the document could not be imported")
            .to_string())
    }
}

/// Choose a document and preview what it would become. None if cancelled.
#[tauri::command]
pub async fn doc_preview(app: AppHandle) -> Result<Option<Value>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Import a document into your notes")
        .add_filter("Documents", KINDS)
        .blocking_pick_file();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let v = ask(&app, json!({"file": path, "preview": true}))?;
        if let (Ok(mut p), Some(sha)) = (PICKED.lock(), v["sha256"].as_str()) {
            *p = Some((path.clone(), sha.to_string()));
        }
        Ok(Some(v))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Deserialize)]
pub struct ImportForm {
    sha256: String,
    kind: String,
    title: String,
    summary: String,
    when_to_use: Option<String>,
}

/// Add the previewed document as a draft note.
#[tauri::command]
pub async fn doc_import(app: AppHandle, form: ImportForm) -> Result<Value, String> {
    let picked = PICKED.lock().ok().and_then(|p| p.clone());
    let Some((path, sha)) = picked.filter(|(_, sha)| *sha == form.sha256) else {
        return Err("choose and preview the document again".into());
    };
    tauri::async_runtime::spawn_blocking(move || {
        let v = ask(
            &app,
            json!({"file": path, "sha256": sha, "kind": form.kind, "title": form.title,
                "summary": form.summary, "when_to_use": form.when_to_use.filter(|w| !w.trim().is_empty())}),
        )?;
        if let Ok(mut p) = PICKED.lock() {
            *p = None;
        }
        Ok(v)
    })
    .await
    .map_err(|e| e.to_string())?
}
