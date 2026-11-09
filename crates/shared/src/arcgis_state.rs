//! When each ArcGIS connection's refresh token ends (R-19, REQ-26): an
//! internal, non-secret record beside the connection files,
//! `connections/.arcgis-state.json`, as `{"<id>": {"refresh_expires_at",
//! "portal"}}`. Never a token: the tokens stay in the credential store
//! entry `arcgis:<id>`, whose F-3b format gains no key. Written by
//! `arcgis signin` and by each exchange; removed with the sign-in (sign-out,
//! or the connection deleted). Missing or unreadable means "not known",
//! and the gateway then exchanges the refresh token (the safe side). Same
//! pattern as `environments/.confirmed.json`: no MCP tool writes the
//! connections folder, and confined scripts can't write outside their run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const FILE: &str = ".arcgis-state.json";

/// One connection's line.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SignInState {
    /// When the refresh token ends (ISO 8601, UTC).
    pub refresh_expires_at: String,
    /// The portal it is for.
    pub portal: String,
}

pub fn path(connections: &Path) -> PathBuf {
    connections.join(FILE)
}

pub fn read(connections: &Path) -> BTreeMap<String, SignInState> {
    std::fs::read(path(connections))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write(connections: &Path, all: &BTreeMap<String, SignInState>) -> Result<(), String> {
    let p = path(connections);
    if all.is_empty() {
        return match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", p.display()))
            }
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    crate::tools::write_atomic(&p, &bytes)
}

/// Record that `id`'s refresh token for `portal` ends `secs` from now, as
/// the portal said. Unknown (`None`, or not positive) removes the line.
pub fn record(connections: &Path, id: &str, portal: &str, secs: Option<i64>) -> Result<(), String> {
    let mut all = read(connections);
    match secs.filter(|s| *s > 0) {
        Some(s) => {
            let end = Utc::now() + chrono::Duration::seconds(s);
            all.insert(
                id.to_string(),
                SignInState {
                    refresh_expires_at: end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    portal: portal.to_string(),
                },
            );
        }
        None => {
            all.remove(id);
        }
    }
    write(connections, &all)
}

/// Forget `id`'s line.
pub fn forget(connections: &Path, id: &str) -> Result<(), String> {
    let mut all = read(connections);
    if all.remove(id).is_none() {
        return Ok(());
    }
    write(connections, &all)
}

/// When `id`'s refresh token for `portal` ends, if recorded and readable.
pub fn expires_at(connections: &Path, id: &str, portal: &str) -> Option<DateTime<Utc>> {
    let s = read(connections).remove(id)?;
    if s.portal != portal {
        return None;
    }
    DateTime::parse_from_rfc3339(&s.refresh_expires_at)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_read_and_forgotten() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path();
        assert!(expires_at(c, "svc", "https://p").is_none(), "nothing yet");
        record(c, "svc", "https://p", Some(14 * 86_400)).unwrap();
        record(c, "other", "https://p", Some(60)).unwrap();
        let end = expires_at(c, "svc", "https://p").unwrap();
        let left = end - Utc::now();
        assert!(left > chrono::Duration::days(13) && left <= chrono::Duration::days(14));
        assert!(
            expires_at(c, "svc", "https://elsewhere").is_none(),
            "another portal"
        );
        let text = std::fs::read_to_string(path(c)).unwrap();
        assert!(
            text.contains("refresh_expires_at") && !text.contains("token\""),
            "{text}"
        );
        record(c, "svc", "https://p", None).unwrap();
        assert!(expires_at(c, "svc", "https://p").is_none(), "unknown");
        forget(c, "other").unwrap();
        assert!(!path(c).exists(), "an empty record is removed");
        std::fs::write(path(c), "not json").unwrap();
        assert!(
            expires_at(c, "svc", "https://p").is_none(),
            "unreadable is unknown"
        );
    }
}
