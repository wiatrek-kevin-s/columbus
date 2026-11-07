//! Data groups in the app: the person's own (made and edited here) and the
//! organization's (from its context pack, read only). A group can only
//! narrow what each connection allows; Columbus's server enforces it in the
//! chats of a project that names it.

use columbus_shared::datagroups::{self as dg, Group, Member};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Serialize)]
pub struct GroupsState {
    dir: String,
    groups: Vec<Group>,
    problems: Vec<(String, String)>,
    /// The organization's groups: {id, title, summary, datasets: [id]}.
    organization: Vec<Value>,
    /// The person's connections: {id, kind}.
    connections: Vec<Value>,
}

fn connections() -> Vec<Value> {
    let dir = columbus_shared::paths::connections_dir();
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
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
                .map(|c| (c.id, c.kind))
        })
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out.into_iter()
        .map(|(id, kind)| json!({"id": id, "kind": kind}))
        .collect()
}

/// The organization's groups, as its pack lists them (read only).
pub(crate) fn organization() -> Vec<Value> {
    let root = std::path::PathBuf::from(crate::shell::places().context).join("data-groups");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "yaml" || x == "yml") {
            continue;
        }
        let Ok(m) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if !m.is_file() || m.len() > 256 * 1024 {
            continue;
        }
        let Ok(v) = std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_yaml::from_str::<Value>(&t).map_err(|e| e.to_string()))
        else {
            continue;
        };
        let datasets: Vec<Value> = v["datasets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|d| d.get("id").cloned().unwrap_or_else(|| d.clone()))
                    .collect()
            })
            .unwrap_or_default();
        out.push(json!({
            "id": v["id"], "title": v["title"], "summary": v["summary"], "datasets": datasets,
        }));
    }
    out.sort_by_key(|v| v["id"].as_str().unwrap_or("").to_string());
    out
}

fn state() -> GroupsState {
    let (groups, problems) = dg::list();
    GroupsState {
        dir: dg::groups_dir().display().to_string(),
        groups,
        problems,
        organization: organization(),
        connections: connections(),
    }
}

#[tauri::command]
pub async fn datagroups_state() -> Result<GroupsState, String> {
    tauri::async_runtime::spawn_blocking(state)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
pub struct GroupForm {
    /// Absent for a new group (its id is made from its name).
    id: Option<String>,
    name: String,
    #[serde(default)]
    description: String,
    datasets: Vec<Member>,
}

fn save(form: GroupForm) -> Result<Group, String> {
    let known: Vec<String> = connections()
        .iter()
        .filter_map(|c| c["id"].as_str().map(String::from))
        .collect();
    if let Some(m) = form
        .datasets
        .iter()
        .find(|m| !known.contains(&m.connection))
    {
        return Err(format!("{} is not one of your connections", m.connection));
    }
    let dir = dg::groups_dir();
    let (id, created) = match form.id {
        Some(id) => {
            let was = dg::load_in(&dir, &id)?;
            (id, was.created)
        }
        None => {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
            let base = columbus_shared::projects::id_from_name(&form.name);
            let org: Vec<String> = organization()
                .iter()
                .filter_map(|g| g["id"].as_str().map(String::from))
                .collect();
            let mut id = base.clone();
            let mut n = 2;
            // A new name, never one of the organization's (a project would
            // not know which was meant), reserved by creating its file.
            loop {
                let taken = org.contains(&id)
                    || std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(dir.join(format!("{id}.yaml")))
                        .is_err();
                if !taken {
                    break;
                }
                if n > 999 {
                    return Err("could not find a free id for this data group".into());
                }
                let tail = format!("-{n}");
                let stem = base[..base.len().min(40 - tail.len())].trim_end_matches('-');
                id = format!("{stem}{tail}");
                n += 1;
            }
            (id, None)
        }
    };
    let new = created.is_none();
    let saved = dg::save_in(
        &dir,
        &Group {
            id: id.clone(),
            name: form.name,
            description: form.description,
            datasets: form.datasets,
            created,
            updated: None,
        },
    );
    if saved.is_err() && new {
        // The reserved, empty file of a new group that didn't save.
        let _ = dg::remove_in(&dir, &id);
    }
    saved
}

#[tauri::command]
pub async fn datagroup_save(form: GroupForm) -> Result<Group, String> {
    tauri::async_runtime::spawn_blocking(move || save(form))
        .await
        .map_err(|e| e.to_string())?
}

/// Remove one of the person's groups. A project that names it then can't
/// start a chat until the group is removed from it too.
#[tauri::command]
pub async fn datagroup_remove(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || dg::remove(&id))
        .await
        .map_err(|e| e.to_string())?
}
