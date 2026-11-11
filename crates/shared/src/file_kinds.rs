//! What a file's name says it is: one table for the two places that must
//! agree (final review of part 3, R-103).
//!
//! * The assistant's project files ([`crate::project_files`]): names it
//!   neither reads nor writes (passwords, keys, connection files, map
//!   documents), and kinds it never writes (data, archives, files the
//!   system runs when they are opened).
//! * Sync (`columbus-sync`): what never goes to a Git server and is never
//!   fetched from one.
//!
//! A kind added here is kept out of both, so the two cannot drift. Letter
//! case never matters. A name is one part of a path: `roads.csv`, not
//! `data/roads.csv`.

/// The kind of a file that is kept out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `.env`, `.env.local` …: settings that usually hold secrets.
    EnvFile,
    /// A key or a store of keys: `*.pem`, `*.key`, `*.pfx`, `id_rsa` …
    Key,
    /// A file that holds passwords by its name: `.pgpass`, `.netrc`,
    /// `credentials` …
    Passwords,
    /// A connection file, which can hold a password: `*.sde`, `*.dsn` …
    Connection,
    /// A map document or a layer file, which can hold a saved connection
    /// (a server, a user, sometimes its password): `*.aprx`, `*.mxd`,
    /// `*.lyrx`, `*.qgs` …
    MapDocument,
    /// A file the system runs or follows when the person opens it:
    /// `*.bat`, `*.lnk`, `*.rdp` …
    Runs,
    /// Geodata: `*.gpkg`, the parts of a shapefile, rasters, point clouds …
    Geodata,
    /// A table or a database: `*.csv`, `*.xlsx`, `*.parquet`, `*.sqlite` …
    Table,
    /// An archive or a disk image: `*.zip`, `*.7z`, `*.iso` …
    Archive,
    /// A file the system itself leaves in folders: `.DS_Store` …
    System,
}

impl Kind {
    /// A password, a key, a connection file, or a map document that can
    /// hold a saved connection: the assistant never gets one (D-16, B1),
    /// whoever put it in the folder.
    pub fn is_secret(self) -> bool {
        matches!(
            self,
            Kind::EnvFile | Kind::Key | Kind::Passwords | Kind::Connection | Kind::MapDocument
        )
    }

    /// Data: it leaves Columbus only through an export (D-19).
    pub fn is_data(self) -> bool {
        matches!(self, Kind::Geodata | Kind::Table)
    }
}

/// A folder whose contents never sync, wherever it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderKind {
    /// `outputs/`: results, by convention.
    Outputs,
    /// `.venv/`: a Python environment.
    Venv,
    /// `__pycache__/`: Python's compiled files.
    PyCache,
    /// `*.gdb/`: a file geodatabase.
    Geodatabase,
}

/// Files that hold passwords or tokens, by their whole name: Git's, the
/// `.netrc` and PostgreSQL's password files, a cloud's `credentials`, and
/// the files where npm, Python's package tools, MySQL, Docker and a web
/// server keep theirs.
const PASSWORD_NAMES: &[&str] = &[
    ".dockercfg",
    ".git-credentials",
    ".htpasswd",
    ".my.cnf",
    ".netrc",
    ".npmrc",
    ".pgpass",
    ".pypirc",
    "_netrc",
    "credentials",
    "pgpass.conf",
];

/// Private keys, by their whole name (SSH's).
const KEY_NAMES: &[&str] = &["id_dsa", "id_ecdsa", "id_ed25519", "id_rsa"];

/// Keys, certificates with their keys, and stores of keys or passwords.
const KEY_ENDINGS: &[&str] = &["jks", "kdbx", "key", "keystore", "p12", "pem", "pfx", "ppk"];

/// Connection files: ArcGIS (`sde`, `ags`, `acs`), Office and ODBC (`odc`,
/// `dsn`, `udl`). They name a server and can hold its password.
const CONNECTION_ENDINGS: &[&str] = &["acs", "ags", "dsn", "odc", "sde", "udl"];

