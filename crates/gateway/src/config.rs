//! Where the gateway finds its policy, connections, workspace and audit log.
//!
//! Windows defaults follow the design (section 3 and 9). Every location can
//! be overridden with an environment variable for development and tests.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use columbus_shared::policy::{Connection, OrgPolicy};

#[derive(Debug, Clone)]
pub struct Paths {
    pub policy: PathBuf,
    pub connections: PathBuf,
    pub workspaces: PathBuf,
    pub audit: PathBuf,
    /// Organization-managed MCP server manifests.
    pub servers: PathBuf,
    /// Personal MCP server manifests (used only if policy allows).
    pub personal_servers: PathBuf,
    /// Organization-installed engine plugins.
    pub plugins: PathBuf,
    /// Personal engine plugins (used only if policy allows).
    pub personal_plugins: PathBuf,
    pub worker_script: PathBuf,
    pub python: PathBuf,
    /// The geo-registry index (`registry.sqlite` and `index.sha256`).
    pub registry: PathBuf,
    /// The organization's context pack (a folder with `pack.yaml`).
    pub context: PathBuf,
    /// The person's own tools (`tools/` beside the workspaces, or
    /// `COLUMBUS_TOOLS`): installed ones and proposals waiting for them.
    pub tools: PathBuf,
    /// The person's registered Python environments (F-4): `environments/`
    /// beside the connections folder, or `COLUMBUS_ENVIRONMENTS`.
    pub environments: PathBuf,
    /// Where Columbus creates environments (`<Columbus data>/envs`).
    pub envs: PathBuf,
    /// The person's saved web map layers (F-13): `map-sources/` beside the
    /// connections folder, or `COLUMBUS_MAP_SOURCES`.
    pub map_sources: PathBuf,
}

#[cfg(test)]
use columbus_shared::paths::is_placeholder;
use columbus_shared::paths::{env_path, platform_dirs};

impl Paths {
    pub fn from_env() -> Self {
        let (program_data, roaming, local) = platform_dirs();
        Paths {
            policy: columbus_shared::paths::policy_file(),
            connections: columbus_shared::paths::connections_dir(),
            workspaces: env_path("COLUMBUS_WORKSPACES").unwrap_or_else(|| local.join("workspaces")),
            audit: env_path("COLUMBUS_AUDIT_DIR").unwrap_or_else(|| program_data.join("audit")),
            servers: env_path("COLUMBUS_SERVERS").unwrap_or_else(|| program_data.join("servers")),
            personal_servers: env_path("COLUMBUS_PERSONAL_SERVERS")
                .unwrap_or_else(|| roaming.join("servers")),
            plugins: env_path("COLUMBUS_PLUGINS").unwrap_or_else(|| program_data.join("plugins")),
            personal_plugins: env_path("COLUMBUS_PERSONAL_PLUGINS")
                .unwrap_or_else(|| roaming.join("plugins")),
            worker_script: env_path("COLUMBUS_WORKER").unwrap_or_else(default_worker_script),
            registry: env_path("COLUMBUS_REGISTRY")
                .unwrap_or_else(|| program_data.join("registry")),
            context: env_path("COLUMBUS_CONTEXT").unwrap_or_else(|| program_data.join("context")),
            tools: columbus_shared::tools::tools_dir(),
            environments: columbus_shared::environments::environments_dir(),
            envs: columbus_shared::environments::envs_dir(),
            map_sources: columbus_shared::map_sources::map_sources_dir(),
            python: env_path("COLUMBUS_PYTHON")
                .or_else(bundled_engine)
                .unwrap_or_else(|| {
                    PathBuf::from(if cfg!(windows) {
                        "python.exe"
                    } else {
                        "python3"
                    })
                }),
        }
    }
}

