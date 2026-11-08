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
        .filter(|c| tools::plain_chars(&c.to_string()).is_ok() && *c != '\n')
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
) -> Result<Option<Value>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (file, signed) = tauri::async_runtime::spawn_blocking({
        let (kind, id) = (kind.clone(), id.clone());
        move || {
            let now = tools::now();
            let v = env!("CARGO_PKG_VERSION");
            let mut s = match kind.as_str() {
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
            // Signed with the person's own key, when this computer keeps one.
            let signed = match signing_key() {
                Ok(k) => share::sign(&mut s, &k).map(|_| None).unwrap_or_else(Some),
                Err(e) => Some(e),
            };
            Ok::<_, String>((share::to_json(&s)?, signed))
        }
    })
    .await
    .map_err(|e| e.to_string())??;
    let name = format!(
        "{}.{}",
        id.trim_start_matches(tools::ID_PREFIX),
        share::EXTENSION
    );
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Save the shared file")
        .set_file_name(&name)
        .add_filter("Columbus shared file", &[share::EXTENSION]);
    if let Some(team) = team_folder() {
        dialog = dialog.set_directory(team);
    }
    let picked = dialog.blocking_save_file();
    let Some(path) = picked.and_then(|p| p.into_path().ok()) else {
        return Ok(None);
    };
    let path = if path.extension().is_some_and(|e| e == share::EXTENSION) {
        path
    } else {
        // Named here, not in the dialog: never over a file it didn't ask about.
        let p = path.with_extension(share::EXTENSION);
        if p.exists() {
            return Err(format!(
                "{} already exists; choose another name",
                p.display()
            ));
        }
        p
    };
    // Written beside and moved into place: never through a link left where
    // the file goes (a shared folder is anyone's to put one in).
    tools::write_atomic(&path, file.as_bytes())?;
    Ok(Some(json!({
        "path": path.display().to_string(),
        "signed": signed.is_none(),
        "unsigned_why": signed,
    })))
}

// ---- signing keys and trusted senders ----------------------------------

const KEY_SERVICE: &str = "Columbus sharing";
const KEY_ACCOUNT: &str = "signing-key";