/// Map documents and layer files: ArcGIS Pro (`aprx`, its template
/// `aptx`, `mapx`, `pagx`, `lyrx`), ArcMap, ArcScene and ArcGlobe (`mxd`,
/// `mxt`, `sxd`, `3dd`, `lyr`), QGIS (`qgs`, `qgz`, `qlr`). Each layer in
/// one says where its data is, and for a database or a service that is a
/// saved connection: a server, a user, sometimes the password.
const MAP_DOCUMENT_ENDINGS: &[&str] = &[
    "3dd", "aprx", "aptx", "lyr", "lyrx", "mapx", "mxd", "mxt", "pagx", "qgs", "qgz", "qlr", "sxd",
];

/// File types an operating system runs or follows when the person opens
/// them (a double-click in Explorer or Finder): programs and installers,
/// scripts the system's own script hosts run, shortcuts and files that
/// point somewhere, Office query files, remote desktop files, macOS
/// bundles. `.py`, `.sh`, `.ps1`, `.sql`, `.md` and `.json` are not here:
/// they open in an editor, or are what a project's files are for.
const RUNS_WHEN_OPENED: &[&str] = &[
    // Windows: programs, installers, libraries.
    "appinstaller",
    "appx",
    "appxbundle",
    "bat",
    "cmd",
    "com",
    "cpl",
    "dll",
    "drv",
    "exe",
    "msc",
    "msi",
    "msix",
    "msixbundle",
    "msp",
    "mst",
    "msu",
    "ocx",
    "pif",
    "scr",
    "sys",
    // Windows: scripts its script hosts run, and their settings.
    "hta",
    "js",
    "jse",
    "ps1xml",
    "psc1",
    "sct",
    "vbe",
    "vbs",
    "wsc",
    "wsf",
    "wsh",
    // Python files the launcher runs without a window, and compiled ones.
    "pyc",
    "pyo",
    "pyw",
    "pyz",
    "pyzw",
    // Shortcuts, and files that point somewhere or change settings.
    "application",
    "appref-ms",
    "chm",
    "diagcab",
    "diagcfg",
    "diagpkg",
    "deskthemepack",
    "hlp",
    "inf",
    "library-ms",
    "lnk",
    "prf",
    "rdp",
    "reg",
    "scf",
    "search-ms",
    "searchconnector-ms",
    "settingcontent-ms",
    "theme",
    "themepack",
    "url",
    "website",
    // Office: files that fetch or run when Excel, Word, PowerPoint or
    // Access opens them, and documents, templates and add-ins that carry
    // macros. (`xlsm`, a workbook with macros, is among the tables.)
    "accda",
    "accde",
    "ade",
    "adp",
    "docm",
    "dotm",
    "dqy",
    "iqy",
    "mad",
    "maf",
    "mag",
    "mam",
    "maq",
    "mar",
    "mas",
    "mau",
    "mav",
    "maw",
    "mda",
    "mde",
    "mdz",
    "oqy",
    "potm",
    "ppam",
    "ppsm",
    "pptm",
    "rqy",
    "sldm",
    "slk",
    "wll",
    "xla",
    "xlam",
    "xll",
    "xltm",
    // Java.
    "jar",
    "jnlp",
    // macOS: bundles and files Finder runs, installs or follows.
    "action",
    "app",
    "command",
    "fileloc",
    "inetloc",
    "mobileconfig",
    "mpkg",
    "pkg",
    "prefpane",
    "saver",
    "terminal",
    "tool",
    "webloc",
    "workflow",
    // Linux desktops: launchers and packages.
    "deb",
    "desktop",
    "flatpakref",
    "flatpakrepo",
    "rpm",
];

