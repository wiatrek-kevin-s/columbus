//! Connection secrets (design section 3, "Connections").
//!
//! Passwords live in the operating system's credential store, never in
//! configuration files, logs, audit lines or tool results, and are never
//! typed into a chat. On Windows that is Credential Manager (DPAPI, per
//! user), under the target `columbus:<connection id>`. The person sets one
//! with `columbus-mcp secret set <connection id>` (or, later, the app).
//!
//! The gateway reads a secret only to open a connection, passes it to the
//! engine worker inside that one request over the private stdio pipe, and
//! never returns it to the agent.
//!
//! Development and CI: with `COLUMBUS_DEV_SECRETS=1`, secrets are read from
//! `COLUMBUS_SECRET_<ID>` environment variables instead (id upper-cased,
//! non-alphanumerics as `_`). `whoami` reports this mode so it cannot be
//! left on unnoticed. Platforms without a native store use only this mode
//! for now; Columbus is Windows-first. Entries Columbus writes itself (OAuth
//! tokens for remote servers) need `COLUMBUS_DEV_SECRETS_DIR` in this mode:
//! they are kept there as plain files, for tests only.

use anyhow::{anyhow, Result};

#[cfg_attr(not(windows), allow(dead_code))]
pub const SERVICE: &str = "columbus";

pub enum SecretStore {
    #[cfg(windows)]
    Windows,
    DevEnv(Option<std::path::PathBuf>),
    Unavailable(String),
}

impl SecretStore {
    pub fn from_env() -> Self {
        if std::env::var("COLUMBUS_DEV_SECRETS").as_deref() == Ok("1") {
            return SecretStore::DevEnv(
                std::env::var_os("COLUMBUS_DEV_SECRETS_DIR").map(Into::into),
            );
        }
        Self::native()
    }

    #[cfg(windows)]
    fn native() -> Self {
        match windows_native_keyring_store::Store::new() {
            Ok(store) => {
                keyring_core::set_default_store(store);
                SecretStore::Windows
            }
            Err(e) => SecretStore::Unavailable(format!("Windows Credential Manager: {e}")),
        }
    }

    #[cfg(not(windows))]
    fn native() -> Self {
        SecretStore::Unavailable(
            "no credential store on this platform yet (Windows Credential Manager only); \
             set COLUMBUS_DEV_SECRETS=1 for development"
                .into(),
        )
    }

    /// Shown by `whoami`.
    pub fn describe(&self) -> String {
        match self {
            #[cfg(windows)]
            SecretStore::Windows => "Windows Credential Manager".into(),
            SecretStore::DevEnv(_) => {
                "DEVELOPMENT MODE: secrets read from COLUMBUS_SECRET_* environment variables".into()
            }
            SecretStore::Unavailable(why) => format!("unavailable: {why}"),
        }
    }

    pub fn is_dev(&self) -> bool {
        matches!(self, SecretStore::DevEnv(_))
    }

    fn env_name(id: &str) -> String {
        let up: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("COLUMBUS_SECRET_{up}")
    }

    /// The password for a connection, or None if none is stored.
    pub fn get(&self, connection_id: &str) -> Result<Option<String>> {
        match self {
            #[cfg(windows)]
            SecretStore::Windows => {
                let entry = keyring_core::Entry::new(SERVICE, connection_id)?;
                match entry.get_password() {
                    Ok(p) => Ok(Some(p)),
                    Err(keyring_core::Error::NoEntry) => Ok(None),
                    Err(e) => Err(anyhow!("reading the credential store failed: {e}")),
                }
            }
            SecretStore::DevEnv(dir) => {
                if let Ok(v) = std::env::var(Self::env_name(connection_id)) {
                    return Ok(Some(v));
                }
                Ok(dir.as_ref().and_then(|d| {
                    std::fs::read_to_string(d.join(Self::env_name(connection_id))).ok()
                }))
            }
            SecretStore::Unavailable(why) => Err(anyhow!("credential store {why}")),
        }
    }

    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn set(&self, connection_id: &str, secret: &str) -> Result<()> {
        match self {
            #[cfg(windows)]
            SecretStore::Windows => {
                keyring_core::Entry::new(SERVICE, connection_id)?.set_password(secret)?;
                Ok(())
            }
            SecretStore::DevEnv(Some(dir)) => {
                std::fs::create_dir_all(dir)?;
                std::fs::write(dir.join(Self::env_name(connection_id)), secret)?;
                Ok(())
            }
            SecretStore::DevEnv(None) => Err(anyhow!(
                "development mode reads {} from the environment; nothing to store",
                Self::env_name(connection_id)
            )),
            SecretStore::Unavailable(why) => Err(anyhow!("credential store {why}")),
        }
    }

    #[cfg_attr(not(windows), allow(unused_variables))]
    pub fn delete(&self, connection_id: &str) -> Result<bool> {
        match self {
            #[cfg(windows)]
            SecretStore::Windows => {
                match keyring_core::Entry::new(SERVICE, connection_id)?.delete_credential() {
                    Ok(()) => Ok(true),
                    Err(keyring_core::Error::NoEntry) => Ok(false),
                    Err(e) => Err(anyhow!("deleting from the credential store failed: {e}")),
                }
            }
            SecretStore::DevEnv(dir) => Ok(dir.as_ref().is_some_and(|d| {
                std::fs::remove_file(d.join(Self::env_name(connection_id))).is_ok()
            })),
            SecretStore::Unavailable(why) => Err(anyhow!("credential store {why}")),
        }
    }
}

/// The broker keeps OAuth tokens for remote servers here too.
impl columbus_broker::oauth::SecretStore for SecretStore {
    fn get(&self, key: &str) -> std::result::Result<Option<String>, String> {
        SecretStore::get(self, key).map_err(|e| e.to_string())
    }
    fn set(&self, key: &str, value: &str) -> std::result::Result<(), String> {
        SecretStore::set(self, key, value).map_err(|e| e.to_string())
    }
    fn delete(&self, key: &str) -> std::result::Result<(), String> {
        SecretStore::delete(self, key)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_names() {
        assert_eq!(
            SecretStore::env_name("roads-prod"),
            "COLUMBUS_SECRET_ROADS_PROD"
        );
        assert_eq!(SecretStore::env_name("gis.pg"), "COLUMBUS_SECRET_GIS_PG");
    }
}
