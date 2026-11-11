//! Reading and writing inside the synced folder. Links are never followed:
//! every folder on the way to a file must be a real folder, and a file must
//! be a plain file. Files are written whole (a new file beside the old one,
//! then moved into place), so a half-written file is never left under a
//! real name.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::Error;

/// Columbus's own half-written files: `.columbus-sync-<random>.tmp`. A scan
/// passes over them.
pub(crate) const TMP_PREFIX: &str = ".columbus-sync-";
pub(crate) const TMP_SUFFIX: &str = ".tmp";

pub(crate) fn is_tmp_name(name: &str) -> bool {
    name.starts_with(TMP_PREFIX) && name.ends_with(TMP_SUFFIX)
}

/// The file `rel` (parts joined by `/`) under `root`.
pub(crate) fn full(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/') {
        p.push(part);
    }
    p
}

/// What a file is now: its size, when it was modified (milliseconds since
/// 1970) and whether it may be run (always false on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stat {
    pub size: u64,
    pub mtime: i64,
    pub exec: bool,
}

pub(crate) fn stat_of(meta: &std::fs::Metadata) -> Stat {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    Stat {
        size: meta.len(),
        mtime,
        exec: is_exec(meta),
    }
}

#[cfg(unix)]
fn is_exec(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn is_exec(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Check that every folder between `root` and the file `rel` is a real
/// folder (not a link), making the missing ones when `create` is set.
/// `Ok(false)` when one is missing and was not to be made.
fn walk_parents(root: &Path, rel: &str, create: bool) -> Result<bool, Error> {
    let mut dir = root.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(Error::Refused(format!(
                    "{} is a link or a file where a folder is expected",
                    dir.display()
                )))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(false);
                }
                match std::fs::create_dir(&dir) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(Error::io(&dir, e)),
                }
            }
            Err(e) => return Err(Error::io(&dir, e)),
        }
    }
    Ok(true)
}

/// The bytes of the plain file `rel`, or `None` if nothing has that name.
/// A link, a folder or anything else of that name is an error; so is a
/// file larger than `max`.
pub(crate) fn read(root: &Path, rel: &str, max: u64) -> Result<Option<(Vec<u8>, Stat)>, Error> {
    if !walk_parents(root, rel, false)? {
        return Ok(None);
    }
    let path = full(root, rel);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&path, e)),
    };
    if !meta.is_file() {
        return Err(Error::Refused(format!(
            "{} is not a plain file",
            path.display()
        )));
    }
    if meta.len() > max {
        return Err(Error::Refused(format!(
            "{} is larger than Columbus reads here",
            path.display()
        )));
    }
    let bytes = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
    // The size and time of what was read (a file may change while it is read).
    let after = std::fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
    Ok(Some((bytes, stat_of(&after))))
}

/// What has the name `rel` now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Nothing,
    File,
    Folder,
    Other,
}

pub(crate) fn kind(root: &Path, rel: &str) -> Result<Kind, Error> {
    if !walk_parents(root, rel, false)? {
        return Ok(Kind::Nothing);
    }
    let path = full(root, rel);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => Ok(Kind::File),
        Ok(m) if m.is_dir() => Ok(Kind::Folder),
        Ok(_) => Ok(Kind::Other),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Kind::Nothing),
        Err(e) => Err(Error::io(&path, e)),
    }
}

/// Write `bytes` as the file `rel`, replacing a plain file of that name.
/// Folders on the way are made; nothing is written through a link. The
/// file is a new one with the permissions any new file gets: nothing sync
/// writes is ever marked as a program (R-104), whatever the file it
/// replaces was.
pub(crate) fn write(root: &Path, rel: &str, bytes: &[u8]) -> Result<Stat, Error> {
    walk_parents(root, rel, true)?;
    let path = full(root, rel);
    match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            return Err(Error::Refused(format!(
                "{} is a folder or a link, not a plain file",
                path.display()
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::io(&path, e)),
    }
    write_new_then_rename(&path, bytes)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
    Ok(stat_of(&meta))
}

/// Write a file whole: a new file beside it, flushed, then moved into place.
pub(crate) fn write_new_then_rename(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Refused(format!("{} has no folder", path.display())))?;
    let tmp = dir.join(format!(
        "{TMP_PREFIX}{}{TMP_SUFFIX}",
        uuid::Uuid::new_v4().simple()
    ));
    let done = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| Error::io(&tmp, e))?;
        f.write_all(bytes).map_err(|e| Error::io(&tmp, e))?;
        f.sync_all().map_err(|e| Error::io(&tmp, e))?;
        drop(f);
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Give the plain file `from` the name `to`, which nothing may have yet.
pub(crate) fn rename_to_new(root: &Path, from: &str, to: &str) -> Result<(), Error> {
    walk_parents(root, from, false)?;
    walk_parents(root, to, true)?;
    let (a, b) = (full(root, from), full(root, to));
    match std::fs::symlink_metadata(&b) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err(Error::Refused(format!("{} is already there", b.display()))),
        Err(e) => return Err(Error::io(&b, e)),
    }
    std::fs::rename(&a, &b).map_err(|e| Error::io(&a, e))
}

