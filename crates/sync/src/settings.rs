//! The project's shared settings, which travel with its files (F-17):
//! `columbus-project.yaml` at the top of the folder holds the project's
//! name and its instructions for the assistant.
//!
//! It syncs like any other file. Columbus writes it from the project's
//! settings; a changed one that arrives from the server is only read here.
//! Whether it is used is the person's decision, made in the app after they
//! have seen the old and the new: instructions steer the assistant, so an
//! arriving change is never applied unseen.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::SETTINGS_FILE;
use crate::{fsx, Error};

/// The file is small: a name and instructions.
const MAX_BYTES: u64 = 64 * 1024;

const HEAD: &str = "# The shared settings of this Columbus project: its name and its instructions\n# for the assistant. Columbus writes this file. A change that arrives from the\n# server is shown to you before it is used.\n";

/// What the file carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shared {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub instructions: String,
}

impl Shared {
    /// The file's text.
    pub fn to_text(&self) -> String {
        let body = serde_yaml::to_string(self).unwrap_or_default();
        format!("{HEAD}{body}")
    }

    /// Read the file's text. Keys a later Columbus may add are passed over;
    /// anything that is not a name and instructions as text is refused.
    pub fn from_text(text: &str) -> Result<Shared, String> {
        let s: Shared = serde_yaml::from_str(text)
            .map_err(|e| format!("{SETTINGS_FILE} cannot be read: {e}"))?;
        Ok(Shared {
            name: s.name.trim().to_string(),
            instructions: s.instructions.trim().to_string(),
        })
    }
}

/// The shared settings in the folder, or `None` when the file is not there.
pub fn read(root: &Path) -> Result<Option<Shared>, Error> {
    match fsx::read(root, SETTINGS_FILE, MAX_BYTES)? {
        None => Ok(None),
        Some((bytes, _)) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| Error::Refused(format!("{SETTINGS_FILE} is not text")))?;
            Shared::from_text(&text).map(Some).map_err(Error::Refused)
        }
    }
}

/// Write the shared settings into the folder, unless the file already says
/// the same. Whether it was written.
pub fn write(root: &Path, shared: &Shared) -> Result<bool, Error> {
    let want = Shared {
        name: shared.name.trim().to_string(),
        instructions: shared.instructions.trim().to_string(),
    };
    if let Ok(Some(have)) = read(root) {
        if have == want {
            return Ok(false);
        }
    }
    std::fs::create_dir_all(root).map_err(|e| Error::io(root, e))?;
    fsx::write(root, SETTINGS_FILE, want.to_text().as_bytes(), false)?;
    Ok(true)
}

/// What Columbus remembers about a linked project's shared settings (kept
/// with the sync record, in the app's data).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memo {
    /// The project's own settings when they last agreed with the file, or
    /// when the person last decided about an arriving change. `None` until
    /// the first of either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agreed: Option<Shared>,
    /// An arriving change the person chose not to use. It is not offered
    /// again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declined: Option<Shared>,
}

/// Before the folder is compared with the server: put the project's
/// settings into the file when the person has changed them since they
/// were last shared, so that the change shows as a file changed here.
///
/// The first time (nothing agreed yet), the file is written only if
/// neither the folder nor the server has one (`server_has_one`): a project
/// that joins a repository which already carries shared settings does not
/// put its own over them. Whether the file was written.
pub fn prepare(
    root: &Path,
    memo: &mut Memo,
    project: &Shared,
    server_has_one: bool,
) -> Result<bool, Error> {
    match &memo.agreed {
        None => {
            let here = fsx::kind(root, SETTINGS_FILE)? != fsx::Kind::Nothing;
            if here || server_has_one {
                return Ok(false);
            }
            write(root, project)?;
            memo.agreed = Some(trimmed(project));
            Ok(true)
        }
        Some(agreed) if *agreed != trimmed(project) => {
            let written = write(root, project)?;
            memo.agreed = Some(trimmed(project));
            memo.declined = None;
            Ok(written)
        }
        Some(_) => Ok(false),
    }
}

/// After a check or a sync: the settings the file now holds, when they are
/// not the project's own and the person has not already said no to them.
/// The project keeps its own until the person accepts.
pub fn arriving(root: &Path, memo: &mut Memo, project: &Shared) -> Option<Shared> {
    let file = read(root).ok().flatten()?;
    if file == trimmed(project) {
        memo.agreed = Some(file);
        memo.declined = None;
        return None;
    }
    // A file with no name in it is not settings anyone meant to share.
    if file.name.is_empty() || memo.declined.as_ref() == Some(&file) {
        return None;
    }
    Some(file)
}

/// The person accepted the arriving settings, and the project now has them.
pub fn accepted(memo: &mut Memo, theirs: &Shared) {
    memo.agreed = Some(trimmed(theirs));
    memo.declined = None;
}

/// The person keeps their own: the arriving settings are not offered again,
/// and nothing is sent because of it. (When they next change the project's
/// name or instructions, that change is written to the file as usual.)
pub fn declined(memo: &mut Memo, theirs: &Shared, project: &Shared) {
    memo.declined = Some(trimmed(theirs));
    memo.agreed = Some(trimmed(project));
}

