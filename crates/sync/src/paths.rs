//! Which paths may sync, and what never does.
//!
//! A synced folder is shared by colleagues on Windows, macOS and Linux, so
//! a path must be one every system can hold: the rules of F-17 are applied
//! on every system alike, to what is in the folder and to what the server
//! lists. Paths are relative, with `/` between their parts.

/// A file larger than this never syncs (D-20).
pub const MAX_FILE: u64 = 5 * 1024 * 1024;

/// The project's shared settings, at the top of the folder (F-17).
pub const SETTINGS_FILE: &str = "columbus-project.yaml";

/// The longest relative path that syncs, in characters: with the folder it
/// lives in, a longer one may not fit a Windows path.
pub const MAX_PATH_CHARS: usize = 200;

/// Windows device names: a file of that name (with any extension) cannot be
/// made there.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com1", "com2", "com3", "com4", "com5",
    "com6", "com7", "com8", "com9", "com¹", "com²", "com³", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
    "lpt6", "lpt7", "lpt8", "lpt9", "lpt¹", "lpt²", "lpt³",
];

/// Why a relative path is refused (F-17), or `None` if it may sync. The
/// words finish the sentence "<path> does not sync: …".
pub fn refused(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("it has no name");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Some("it is an absolute path");
    }
    if path.contains('\\') {
        return Some("its name has a backslash");
    }
    if path.chars().count() > MAX_PATH_CHARS {
        return Some("its path is longer than 200 characters");
    }
    for part in path.split('/') {
        if let Some(why) = refused_name(part) {
            return Some(why);
        }
    }
    None
}

/// Why one name (a folder's or a file's) is refused, or `None`.
pub fn refused_name(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("it has an empty name in its path");
    }
    if name == "." || name == ".." {
        return Some("its path has . or .. in it");
    }
    if name.len() > 255 {
        return Some("a name in its path is longer than 255 bytes");
    }
    if name.chars().any(|c| c.is_control()) {
        return Some("its name has a control character");
    }
    // Characters a file system may drop when it compares names (so that
    // ".g\u{200c}it" would be ".git" there).
    if name.chars().any(|c| {
        matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
    }) {
        return Some("its name has an invisible character");
    }
    if name
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Some("its name has a character Windows does not allow (< > : \" | ? *)");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("its name ends with a dot or a space, which Windows does not allow");
    }
    let lower = name.to_lowercase();
    // Git's own folder, by any spelling a file system may take for it
    // (case, and the short name NTFS gives it).
    if lower == ".git" || lower == "git~1" {
        return Some("it is in a .git folder");
    }
    if lower == ".columbus" {
        return Some("it is in a .columbus folder");
    }
    let stem = lower.split('.').next().unwrap_or("").trim_end_matches(' ');
    if RESERVED.contains(&stem) {
        return Some("its name is one Windows keeps for itself (CON, NUL, COM1 …)");
    }
    None
}

/// File name endings that never sync: keys and connection files, geodata
/// formats, archives.
const IGNORED_ENDINGS: &[&str] = &[
    ".pem", ".key", ".pfx", ".p12", ".sde", ".ags", ".gpkg", ".shp", ".shx", ".dbf", ".prj",
    ".cpg", ".sbn", ".sbx", ".tif", ".tiff", ".las", ".laz", ".zip",
];

/// Folders that never sync, wherever they are.
const IGNORED_FOLDERS: &[&str] = &["__pycache__", ".venv", "outputs"];

/// Files the system itself leaves in folders.
const SYSTEM_FILES: &[&str] = &[".ds_store", "thumbs.db", "desktop.ini"];

/// The rule of the ignore list a path falls under, or `None`. Data, secrets
/// and outputs stay out of a repository (D-20): `.env*`, keys and
/// certificates, connection files (`.sde`, `.ags`), geodata (`*.gdb/`,
/// `.gpkg`, shapefile parts, rasters, point clouds), archives,
/// `__pycache__/`, `.venv/` and `outputs/`. Letter case does not matter.
pub fn ignored(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    let mut parts: Vec<&str> = lower.split('/').collect();
    let name = parts.pop().unwrap_or("");
    for folder in &parts {
        if let Some(why) = ignored_folder(folder) {
            return Some(why);
        }
    }
    if name.starts_with(".env") {
        return Some("an .env file is on the ignore list (it usually holds secrets)");
    }
    if SYSTEM_FILES.contains(&name) {
        return Some("a file the system makes is on the ignore list");
    }
    if IGNORED_ENDINGS.iter().any(|e| name.ends_with(e)) {
        return Some(ending_rule(name));
    }
    None
}

/// The same for a folder's own name (so a walk need not enter it).
pub fn ignored_folder(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if IGNORED_FOLDERS.contains(&lower.as_str()) {
        return Some(match lower.as_str() {
            "outputs" => "the outputs folder is on the ignore list",
            ".venv" => "a .venv folder is on the ignore list",
            _ => "a __pycache__ folder is on the ignore list",
        });
    }
    if lower.ends_with(".gdb") {
        return Some("a file geodatabase (.gdb) is on the ignore list");
    }
    None
}

fn ending_rule(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext {
        "pem" | "key" | "pfx" | "p12" => "keys and certificates are on the ignore list",
        "sde" | "ags" => "connection files are on the ignore list (they can hold a password)",
        "zip" => "archives are on the ignore list",
        _ => "geodata is on the ignore list (data leaves Columbus only through export)",
    }
}

