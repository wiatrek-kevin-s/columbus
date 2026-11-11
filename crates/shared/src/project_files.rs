//! A project's files (build brief 3, D-19, F-17): `projects/<id>/files/`,
//! plain files in folders. Scripts, notes, small things worth keeping. The
//! person puts their own there; the assistant lists, reads and writes text
//! files there through Columbus's server (policy `allow_project_files`) and
//! runs scripts kept there.
//!
//! This module holds what the server and the app share: where the folder
//! is, which paths are refused, the size limits, and a listing that never
//! follows a link. Reading and writing are the server's
//! (`crates/gateway/src/project_files.rs`).

use std::path::{Path, PathBuf};

use crate::file_kinds::{self, Kind};

/// The folder's name inside the project's own folder.
pub const FOLDER: &str = "files";
/// The largest text file the assistant reads or writes.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// The most files, and bytes in all, the folder may hold once the assistant
/// has written to it. The person's own files count; they are never removed.
pub const MAX_FILES: usize = 1000;
pub const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
/// How deep a file may lie: this many folders above it at most.
pub const MAX_FOLDERS_DEEP: usize = 8;
/// The longest name of a file or folder, and the longest whole path, in
/// characters.
pub const MAX_NAME: usize = 100;
pub const MAX_PATH: usize = 240;
/// The project's shared settings, at the top of the folder when the project
/// is linked to a repository (F-17): written by sync, read-only here.
pub const SETTINGS_FILE: &str = "columbus-project.yaml";
/// Names Columbus makes for itself while it writes a file; never a file's
/// own name (`.columbus` is refused).
pub const TEMPORARY_PREFIX: &str = ".columbus-tmp-";

/// The record of which data labels each file carries (`path` to the
/// labels of the chat that wrote it, such as `connection:roads-prod`):
/// beside the files folder, in the project's own folder, so it never
/// syncs. The server writes it; sync reads it, to hold a file that may
/// quote data until the person chooses to send it.
pub const LABELS_FILE: &str = "files-labels.json";

/// The data labels each file carries, by its path in the folder.
pub type Labels = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

/// Read the labels record in `folder` (the project's own folder,
/// `projects/<id>`). No record: no file carries a label. A record that is
/// there but does not read is an error, never "no labels".
pub fn load_labels(folder: &Path) -> Result<Labels, String> {
    let path = folder.join(LABELS_FILE);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Labels::new()),
        Ok(m) if m.is_file() && m.len() <= 4 * 1024 * 1024 => {}
        _ => {
            return Err(format!(
                "the record of where this project's files came from ({LABELS_FILE}) is not a plain file, so the files are not used until the person removes it"
            ))
        }
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Labels>(&t).ok())
        .ok_or_else(|| {
            format!(
                "the record of where this project's files came from ({LABELS_FILE}) does not read, so the files are not used until the person removes it"
            )
        })
}

/// Windows' device names: a file may not be called one, whatever follows
/// the first dot.
const DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Where a project's files are: `<projects>/<id>/files`.
pub fn files_dir_in(projects: &Path, id: &str) -> Result<PathBuf, String> {
    if !crate::projects::id_ok(id) {
        return Err(format!("{id:?} is not a project id"));
    }
    Ok(projects.join(id).join(FOLDER))
}

pub fn files_dir(id: &str) -> Result<PathBuf, String> {
    files_dir_in(&crate::projects::projects_dir(), id)
}