/// Geodata: GeoPackage, the parts of a shapefile, GeoJSON and its line
/// forms, FlatGeobuf, KML, GPX, GML, MapInfo and CAD drawings, rasters and
/// grids (also as text: `asc`, `xyz`), point clouds, OpenStreetMap
/// extracts, tile sets, Esri's packages and mobile geodatabases, and the
/// files of a file geodatabase found outside their `.gdb` folder.
const GEODATA_ENDINGS: &[&str] = &[
    "aih",
    "ain",
    "asc",
    "atx",
    "cpg",
    "dbf",
    "dgn",
    "dwg",
    "dxf",
    "e57",
    "ecw",
    "fbn",
    "fbx",
    "fgb",
    "gdbindexes",
    "gdbtable",
    "gdbtablx",
    "geodatabase",
    "geojson",
    "geojsonl",
    "geojsons",
    "gml",
    "gpk",
    "gpkg",
    "gpkx",
    "gpx",
    "grb",
    "grib",
    "grib2",
    "h5",
    "hdf",
    "hdf5",
    "img",
    "ixs",
    "jp2",
    "kml",
    "kmz",
    "las",
    "laz",
    "lpk",
    "lpkx",
    "mbtiles",
    "mif",
    "mmpk",
    "mpk",
    "mpkx",
    "mxs",
    "nc",
    "osm",
    "ovr",
    "pbf",
    "pmtiles",
    "ppkx",
    "prj",
    "qix",
    "qpj",
    "sbn",
    "sbx",
    "sd",
    "shp",
    "shx",
    "sid",
    "slpk",
    "spx",
    "tif",
    "tiff",
    "topojson",
    "tpk",
    "tpkx",
    "vrt",
    "vtpk",
    "xyz",
];

/// Tables and databases: delimited text, spreadsheets, column files,
/// records one per line, tables as Python, R, MATLAB and statistics
/// programs save them, and database files with the files they keep beside
/// them.
const TABLE_ENDINGS: &[&str] = &[
    "accdb",
    "arrow",
    "csv",
    "db",
    "db3",
    "dta",
    "duckdb",
    "feather",
    "geoparquet",
    "jsonl",
    "mat",
    "mdb",
    "ndjson",
    "npy",
    "npz",
    "ods",
    "parquet",
    "pickle",
    "pkl",
    "rdata",
    "rds",
    "sas7bdat",
    "sav",
    "sqlite",
    "sqlite3",
    "tab",
    "tsv",
    "wal",
    "xls",
    "xlsb",
    "xlsm",
    "xlsx",
];

/// What a database keeps beside its file while it is open: `roads.gpkg-wal`.
const DATABASE_SIDE_FILES: &[&str] = &["-journal", "-shm", "-wal"];

/// Archives and disk images: whatever they hold travels unseen.
const ARCHIVE_ENDINGS: &[&str] = &[
    "7z", "bz2", "cab", "dmg", "gz", "iso", "rar", "tar", "tbz2", "tgz", "txz", "vhd", "vhdx",
    "xz", "zip", "zst",
];

/// Files the system itself leaves in folders.
const SYSTEM_NAMES: &[&str] = &[".ds_store", "desktop.ini", "thumbs.db"];

/// Folders that never sync, wherever they are.
const FOLDERS: &[(&str, FolderKind)] = &[
    ("__pycache__", FolderKind::PyCache),
    (".venv", FolderKind::Venv),
    ("outputs", FolderKind::Outputs),
];

/// What follows a name's last dot, in lower case; empty when it has no dot.
pub fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default()
}

/// The extension that makes a name one the system runs or follows when it
/// is opened, if it is one. For a file's name and for a folder's (a macOS
/// bundle is a folder: `Tool.app`, `Do.workflow`).
pub fn runs_when_opened(name: &str) -> Option<String> {
    let ext = extension(name);
    // `Name.{…}`: Explorer opens the system object with that id.
    let shell_object = ext.starts_with('{') && ext.ends_with('}');
    (RUNS_WHEN_OPENED.contains(&ext.as_str()) || shell_object).then_some(ext)
}

