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
