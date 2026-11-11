//! Which paths may sync, and what never does.
//!
//! A synced folder is shared by colleagues on Windows, macOS and Linux, so
//! a path must be one every system can hold: the rules of F-17 are applied
//! on every system alike, to what is in the folder and to what the server
//! lists. Paths are relative, with `/` between their parts.

use columbus_shared::file_kinds::{self, FolderKind, Kind};

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

/// Why a path never syncs by what its name says it is, or `None`. The
/// table is `columbus_shared::file_kinds`, the same one that says what the
/// assistant may not write into a project's files, so the two cannot drift
/// (R-103). Kept out, on both sides (never sent, never fetched):
///
/// * passwords, keys and connection files (`.env*`, `.pgpass`, `id_rsa`,
///   `.pem`, `.sde`, `.dsn` …);
/// * data (geodata such as `.gpkg`, shapefile parts, `.geojson`, rasters;
///   tables and databases such as `.csv`, `.xlsx`, `.parquet`, `.sqlite`):
///   data leaves Columbus only through an export (D-19, D-20);
/// * archives and disk images;
/// * files the system runs or follows when they are opened (`.bat`,
///   `.lnk`, `.url` …);
/// * `__pycache__/`, `.venv/`, `outputs/`, `*.gdb/`, and the files the
///   system itself leaves in folders.
///
/// Letter case does not matter. The words finish the sentence
/// "<path> does not sync: …".
pub fn ignored(path: &str) -> Option<&'static str> {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().unwrap_or("");
    for folder in &parts {
        if let Some(why) = ignored_folder(folder) {
            return Some(why);
        }
    }
    Some(match file_kinds::kind_of(name)? {
        Kind::EnvFile => "an .env file is on the ignore list (it usually holds secrets)",
        Kind::Key => "keys and certificates are on the ignore list",
        Kind::Passwords => "password files are on the ignore list",
        Kind::Connection => "connection files are on the ignore list (they can hold a password)",
        Kind::Runs => RUNS,
        Kind::Geodata => "geodata is on the ignore list (data leaves Columbus only through export)",
        Kind::Table => {
            "tables and databases are on the ignore list (data leaves Columbus only through export)"
        }
        Kind::Archive => "archives are on the ignore list",
        Kind::System => "a file the system makes is on the ignore list",
    })
}

const RUNS: &str = "it is a kind the system runs or follows when it is opened (a program, a shortcut, an installer), which never syncs";

/// The same for a folder's own name (so a walk need not enter it).
pub fn ignored_folder(name: &str) -> Option<&'static str> {
    if let Some(kind) = file_kinds::folder_kind(name) {
        return Some(match kind {
            FolderKind::Outputs => "the outputs folder is on the ignore list",
            FolderKind::Venv => "a .venv folder is on the ignore list",
            FolderKind::PyCache => "a __pycache__ folder is on the ignore list",
            FolderKind::Geodatabase => "a file geodatabase (.gdb) is on the ignore list",
        });
    }
    None
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
            // Tables and databases (R-103).
            ("data.csv", "tables"),
            ("data/codes.TSV", "tables"),
            ("book.xlsx", "tables"),
            ("old.xls", "tables"),
            ("big.parquet", "tables"),
            ("cache.sqlite", "tables"),
            ("cache.duckdb", "tables"),
            ("access.mdb", "tables"),
            ("roads.gpkg-wal", "tables"),
            // More geodata.
            ("roads.geojson", "geodata"),
            ("roads.fgb", "geodata"),
            ("places.kml", "geodata"),
            ("places.kmz", "geodata"),
            ("track.gpx", "geodata"),
            ("parcels.gml", "geodata"),
            // Password, key and connection files, by the names the
            // assistant is kept from.
            (".pgpass", "password"),
            ("home/.netrc", "password"),
            ("_netrc", "password"),
            ("pgpass.conf", "password"),
            ("aws/credentials", "password"),
            ("id_rsa", "keys"),
            ("id_ed25519", "keys"),
            ("store.jks", "keys"),
            ("debug.keystore", "keys"),
            ("vault.kdbx", "keys"),
            ("putty.ppk", "keys"),
            ("prod.dsn", "connection"),
            ("prod.udl", "connection"),
            ("cube.odc", "connection"),
            // Files the system runs or follows when they are opened.
            ("open me.bat", "runs or follows"),
            ("report.pdf.lnk", "runs or follows"),
            // More archives.
            ("bundle.7z", "archives"),
            ("bundle.tar.gz", "archives"),
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
            "csv.md",
            "settings.json",
            "run.sh",
            "tool.ps1",
            "query.sql",
            "columbus-project.yaml",
        ] {
            assert_eq!(ignored(path), None, "{path}");
        }
    }

    /// Every entry of the table sync shares with the assistant's project
    /// files (`columbus_shared::file_kinds`) is kept out of a sync, as a
    /// file at the top and inside a folder.
    #[test]
    fn everything_in_the_shared_table_never_syncs() {
        for (name, kind) in columbus_shared::file_kinds::examples() {
            for path in [name.clone(), format!("a/b/{name}")] {
                assert!(ignored(&path).is_some(), "{path} ({kind:?}) would sync");
            }
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