/// Remove the folders `rel` was in that are now empty, up to (not
/// including) `root`.
pub(crate) fn remove_empty_parents(root: &Path, rel: &str) {
    let mut parts: Vec<&str> = rel.split('/').collect();
    parts.pop();
    while !parts.is_empty() {
        let dir = full(root, &parts.join("/"));
        // Fails, as it should, while anything is left in it.
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
        parts.pop();
    }
}

/// Move a file to another folder, which may be on another disk (then it is
/// copied and the original removed).
pub(crate) fn move_file(from: &Path, to: &Path) -> Result<(), Error> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|e| Error::io(to, e))?;
    std::fs::remove_file(from).map_err(|e| Error::io(from, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_written_whole_and_read_back() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        assert_eq!(read(root, "a/b/c.txt", 100).unwrap(), None);
        assert_eq!(kind(root, "a/b/c.txt").unwrap(), Kind::Nothing);
        let st = write(root, "a/b/c.txt", b"one").unwrap();
        assert_eq!(st.size, 3);
        let (bytes, st2) = read(root, "a/b/c.txt", 100).unwrap().unwrap();
        assert_eq!(bytes, b"one");
        assert_eq!(st, st2);
        write(root, "a/b/c.txt", b"three").unwrap();
        assert_eq!(read(root, "a/b/c.txt", 100).unwrap().unwrap().0, b"three");
        assert!(
            read(root, "a/b/c.txt", 2).is_err(),
            "over the size asked for"
        );
        assert_eq!(kind(root, "a/b").unwrap(), Kind::Folder);
        assert_eq!(kind(root, "a/b/c.txt").unwrap(), Kind::File);
        // No temporary file is left.
        let names: Vec<_> = std::fs::read_dir(root.join("a/b"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["c.txt"]);
        // A folder where a file is wanted, and a file where a folder is.
        assert!(write(root, "a/b", b"x").is_err());
        assert!(write(root, "a/b/c.txt/d", b"x").is_err());
        assert!(read(root, "a/b", 100).is_err());

        rename_to_new(root, "a/b/c.txt", "a/b/c (my copy).txt").unwrap();
        assert_eq!(kind(root, "a/b/c.txt").unwrap(), Kind::Nothing);
        write(root, "a/b/c.txt", b"theirs").unwrap();
        assert!(rename_to_new(root, "a/b/c.txt", "a/b/c (my copy).txt").is_err());

        std::fs::remove_file(root.join("a/b/c.txt")).unwrap();
        remove_empty_parents(root, "a/b/c.txt");
        assert!(root.join("a/b").exists(), "the copy is still in it");
        std::fs::remove_file(root.join("a/b/c (my copy).txt")).unwrap();
        remove_empty_parents(root, "a/b/c.txt");
        assert!(!root.join("a").exists());
        assert!(root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("files");
        let outside = d.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        // Through a linked folder: neither read nor written.
        assert!(read(&root, "out/secret.txt", 100).is_err());
        assert!(write(&root, "out/new.txt", b"x").is_err());
        assert!(!outside.join("new.txt").exists());
        // A linked file: not read, not written through.
        assert!(read(&root, "link.txt", 100).is_err());
        assert!(write(&root, "link.txt", b"changed").is_err());
        assert_eq!(
            std::fs::read(outside.join("secret.txt")).unwrap(),
            b"secret"
        );
        assert_eq!(kind(&root, "link.txt").unwrap(), Kind::Other);
    }

    /// Nothing sync writes is a program, even over a file that was one.
    #[cfg(unix)]
    #[test]
    fn a_file_sync_writes_is_never_marked_as_a_program() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let st = write(d.path(), "run.sh", b"#!/bin/sh\n").unwrap();
        assert!(!st.exec);
        let path = d.path().join("run.sh");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(stat_of(&std::fs::metadata(&path).unwrap()).exec);
        let st = write(d.path(), "run.sh", b"#!/bin/sh\necho newer\n").unwrap();
        assert!(!st.exec, "the newer version is not a program");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
            0
        );
    }
}
