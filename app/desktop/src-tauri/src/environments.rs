//! Settings › Python environments: the person's registered Python environments
//! (build brief Part 5, F-4, REQ-11), and the environments their
//! assistant proposed (T-7).
//!
//! Registering, confirming again, removing and approving a proposal are
//! the person's own actions at the keyboard: each is confirmed with Touch
//! ID or the account password on macOS, Windows Hello on Windows (the
//! connection-confirmation path, `settings::confirm`), and only then do the
//! shared library functions run, passing the id as what was typed (as
//! `columbus-mcp env add|confirm|remove|approve` do after the id is typed
//! on the terminal). The assistant has no way to call any of this: only
//! the app's own main window may (capabilities/main.json). Declining a
//! proposal needs no confirmation.

use std::path::{Path, PathBuf};

use columbus_shared::env_create as create;
use columbus_shared::environments::{self as envs, Confinement, CreatedBy, Registration, Trust};
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::settings::{confirm, confirm_with, policy_now, ONE_AT_A_TIME};

fn now() -> String {
    columbus_shared::tools::now()
}

/// One registration as the page shows it.
fn view_json(v: &envs::View) -> Value {
    match &v.env {
        Some(e) => json!({
            "id": v.id,
            "status": v.status.as_str(),
            "reason": v.status.reason(),
            "python": e.python,
            "trust": e.trust.as_str(),
            "trust_line": e.trust_line(),
            "confinement": e.confinement.as_str(),
            "created_by": e.created_by.as_str(),
            "description": e.description,
            "python_version": e.probe.python_version,
            "arcpy": e.probe.arcpy,
            "arcpy_line": e.arcpy_line(),
            "confirmed_at": e.confirmed_at,
        }),
        None => json!({
            "id": v.id,
            "status": v.status.as_str(),
            "reason": v.status.reason(),
        }),
    }
}

/// What the probe found, in the words the confirmation prompt shows.
fn found_words(found: &envs::Found) -> String {
    let arcpy = if found.arcpy {
        format!(
            "arcpy: yes {}; ArcGIS Pro licence: {}",
            envs::EXPERIMENTAL,
            envs::licence_words(found.licence.as_deref())
        )
    } else {
        "arcpy: no".into()
    };
    format!(
        "Python {}, {} packages, {arcpy}",
        found.python_version,
        found.packages.len()
    )
}

fn trust_words(trust: Trust, confinement: Confinement) -> String {
    match trust {
        Trust::Governed => format!(
            "governed: scripts run confined at level {}, {}, extracts only",
            confinement.as_str(),
            if confinement == Confinement::B {
                "able to reach the internet"
            } else {
                "no network"
            }
        ),
        Trust::Trusted => format!(
            "trusted {}: scripts run unconfined, as you, each run approved. {}",
            envs::EXPERIMENTAL,
            envs::TRUSTED_CARD
        ),
    }
}