/// The name a person's version is kept under beside a file both sides
/// changed: `notes.md` becomes `notes (my copy 2026-11-11).md`; `n` above 1
/// gives `notes (my copy 2026-11-11 2).md` when that name is taken.
pub fn copy_name(path: &str, date: &str, n: u32) -> String {
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (Some(d), n),
        None => (None, path),
    };
    // The last dot splits the name, unless it leads it (".gitignore").
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let tail = if n > 1 {
        format!(" (my copy {date} {n})")
    } else {
        format!(" (my copy {date})")
    };
    let copy = format!("{stem}{tail}{ext}");
    match dir {
        Some(d) => format!("{d}/{copy}"),
        None => copy,
    }
}

/// A path as a file system that ignores letter case sees it: two paths with
/// the same key cannot both be in one folder on Windows or (usually) macOS.
pub fn case_key(path: &str) -> String {
    path.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_f17_refuses() {
        for ok in [
            "a.py",
            "scripts/widths.py",
            "notes/Übersicht 2026.md",
            ".gitignore",
            "columbus-project.yaml",
            "a b/c d.txt",
            "git/x",
            "x.git",
            "connect.txt",
            "com10.txt",
            "nulls.csv",
        ] {
            assert_eq!(refused(ok), None, "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "\\\\server\\share\\x",
            "C:\\x",
            "C:/x",
            "a\\b",
            "../x",
            "a/../b",
            "a/./b",
            "a//b",
            ".git/config",
            "a/.GIT/hooks/pre-commit",
            "a/git~1/config",
            "GIT~1/x",
            ".columbus/trash/x",
            "a/.Columbus/x",
            "NUL",
            "nul.txt",
            "a/CON",
            "aux.tar.gz",
            "COM1",
            "lpt9.log",
            "com¹",
            "a:b.txt",
            "what?.md",
            "star*.md",
            "quote\".md",
            "pipe|.md",
            "less<.md",
            "name.",
            "name ",
            "dir./x",
            "tab\tname",
            "new\nline",
            ".g\u{200c}it/config",
            "a/\u{feff}b",
        ] {
            assert!(refused(bad).is_some(), "{bad:?} should be refused");
        }
        assert!(refused(&"x".repeat(201)).is_some());
        assert!(refused(&"x".repeat(200)).is_none());
        assert!(
            refused(&format!("{}/a", "é".repeat(130))).is_some(),
            "a name over 255 bytes"
        );
    }

    #[test]
    fn the_ignore_list() {
        for (path, word) in [
            (".env", ".env"),
            (".env.local", ".env"),
            ("conf/.ENV.production", ".env"),
            ("server.pem", "keys"),
            ("id_rsa.key", "keys"),
            ("cert.PFX", "keys"),
            ("store.p12", "keys"),
            ("prod.sde", "connection"),
            ("server.ags", "connection"),
            ("__pycache__/x.pyc", "__pycache__"),
            ("scripts/__pycache__/x.cpython-312.pyc", "__pycache__"),
            (".venv/pyvenv.cfg", ".venv"),
            ("data/roads.gdb/a00000001.gdbtable", "geodatabase"),
            ("Roads.GDB/gdb", "geodatabase"),
            ("parcels.gpkg", "geodata"),
            ("roads.shp", "geodata"),
            ("roads.shx", "geodata"),
            ("roads.dbf", "geodata"),
            ("roads.prj", "geodata"),
            ("roads.cpg", "geodata"),
            ("roads.sbn", "geodata"),
            ("roads.sbx", "geodata"),
            ("dem.tif", "geodata"),
            ("dem.TIFF", "geodata"),
            ("cloud.las", "geodata"),
            ("cloud.laz", "geodata"),
            ("bundle.zip", "archives"),
            ("outputs/report.csv", "outputs"),
            ("a/Outputs/b/c.txt", "outputs"),
            (".DS_Store", "system"),
            ("pics/Thumbs.db", "system"),
            ("desktop.ini", "system"),
        ] {
            let why = ignored(path);
            assert!(why.is_some_and(|w| w.contains(word)), "{path}: {why:?}");
        }
        for path in [
            "scripts/widths.py",
            "notes.md",
            "environment.yml",
            "keys.md",
            "my.env.txt",
            "outputs.md",
            "shp_notes.txt",
            "a.gdb.txt",
            "zip.py",
            "data.csv",
            "columbus-project.yaml",
        ] {
            assert_eq!(ignored(path), None, "{path}");
        }
    }

    #[test]
    fn copy_names() {
        let d = "2026-11-11";
        assert_eq!(copy_name("notes.md", d, 1), "notes (my copy 2026-11-11).md");
        assert_eq!(
            copy_name("a/b/notes.md", d, 1),
            "a/b/notes (my copy 2026-11-11).md"
        );
        assert_eq!(copy_name("Makefile", d, 1), "Makefile (my copy 2026-11-11)");
        assert_eq!(
            copy_name(".gitignore", d, 1),
            ".gitignore (my copy 2026-11-11)"
        );
        assert_eq!(
            copy_name("x.tar.gz", d, 2),
            "x.tar (my copy 2026-11-11 2).gz"
        );
        // A copy's name may itself sync once the person keeps both.
        assert_eq!(refused(&copy_name("a/notes.md", d, 3)), None);
        assert_eq!(ignored(&copy_name("a/notes.md", d, 3)), None);
    }
}
