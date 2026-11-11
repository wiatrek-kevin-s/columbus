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

#[cfg(test)]
mod tests {
    use super::*;

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