/// Why a file or folder name is refused (F-17), in plain words; `None` for
/// a name that is fine.
pub fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("a name is missing between two slashes".into());
    }
    if name == "." || name == ".." {
        return Some(format!(
            "{name:?} is not a name: a path may not climb out of the project's files"
        ));
    }
    if name.chars().count() > MAX_NAME {
        return Some(format!("a name is longer than {MAX_NAME} characters"));
    }
    if let Some(c) = name.chars().find(|c| {
        crate::canon::hidden_char(*c)
            || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return Some(if c.is_control() || crate::canon::hidden_char(c) {
            format!(
                "the name holds a character that cannot be seen (U+{:04X})",
                c as u32
            )
        } else {
            format!("names may not hold {c:?}")
        });
    }
    if name.starts_with(' ') || name.ends_with(' ') || name.ends_with('.') {
        return Some(format!(
            "{name:?} starts or ends with a space, or ends with a dot, which Windows drops"
        ));
    }
    let lower = name.to_lowercase();
    if lower.starts_with(".git") || lower.starts_with(".columbus") || lower == ".forgejo" {
        return Some(format!(
            "{name:?} is kept for Git and for Columbus itself (names that start with .git or .columbus, and .forgejo)"
        ));
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    if DEVICE_NAMES.contains(&stem.as_str()) {
        return Some(format!(
            "{name:?} is a name Windows keeps for a device ({stem})"
        ));
    }
    None
}

/// The parts of a path as the assistant gives it (`scripts/widths.py`,
/// with `/` between folders), or why it is refused (F-17): absolute paths,
/// `..`, `.git`, `.columbus`, Windows' reserved names, characters Windows
/// refuses or nobody can see, and paths too long or too deep. Names that
/// differ only by case from one already there are refused where the folder
/// is read (the server).
pub fn parts(path: &str) -> Result<Vec<String>, String> {
    let refused = |why: String| format!("{path:?} is refused: {why}");
    if path.is_empty() {
        return Err(
            "give a path inside the project's files, such as scripts/widths.py".to_string(),
        );
    }
    if path.chars().count() > MAX_PATH {
        return Err(refused(format!("it is longer than {MAX_PATH} characters")));
    }
    let mut chars = path.chars();
    let first = chars.next().unwrap_or(' ');
    let drive = first.is_ascii_alphabetic() && chars.next() == Some(':');
    if first == '/' || first == '\\' || first == '~' || drive {
        return Err(refused(
            "it is an absolute path; give one inside the project's files, such as scripts/widths.py"
                .into(),
        ));
    }
    if path.contains('\\') {
        return Err(refused("use / between folders, not \\".into()));
    }
    let parts: Vec<String> = path.split('/').map(String::from).collect();
    for p in &parts {
        if let Some(why) = name_problem(p) {
            return Err(refused(why));
        }
    }
    if parts.len() > MAX_FOLDERS_DEEP + 1 {
        return Err(refused(format!(
            "it is more than {MAX_FOLDERS_DEEP} folders deep"
        )));
    }
    Ok(parts)
}

/// A name that looks like a password, key or connection file
/// ([`file_kinds`], the table shared with sync). The assistant never gets a
/// password or a connection file (D-16, B1), so it neither reads nor writes
/// these, whoever put them in the folder.
pub fn kept_from_assistant(name: &str) -> Option<&'static str> {
    match file_kinds::kind_of(name)? {
        Kind::EnvFile | Kind::Key | Kind::Passwords => Some(
            "it looks like a password or key file, which the assistant never reads or writes",
        ),
        Kind::Connection => Some("it looks like a connection file, which the assistant never reads or writes (connections stay with Columbus)"),
        _ => None,
    }
}

/// Why the assistant may not write a file of this name, if it may not:
/// the project's shared settings (sync writes them), or one of the kinds
/// that never sync ([`file_kinds`]): a password, key or connection file,
/// a type the system runs when it is opened, data, an archive, a file the
/// system makes for itself. Data leaves a chat only through an export
/// (D-19).
pub fn write_problem(parts: &[String]) -> Option<String> {
    let name = parts.last()?;
    if parts.len() == 1 && name.eq_ignore_ascii_case(SETTINGS_FILE) {
        return Some(format!(
            "{SETTINGS_FILE} holds the project's shared settings and is written when the project syncs; the assistant may read it, not change it"
        ));
    }
    const NOT_THOSE: &str =
        "the assistant writes scripts Columbus runs in its sandbox (.py) and notes, not those";
    if let Some(why) = kept_from_assistant(name) {
        return Some(why.into());
    }
    let ext = file_kinds::extension(name);
    match file_kinds::kind_of(name)? {
        Kind::Runs => Some(format!(
            "a .{ext} file is one the system runs or follows when it is opened; {NOT_THOSE}"
        )),
        Kind::Geodata | Kind::Table => Some(format!(
            "a .{ext} file is data, and a project's files are for scripts and notes: results leave a chat only through an export (export_output)"
        )),
        Kind::Archive => Some(format!(
            "a .{ext} file is an archive, and a project's files are for scripts and notes the person can read"
        )),
        Kind::System => Some(format!(
            "{name} is a file the system makes for itself; the assistant does not write it"
        )),
        // Told apart above.
        Kind::EnvFile | Kind::Key | Kind::Passwords | Kind::Connection => None,
    }
}

