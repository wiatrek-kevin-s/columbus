//! Where Columbus keeps its settings, for the server and the Columbus app
//! alike, so the two never disagree.
//!
//! * The organization's folder: `ProgramData\Columbus` on Windows,
//!   `/Library/Application Support/Columbus` on macOS, `/etc/columbus`
//!   elsewhere. Holds `policy.yaml`.
//! * The person's settings: `%APPDATA%\Columbus`,
//!   `~/Library/Application Support/Columbus` or `$XDG_CONFIG_HOME/columbus`.
//!   Holds `connections/`.
//! * The person's local data: `%LOCALAPPDATA%\Columbus`, the same folder on
//!   macOS, or `$XDG_DATA_HOME/columbus`.
//!
//! `COLUMBUS_POLICY` and `COLUMBUS_CONNECTIONS` override the two settings
//! locations.

use std::path::PathBuf;

/// A location from the environment. Empty counts as unset, and so does an
/// unfilled `${...}` placeholder: Claude desktop passes an optional `.mcpb`
/// setting the person left empty as the literal `${user_config.<name>}`.
pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty() && !is_placeholder(v))
        .map(PathBuf::from)
}

pub fn is_placeholder(v: &std::ffi::OsStr) -> bool {
    v.to_str()
        .map(str::trim)
        .is_some_and(|s| s.starts_with("${") && s.ends_with('}'))
}

/// (organization folder, person's settings, person's local data).
pub fn platform_dirs() -> (PathBuf, PathBuf, PathBuf) {
    if cfg!(target_os = "macos") {
        let home = env_path("HOME").unwrap_or_else(|| PathBuf::from("."));
        let user = home.join("Library/Application Support/Columbus");
        (
            PathBuf::from("/Library/Application Support/Columbus"),
            user.clone(),
            user,
        )
    } else if cfg!(windows) {
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

/// The organization's policy file.
pub fn policy_file() -> PathBuf {
    env_path("COLUMBUS_POLICY").unwrap_or_else(|| platform_dirs().0.join("policy.yaml"))
}

/// The person's connections folder.
pub fn connections_dir() -> PathBuf {
    env_path("COLUMBUS_CONNECTIONS").unwrap_or_else(|| platform_dirs().1.join("connections"))
}

/// The folders of connected servers' manifests: the organization's and the
/// person's.
pub fn servers_dirs() -> Vec<PathBuf> {
    let (org, user, _) = platform_dirs();
    vec![
        env_path("COLUMBUS_SERVERS").unwrap_or_else(|| org.join("servers")),
        env_path("COLUMBUS_PERSONAL_SERVERS").unwrap_or_else(|| user.join("servers")),
    ]
}