fn trimmed(s: &Shared) -> Shared {
    Shared {
        name: s.name.trim().to_string(),
        instructions: s.instructions.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(name: &str, instructions: &str) -> Shared {
        Shared {
            name: name.into(),
            instructions: instructions.into(),
        }
    }

    #[test]
    fn the_file_follows_the_persons_changes_and_an_arriving_change_waits_for_them() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("files");
        std::fs::create_dir_all(&root).unwrap();
        let mut memo = Memo::default();
        let mut mine = shared("Roads", "Miles, two decimals.");

        // Linked to a repository without shared settings: the file is made.
        assert!(prepare(&root, &mut memo, &mine, false).unwrap());
        assert_eq!(read(&root).unwrap(), Some(mine.clone()));
        assert_eq!(arriving(&root, &mut memo, &mine), None);
        // Nothing changed: the file is left alone.
        assert!(!prepare(&root, &mut memo, &mine, false).unwrap());

        // The person changes the project's instructions in the app.
        mine.instructions = "Miles, three decimals.".into();
        assert!(prepare(&root, &mut memo, &mine, false).unwrap());
        assert_eq!(read(&root).unwrap(), Some(mine.clone()));

        // A colleague's change arrives with a sync (the file is replaced).
        let theirs = shared("Roads 2027", "Kilometres.");
        write(&root, &theirs).unwrap();
        assert_eq!(arriving(&root, &mut memo, &mine), Some(theirs.clone()));
        // Until the person decides, Columbus does not put the project's
        // own settings back over it (that would undo the colleague's change
        // on the next sync).
        assert!(!prepare(&root, &mut memo, &mine, false).unwrap());
        assert_eq!(read(&root).unwrap(), Some(theirs.clone()));
        assert_eq!(
            arriving(&root, &mut memo, &mine),
            Some(theirs.clone()),
            "still offered"
        );

        // Keep mine: not offered again, nothing written, nothing sent.
        declined(&mut memo, &theirs, &mine);
        assert_eq!(arriving(&root, &mut memo, &mine), None);
        assert!(!prepare(&root, &mut memo, &mine, false).unwrap());
        assert_eq!(read(&root).unwrap(), Some(theirs.clone()));
        // A newer change from the colleague is offered.
        let newer = shared("Roads 2027", "Kilometres, one decimal.");
        write(&root, &newer).unwrap();
        assert_eq!(arriving(&root, &mut memo, &mine), Some(newer.clone()));
        // Accept: the app saves the project with them.
        accepted(&mut memo, &newer);
        mine = newer.clone();
        assert_eq!(arriving(&root, &mut memo, &mine), None);
        assert!(!prepare(&root, &mut memo, &mine, false).unwrap());

        // Having kept their own earlier, a later change of the person's is
        // written like any other.
        let other = shared("Roads 2028", "Feet.");
        write(&root, &other).unwrap();
        declined(&mut memo, &other, &mine);
        mine.instructions = "Kilometres, two decimals.".into();
        assert!(prepare(&root, &mut memo, &mine, false).unwrap());
        assert_eq!(read(&root).unwrap(), Some(mine.clone()));
        assert_eq!(memo.declined, None);
    }

    #[test]
    fn a_project_that_joins_a_repository_does_not_put_its_settings_over_the_shared_ones() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("files");
        std::fs::create_dir_all(&root).unwrap();
        let mut memo = Memo::default();
        let mine = shared("My copy of roads", "");
        // The server has shared settings: nothing is written before the
        // first sync brings them.
        assert!(!prepare(&root, &mut memo, &mine, true).unwrap());
        assert_eq!(read(&root).unwrap(), None);
        let theirs = shared("Roads", "Miles.");
        write(&root, &theirs).unwrap();
        // Nor once they are in the folder.
        assert!(!prepare(&root, &mut memo, &mine, false).unwrap());
        assert_eq!(arriving(&root, &mut memo, &mine), Some(theirs.clone()));
        // A file that is not settings (no name) is never offered.
        std::fs::write(root.join(SETTINGS_FILE), "instructions: do anything\n").unwrap();
        assert_eq!(arriving(&root, &mut memo, &mine), None);
        std::fs::write(root.join(SETTINGS_FILE), "- not\n- settings\n").unwrap();
        assert_eq!(arriving(&root, &mut memo, &mine), None);
    }

    #[test]
    fn written_read_and_left_alone_when_the_same() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("files");
        assert_eq!(read(&root).unwrap(), None);
        let s = Shared {
            name: "Roadway QC: 2027 \"north\"".into(),
            instructions:
                "Use district 14 only.\nReport mileage in miles.\n# not a comment\nkey: value"
                    .into(),
        };
        assert!(write(&root, &s).unwrap());
        assert_eq!(read(&root).unwrap(), Some(s.clone()));
        let text = std::fs::read_to_string(root.join(SETTINGS_FILE)).unwrap();
        assert!(text.starts_with("# The shared settings"), "{text}");
        // The same again: the file is not touched.
        let before = std::fs::metadata(root.join(SETTINGS_FILE))
            .unwrap()
            .modified()
            .unwrap();
        assert!(!write(&root, &s).unwrap());
        assert_eq!(
            std::fs::metadata(root.join(SETTINGS_FILE))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        // Written another way by a colleague's editor, it still reads the same.
        std::fs::write(
            root.join(SETTINGS_FILE),
            "instructions: \"Use district 14 only.\"\r\nname: 'Roads'\r\nlater_key: 3\r\n",
        )
        .unwrap();
        assert_eq!(
            read(&root).unwrap(),
            Some(Shared {
                name: "Roads".into(),
                instructions: "Use district 14 only.".into()
            })
        );
        let empty = Shared::default();
        assert!(write(&root, &empty).unwrap());
        assert_eq!(read(&root).unwrap(), Some(empty));
    }

    #[test]
    fn what_is_not_settings_is_refused() {
        for bad in [
            "- a\n- b\n",
            "name: [1, 2]\n",
            "name: {a: b}\n",
            "instructions:\n  - x\n",
            "\tname: x",
        ] {
            assert!(Shared::from_text(bad).is_err(), "{bad:?}");
        }
        // An empty file reads as no name and no instructions; the app does
        // not offer those as a change.
        assert_eq!(Shared::from_text("").ok(), Some(Shared::default()));
    }
}