/// One file in the folder.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Entry {
    /// Its path inside the folder, with `/` between folders.
    pub path: String,
    pub bytes: u64,
    /// When it last changed (RFC 3339, local time), where the system says.
    pub modified: Option<String>,
}

/// What a project's folder holds.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Listing {
    /// Every plain file, sorted by path.
    pub files: Vec<Entry>,
    /// Every folder, sorted, so an empty one shows too.
    pub folders: Vec<String>,
    /// What is there but not offered: links, names the rules refuse, files
    /// too deep. Each with its path and why.
    pub skipped: Vec<Skipped>,
    /// The bytes of all the files listed.
    pub total_bytes: u64,
    /// The listing stopped at its limit: there is more.
    pub more: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Skipped {
    pub path: String,
    pub why: String,
}

/// List the folder at `dir` (the project's files, or a folder inside them
/// whose path there is `prefix`), without following a link anywhere: a
/// link, and anything that is not a plain file or folder, is listed as
/// skipped. At most `cap` files are listed. A folder that is not there
/// yet is an empty listing.
pub fn list(dir: &Path, prefix: &[String], cap: usize) -> std::io::Result<Listing> {
    let mut out = Listing::default();
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
            return Err(std::io::Error::other(
                "it is a link or not a folder, so it is not read",
            ))
        }
        Ok(_) => {}
    }
    walk(dir, prefix, cap, &mut out)?;
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    out.folders.sort();
    out.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn walk(dir: &Path, prefix: &[String], cap: usize, out: &mut Listing) -> std::io::Result<()> {
    let mut names: Vec<(String, std::path::PathBuf)> = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        match e.file_name().into_string() {
            Ok(n) => names.push((n, e.path())),
            Err(_) => out.skipped.push(Skipped {
                path: prefix.join("/"),
                why: "a name in this folder is not text".into(),
            }),
        }
    }
    names.sort();
    for (name, full) in names {
        let mut parts = prefix.to_vec();
        parts.push(name.clone());
        let path = parts.join("/");
        let skip = |out: &mut Listing, why: String| {
            out.skipped.push(Skipped {
                path: path.clone(),
                why,
            })
        };
        // Columbus's own half-written files are not the folder's.
        if name.starts_with(TEMPORARY_PREFIX) {
            continue;
        }
        if let Some(why) = name_problem(&name) {
            skip(out, why);
            continue;
        }
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(e) => {
                skip(out, format!("it cannot be read ({e})"));
                continue;
            }
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            skip(out, "it is a link, and links are never followed".into());
        } else if kind.is_dir() {
            if parts.len() > MAX_FOLDERS_DEEP {
                skip(
                    out,
                    format!("it is more than {MAX_FOLDERS_DEEP} folders deep"),
                );
                continue;
            }
            out.folders.push(path.clone());
            walk(&full, &parts, cap, out)?;
        } else if kind.is_file() {
            if out.files.len() >= cap {
                out.more = true;
                continue;
            }
            out.total_bytes += meta.len();
            out.files.push(Entry {
                path,
                bytes: meta.len(),
                modified: meta.modified().ok().map(|t| {
                    chrono::DateTime::<chrono::Local>::from(t)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
                }),
            });
        } else {
            skip(out, "it is not a plain file".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_the_rules_refuse() {
        for ok in [
            "widths.py",
            "scripts/widths.py",
            "notes/2026/November review.md",
            "a/b/c/d/e/f/g/h/i.txt",
            "report (my copy 2026-11-10).md",
            "Straße/données.csv",
            "columbus-project.yaml",
            ".hidden/notes.txt",
        ] {
            assert!(parts(ok).is_ok(), "{ok}: {:?}", parts(ok));
        }
        assert_eq!(
            parts("scripts/widths.py").unwrap(),
            ["scripts", "widths.py"]
        );
        for (bad, why) in [
            ("", "give a path"),
            ("/etc/passwd", "absolute"),
            ("\\\\server\\share\\x.py", "absolute"),
            ("C:\\Users\\me\\x.py", "absolute"),
            ("c:x.py", "absolute"),
            ("~/x.py", "absolute"),
            ("scripts\\x.py", "use / between"),
            ("../x.py", "climb out"),
            ("scripts/../../x.py", "climb out"),
            ("scripts/./x.py", "climb out"),
            ("scripts//x.py", "missing between"),
            ("scripts/", "missing between"),
            (".git/config", "kept for Git"),
            ("x/.GIT/hooks/pre-commit", "kept for Git"),
            (".github/workflows/ci.yml", "kept for Git"),
            (".gitignore", "kept for Git"),
            (".columbus/state.json", "kept for Git"),
            (".Columbus-tmp-1", "kept for Git"),
            ("scripts/nul.py", "Windows keeps"),
            ("NUL", "Windows keeps"),
            ("Com1.txt", "Windows keeps"),
            ("aux", "Windows keeps"),
            ("lpt9.tar.gz", "Windows keeps"),
            ("ab:c.py", "may not hold"),
            ("what?.py", "may not hold"),
            ("a*.py", "may not hold"),
            ("x.py ", "Windows drops"),
            ("x.py.", "Windows drops"),
            (" x.py", "Windows drops"),
            ("a\u{0}b.py", "cannot be seen"),
            ("a\nb.py", "cannot be seen"),
            ("a\u{202E}b.py", "cannot be seen"),
            (".g\u{200C}it/config", "cannot be seen"),
            ("a/b/c/d/e/f/g/h/i/j.txt", "folders deep"),
        ] {
            let e = parts(bad).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
        assert!(parts(&"n".repeat(MAX_NAME + 1))
            .unwrap_err()
            .contains("longer than"));
        assert!(parts(&format!("{}/x", "ab/".repeat(100)))
            .unwrap_err()
            .contains("longer than"));
    }

    #[test]
    fn what_the_assistant_may_not_write_or_read() {
        let p = |s: &str| parts(s).unwrap();
        for ok in [
            "scripts/widths.py",
            "notes.md",
            "data/codes.md",
            "settings.json",
            "query.sql",
            "style.qml",
            "docs/columbus-project.yaml",
            "run.sh",
            "tool.ps1",
            "outputs/notes.txt",
        ] {
            assert_eq!(write_problem(&p(ok)), None, "{ok}");
        }
        for (bad, why) in [
            ("columbus-project.yaml", "shared settings"),
            ("Columbus-Project.YAML", "shared settings"),
            ("open me.bat", "runs or follows"),
            ("scripts/setup.CMD", "runs or follows"),
            ("report.pdf.lnk", "runs or follows"),
            ("link.url", "runs or follows"),
            ("run.vbs", "runs or follows"),
            ("page.hta", "runs or follows"),
            ("x.js", "runs or follows"),
            ("start.command", "runs or follows"),
            ("app.desktop", "runs or follows"),
            (".env", "password or key"),
            (".env.local", "password or key"),
            ("certs/server.pem", "password or key"),
            ("id_rsa", "password or key"),
            (".pgpass", "password or key"),
            ("home/.netrc", "password or key"),
            ("prod.sde", "connection file"),
            ("server.AGS", "connection file"),
            ("prod.dsn", "connection file"),
            // Data (R-103): results leave a chat only through an export.
            ("data/codes.csv", "only through an export"),
            ("codes.TSV", "only through an export"),
            ("roads.geojson", "only through an export"),
            ("roads.gpkg", "only through an export"),
            ("book.xlsx", "only through an export"),
            ("big.parquet", "only through an export"),
            ("places.kml", "only through an export"),
            ("bundle.zip", "archive"),
            ("desktop.ini", "the system makes"),
        ] {
            let e = write_problem(&p(bad)).unwrap_or_else(|| panic!("{bad} was allowed"));
            assert!(e.contains(why), "{bad}: {e}");
        }
        assert!(kept_from_assistant("notes.md").is_none());
        assert!(kept_from_assistant("keys.txt").is_none());
        // Data and files that run are not written, and can be read when the
        // person put them there: only passwords, keys and connection files
        // are kept from the assistant altogether.
        for readable in ["codes.csv", "open me.bat", "bundle.zip", "roads.geojson"] {
            assert!(kept_from_assistant(readable).is_none(), "{readable}");
        }
    }

    /// Every entry of the table shared with sync
    /// ([`crate::file_kinds`]): the assistant writes none of them, and
    /// neither reads nor writes the ones that hold secrets.
    #[test]
    fn the_assistant_writes_nothing_that_never_syncs() {
        for (name, kind) in crate::file_kinds::examples() {
            for path in [name.clone(), format!("a/{name}")] {
                // A name the path rules refuse is refused before this
                // (`.git-credentials`: names that start with .git).
                let refused = match parts(&path) {
                    Err(_) => true,
                    Ok(p) => write_problem(&p).is_some(),
                };
                assert!(refused, "{path} ({kind:?}) may be written");
            }
            assert_eq!(
                kept_from_assistant(&name).is_some(),
                kind.is_secret(),
                "{name} ({kind:?})"
            );
        }
    }

    #[test]
    fn the_listing_never_follows_a_link() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("files");
        // Not there yet: an empty listing.
        assert_eq!(list(&root, &[], 100).unwrap(), Listing::default());
        std::fs::create_dir_all(root.join("scripts/deep")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("scripts/widths.py"), "print(1)\n").unwrap();
        std::fs::write(root.join("scripts/deep/b.py"), "b").unwrap();
        std::fs::write(root.join("notes.md"), "# notes").unwrap();
        std::fs::write(root.join(".git/config"), "x").unwrap();
        std::fs::write(root.join(".columbus-tmp-abc"), "half").unwrap();
        #[cfg(unix)]
        {
            let outside = d.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("secret.txt"), "s").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("link-folder")).unwrap();
            std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("scripts/link.py"))
                .unwrap();
        }
        let l = list(&root, &[], 100).unwrap();
        let paths: Vec<&str> = l.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            ["notes.md", "scripts/deep/b.py", "scripts/widths.py"]
        );
        assert_eq!(l.folders, ["empty", "scripts", "scripts/deep"]);
        assert_eq!(l.total_bytes, 7 + 1 + 9);
        assert!(l.files.iter().all(|f| f.modified.is_some()));
        assert!(!l.more);
        let skipped: Vec<&str> = l.skipped.iter().map(|s| s.path.as_str()).collect();
        if cfg!(unix) {
            assert_eq!(skipped, [".git", "link-folder", "scripts/link.py"]);
            assert!(l.skipped[1].why.contains("link"));
        } else {
            assert_eq!(skipped, [".git"]);
        }
        // A folder inside, with its place in the paths; and the limit.
        let s = list(&root.join("scripts"), &["scripts".into()], 1).unwrap();
        assert_eq!(s.files.len(), 1);
        assert!(s.more);
        assert_eq!(s.files[0].path, "scripts/deep/b.py");
        // The folder itself a link: refused.
        #[cfg(unix)]
        {
            let l = d.path().join("files-link");
            std::os::unix::fs::symlink(&root, &l).unwrap();
            assert!(list(&l, &[], 10).is_err());
        }
        assert!(
            files_dir_in(d.path(), "roadway-qc")
                .unwrap()
                .ends_with("roadway-qc/files")
                || cfg!(windows)
        );
        assert!(files_dir_in(d.path(), "../x").is_err());
    }
}
