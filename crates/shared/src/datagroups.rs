//! The person's own data groups: a named set of datasets across their
//! connections, each with an optional filter, that a project can attach so
//! its chats see only those datasets, and only those rows.
//!
//! One file per group, `data-groups/<id>.yaml`, in the person's own folder,
//! written by the Columbus app. A group can only narrow: it never makes a
//! dataset readable that its connection doesn't allow, and its filters are
//! ANDed onto the connection's own row filter. The organization's groups
//! come from its context pack (`data-groups/*.yaml` there) and are read only.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::policy::{DataGroup, GroupDataset, Mode};
use crate::sqlcheck::{check_where, SqlDialect};

/// The most datasets one group lists.
pub const MAX_DATASETS: usize = 200;
const MAX_FILTER: usize = 2000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub datasets: Vec<Member>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

/// One dataset of a group: which connection, its name there, and an
/// optional filter (a where clause) on its rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub connection: String,
    pub dataset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

/// The person's data groups folder: `COLUMBUS_DATA_GROUPS`, else
/// `data-groups/` beside the tools folder.
pub fn groups_dir() -> PathBuf {
    if let Some(d) = crate::paths::env_path("COLUMBUS_DATA_GROUPS") {
        return d;
    }
    let t = crate::tools::tools_dir();
    t.parent()
        .map(Path::to_path_buf)
        .unwrap_or(t)
        .join("data-groups")
}

/// A group's id: as a project's (3 to 40 lowercase letters, digits and
/// single hyphens, starting with a letter).
pub fn id_ok(id: &str) -> bool {
    crate::projects::id_ok(id)
}

fn plain_id_ok(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

impl Group {
    /// What is wrong with it, if anything. Filters are checked here in a
    /// general SQL form, and again for each connection's own dialect when a
    /// session uses the group.
    pub fn check(&self) -> Result<(), String> {
        if !id_ok(&self.id) {
            return Err("a data group's id is 3 to 40 lowercase letters, digits and single hyphens, starting with a letter".into());
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err("a data group's name is one line of at most 80 characters".into());
        }
        if self.description.chars().count() > 2000
            || self
                .description
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(
                "a data group's description is plain text of at most 2,000 characters".into(),
            );
        }
        if self.datasets.is_empty() {
            return Err("a data group lists at least one dataset".into());
        }
        if self.datasets.len() > MAX_DATASETS {
            return Err(format!(
                "a data group lists at most {MAX_DATASETS} datasets"
            ));
        }
        for m in &self.datasets {
            if !plain_id_ok(&m.connection) {
                return Err(format!("{:?} is not a connection id", m.connection));
            }
            let d = m.dataset.trim();
            if d.is_empty() || d.chars().count() > 256 || d.chars().any(char::is_control) {
                return Err(format!("{:?} is not a dataset name", m.dataset));
            }
            if let Some(f) = &m.filter {
                if f.chars().count() > MAX_FILTER {
                    return Err(format!(
                        "the filter on {d} is longer than {MAX_FILTER} characters"
                    ));
                }
                check_where(f, SqlDialect::Generic)
                    .map_err(|e| format!("the filter on {d} is not one Columbus runs: {e}"))?;
            }
        }
        Ok(())
    }

    /// The group as the server's scope takes it: one `DataGroup` per
    /// connection, each filter checked again in that connection's dialect
    /// (`kind_of` gives a connection's type; a connection the person no
    /// longer has is refused).
    pub fn to_scope(
        &self,
        kind_of: impl Fn(&str) -> Option<String>,
    ) -> Result<Vec<DataGroup>, String> {
        let mut by_conn: std::collections::BTreeMap<String, Vec<GroupDataset>> = Default::default();
        for m in &self.datasets {
            let kind = kind_of(&m.connection).ok_or_else(|| {
                format!(
                    "data group {} uses {}, which is not one of your connections",
                    self.id, m.connection
                )
            })?;
            let filter = match &m.filter {
                Some(f) if !f.trim().is_empty() => Some(
                    check_where(f, SqlDialect::for_connection_type(&kind)).map_err(|e| {
                        format!(
                            "data group {}: the filter on {} is not one Columbus runs: {e}",
                            self.id, m.dataset
                        )
                    })?,
                ),
                _ => None,
            };
            by_conn
                .entry(m.connection.clone())
                .or_default()
                .push(GroupDataset {
                    physical: m.dataset.trim().to_string(),
                    filter,
                });
        }
        Ok(by_conn
            .into_iter()
            .map(|(connection, datasets)| DataGroup {
                id: self.id.clone(),
                connection,
                datasets,
                mode: Mode::Read,
            })
            .collect())
    }
}

fn file_in(dir: &Path, id: &str) -> Result<PathBuf, String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a data group id"));
    }
    Ok(dir.join(format!("{id}.yaml")))
}

