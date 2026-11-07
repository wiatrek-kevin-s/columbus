//! Projects: a piece of work with its own instructions for the assistant,
//! the connections its chats may use, and a default export folder.
//!
//! A project is one file, `projects/<id>/project.yaml`, in the person's
//! own folder, written by the Columbus app. A session started for a project
//! (`COLUMBUS_PROJECT=<id>`) can only narrow what the person already has:
//! its connections are a subset of theirs, and its instructions are text
//! for the assistant, never rules Columbus relaxes anything for.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The longest instructions a project keeps (they go to the assistant in
/// every chat of the project).
pub const MAX_INSTRUCTIONS: usize = 8000;
/// The most connections a project lists.
pub const MAX_CONNECTIONS: usize = 64;
const FILE: &str = "project.yaml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// Given to the assistant in every chat of the project.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    /// The connections its chats may use; empty means every connection the
    /// person has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<String>,
    /// The export folder (an id from the policy's export folders) its
    /// results go to unless the person says otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

/// The person's projects folder: `COLUMBUS_PROJECTS`, else `projects/`
/// beside the tools folder.
pub fn projects_dir() -> PathBuf {
    if let Some(d) = crate::paths::env_path("COLUMBUS_PROJECTS") {
        return d;
    }
    let t = crate::tools::tools_dir();
    t.parent()
        .map(Path::to_path_buf)
        .unwrap_or(t)
        .join("projects")
}

/// A project's id: 3 to 40 lowercase letters, digits and single hyphens,
/// starting with a letter (as a workflow's).
pub fn id_ok(id: &str) -> bool {
    (3..=40).contains(&id.len())
        && id.starts_with(|c: char| c.is_ascii_lowercase())
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn plain_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// An id made from a name: "Roadway QC 2027" → "roadway-qc-2027".
pub fn id_from_name(name: &str) -> String {
    let mut id = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
        if id.len() >= 40 {
            break;
        }
    }
    let mut id = id.trim_end_matches('-').to_string();
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id.insert_str(0, "p-");
    }
    while id.len() < 3 {
        id.push('x');
    }
    id.truncate(40);
    id.trim_end_matches('-').to_string()
}

impl Project {
    /// What is wrong with it, if anything.
    pub fn check(&self) -> Result<(), String> {
        if !id_ok(&self.id) {
            return Err("a project's id is 3 to 40 lowercase letters, digits and single hyphens, starting with a letter".into());
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err("a project's name is one line of at most 80 characters".into());
        }
        if self.instructions.chars().count() > MAX_INSTRUCTIONS {
            return Err(format!(
                "a project's instructions are at most {MAX_INSTRUCTIONS} characters"
            ));
        }
        if self
            .instructions
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err("a project's instructions are plain text".into());
        }
        if self.connections.len() > MAX_CONNECTIONS {
            return Err(format!(
                "a project lists at most {MAX_CONNECTIONS} connections"
            ));
        }
        for c in &self.connections {
            if !plain_id_ok(c) {
                return Err(format!("{c:?} is not a connection id"));
            }
        }
        if let Some(f) = &self.export_folder {
            if !plain_id_ok(f) {
                return Err(format!("{f:?} is not an export folder id"));
            }
        }
        Ok(())
    }
}

fn file_in(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a project id"));
    }
    Ok(dir.join(id).join(FILE))
}

/// One project, from its file (only a plain file: never a link).
pub fn load_in(dir: &Path, id: &str) -> Result<Project, String> {
    let path = file_in(dir, id)?;
    // Its folder is the project's own, not a link to somewhere else.
    if let Ok(m) = std::fs::symlink_metadata(dir.join(id)) {
        if !m.is_dir() {
            return Err(format!("project {id:?}'s folder is a link or not a folder"));
        }
    }
    let meta = std::fs::symlink_metadata(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("there is no project {id:?}")
        } else {
            format!("project {id:?} cannot be read: {e}")
        }
    })?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        return Err(format!("project {id:?}'s file is not a plain, small file"));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("project {id:?}: {e}"))?;
    let p: Project =
        serde_yaml::from_str(&text).map_err(|e| format!("project {id:?}'s file: {e}"))?;
    if p.id != id {
        return Err(format!("project {id:?}'s file names another id"));
    }
    p.check()?;
    Ok(p)
}

pub fn load(id: &str) -> Result<Project, String> {
    load_in(&projects_dir(), id)
}