/// The engine environment a release sets up beside the executable
/// (`engine\setup-engine.ps1` makes `engine\venv`; a pixi environment works
/// too), so no separate Python install has to be on PATH.
fn bundled_engine() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidates: &[&[&str]] = if cfg!(windows) {
        &[
            &["engine", "venv", "Scripts", "python.exe"],
            &["engine", ".pixi", "envs", "default", "python.exe"],
        ]
    } else {
        &[
            &["engine", "venv", "bin", "python3"],
            &["engine", ".pixi", "envs", "default", "bin", "python3"],
        ]
    };
    let in_folder = |dir: &Path| {
        candidates
            .iter()
            .map(|parts| parts.iter().fold(dir.to_path_buf(), |p, s| p.join(s)))
            .find(|p| p.is_file())
    };
    // Beside the program; else in the person's own Columbus folder, where
    // the install guide sets it up (the Columbus app's own server has none
    // beside it).
    in_folder(dir).or_else(|| {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|v| !v.is_empty())?;
        in_folder(&PathBuf::from(home).join("Columbus"))
    })
}

/// The companion window's files: `COLUMBUS_UI_DIR`, else `ui/` beside the
/// executable (where releases put it), when it holds an `index.html`.
pub fn ui_dir() -> Option<PathBuf> {
    if let Some(d) = env_path("COLUMBUS_UI_DIR") {
        return Some(d);
    }
    let exe = std::env::current_exe().ok()?;
    let d = exe.parent()?.join("ui");
    d.join("index.html").is_file().then_some(d)
}

/// Where running sessions announce themselves to the Columbus app:
/// `COLUMBUS_LIVE_DIR`, else `live/` beside the session workspaces.
pub fn live_dir(paths: &Paths) -> PathBuf {
    env_path("COLUMBUS_LIVE_DIR").unwrap_or_else(|| {
        paths
            .workspaces
            .parent()
            .unwrap_or(&paths.workspaces)
            .join("live")
    })
}

/// The worker script ships beside the executable in releases
/// (`workers/python/columbus_worker.py`) and in the source tree.
fn default_worker_script() -> PathBuf {
    let rel = Path::new("workers")
        .join("python")
        .join("columbus_worker.py");
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(5) {
            let candidate = dir.join(&rel);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    rel
}

/// Where the policy came from, reported by `whoami`.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct PolicySource {
    pub path: String,
    pub found: bool,
}

pub fn load_policy(paths: &Paths) -> Result<(OrgPolicy, PolicySource)> {
    let found = paths.policy.exists();
    let policy = if found {
        OrgPolicy::load(&paths.policy).with_context(|| "loading organization policy")?
    } else {
        OrgPolicy::default()
    };
    Ok((
        policy,
        PolicySource {
            path: paths.policy.display().to_string(),
            found,
        },
    ))
}

pub fn load_connections(paths: &Paths) -> Result<Vec<Connection>> {
    Connection::load_dir(&paths.connections).with_context(|| "loading connections")
}

/// What a session reads from its settings files: the organization's policy
/// and the person's connections. A running session re-reads them whenever
/// they change (see `State::refresh`).
#[derive(Debug, Clone)]
pub struct Settings {
    pub policy: OrgPolicy,
    pub source: PolicySource,
    pub connections: Vec<Connection>,
    /// The policy file exists but sets nothing (empty, or comments only).
    pub policy_empty: bool,
}

/// Read the settings files once: a hash of exactly the bytes read, and the
/// settings parsed from those same bytes (so a file that changes while it
/// is read is seen as changed again next time, never half-applied). The
/// hash covers every connection file's name and contents and the policy
/// file's, so any change, or a read that fails, shows as a new hash.
pub fn read_settings(paths: &Paths) -> ([u8; 32], Result<Settings>) {
    let (digest, _, settings) = read_settings_at(paths);
    (digest, settings)
}

/// A regular file's bytes; anything else (a pipe, a device, a folder) is
/// refused without being opened, so it can never stall the server.
fn read_regular(path: &Path) -> std::io::Result<Vec<u8>> {
    let md = std::fs::metadata(path)?;
    if !md.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    std::fs::read(path)
}

