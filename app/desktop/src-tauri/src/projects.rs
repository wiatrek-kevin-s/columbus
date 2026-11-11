//! Projects in the app: the list, a project's instructions, the connections
//! its chats may use and its default export folder. The file is the
//! person's own (`projects/<id>/project.yaml`); the server reads it when a
//! chat in the project starts, and can only narrow what the person has.
//! Changing a project is the person's own business, like their notes: it
//! needs no confirmation at the keyboard.

use columbus_shared::projects::{self as pj, Project};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::settings::policy_now;

#[derive(Serialize)]
pub struct ProjectsState {
    dir: String,
    projects: Vec<Project>,
    /// Project files that could not be read: (id, why).
    problems: Vec<(String, String)>,
    /// The person's connection ids, for the context controls.
    connections: Vec<String>,
    /// The organization's export folders: {id, path, granted}.
    export_folders: Vec<Value>,
}

fn connection_ids() -> Vec<String> {
    let dir = columbus_shared::paths::connections_dir();
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
            if ext != "yaml" && ext != "yml" {
                return None;
            }
            columbus_shared::policy::Connection::load(&p)
                .ok()
                .map(|c| c.id)
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn state() -> ProjectsState {
    let (projects, problems) = pj::list();
    let policy = policy_now();
    ProjectsState {
        dir: pj::projects_dir().display().to_string(),
        projects,
        problems,
        connections: connection_ids(),
        export_folders: policy
            .exports
            .iter()
            .map(|f| json!({"id": f.id, "path": f.path, "granted": f.granted}))
            .collect(),
    }
}

#[tauri::command]
pub async fn projects_state() -> Result<ProjectsState, String> {
    tauri::async_runtime::spawn_blocking(state)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct ProjectForm {
    /// Absent for a new project (its id is made from its name).
    id: Option<String>,
    name: String,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    connections: Vec<String>,
    #[serde(default)]
    export_folder: Option<String>,
    #[serde(default)]
    data_groups: Vec<String>,
}

fn save(form: ProjectForm) -> Result<Project, String> {
    let known = connection_ids();
    if let Some(c) = form.connections.iter().find(|c| !known.contains(c)) {
        return Err(format!("{c} is not one of your connections"));
    }
    let org = crate::datagroups::organization();
    for g in &form.data_groups {
        let theirs = org.iter().any(|o| o["id"].as_str() == Some(g.as_str()));
        if !theirs && columbus_shared::datagroups::load(g).is_err() {
            return Err(format!(
                "{g} is not one of your data groups or your organization's"
            ));
        }
    }
    let folder = form.export_folder.filter(|f| !f.is_empty());
    if let Some(f) = &folder {
        if !policy_now().exports.iter().any(|x| &x.id == f) {
            return Err(format!(
                "{f} is not one of your organization's export folders"
            ));
        }
    }
    let dir = pj::projects_dir();
    let (id, created) = match form.id {
        Some(id) => {
            let was = pj::load_in(&dir, &id)?;
            (id, was.created)
        }
        None => {
            // Its folder is made first, so two saves at once never share an id.
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
            let base = pj::id_from_name(&form.name);
            let mut id = base.clone();
            let mut n = 2;
            loop {
                match std::fs::create_dir(dir.join(&id)) {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => {
                        let tail = format!("-{n}");
                        let stem = base[..base.len().min(40 - tail.len())].trim_end_matches('-');
                        id = format!("{stem}{tail}");
                        n += 1;
                    }
                    Err(e) => return Err(format!("cannot make the project's folder: {e}")),
                }
            }
            (id, None)
        }
    };
    pj::save_in(
        &dir,
        &Project {
            id,
            name: form.name,
            instructions: form.instructions,
            connections: form.connections,
            export_folder: folder,
            data_groups: form.data_groups,
            created,
            updated: None,
        },
    )
}

#[tauri::command]
pub async fn project_save(form: ProjectForm) -> Result<Project, String> {
    tauri::async_runtime::spawn_blocking(move || save(form))
        .await
        .map_err(|e| e.to_string())?
}

/// Remove a project. Its chats stay, under Unfiled; its databases stay in
/// the workspaces folder.
#[tauri::command]
pub async fn project_remove(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || pj::remove(&id))
        .await
        .map_err(|e| e.to_string())?
}

/// The project databases of one project (or, with no project, the shared
/// ones from before projects), as Columbus's server lists them for that
/// project: names, tables, size, and why one is withheld there.
#[tauri::command]
pub async fn project_dbs(app: tauri::AppHandle, project: Option<String>) -> Result<Value, String> {
    let project = project.filter(|p| !p.is_empty());
    if let Some(id) = &project {
        // Only a project that exists: the id goes to the server as data,
        // and it reads the project's file itself.
        pj::load(id)?;
    }
    let (server, engine) = crate::registry::program(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut req = json!({"tool": "project_db", "args": {"action": "list"}});
        if let Some(id) = project {
            req["project"] = json!(id);
        }
        let v = crate::registry::app_query(&server, engine.as_deref(), &req)?;
        let dbs: Vec<Value> = v["result"]["result"]["databases"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|d| d["scope"] == "project")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({"databases": dbs}))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A project's files (build brief 3, D-19, F-17): `projects/<id>/files/`,
/// as the Files section of its page lists them. Names, sizes and times
/// only: the listing never follows a link and opens no file.
#[tauri::command]
pub async fn project_files(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use columbus_shared::project_files as pf;
        // Only a project that exists.
        pj::load(&id)?;
        let dir = pf::files_dir(&id)?;
        let listing = pf::list(&dir, &[], pf::MAX_FILES)
            .map_err(|e| format!("the project's files cannot be listed: {e}"))?;
        Ok(json!({
            "dir": dir.display().to_string(),
            "exists": dir.is_dir(),
            // Whether the assistant may use them (policy allow_project_files).
            "allowed": policy_now().allow_project_files,
            "files": listing.files,
            "folders": listing.folders,
            "skipped": listing.skipped,
            "total_bytes": listing.total_bytes,
            "more": listing.more,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Open a project's files folder in the system's file manager, making it
/// first if it is not there yet, so the person can put their own scripts
/// and notes in it.
#[tauri::command]
pub async fn project_files_open(id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        pj::load(&id)?;
        let dir = columbus_shared::project_files::files_dir(&id)?;
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(format!(
                    "{} is a link or not a folder, so it is not opened",
                    dir.display()
                ))
            }
            Err(_) => std::fs::create_dir(&dir)
                .map_err(|e| format!("cannot make {}: {e}", dir.display()))?,
        }
        open_folder(&dir).map_err(|e| format!("could not open {}: {e}", dir.display()))?;
        Ok(dir.display().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Show a folder in Explorer, Finder or the desktop's file manager.
fn open_folder(dir: &std::path::Path) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    // Tests only (debug builds): the folder is written to a file instead
    // of opening a file manager.
    #[cfg(debug_assertions)]
    if let Some(to) = std::env::var_os("COLUMBUS_APP_TEST_OPEN") {
        return std::fs::write(to, dir.display().to_string());
    }
    let mut cmd = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Command::new(std::path::Path::new(&root).join("explorer.exe"))
    } else if cfg!(target_os = "macos") {
        Command::new("/usr/bin/open")
    } else {
        Command::new("xdg-open")
    };
    let mut child = cmd
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Explorer answers 1 even when it opened the folder: not waited on.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
