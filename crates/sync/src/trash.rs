//! Where a file goes when sync takes it out of the folder: a file a
//! colleague deleted, or a copy the person chose not to keep. It is never
//! simply erased.
//!
//! It goes to the system's trash (the Recycle Bin on Windows, the Trash on
//! macOS and Linux), where the person already looks for deleted files and
//! can put one back. Where the system has no trash for the folder (some
//! network drives, a Linux system without one), or it refuses, the file is
//! moved to a folder of Columbus's own instead, kept out of sync, and the
//! report says where.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{fsx, Error};

/// Where removed files are kept.
#[derive(Debug, Clone)]
pub enum Keep {
    /// The system's trash; `fallback` is the folder used when the system's
    /// trash cannot take a file.
    SystemTrash { fallback: PathBuf },
    /// Always this folder (tests, and systems where the person prefers it).
    Folder(PathBuf),
}

/// Where a removed file is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kept {
    /// In the system's trash.
    Trash,
    /// In Columbus's own folder, at this path.
    Folder(String),
}

/// Take the plain file `rel` out of the folder `root`, keeping it.
pub fn remove(root: &Path, rel: &str, keep: &Keep) -> Result<Kept, Error> {
    let path = fsx::full(root, rel);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            return Err(Error::Refused(format!(
                "{} is not a plain file",
                path.display()
            )))
        }
        Err(e) => return Err(Error::io(&path, e)),
    }
    let folder = match keep {
        Keep::SystemTrash { fallback } => {
            if to_system_trash(&path) && !path.exists() {
                return Ok(Kept::Trash);
            }
            fallback
        }
        Keep::Folder(f) => f,
    };
    let to = free_place(folder, rel);
    fsx::move_file(&path, &to)?;
    Ok(Kept::Folder(to.display().to_string()))
}

/// A place in `folder` for `rel` that nothing has yet:
/// `<folder>/<time>/<rel>`.
fn free_place(folder: &Path, rel: &str) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H%M%S").to_string();
    for n in 0u32.. {
        let dir = if n == 0 {
            folder.join(&stamp)
        } else {
            folder.join(format!("{stamp} {n}"))
        };
        let to = fsx::full(&dir, rel);
        if std::fs::symlink_metadata(&to).is_err() {
            return to;
        }
    }
    unreachable!("some number is free")
}

/// Ask the system to move `path` to its trash. On a thread of its own: on
/// Windows the shell's file operation needs a thread set up for it (COM),
/// and a thread that is ours alone cannot clash with how another part of
/// the program set its own up. Whatever goes wrong there (a refusal, a
/// panic) just means the file was not moved.
fn to_system_trash(path: &Path) -> bool {
    let path = path.to_path_buf();
    std::thread::Builder::new()
        .name("columbus-trash".into())
        .spawn(move || system_trash(&path))
        .ok()
        .and_then(|t| t.join().ok())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn system_trash(path: &Path) -> bool {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    // The file manager's own call: no Finder script, so nothing asks the
    // person to let Columbus control Finder.
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path).is_ok()
}

#[cfg(not(target_os = "macos"))]
fn system_trash(path: &Path) -> bool {
    trash::delete(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_file_is_kept_in_columbuss_folder() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("files");
        let kept = d.path().join("removed");
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/old.py"), b"print('old')\n").unwrap();
        let keep = Keep::Folder(kept.clone());
        let Kept::Folder(to) = remove(&root, "scripts/old.py", &keep).unwrap() else {
            panic!("kept in the folder");
        };
        assert!(!root.join("scripts/old.py").exists());
        assert!(Path::new(&to).starts_with(&kept));
        assert!(to.replace('\\', "/").ends_with("scripts/old.py"), "{to}");
        assert_eq!(std::fs::read(&to).unwrap(), b"print('old')\n");
        // The same name again, at once, does not replace the first.
        std::fs::write(root.join("scripts/old.py"), b"second\n").unwrap();
        let Kept::Folder(to2) = remove(&root, "scripts/old.py", &keep).unwrap() else {
            panic!("kept in the folder");
        };
        assert_ne!(to, to2);
        assert_eq!(std::fs::read(&to).unwrap(), b"print('old')\n");
        assert_eq!(std::fs::read(&to2).unwrap(), b"second\n");
        // Only a plain file is removed.
        assert!(remove(&root, "scripts", &keep).is_err());
        assert!(remove(&root, "nothing.txt", &keep).is_err());
    }
}