/// As [`read_settings`], with the newest modification time among the files
/// (and the connections folder), so a caller can wait until a change has
/// settled before using it.
pub fn read_settings_at(
    paths: &Paths,
) -> ([u8; 32], Option<std::time::SystemTime>, Result<Settings>) {
    use sha2::{Digest, Sha256};
    let mut newest: Option<std::time::SystemTime> = None;
    let mut seen = |p: &Path| {
        if let Ok(t) = std::fs::metadata(p).and_then(|m| m.modified()) {
            newest = Some(newest.map_or(t, |n| n.max(t)));
        }
    };
    seen(&paths.policy);
    seen(&paths.connections);
    let mut h = Sha256::new();
    let mut feed = |tag: &[u8], bytes: &[u8]| {
        h.update((tag.len() as u64).to_le_bytes());
        h.update(tag);
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    };
    let policy_bytes = match read_regular(&paths.policy) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    };
    match &policy_bytes {
        Ok(Some(b)) => feed(b"policy", b),
        Ok(None) => feed(b"policy-absent", b""),
        Err(e) => feed(b"policy-error", e.to_string().as_bytes()),
    }
    let mut files: Vec<(PathBuf, std::io::Result<Vec<u8>>)> = Vec::new();
    let listing = match std::fs::read_dir(&paths.connections) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(e) => {
                        let path = e.path();
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                        if ext == "yaml" || ext == "yml" {
                            seen(&path);
                            let bytes = read_regular(&path);
                            files.push((path, bytes));
                        }
                    }
                    Err(e) => feed(b"listing-error", e.to_string().as_bytes()),
                }
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            feed(b"connections-absent", b"");
            Ok(())
        }
        Err(e) => {
            feed(b"connections-error", e.to_string().as_bytes());
            Err(e)
        }
    };
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, bytes) in &files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match bytes {
            Ok(b) => feed(name.as_bytes(), b),
            Err(e) => feed(format!("{name}/error").as_bytes(), e.to_string().as_bytes()),
        }
    }
    let digest: [u8; 32] = h.finalize().into();
    // Times again, after reading: a file written while it was read shows as
    // just changed, so it is not taken as settled.
    seen(&paths.policy);
    seen(&paths.connections);
    for (path, _) in &files {
        seen(path);
    }

    let parsed = (|| -> Result<Settings> {
        let found = match policy_bytes {
            Ok(Some(b)) => Some(b),
            Ok(None) => None,
            Err(e) => {
                return Err(anyhow::anyhow!("{}: {e}", paths.policy.display()))
                    .context("loading organization policy")
            }
        };
        let mut policy_empty = false;
        let policy = match &found {
            Some(b) => {
                let text = std::str::from_utf8(b).map_err(|e| {
                    anyhow::anyhow!("{}: not UTF-8 text ({e})", paths.policy.display())
                })?;
                policy_empty = match serde_yaml::from_str::<serde_yaml::Value>(text) {
                    Ok(serde_yaml::Value::Null) => true,
                    Ok(serde_yaml::Value::Mapping(m)) => m.is_empty(),
                    _ => false,
                };
                OrgPolicy::parse(text, &paths.policy).context("loading organization policy")?
            }
            None => OrgPolicy::default(),
        };
        if let Err(e) = listing {
            return Err(anyhow::anyhow!("{}: {e}", paths.connections.display()))
                .context("loading connections");
        }
        let mut connections = Vec::new();
        for (path, bytes) in files {
            let bytes = bytes
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
                .context("loading connections")?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|e| anyhow::anyhow!("{}: not UTF-8 text ({e})", path.display()))
                .context("loading connections")?;
            connections.push(Connection::parse(text, &path).context("loading connections")?);
        }
        Connection::check_ids(&mut connections).context("loading connections")?;
        Ok(Settings {
            policy,
            source: PolicySource {
                path: paths.policy.display().to_string(),
                found: found.is_some(),
            },
            connections,
            policy_empty,
        })
    })();
    (digest, newest, parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfilled_mcpb_settings_count_as_unset() {
        use std::ffi::OsStr;
        assert!(is_placeholder(OsStr::new("${user_config.python}")));
        assert!(!is_placeholder(OsStr::new(
            "/Users/me/engine/venv/bin/python3"
        )));
        assert!(!is_placeholder(OsStr::new(
            "C:\\Columbus\\${odd}\\python.exe"
        )));
    }
}