/// The kind of a file by its name, if it is one that is kept out.
pub fn kind_of(name: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    if SYSTEM_NAMES.contains(&lower.as_str()) {
        return Some(Kind::System);
    }
    if lower.starts_with(".env") {
        return Some(Kind::EnvFile);
    }
    if PASSWORD_NAMES.contains(&lower.as_str()) {
        return Some(Kind::Passwords);
    }
    if KEY_NAMES.contains(&lower.as_str()) {
        return Some(Kind::Key);
    }
    let ext = extension(&lower);
    let ext = ext.as_str();
    if KEY_ENDINGS.contains(&ext) {
        return Some(Kind::Key);
    }
    if CONNECTION_ENDINGS.contains(&ext) {
        return Some(Kind::Connection);
    }
    if MAP_DOCUMENT_ENDINGS.contains(&ext) {
        return Some(Kind::MapDocument);
    }
    if runs_when_opened(&lower).is_some() {
        return Some(Kind::Runs);
    }
    if GEODATA_ENDINGS.contains(&ext) {
        return Some(Kind::Geodata);
    }
    if TABLE_ENDINGS.contains(&ext) || DATABASE_SIDE_FILES.iter().any(|s| ext.ends_with(s)) {
        return Some(Kind::Table);
    }
    if ARCHIVE_ENDINGS.contains(&ext) {
        return Some(Kind::Archive);
    }
    None
}

/// The kind of a folder by its name, if its contents never sync.
pub fn folder_kind(name: &str) -> Option<FolderKind> {
    let lower = name.to_lowercase();
    if let Some((_, kind)) = FOLDERS.iter().find(|(n, _)| *n == lower) {
        return Some(*kind);
    }
    lower.ends_with(".gdb").then_some(FolderKind::Geodatabase)
}

