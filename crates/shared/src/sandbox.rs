//! The session workspace and path checks (design section 3, "Sandbox").
//!
//! Relative paths land in the workspace. Any path outside it or the folders
//! the person granted is refused. Checks are lexical first (so `..` cannot
//! climb out), then on the real file system (so a symlink or junction inside
//! the workspace cannot point out of it).

use std::path::{Component, Path, PathBuf};

use crate::error::Denied;

#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
    granted: Vec<PathBuf>,
}

/// Windows device names that open devices instead of files.
const DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "CONIN$",
    "CONOUT$",
];

impl Sandbox {
    /// Create the workspace folder if needed.
    pub fn new(root: impl Into<PathBuf>, granted: Vec<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        let root = std::fs::canonicalize(&root)?;
        let granted = granted
            .into_iter()
            .filter_map(|g| std::fs::canonicalize(g).ok())
            .collect();
        Ok(Sandbox { root, granted })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn granted(&self) -> &[PathBuf] {
        &self.granted
    }

    /// Resolve a path the agent supplied. Returns an absolute path inside
    /// the workspace or a granted folder, or refuses.
    pub fn resolve(&self, requested: &str) -> Result<PathBuf, Denied> {
        if requested.trim().is_empty() {
            return Err(Denied::Path("empty path".into()));
        }
        let lower = requested.to_ascii_lowercase();
        if lower.starts_with(r"\\?\")
            || lower.starts_with(r"\\.\")
            || lower.starts_with("//?/")
            || lower.starts_with("//./")
        {
            return Err(Denied::Path(
                "device and verbatim paths are not allowed".into(),
            ));
        }
        if requested.contains('\0') {
            return Err(Denied::Path("path contains a NUL byte".into()));
        }
        let candidate = Path::new(requested);
        let joined = if candidate.is_absolute() || candidate.has_root() {
            candidate.to_path_buf()
        } else {
            self.root.join(candidate)
        };
        let lexical = normalize_lexically(&joined).ok_or_else(|| {
            Denied::Path(format!("{requested:?} climbs above the file system root"))
        })?;
        check_components(&lexical)?;

        let real = real_path(&lexical)
            .map_err(|e| Denied::Path(format!("cannot inspect {requested:?}: {e}")))?;
        if self.contains(&real) {
            Ok(real)
        } else {
            Err(Denied::Path(format!(
                "{requested:?} is outside the workspace and the granted folders"
            )))
        }
    }

    fn contains(&self, p: &Path) -> bool {
        p.starts_with(&self.root) || self.granted.iter().any(|g| p.starts_with(g))
    }
}

/// Remove `.` and resolve `..` without touching the file system.
/// Returns None if `..` would go above the root.
fn normalize_lexically(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() || out.as_os_str().is_empty() {
                    return None;
                }
            }
            Component::Normal(s) => out.push(s),
        }
    }
    Some(out)
}

fn check_components(p: &Path) -> Result<(), Denied> {
    for c in p.components() {
        if let Component::Normal(s) = c {
            let s = s.to_string_lossy();
            // Alternate data streams (`file.txt:hidden`) on Windows.
            if cfg!(windows) && s.contains(':') {
                return Err(Denied::Path(
                    "alternate data streams are not allowed".into(),
                ));
            }
            let stem = s
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end()
                .to_ascii_uppercase();
            if DEVICE_NAMES.contains(&stem.as_str()) {
                return Err(Denied::Path(format!("{s:?} is a device name")));
            }
        }
    }
    Ok(())
}

/// Canonicalize the longest existing ancestor (following symlinks and
/// junctions), then re-append the parts that do not exist yet.
fn real_path(p: &Path) -> std::io::Result<PathBuf> {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => {
                rest.push(name.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }
    let mut real = std::fs::canonicalize(&existing)?;
    for part in rest.into_iter().rev() {
        real.push(part);
    }
    Ok(real)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (tempfile::TempDir, tempfile::TempDir, Sandbox) {
        let ws = tempfile::tempdir().unwrap();
        let granted = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(ws.path().join("session"), vec![granted.path().into()]).unwrap();
        (ws, granted, sb)
    }

    #[test]
    fn relative_paths_land_in_workspace() {
        let (_w, _g, sb) = sandbox();
        let p = sb.resolve("out/roads.gpkg").unwrap();
        assert!(p.starts_with(sb.root()));
        assert!(p.ends_with("out/roads.gpkg"));
    }

    #[test]
    fn climbing_out_is_refused() {
        let (_w, _g, sb) = sandbox();
        assert!(sb.resolve("../escape.txt").is_err());
        assert!(sb.resolve("a/../../escape.txt").is_err());
        assert!(sb.resolve("a/../ok.txt").is_ok());
    }

    #[test]
    fn absolute_paths_must_be_granted() {
        let (_w, g, sb) = sandbox();
        assert!(sb
            .resolve(&g.path().join("data.gpkg").to_string_lossy())
            .is_ok());
        let outside = tempfile::tempdir().unwrap();
        assert!(sb
            .resolve(&outside.path().join("x").to_string_lossy())
            .is_err());
    }

    #[test]
    fn device_names_and_verbatim_paths_refused() {
        let (_w, _g, sb) = sandbox();
        assert!(sb.resolve("NUL").is_err());
        assert!(sb.resolve("out/con.txt").is_err());
        assert!(sb.resolve(r"\\?\C:\Windows\System32").is_err());
        assert!(sb.resolve(r"\\.\PhysicalDrive0").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_out_of_workspace_is_refused() {
        let (_w, _g, sb) = sandbox();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), sb.root().join("link")).unwrap();
        assert!(sb.resolve("link/secret.txt").is_err());
    }
}
