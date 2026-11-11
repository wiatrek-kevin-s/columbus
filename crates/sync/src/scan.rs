//! What is in the folder now: every plain file that may sync, with what it
//! holds, and everything that was passed over, with why. Links are never
//! followed, and nothing outside the folder is read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::fsx::{self, Stat};
use crate::hash::content_hash;
use crate::paths::{self, MAX_FILE};
use crate::{Error, Skipped};

/// The most files one folder may hold and still sync. Sync is for scripts
/// and notes; a folder beyond this holds something else.
pub const MAX_FILES: usize = 20_000;
/// Folders deeper than this are not entered.
const MAX_DEPTH: usize = 32;

/// A file in the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Local {
    /// A plain file that may sync.
    File { stat: Stat, hash: String },
    /// A file larger than 5 MB: it is there, and it does not sync.
    TooBig { size: u64 },
}

impl Local {
    pub fn hash(&self) -> Option<&str> {
        match self {
            Local::File { hash, .. } => Some(hash),
            Local::TooBig { .. } => None,
        }
    }
}

/// The folder as found.
#[derive(Debug, Default)]
pub struct Scan {
    pub files: BTreeMap<String, Local>,
    pub skipped: Vec<Skipped>,
}

/// Walk `root`. `held` are the copies kept for files that need a look: they
/// are passed over without a word (the page lists them under Needs a look).
pub fn scan(root: &Path, held: &BTreeSet<String>) -> Result<Scan, Error> {
    let mut out = Scan::default();
    match std::fs::symlink_metadata(root) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(Error::Refused(format!(
                "{} is a link or a file, not a folder",
                root.display()
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(Error::io(root, e)),
    }
    // (folder as a relative path, or "" for the top; its depth)
    let mut todo = vec![(String::new(), 0usize)];
    while let Some((dir, depth)) = todo.pop() {
        let full = if dir.is_empty() {
            root.to_path_buf()
        } else {
            fsx::full(root, &dir)
        };
        let entries = std::fs::read_dir(&full).map_err(|e| Error::io(&full, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(&full, e))?;
            let Ok(name) = entry.file_name().into_string() else {
                out.skip(
                    format!("{}{}", prefix(&dir), entry.file_name().to_string_lossy()),
                    "its name is not valid text",
                );
                continue;
            };
            if fsx::is_tmp_name(&name) {
                continue;
            }
            let rel = format!("{}{name}", prefix(&dir));
            let meta =
                std::fs::symlink_metadata(entry.path()).map_err(|e| Error::io(&entry.path(), e))?;
            let ft = meta.file_type();
            if ft.is_symlink() {
                out.skip(rel, "it is a link (links are never followed)");
                continue;
            }
            if let Some(why) = paths::refused_name(&name) {
                out.skip(rel, why);
                continue;
            }
            if ft.is_dir() {
                if let Some(why) = paths::ignored_folder(&name) {
                    out.skip(format!("{rel}/"), why);
                } else if depth + 1 >= MAX_DEPTH {
                    out.skip(format!("{rel}/"), "it is too many folders deep");
                } else {
                    todo.push((rel, depth + 1));
                }
                continue;
            }
            if !ft.is_file() {
                out.skip(rel, "it is not a plain file");
                continue;
            }
            if held.contains(&rel) {
                continue;
            }
            if let Some(why) = paths::refused(&rel) {
                out.skip(rel, why);
                continue;
            }
            if let Some(why) = paths::ignored(&rel) {
                out.skip(rel, why);
                continue;
            }
            if out.files.len() >= MAX_FILES {
                return Err(Error::Refused(format!(
                    "this folder holds more than {MAX_FILES} files; sync is for scripts and notes, not data"
                )));
            }
            if meta.len() > MAX_FILE {
                out.skip(rel.clone(), "it is larger than 5 MB");
                out.files.insert(rel, Local::TooBig { size: meta.len() });
                continue;
            }
            match fsx::read(root, &rel, MAX_FILE) {
                Ok(Some((bytes, stat))) => {
                    let hash = content_hash(&bytes);
                    out.files.insert(rel, Local::File { stat, hash });
                }
                // Gone, or grown, since it was listed: the next look finds it.
                Ok(None) => {}
                Err(Error::Refused(_)) => {
                    out.skip(rel.clone(), "it is larger than 5 MB");
                    out.files.insert(rel, Local::TooBig { size: meta.len() });
                }
                Err(e) => return Err(e),
            }
        }
    }
    out.skipped.sort();
    Ok(out)
}

fn prefix(dir: &str) -> String {
    if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    }
}

impl Scan {
    fn skip(&mut self, path: String, why: &str) {
        self.skipped.push(Skipped {
            path,
            why: why.to_string(),
            on_server: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(root: &Path, rel: &str, bytes: &[u8]) {
        let p = fsx::full(root, rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[test]
    fn what_a_scan_finds_and_what_it_passes_over() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        put(root, "notes.md", b"hello\n");
        put(root, "scripts/widths.py", b"print(1)\r\n");
        put(root, "scripts/__pycache__/widths.cpython-312.pyc", b"\0\0");
        put(root, ".env", b"KEY=1");
        put(root, "roads.shp", b"x");
        put(root, "data/roads.gdb/a00000001.gdbtable", b"x");
        put(root, "outputs/report.csv", b"a,b\n");
        put(root, ".git/config", b"[core]");
        put(root, ".columbus/x", b"x");
        put(root, "kept (my copy 2026-11-11).md", b"mine");
        put(root, ".columbus-sync-0123.tmp", b"half");
        put(root, "big.bin", &vec![7u8; (MAX_FILE + 1) as usize]);
        put(root, "edge.bin", &vec![7u8; MAX_FILE as usize]);
        let held: BTreeSet<String> = ["kept (my copy 2026-11-11).md".to_string()].into();
        let s = scan(root, &held).unwrap();
        let names: Vec<&str> = s.files.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["big.bin", "edge.bin", "notes.md", "scripts/widths.py"]
        );
        assert!(matches!(s.files["big.bin"], Local::TooBig { .. }));
        assert!(
            matches!(s.files["edge.bin"], Local::File { .. }),
            "5 MB exactly still syncs"
        );
        assert_eq!(
            s.files["notes.md"].hash(),
            Some(content_hash(b"hello\n").as_str())
        );
        // CRLF and LF are the same content.
        assert_eq!(
            s.files["scripts/widths.py"].hash(),
            Some(content_hash(b"print(1)\n").as_str())
        );
        let skipped: BTreeMap<&str, &str> = s
            .skipped
            .iter()
            .map(|k| (k.path.as_str(), k.why.as_str()))
            .collect();
        for (path, word) in [
            (".env", ".env"),
            ("roads.shp", "geodata"),
            ("data/roads.gdb/", "geodatabase"),
            ("outputs/", "outputs"),
            ("scripts/__pycache__/", "__pycache__"),
            (".git", ".git"),
            (".columbus", ".columbus"),
            ("big.bin", "5 MB"),
        ] {
            assert!(
                skipped.get(path).is_some_and(|w| w.contains(word)),
                "{path}: {skipped:?}"
            );
        }
        assert!(!skipped.contains_key("kept (my copy 2026-11-11).md"));
        assert!(!skipped.keys().any(|k| k.contains(".tmp")));
        // A folder that is not there holds nothing.
        assert!(scan(&root.join("nope"), &held).unwrap().files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_named_and_never_followed() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("files");
        let outside = d.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        let s = scan(&root, &BTreeSet::new()).unwrap();
        assert_eq!(s.files.keys().collect::<Vec<_>>(), ["a.txt"]);
        let named: Vec<&str> = s.skipped.iter().map(|k| k.path.as_str()).collect();
        assert_eq!(named, ["link.txt", "out"]);
        assert!(s.skipped.iter().all(|k| k.why.contains("link")));
        // A folder that is itself a link is not a folder to sync.
        std::os::unix::fs::symlink(&outside, d.path().join("linked")).unwrap();
        assert!(scan(&d.path().join("linked"), &BTreeSet::new()).is_err());
    }
}