/// The person's sharing key (PKCS#8), made the first time they share.
fn signing_key() -> Result<Vec<u8>, String> {
    crate::chat::ensure_store()?;
    let e = keyring_core::Entry::new(KEY_SERVICE, KEY_ACCOUNT).map_err(|e| e.to_string())?;
    match e.get_password() {
        Ok(h) => hex::decode(h.trim())
            .map_err(|_| "the sharing key in the keychain isn't readable".to_string()),
        Err(keyring_core::Error::NoEntry) => {
            let k = share::new_key()?;
            e.set_password(&hex::encode(&k))
                .map_err(|e| e.to_string())?;
            Ok(k)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The person's own sharing key, to read to a coworker (made if needed).
#[tauri::command]
pub async fn share_my_key() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let k = signing_key()?;
        let public = share::public_key(&k)?;
        Ok(json!({"key": public, "fingerprint": share::fingerprint(&public)}))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Trust a sender's key under a name: files they sign then show as theirs.
#[tauri::command]
pub fn share_trust(name: String, key: String) -> Result<(), String> {
    let policy =
        columbus_shared::policy::OrgPolicy::load(&paths::policy_file()).unwrap_or_default();
    share::trust(
        &share::trust_file(),
        &name,
        &key,
        &tools::now(),
        &policy.share_signers,
    )
}

#[tauri::command]
pub fn share_untrust(key: String) -> Result<(), String> {
    share::untrust(&share::trust_file(), &key)
}

// ---- the team library --------------------------------------------------

fn settings_file() -> PathBuf {
    let f = share::trust_file();
    f.with_file_name("sharing.json")
}

/// The team library folder, if the person chose one and it is there.
fn team_folder() -> Option<PathBuf> {
    let v: Value = serde_json::from_slice(&std::fs::read(settings_file()).ok()?).ok()?;
    let p = PathBuf::from(v["team_folder"].as_str()?);
    p.is_dir().then_some(p)
}

/// Sharing as the person has it set up: the team folder, who they trust,
/// whom their organization vouches for, and whether it requires signatures.
#[tauri::command]
pub fn share_settings() -> Value {
    let policy =
        columbus_shared::policy::OrgPolicy::load(&paths::policy_file()).unwrap_or_default();
    let named = |t: &share::TrustedSender| json!({"name": t.name, "key": t.key, "fingerprint": share::fingerprint(&t.key), "added": t.added});
    json!({
        "team_folder": team_folder().map(|p| p.display().to_string()),
        "trusted": share::trusted(&share::trust_file()).iter().map(named).collect::<Vec<_>>(),
        "organization": policy.share_signers.iter().map(named).collect::<Vec<_>>(),
        "require_signed": policy.require_signed_shares,
    })
}

/// Choose (or, with `clear`, stop using) the team library folder.
#[tauri::command]
pub async fn share_team_folder(
    app: AppHandle,
    clear: Option<bool>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let chosen = if clear == Some(true) {
        None
    } else {
        let picked = app
            .dialog()
            .file()
            .set_title("Choose the team library folder (a shared drive or a synced folder)")
            .blocking_pick_folder();
        match picked.and_then(|p| p.into_path().ok()) {
            Some(p) => Some(p),
            None => return Ok(team_folder().map(|p| p.display().to_string())),
        }
    };
    let v = json!({"team_folder": chosen.as_ref().map(|p| p.display().to_string())});
    tools::write_atomic(&settings_file(), v.to_string().as_bytes())?;
    Ok(chosen.map(|p| p.display().to_string()))
}

/// The library's file of this name: a plain `.columbus` file directly in
/// the folder (no other folder, no link).
fn team_file(name: &str) -> Result<PathBuf, String> {
    let dir = team_folder().ok_or("choose a team library folder first")?;
    if name.is_empty()
        || name.contains(['/', '\\', ':'])
        || name.starts_with('.')
        || !name.ends_with(&format!(".{}", share::EXTENSION))
    {
        return Err("not a file in the team library".into());
    }
    let p = dir.join(name);
    match std::fs::symlink_metadata(&p) {
        Ok(m) if m.is_file() => Ok(p),
        _ => Err("not a file in the team library".into()),
    }
}

/// The shared files in the team library: what each holds, who signed it,
/// and how it stands against what the person has installed.
#[tauri::command]
pub async fn share_team_list() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(dir) = team_folder() else {
            return Ok(json!({"folder": null, "files": []}));
        };
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .filter(|n| n.ends_with(&format!(".{}", share::EXTENSION)) && !n.starts_with('.'))
            .collect();
        names.sort();
        let more = names.len() > 100;
        names.truncate(100);
        let ctx = here();
        let files: Vec<Value> = names
            .iter()
            .map(|n| match team_file(n).and_then(|p| describe_in(&p, &ctx)) {
                Ok(mut v) => {
                    v["name"] = json!(n);
                    v
                }
                Err(e) => json!({"name": n, "problem": e}),
            })
            .collect();
        Ok(json!({"folder": dir.display().to_string(), "files": files, "more": more}))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Open a file from the team library, as Open a shared file does.
#[tauri::command]
pub async fn share_team_open(name: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || describe(&team_file(&name)?))
        .await
        .map_err(|e| e.to_string())?
}

/// Tools the organization installed for everyone on this computer.
#[tauri::command]
pub fn org_tools_list() -> Value {
    let policy =
        columbus_shared::policy::OrgPolicy::load(&paths::policy_file()).unwrap_or_default();
    let root = tools::org_tools_dir();
    json!({
        "on": policy.allow_org_tools,
        "folder": root.display().to_string(),
        "tools": tools::list_installed(&root),
    })
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

/// What describing a shared file compares it with, read once for many.
struct Here {
    troot: PathBuf,
    installed: Vec<tools::ToolView>,
    waiting: Vec<String>,
    workflows: Vec<String>,
    workflows_waiting: Vec<String>,
    policy: columbus_shared::policy::OrgPolicy,
    connections: Vec<String>,
    trusted: Vec<share::TrustedSender>,
}

fn here() -> Here {
    let troot = tools::tools_dir();
    let root = wf::workflows_dir();
    Here {
        installed: tools::list_installed(&troot),
        waiting: tools::list_proposals(&troot)
            .into_iter()
            .map(|t| t.id)
            .collect(),
        workflows: wf::list_installed(&root, &troot)
            .into_iter()
            .map(|x| x.id)
            .collect(),
        workflows_waiting: wf::list_proposals(&root, &troot)
            .into_iter()
            .map(|x| x.id)
            .collect(),
        policy: columbus_shared::policy::OrgPolicy::load(&paths::policy_file()).unwrap_or_default(),
        connections: crate::settings::connection_ids(),
        trusted: share::trusted(&share::trust_file()),
        troot,
    }
}

fn describe(path: &std::path::Path) -> Result<Value, String> {
    describe_in(path, &here())
}

fn describe_in(path: &std::path::Path, here: &Here) -> Result<Value, String> {
    let bytes = share::read_bytes(path)?;
    let sha256 = columbus_shared::canon::sha256_hex(&bytes);
    let s = share::parse_bytes(&bytes)?;
    let installed: BTreeMap<String, (String, bool)> = here
        .installed
        .iter()
        .map(|t| {
            (
                t.id.clone(),
                (t.hash.clone(), t.state.as_deref() == Some("ready")),
            )
        })
        .collect();
    let waiting = &here.waiting;
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
                "already_installed": installed.get(&t.id).is_some_and(|(h, ready)| *h == t.hash && *ready),
                "replaces": installed.get(&t.id).is_some_and(|(h, ready)| *h != t.hash || !*ready),
                "waiting": waiting.contains(&t.id),
            })
        })
        .collect();
    let workflow = match &s.workflow {
        Some(text) => {
            let w = wf::check(text)?;
            let needs = share::needs(&w);
            let replaces = here.workflows.contains(&w.id);
            let waiting = here.workflows_waiting.contains(&w.id);
            json!({"id": w.id, "name": w.name, "purpose": w.purpose, "plain": wf::describe(&w),
                "review": w.review.as_ref().map(|r| r.criteria.clone()), "needs": needs,
                "replaces": replaces, "waiting": waiting})
        }
        None => Value::Null,
    };
    let policy = &here.policy;
    let connections = &here.connections;
    let _ = &here.troot;
    Ok(json!({
        "path": path.display().to_string(),
        "file": path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        "sha256": sha256,
        "kind": s.kind,
        "shared_by": s.shared_by,
        "shared_at": s.shared_at,
        "signer": share::signer(&s, &here.trusted, &policy.share_signers),
        "require_signed": policy.require_signed_shares,
        "columbus": s.columbus,
        "tools": tool_list,
        "workflow": workflow,
        "allowed": policy.allow_shared_tools,
        "mine": {"connections": connections, "exports": policy.exports.iter().map(|e| json!({"id": e.id, "path": e.path.display().to_string()})).collect::<Vec<_>>()},
    }))
}

/// File what a shared file holds, as proposals the person then reviews:
/// `connections` maps each name the workflow uses to one of theirs.
#[tauri::command]
pub async fn share_import(
    app: AppHandle,
    path: String,
    sha256: String,
    connections: BTreeMap<String, String>,
    export: Option<String>,
) -> Result<Value, String> {
    let (server, engine) = crate::registry::program(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let req =
            json!({"file": PathBuf::from(&path), "sha256": sha256, "connections": connections, "export": export})
                .to_string();
        let text = crate::registry::run(
            &server,
            engine.as_deref(),
            &["share", "import", &req],
            Duration::from_secs(900),
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