/// Every ending and name in the table, with its kind: for the tests of
/// the two crates that use it, so that each proves it keeps out all of
/// them.
pub fn examples() -> Vec<(String, Kind)> {
    let mut out: Vec<(String, Kind)> = Vec::new();
    let lists: [(&[&str], Kind); 7] = [
        (KEY_ENDINGS, Kind::Key),
        (CONNECTION_ENDINGS, Kind::Connection),
        (MAP_DOCUMENT_ENDINGS, Kind::MapDocument),
        (RUNS_WHEN_OPENED, Kind::Runs),
        (GEODATA_ENDINGS, Kind::Geodata),
        (TABLE_ENDINGS, Kind::Table),
        (ARCHIVE_ENDINGS, Kind::Archive),
    ];
    for (endings, kind) in lists {
        out.extend(endings.iter().map(|e| (format!("thing.{e}"), kind)));
    }
    out.extend(
        DATABASE_SIDE_FILES
            .iter()
            .map(|s| (format!("thing.gpkg{s}"), Kind::Table)),
    );
    out.extend(
        PASSWORD_NAMES
            .iter()
            .map(|n| (n.to_string(), Kind::Passwords)),
    );
    out.extend(KEY_NAMES.iter().map(|n| (n.to_string(), Kind::Key)));
    out.extend(SYSTEM_NAMES.iter().map(|n| (n.to_string(), Kind::System)));
    out.push((".env".into(), Kind::EnvFile));
    out.push((".env.local".into(), Kind::EnvFile));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_of_the_table_is_found_in_any_letter_case() {
        let all = examples();
        assert!(all.len() > 200, "{}", all.len());
        for (name, kind) in &all {
            assert_eq!(kind_of(name), Some(*kind), "{name}");
            assert_eq!(kind_of(&name.to_uppercase()), Some(*kind), "{name}");
        }
        // No ending is in two lists: each name has one kind.
        let mut seen = std::collections::BTreeSet::new();
        for (name, _) in &all {
            assert!(seen.insert(name.clone()), "{name} is listed twice");
        }
    }

    #[test]
    fn the_kinds_the_review_named() {
        // Data formats Columbus reads as layers or tables.
        for name in [
            "a.csv",
            "a.tsv",
            "a.geojson",
            "a.parquet",
            "a.fgb",
            "a.kml",
            "a.kmz",
            "a.xlsx",
            "a.xls",
            "a.sqlite",
            "a.duckdb",
            "a.mdb",
            "a.gpx",
            "a.gml",
            "a.gpkg",
            "a.shp",
            "a.tif",
            "roads.gpkg-wal",
            "roads.sqlite-journal",
            // The re-check's (R-105): tile sets, grids and points as text,
            // OpenStreetMap extracts, a file geodatabase's loose files.
            "tiles.mbtiles",
            "tiles.pmtiles",
            "dem.asc",
            "points.xyz",
            "town.osm",
            "town.osm.pbf",
            "a00000009.gdbtable",
            "a00000009.gdbtablx",
            "a00000009.gdbindexes",
            "a00000009.spx",
            "a00000009.FDO_UUID.atx",
        ] {
            assert!(kind_of(name).is_some_and(Kind::is_data), "{name}");
        }
        // Names that look like passwords, keys or connection files.
        for name in [
            ".pgpass",
            ".netrc",
            "_netrc",
            "pgpass.conf",
            "credentials",
            "id_rsa",
            "id_ed25519",
            ".env",
            ".env.production",
            "x.dsn",
            "x.udl",
            "x.odc",
            "x.kdbx",
            "x.ppk",
            "x.jks",
            "x.keystore",
            "x.sde",
            "x.ags",
            "x.pem",
            "x.key",
            "x.pfx",
            "x.p12",
            // Other tools' password and token files (R-105).
            ".npmrc",
            ".pypirc",
            ".my.cnf",
            ".htpasswd",
            ".dockercfg",
        ] {
            assert!(kind_of(name).is_some_and(Kind::is_secret), "{name}");
        }
        // Map documents and layer files, which can hold a saved connection
        // (R-105).
        for name in [
            "County.aprx",
            "roads.mxd",
            "Roads.MXD",
            "roads.mapx",
            "roads.lyrx",
            "roads.lyr",
            "county.qgs",
            "county.qgz",
            "roads.qlr",
            "layout.pagx",
        ] {
            assert_eq!(kind_of(name), Some(Kind::MapDocument), "{name}");
            assert!(kind_of(name).is_some_and(Kind::is_secret), "{name}");
        }
        // Files that run when opened.
        for name in [
            "a.pyw",
            "a.pyz",
            "a.pyzw",
            "a.rdp",
            "a.iqy",
            "a.slk",
            "a.bat",
            "a.lnk",
            "a.url",
            "a.app",
            "a.workflow",
            "a.command",
            // Office files that carry macros (R-105).
            "a.docm",
            "a.dotm",
            "a.pptm",
            "a.ppam",
            "a.xlam",
            "a.xltm",
        ] {
            assert_eq!(kind_of(name), Some(Kind::Runs), "{name}");
            assert!(runs_when_opened(name).is_some(), "{name}");
        }
        assert_eq!(
            runs_when_opened("Panel.{21EC2020-3AEA-1069-A2DD-08002B30309D}").as_deref(),
            Some("{21ec2020-3aea-1069-a2dd-08002b30309d}")
        );
        // A workbook with macros stays what it was: a table.
        assert_eq!(kind_of("book.xlsm"), Some(Kind::Table));
    }

    #[test]
    fn what_is_not_in_the_table() {
        // Scripts, notes and settings: what a project's files are for.
        for name in [
            "widths.py",
            "notes.md",
            "query.sql",
            "settings.json",
            "environment.yml",
            "style.qml",
            "run.sh",
            "tool.ps1",
            "readme.txt",
            "report.pdf",
            "map.png",
            "notebook.ipynb",
            "columbus-project.yaml",
            "keys.md",
            "csv",
            "my.env.txt",
            "data.csv.md",
            "zip.py",
            "Makefile",
            // Whole names only: another file that merely ends so is not one.
            "notes.npmrc",
            "x.pypirc",
            "npmrc",
            "report.docx",
            "slides.pptx",
            "style.qml",
            "legend.sld",
        ] {
            assert_eq!(kind_of(name), None, "{name}");
            assert_eq!(runs_when_opened(name), None, "{name}");
        }
    }

    #[test]
    fn folders_that_never_sync() {
        for (name, kind) in [
            ("outputs", FolderKind::Outputs),
            ("Outputs", FolderKind::Outputs),
            (".venv", FolderKind::Venv),
            ("__pycache__", FolderKind::PyCache),
            ("roads.gdb", FolderKind::Geodatabase),
            ("Roads.GDB", FolderKind::Geodatabase),
        ] {
            assert_eq!(folder_kind(name), Some(kind), "{name}");
        }
        for name in ["scripts", "output", "venv", "gdb", "a.gdb.txt"] {
            assert_eq!(folder_kind(name), None, "{name}");
        }
    }
}
