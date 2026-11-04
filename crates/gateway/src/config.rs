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
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Paths {
    pub fn from_env() -> Self {
        let (program_data, roaming, local) = platform_dirs();
        Paths {
            policy: env_path("COLUMBUS_POLICY").unwrap_or_else(|| program_data.join("policy.yaml")),
            connections: env_path("COLUMBUS_CONNECTIONS")
                .unwrap_or_else(|| roaming.join("connections")),
            workspaces: env_path("COLUMBUS_WORKSPACES").unwrap_or_else(|| local.join("workspaces")),
            audit: env_path("COLUMBUS_AUDIT_DIR").unwrap_or_else(|| program_data.join("audit")),
            servers: env_path("COLUMBUS_SERVERS").unwrap_or_else(|| program_data.join("servers")),
            personal_servers: env_path("COLUMBUS_PERSONAL_SERVERS")
                .unwrap_or_else(|| roaming.join("servers")),
            plugins: env_path("COLUMBUS_PLUGINS").unwrap_or_else(|| program_data.join("plugins")),
            personal_plugins: env_path("COLUMBUS_PERSONAL_PLUGINS")
                .unwrap_or_else(|| roaming.join("plugins")),
            worker_script: env_path("COLUMBUS_WORKER").unwrap_or_else(default_worker_script),
            python: env_path("COLUMBUS_PYTHON").unwrap_or_else(|| {
                PathBuf::from(if cfg!(windows) {
                    "python.exe"
                } else {
                    "python3"
                })
            }),
        }
    }
}

/// (ProgramData\Columbus, %APPDATA%\Columbus, %LOCALAPPDATA%\Columbus) on
/// Windows; /etc/columbus and XDG folders elsewhere.
fn platform_dirs() -> (PathBuf, PathBuf, PathBuf) {
    if cfg!(windows) {
        let pd = env_path("ProgramData").unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        let roaming = env_path("APPDATA").unwrap_or_else(|| pd.clone());
        let local = env_path("LOCALAPPDATA").unwrap_or_else(|| roaming.clone());
        (
            pd.join("Columbus"),
            roaming.join("Columbus"),
            local.join("Columbus"),
        )
    } else {
        let home = env_path("HOME").unwrap_or_else(|| PathBuf::from("."));
        let config = env_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let data = env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        (
            PathBuf::from("/etc/columbus"),
            config.join("columbus"),
            data.join("columbus"),
        )
    }
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
