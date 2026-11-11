//! What Columbus remembers about a synced folder (F-18): the link to its
//! repository and the state it was last synced in. It is kept in the app's
//! data, never in the folder that syncs, and it holds no secret.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

/// The repository and branch a folder syncs with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    /// The server's id in `git-servers.yaml`.
    pub server: String,
    pub owner: String,
    pub repo: String,
    pub branch: String,
}

/// A file as it was when it was last synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Synced {
    /// The id of the server's version (a Git blob id, 40 or 64 hex digits).
    pub blob: String,
    /// The file in the folder then: its size, when it was modified
    /// (milliseconds since 1970) and what it held ([`crate::hash::content_hash`]).
    /// The hash decides whether it has changed; the size and time alone
    /// never do.
    pub size: u64,
    pub mtime: i64,
    pub hash: String,
}

/// How a file came to need a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Changed here and on the server: the server's version has the name,
    /// the person's is in `copy`.
    BothChanged,
    /// Changed here, deleted on the server: the person's version is in
    /// `copy`, and nothing has the name.
    DeletedOnServer,
    /// Deleted here, changed on the server: the server's version has the
    /// name again; there is no copy.
    DeletedHere,
}

/// A file that needs a look: both versions were kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub path: String,
    /// Where the person's version is kept (`name (my copy DATE).ext`). It
    /// does not sync until the person has chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<String>,
    pub kind: ConflictKind,
    /// The day it was found (`YYYY-MM-DD`).
    pub found: String,
}

/// The last synced state of one folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The commit the folder was last synced with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// When (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<String>,
    /// Every synced path.
    #[serde(default)]
    pub files: BTreeMap<String, Synced>,
    /// Files that need a look.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<Conflict>,
    /// Files the server keeps with Git LFS, by path, with the id of the
    /// pointer seen there: they are never fetched, and not asked for again.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lfs: BTreeMap<String, String>,
}

impl State {
    /// The paths of the copies kept for files that need a look: they stay
    /// out of sync until the person has chosen.
    pub fn held(&self) -> std::collections::BTreeSet<String> {
        self.conflicts
            .iter()
            .filter_map(|c| c.copy.clone())
            .collect()
    }
}

/// A folder's link and state, as one file in the app's data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The file's format, for later versions.
    pub version: u32,
    pub link: Link,
    #[serde(default)]
    pub state: State,
}

impl Record {
    pub fn new(link: Link) -> Self {
        Record {
            version: 1,
            link,
            state: State::default(),
        }
    }

    /// Read a record. `Ok(None)` when there is none (the folder is not
    /// linked).
    pub fn load(path: &Path) -> Result<Option<Record>, Error> {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(path, e)),
        };
        if !meta.is_file() || meta.len() > 64 * 1024 * 1024 {
            return Err(Error::Refused(format!(
                "{} is not a plain file Columbus wrote",
                path.display()
            )));
        }
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let r: Record = serde_json::from_str(&text)
            .map_err(|e| Error::Refused(format!("{} cannot be read: {e}", path.display())))?;
        if r.version != 1 {
            return Err(Error::Refused(format!(
                "{} was written by a newer Columbus (format {})",
                path.display(),
                r.version
            )));
        }
        Ok(Some(r))
    }

    /// Write the record whole: a new file beside it, then moved into place.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let dir = path
            .parent()
            .ok_or_else(|| Error::Refused("the sync record has no folder".into()))?;
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Refused(format!("the sync record cannot be written: {e}")))?;
        crate::fsx::write_new_then_rename(path, text.as_bytes())
    }

    /// Forget the record (Disconnect). The folder's files are not touched.
    pub fn forget(path: &Path) -> Result<(), Error> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_saved_read_and_forgotten() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sync").join("roads.json");
        assert_eq!(Record::load(&p).unwrap(), None);
        let mut r = Record::new(Link {
            server: "github".into(),
            owner: "kevin".into(),
            repo: "roads".into(),
            branch: "main".into(),
        });
        r.state.commit = Some("a".repeat(40));
        r.state.files.insert(
            "a.py".into(),
            Synced {
                blob: "b".repeat(40),
                size: 3,
                mtime: 1,
                hash: "sha256:00".into(),
            },
        );
        r.state.conflicts.push(Conflict {
            path: "n.md".into(),
            copy: Some("n (my copy 2026-11-11).md".into()),
            kind: ConflictKind::BothChanged,
            found: "2026-11-11".into(),
        });
        r.save(&p).unwrap();
        assert_eq!(Record::load(&p).unwrap(), Some(r.clone()));
        assert_eq!(
            r.state.held().into_iter().collect::<Vec<_>>(),
            ["n (my copy 2026-11-11).md"]
        );
        // No temporary file is left beside it.
        let names: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["roads.json"]);
        Record::forget(&p).unwrap();
        Record::forget(&p).unwrap();
        assert_eq!(Record::load(&p).unwrap(), None);
    }

    #[test]
    fn a_record_from_a_newer_columbus_is_not_guessed_at() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        std::fs::write(
            &p,
            r#"{"version":2,"link":{"server":"s","owner":"o","repo":"r","branch":"b"}}"#,
        )
        .unwrap();
        assert!(Record::load(&p).unwrap_err().to_string().contains("newer"));
        std::fs::write(&p, "not json").unwrap();
        assert!(Record::load(&p).is_err());
    }
}