/// One group, from its file (only a plain, small file: never a link).
pub fn load_in(dir: &Path, id: &str) -> Result<Group, String> {
    let path = file_in(dir, id)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("there is no data group {id:?}")
        } else {
            format!("data group {id:?} cannot be read: {e}")
        }
    })?;
    if !meta.is_file() || meta.len() > 256 * 1024 {
        return Err(format!(
            "data group {id:?}'s file is not a plain, small file"
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("data group {id:?}: {e}"))?;
    let g: Group =
        serde_yaml::from_str(&text).map_err(|e| format!("data group {id:?}'s file: {e}"))?;
    if g.id != id {
        return Err(format!("data group {id:?}'s file names another id"));
    }
    g.check()?;
    Ok(g)
}

pub fn load(id: &str) -> Result<Group, String> {
    load_in(&groups_dir(), id)
}

/// Every group, by name, and the ones that could not be read (id, why).
pub fn list_in(dir: &Path) -> (Vec<Group>, Vec<(String, String)>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (ok, bad);
    };
    for e in entries.flatten() {
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let Some(id) = name.strip_suffix(".yaml") else {
            continue;
        };
        if !id_ok(id) {
            continue;
        }
        match load_in(dir, id) {
            Ok(g) => ok.push(g),
            Err(why) => bad.push((id.to_string(), why)),
        }
    }
    ok.sort_by_key(|g| g.name.to_lowercase());
    bad.sort();
    (ok, bad)
}

pub fn list() -> (Vec<Group>, Vec<(String, String)>) {
    list_in(&groups_dir())
}

/// Write a group (replacing the one of the same id), stamped now.
pub fn save_in(dir: &Path, g: &Group) -> Result<Group, String> {
    let mut g = g.clone();
    g.name = g.name.trim().to_string();
    g.description = g.description.trim().to_string();
    for m in &mut g.datasets {
        m.dataset = m.dataset.trim().to_string();
        m.filter = m
            .filter
            .take()
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty());
    }
    g.check()?;
    let now = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    if g.created.is_none() {
        g.created = Some(now.clone());
    }
    g.updated = Some(now);
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let text = serde_yaml::to_string(&g).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".{}.{}", g.id, uuid::Uuid::new_v4().simple()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    let wrote = std::io::Write::write_all(&mut f, text.as_bytes())
        .and_then(|()| f.sync_all())
        .and_then(|()| std::fs::rename(&tmp, file_in(dir, &g.id).expect("checked id")));
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot save the data group: {e}"));
    }
    Ok(g)
}

pub fn save(g: &Group) -> Result<Group, String> {
    save_in(&groups_dir(), g)
}

pub fn remove_in(dir: &Path, id: &str) -> Result<(), String> {
    match std::fs::remove_file(file_in(dir, id)?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove data group {id:?}: {e}")),
    }
}

pub fn remove(id: &str) -> Result<(), String> {
    remove_in(&groups_dir(), id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(id: &str) -> Group {
        Group {
            id: id.into(),
            name: "District 14 roadway core".into(),
            description: "Inventory and centerline for district 14.".into(),
            datasets: vec![
                Member {
                    connection: "roads".into(),
                    dataset: "ROADS.INVENTORY".into(),
                    filter: None,
                },
                Member {
                    connection: "roads".into(),
                    dataset: "ROADS.CENTERLINE".into(),
                    filter: Some(" STATUS <> 'R' ".into()),
                },
                Member {
                    connection: "signs".into(),
                    dataset: "signs".into(),
                    filter: Some("".into()),
                },
            ],
            created: None,
            updated: None,
        }
    }

    #[test]
    fn saved_loaded_listed_removed() {
        let d = tempfile::tempdir().unwrap();
        let s = save_in(d.path(), &g("d14-core")).unwrap();
        assert_eq!(s.datasets[1].filter.as_deref(), Some("STATUS <> 'R'"));
        assert_eq!(s.datasets[2].filter, None);
        assert_eq!(load_in(d.path(), "d14-core").unwrap(), s);
        assert_eq!(list_in(d.path()).0.len(), 1);
        remove_in(d.path(), "d14-core").unwrap();
        assert!(load_in(d.path(), "d14-core").is_err());
    }

    #[test]
    fn as_a_scope_one_group_per_connection_with_checked_filters() {
        let s = g("d14-core");
        let scope = s
            .to_scope(|c| Some(if c == "roads" { "oracle" } else { "gpkg" }.into()))
            .unwrap();
        assert_eq!(scope.len(), 2);
        let roads = scope.iter().find(|x| x.connection == "roads").unwrap();
        assert_eq!(roads.datasets.len(), 2);
        assert!(roads.datasets[1]
            .filter
            .as_deref()
            .unwrap()
            .contains("STATUS"));
        assert!(scope.iter().all(|x| x.mode == Mode::Read));
        // A connection the person no longer has: refused, never skipped.
        assert!(s
            .to_scope(|c| (c == "roads").then(|| "oracle".into()))
            .is_err());
    }

    #[test]
    fn what_is_refused() {
        let d = tempfile::tempdir().unwrap();
        for bad in ["../x", "a", "Caps-id"] {
            assert!(save_in(d.path(), &g(bad)).is_err(), "{bad}");
        }
        let mut sneaky = g("sneaky");
        sneaky.datasets[0].filter = Some("1=1; DROP TABLE x".into());
        assert!(save_in(d.path(), &sneaky).is_err());
        let mut sub = g("subquery");
        sub.datasets[0].filter = Some("ID IN (SELECT ID FROM SECRETS)".into());
        assert!(
            save_in(d.path(), &sub).is_err(),
            "a filter may not read other tables"
        );
        let mut empty = g("empty-one");
        empty.datasets.clear();
        assert!(save_in(d.path(), &empty).is_err());
        let mut conn = g("bad-conn");
        conn.datasets[0].connection = "a/b".into();
        assert!(save_in(d.path(), &conn).is_err());
        std::fs::write(
            d.path().join("other.yaml"),
            "id: else\nname: X\ndatasets: []\n",
        )
        .unwrap();
        assert!(load_in(d.path(), "other").is_err());
    }
}