/// Every project, most recently changed first, and the ones that could not
/// be read (id, why).
pub fn list_in(dir: &Path) -> (Vec<Project>, Vec<(String, String)>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (ok, bad);
    };
    for e in entries.flatten() {
        let Ok(id) = e.file_name().into_string() else {
            continue;
        };
        if !id_ok(&id) || !e.path().join(FILE).exists() {
            continue;
        }
        match load_in(dir, &id) {
            Ok(p) => ok.push(p),
            Err(why) => bad.push((id, why)),
        }
    }
    ok.sort_by(|a, b| {
        b.updated
            .cmp(&a.updated)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    bad.sort();
    (ok, bad)
}

pub fn list() -> (Vec<Project>, Vec<(String, String)>) {
    list_in(&projects_dir())
}

/// Write a project (replacing the one of the same id), stamped now.
pub fn save_in(dir: &Path, p: &Project) -> Result<Project, String> {
    let mut p = p.clone();
    p.name = p.name.trim().to_string();
    p.instructions = p.instructions.trim().to_string();
    p.connections.sort();
    p.connections.dedup();
    p.check()?;
    let now = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    if p.created.is_none() {
        p.created = Some(now.clone());
    }
    p.updated = Some(now);
    let folder = dir.join(&p.id);
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
    if std::fs::symlink_metadata(&folder)
        .map(|m| !m.is_dir())
        .unwrap_or(true)
    {
        return Err(format!("{} is not a plain folder", folder.display()));
    }
    let text = serde_yaml::to_string(&p).map_err(|e| e.to_string())?;
    // Written beside it, then put in place: never half a file.
    let tmp = folder.join(format!(".{FILE}.{}", uuid::Uuid::new_v4().simple()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let wrote = std::io::Write::write_all(&mut f, text.as_bytes())
        .and_then(|()| f.sync_all())
        .and_then(|()| std::fs::rename(&tmp, folder.join(FILE)));
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot save the project: {e}"));
    }
    Ok(p)
}

pub fn save(p: &Project) -> Result<Project, String> {
    save_in(&projects_dir(), p)
}

/// Remove a project's file (its chats and databases are kept; they are the
/// person's work).
pub fn remove_in(dir: &Path, id: &str) -> Result<(), String> {
    let path = file_in(dir, id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => {
            let _ = std::fs::remove_dir(dir.join(id));
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove project {id:?}: {e}")),
    }
}

pub fn remove(id: &str) -> Result<(), String> {
    remove_in(&projects_dir(), id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str) -> Project {
        Project {
            id: id.into(),
            name: "Roadway QC".into(),
            instructions: "Use district 14 only.\nReport mileage in miles.".into(),
            connections: vec!["roads".into(), "roads".into(), "aaa".into()],
            export_folder: Some("outbox".into()),
            created: None,
            updated: None,
        }
    }

    #[test]
    fn saved_loaded_listed_removed() {
        let d = tempfile::tempdir().unwrap();
        let saved = save_in(d.path(), &p("roadway-qc")).unwrap();
        assert_eq!(saved.connections, ["aaa", "roads"]);
        assert!(saved.created.is_some() && saved.updated.is_some());
        let back = load_in(d.path(), "roadway-qc").unwrap();
        assert_eq!(back, saved);
        let (ok, bad) = list_in(d.path());
        assert_eq!(ok.len(), 1);
        assert!(bad.is_empty());
        remove_in(d.path(), "roadway-qc").unwrap();
        assert!(load_in(d.path(), "roadway-qc").is_err());
        assert!(!d.path().join("roadway-qc").exists());
    }

    #[test]
    fn what_is_refused() {
        let d = tempfile::tempdir().unwrap();
        for bad in ["../x", "a", "Has-Caps", "ends-", "two--hyphens"] {
            assert!(save_in(d.path(), &p(bad)).is_err(), "{bad}");
            assert!(load_in(d.path(), bad).is_err(), "{bad}");
        }
        let mut long = p("long-one");
        long.instructions = "x".repeat(MAX_INSTRUCTIONS + 1);
        assert!(save_in(d.path(), &long).is_err());
        let mut odd = p("odd-conn");
        odd.connections = vec!["a/b".into()];
        assert!(save_in(d.path(), &odd).is_err());
        let mut name = p("bad-name");
        name.name = "two\nlines".into();
        assert!(save_in(d.path(), &name).is_err());
        // A file naming another id, or with fields Columbus doesn't know.
        std::fs::create_dir_all(d.path().join("other")).unwrap();
        std::fs::write(
            d.path().join("other/project.yaml"),
            "id: elsewhere\nname: X\n",
        )
        .unwrap();
        assert!(load_in(d.path(), "other").is_err());
        std::fs::write(
            d.path().join("other/project.yaml"),
            "id: other\nname: X\nrules: loose\n",
        )
        .unwrap();
        assert!(load_in(d.path(), "other").is_err());
        let (ok, bad) = list_in(d.path());
        assert!(ok.is_empty());
        assert_eq!(bad.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_file_is_not_read() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("p.yaml"), "id: linked\nname: X\n").unwrap();
        std::fs::create_dir_all(d.path().join("linked")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("p.yaml"),
            d.path().join("linked/project.yaml"),
        )
        .unwrap();
        assert!(load_in(d.path(), "linked").is_err());
    }

    #[test]
    fn ids_from_names() {
        assert_eq!(id_from_name("Roadway QC 2027"), "roadway-qc-2027");
        assert_eq!(id_from_name("2027 bridges"), "p-2027-bridges");
        assert_eq!(id_from_name("!!"), "p-x");
        assert!(id_ok(&id_from_name(&"long name ".repeat(20))));
        assert!(id_ok(&id_from_name("Ünïcode straße")));
    }
}