/// The registrations, the proposals waiting, and what policy allows.
#[tauri::command]
pub async fn settings_environments() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let dir = envs::environments_dir();
        let p = policy_now().environments;
        let list: Vec<Value> = envs::list(&dir).iter().map(view_json).collect();
        let proposals: Vec<Value> = create::list_proposals(&dir)
            .iter()
            .map(|(p, _)| create::describe(p))
            .collect();
        Ok(json!({
            "dir": dir.display().to_string(),
            "envs_dir": envs::envs_dir().display().to_string(),
            "allow": p.allow,
            "allow_trusted": p.allow_trusted,
            "allow_network": p.allow_network,
            "index_hosts": p.index_hosts,
            "environments": list,
            "proposals": proposals,
            "confirm_with": confirm_with(),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Settings › Python environments › Add: probe the interpreter, then register it
/// once the person confirms what was found.
#[tauri::command]
pub async fn settings_env_add(
    id: String,
    python: String,
    trust: String,
    confinement: Option<String>,
    description: Option<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        add(&id, &python, &trust, confinement.as_deref(), description)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn add(
    id: &str,
    python: &str,
    trust: &str,
    confinement: Option<&str>,
    description: Option<String>,
) -> Result<Value, String> {
    let id = id.trim();
    let python = PathBuf::from(python.trim());
    let trust = Trust::parse(trust).ok_or("trust must be governed or trusted")?;
    let confinement = match confinement.map(str::trim).filter(|c| !c.is_empty()) {
        None => Confinement::A,
        Some(c) => Confinement::parse(c).ok_or("confinement must be A or B")?,
    };
    let p = policy_now().environments;
    // Refused before anything runs, as the terminal does.
    if !p.allow {
        return Err("organization policy does not allow environments (environments.allow is off), so none can be registered".into());
    }
    if !envs::id_ok(id) {
        return Err(format!(
            "{id:?} is not an environment id (lowercase letters, digits and -, at most 40)"
        ));
    }
    if trust == Trust::Trusted && !p.allow_trusted {
        return Err("organization policy does not allow trusted environments (environments.allow_trusted is off); register it as governed".into());
    }
    if confinement == Confinement::B && !p.allow_network {
        return Err("organization policy does not allow governed environments network access (environments.allow_network is off); use confinement A".into());
    }
    let dir = envs::environments_dir();
    if envs::ids(&dir).iter().any(|i| i == id) || dir.join(format!("{id}.yaml")).exists() {
        return Err(format!(
            "an environment {id:?} is already registered; remove it first, or confirm it again"
        ));
    }
    envs::check_interpreter(&python)?;
    let found = envs::probe(&python, envs::probe_limit())?;
    confirm(&format!(
        "register the Python environment {id} ({}: {}; {}) so your assistant's scripts can run in it",
        python.display(),
        found_words(&found),
        trust_words(trust, confinement)
    ))?;
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let reg = Registration {
        id: id.to_string(),
        python,
        trust,
        confinement,
        created_by: CreatedBy::Person,
        description: description
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty()),
    };
    // The id is what the person confirmed, as the terminal's typed id.
    envs::register(&dir, &envs::envs_dir(), &p, &reg, &found, id, &now())?;
    Ok(view_json(&envs::view(&dir, id)))
}

/// Confirm an environment again (after it was marked `changed`): probe it
/// as its file stands now, and record it once the person confirms.
#[tauri::command]
pub async fn settings_env_confirm(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = policy_now().environments;
        if !p.allow {
            return Err(
                "organization policy does not allow environments (environments.allow is off)"
                    .into(),
            );
        }
        let dir = envs::environments_dir();
        let env = envs::pending(&dir, &id)?;
        let python = PathBuf::from(&env.python);
        envs::check_interpreter(&python)?;
        let found = envs::probe(&python, envs::probe_limit())?;
        confirm(&format!(
            "confirm the Python environment {id} as it is now ({}: {}; {}, created by {})",
            env.python,
            found_words(&found),
            trust_words(env.trust, env.confinement),
            env.created_by.as_str()
        ))?;
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        envs::confirm(&dir, &envs::envs_dir(), &p, &id, &found, &id, &now())?;
        Ok(view_json(&envs::view(&dir, &id)))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove a registration (and the folder of one Columbus created), after
/// the person confirms. A folder the person registered is never deleted.
#[tauri::command]
pub async fn settings_env_remove(id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = envs::environments_dir();
        let root = envs::envs_dir();
        let plan = envs::removal(&dir, &root, &id)?;
        confirm(&match &plan.folder {
            Some(f) => format!(
                "remove the Python environment {id} and delete its folder {}, which Columbus created",
                f.display()
            ),
            None => format!(
                "remove the Python environment {id} (its interpreter folder is left as it is)"
            ),
        })?;
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let done = envs::remove(&dir, &root, &id, &id)?;
        Ok(if done.folder.is_some() {
            format!("Removed {id} and its folder.")
        } else {
            format!("Removed {id}. Its interpreter folder is left as it is.")
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Approve an environment the assistant proposed: after the person
/// confirms, Columbus creates it with its own uv and registers it
/// governed (`env_create::approve`, bound to the proposal they were shown
/// by its fingerprint). Takes as long as the downloads do.
#[tauri::command]
pub async fn settings_env_approve(
    app: AppHandle,
    id: String,
    fingerprint: String,
) -> Result<Value, String> {
    let engine = engine(&app)?;
    tauri::async_runtime::spawn_blocking(move || approve(&engine, &id, &fingerprint))
        .await
        .map_err(|e| e.to_string())?
}

/// The engine's Python, whose uv creates environments.
fn engine(app: &AppHandle) -> Result<PathBuf, String> {
    let (_, engine) = crate::registry::program(app)?;
    engine.ok_or_else(|| "the GIS engine was not found, so Columbus cannot create environments: set it up under Settings › Assistant".into())
}

fn approve(engine: &Path, id: &str, fingerprint: &str) -> Result<Value, String> {
    let p = policy_now().environments;
    if !p.allow {
        return Err("organization policy does not allow environments (environments.allow is off), so none can be created".into());
    }
    let dir = envs::environments_dir();
    let (prop, fp) = create::read_proposal(&dir, id)?;
    if fp != fingerprint {
        return Err(
            "the proposal changed after you looked at it; nothing was created. Look again".into(),
        );
    }
    create::check_proposal(&prop, &p)?;
    let root = envs::envs_dir();
    confirm(&format!(
        "create the Python environment {id} in {} (Python {}; {}) from {} only, wheels only, and register it governed",
        root.join(id).display(),
        prop.python_version.as_deref().unwrap_or("the engine's own version"),
        prop.packages.join(", "),
        p.index_hosts.join(", ")
    ))?;
    let env = create::approve(&dir, &root, &p, engine, id, fingerprint, id, &now())?;
    Ok(view_json(&envs::view(&dir, &env.id)))
}

/// Decline a proposal: it is deleted. Nothing else changes, so nothing is
/// confirmed.
#[tauri::command]
pub async fn settings_env_decline(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || create::decline(&envs::environments_dir(), &id))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_say_what_is_confirmed() {
        let found = envs::Found {
            python_version: "3.12.4".into(),
            prefixes: vec![],
            site_dirs: vec![],
            packages: vec!["six==1.17.0".into()],
            conda_meta: None,
            arcpy: true,
            licence: Some("ArcInfo".into()),
        };
        let w = found_words(&found);
        assert!(
            w.contains("Python 3.12.4") && w.contains("1 packages"),
            "{w}"
        );
        assert!(w.contains("ArcGIS Pro licence: Advanced"), "{w}");
        assert!(trust_words(Trust::Governed, Confinement::A).contains("no network"));
        assert!(trust_words(Trust::Trusted, Confinement::A).contains(envs::TRUSTED_CARD));
    }
}
