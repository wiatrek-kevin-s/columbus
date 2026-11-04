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
//! for now; Columbus is Windows-first.

use anyhow::{anyhow, Result};

#[cfg_attr(not(windows), allow(dead_code))]
pub const SERVICE: &str = "columbus";

pub enum SecretStore {
    #[cfg(windows)]
    Windows,
    DevEnv,
    Unavailable(String),
}

impl SecretStore {
    pub fn from_env() -> Self {
        if std::env::var("COLUMBUS_DEV_SECRETS").as_deref() == Ok("1") {
            return SecretStore::DevEnv;
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
            SecretStore::DevEnv => {
                "DEVELOPMENT MODE: secrets read from COLUMBUS_SECRET_* environment variables".into()
            }
            SecretStore::Unavailable(why) => format!("unavailable: {why}"),
        }
    }

    pub fn is_dev(&self) -> bool {
        matches!(self, SecretStore::DevEnv)
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
            SecretStore::DevEnv => Ok(std::env::var(Self::env_name(connection_id)).ok()),
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
            SecretStore::DevEnv => Err(anyhow!(
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
            SecretStore::DevEnv => Ok(false),
            SecretStore::Unavailable(why) => Err(anyhow!("credential store {why}")),
        }
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
