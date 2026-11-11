//! Settings › Git servers: the servers a project may sync with (build
//! brief 3, D-20, REQ-42). GitHub, a GitHub Enterprise Server, a Gitea or
//! a Forgejo is connected by pasting a token into the app.
//!
//! * The list of servers, without tokens, is `git-servers.yaml` in the
//!   app's settings (F-18): `id`, `kind`, `url`, `user`, and optionally the
//!   company's `certificate` file for the server.
//! * A token is kept only in the operating system's credential store, the
//!   way the app keeps its AI key (service `columbus-app`, account
//!   `git-server:<id>`). It is never written to a file, a log, an error or
//!   anything a page can read back: a page can give one, and remove a
//!   server; it cannot read one. Where the system has no credential store
//!   Columbus can use (Linux, for now), no server can be connected.
//! * Sync is the organization's to allow (`allow_sync`), and it may name
//!   the hosts (`sync.hosts`). Both are checked here, in the app's own
//!   code, at every use: a server connected before the policy changed
//!   stops working when it no longer allows it.
//!
//! The assistant has no part in this: nothing here is a tool, and only the
//! main window's own pages may call these commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use columbus_shared::policy::DatasetRules;
use columbus_sync::remote::{Address, Kind, Server};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::settings::{policy_now, write_atomic};

/// The service the app keeps its own secrets under (see `chat.rs`).
const KEY_SERVICE: &str = "columbus-app";

const FILE: &str = "git-servers.yaml";
const HEAD: &str = "# Git servers connected in the Columbus app (Settings > Git servers).\n# Tokens are kept in the system's credential store, never in this file.\n";

/// The most servers kept.
const MAX_SERVERS: usize = 20;

/// One connected server, as `git-servers.yaml` keeps it (F-18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub id: String,
    pub kind: Kind,
    /// `https://host`, or `https://host/path`.
    pub url: String,
    /// The account's login on the server, as it answered when connected.
    pub user: String,
    /// The company's certificate file (.pem) for this server, if it needs
    /// one beyond the system's trust store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<String>,
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn file(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|d| d.join(FILE))
        .map_err(|_| "this system gave the app no settings folder".to_string())
}

pub(crate) fn parse(text: &str) -> Result<Vec<Entry>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let entries: Vec<Entry> =
        serde_yaml::from_str(text).map_err(|e| format!("{FILE} cannot be read: {e}"))?;
    for e in &entries {
        if !id_ok(&e.id) {
            return Err(format!("{FILE}: {:?} is not a server id", e.id));
        }
        // The address is checked as it was when it was saved.
        Address::parse(e.kind, &e.url).map_err(|err| format!("{FILE}: {}: {err}", e.id))?;
    }
    Ok(entries)
}

fn load(app: &AppHandle) -> Result<Vec<Entry>, String> {
    let path = file(app)?;
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(m) if !m.is_file() || m.len() > 256 * 1024 => {
            Err(format!("{} is not a plain, small file", path.display()))
        }
        Ok(_) => {
            parse(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?)
        }
    }
}

pub(crate) fn to_text(entries: &[Entry]) -> Result<String, String> {
    let body = if entries.is_empty() {
        "[]\n".to_string()
    } else {
        serde_yaml::to_string(entries).map_err(|e| e.to_string())?
    };
    Ok(format!("{HEAD}{body}"))
}

fn save(app: &AppHandle, entries: &[Entry]) -> Result<(), String> {
    write_atomic(&file(app)?, &to_text(entries)?)
}

/// Where tokens are kept: the operating system's credential store.
mod tokens {
    use super::*;

    /// What was read since the app started, by server id: each token is
    /// read once per run (on a Mac every read can ask for the person's
    /// password).
    static READ: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

    /// Tests only (debug builds, `COLUMBUS_APP_TEST_TOKENS=memory`): tokens
    /// are kept in this program's memory instead, so the end-to-end script
    /// can run where there is no credential store, and never touches a
    /// real one. Nothing is written anywhere. Release builds do not read
    /// the variable.
    #[cfg(debug_assertions)]
    fn in_memory() -> bool {
        std::env::var("COLUMBUS_APP_TEST_TOKENS").as_deref() == Ok("memory")
    }

    #[cfg(not(debug_assertions))]
    fn in_memory() -> bool {
        false
    }

    fn account(id: &str) -> String {
        format!("git-server:{id}")
    }

    fn entry(id: &str) -> Result<keyring_core::Entry, String> {
        crate::chat::ensure_store()?;
        keyring_core::Entry::new(KEY_SERVICE, &account(id)).map_err(|e| e.to_string())
    }

    fn cache<R>(f: impl FnOnce(&mut HashMap<String, Option<String>>) -> R) -> R {
        let mut read = READ.lock().unwrap_or_else(|e| e.into_inner());
        f(read.get_or_insert_with(HashMap::new))
    }

    /// Whether a token can be kept on this system at all, and if not, why.
    pub(crate) fn available() -> Result<(), String> {
        if in_memory() {
            return Ok(());
        }
        crate::chat::ensure_store().map_err(|e| {
            format!("A token cannot be kept safely on this computer ({e}), so no Git server can be connected here.")
        })
    }

    pub(crate) fn get(id: &str) -> Result<Option<String>, String> {
        if let Some(known) = cache(|c| c.get(id).cloned()) {
            return Ok(known);
        }
        if in_memory() {
            return Ok(None);
        }
        let found = match entry(id)?.get_password() {
            Ok(t) => Some(t),
            Err(keyring_core::Error::NoEntry) => None,
            Err(e) => return Err(format!("the credential store could not be read: {e}")),
        };
        cache(|c| c.insert(id.to_string(), found.clone()));
        Ok(found)
    }

    pub(crate) fn set(id: &str, token: &str) -> Result<(), String> {
        if !in_memory() {
            entry(id)?
                .set_password(token)
                .map_err(|e| format!("the token could not be kept in the credential store: {e}"))?;
        }
        cache(|c| c.insert(id.to_string(), Some(token.to_string())));
        Ok(())
    }

    pub(crate) fn delete(id: &str) -> Result<(), String> {
        if !in_memory() {
            match entry(id).and_then(|e| match e.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.to_string()),
            }) {
                Ok(()) => {}
                // No store at all: then there is no token in it either.
                Err(_) if crate::chat::ensure_store().is_err() => {}
                Err(e) => {
                    return Err(format!(
                        "the token could not be removed from the credential store: {e}"
                    ))
                }
            }
        }
        cache(|c| c.insert(id.to_string(), None));
        Ok(())
    }
}

/// Tests only (debug builds): with `COLUMBUS_SYNC_TEST_LOOPBACK` set, a
/// server's address may be plain http on this computer, so that the
/// end-to-end script's stand-in servers can be connected. Release builds
/// accept https only.
fn test_setup() {
    #[cfg(debug_assertions)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            if std::env::var_os("COLUMBUS_SYNC_TEST_LOOPBACK").is_some() {
                columbus_sync::remote::allow_loopback_http_for_tests();
            }
        });
    }
}

/// Whether the organization allows sync at all, and if not, the words for
/// the person.
pub(crate) fn allowed() -> Result<(), String> {
    test_setup();
    if policy_now().allow_sync {
        Ok(())
    } else {
        Err("Sync is turned off by your organization's policy (allow_sync).".into())
    }
}

/// The hosts the organization names (`sync.hosts`); empty means any.
fn listed_hosts() -> Vec<String> {
    policy_now().sync.hosts
}

/// Whether a host may be synced with (`sync.hosts`).
pub(crate) fn host_allowed(host: &str) -> Result<(), String> {
    let hosts = listed_hosts();
    let ok = hosts.is_empty()
        || DatasetRules {
            allow: hosts.clone(),
            deny: vec![],
        }
        .permits(host)
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{host} is not on your organization's list of Git servers (sync.hosts: {}).",
            hosts.join(", ")
        ))
    }
}

/// A connected server, ready to use: its entry, and the connection with
/// its token. Checks the policy again, every time.
pub(crate) fn open(app: &AppHandle, id: &str) -> Result<(Entry, Server), String> {
    allowed()?;
    let entry = load(app)?
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| {
            "This project's Git server is no longer connected. Connect it again in Settings > Git servers, or disconnect the project.".to_string()
        })?;
    let address = Address::parse(entry.kind, &entry.url).map_err(|e| e.to_string())?;
    host_allowed(&address.host)?;
    let token = tokens::get(id)?.ok_or_else(|| {
        format!(
            "The token for {} is not on this computer any more. Connect the server again in Settings > Git servers.",
            address.host
        )
    })?;
    let server = Server::new(
        &address,
        &token,
        entry.certificate.as_deref().map(Path::new),
    )
    .map_err(|e| e.to_string())?;
    Ok((entry, server))
}

/// A server's token, if this run has read or kept it: so that a sync can
/// make sure the token itself never travels inside a file.
pub(crate) fn token_of(id: &str) -> Option<String> {
    tokens::get(id).ok().flatten()
}

/// The connected servers, for a page that links a project.
pub(crate) fn entries(app: &AppHandle) -> Vec<Entry> {
    load(app).unwrap_or_default()
}

#[derive(Serialize)]
pub struct ServerView {
    id: String,
    kind: Kind,
    url: String,
    host: String,
    user: String,
    certificate: Option<String>,
    /// Why it cannot be used now (the policy no longer lists its host).
    problem: Option<String>,
}

fn view(e: &Entry) -> ServerView {
    let host = Address::parse(e.kind, &e.url)
        .map(|a| a.host)
        .unwrap_or_default();
    ServerView {
        problem: host_allowed(&host).err(),
        id: e.id.clone(),
        kind: e.kind,
        url: e.url.clone(),
        host,
        user: e.user.clone(),
        certificate: e.certificate.clone(),
    }
}

#[derive(Serialize)]
pub struct ServersState {
    /// Whether the organization allows sync, and if not, the words to show.
    allowed: bool,
    why_not: Option<String>,
    /// The hosts the organization names; empty means any.
    hosts: Vec<String>,
    /// Why no token can be kept on this system, if so.
    no_store: Option<String>,
    /// Where the list is kept.
    file: String,
    servers: Vec<ServerView>,
    problem: Option<String>,
}

#[tauri::command]
pub async fn git_servers_state(app: AppHandle) -> Result<ServersState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let why_not = allowed().err();
        let (servers, problem) = match load(&app) {
            Ok(list) => (list.iter().map(view).collect(), None),
            Err(e) => (Vec::new(), Some(e)),
        };
        Ok(ServersState {
            allowed: why_not.is_none(),
            // With sync off, nothing is offered and nothing is listed.
            servers: if why_not.is_none() {
                servers
            } else {
                Vec::new()
            },
            why_not,
            hosts: listed_hosts(),
            no_store: tokens::available().err(),
            file: file(&app)
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            problem,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Deserialize)]
pub struct ConnectForm {
    /// `github`, `github-enterprise`, `gitea` or `forgejo`.
    kind: String,
    /// The server's address (not for `github`).
    #[serde(default)]
    url: Option<String>,
    token: String,
    /// The company's certificate file, if the server needs one.
    #[serde(default)]
    certificate: Option<String>,
}

fn address_of(kind: &str, url: Option<&str>) -> Result<Address, String> {
    let url = url.map(str::trim).unwrap_or_default();
    match kind {
        "github" => Address::parse(Kind::GitHub, "https://github.com"),
        "github-enterprise" => {
            let a = Address::parse(Kind::GitHub, url);
            if a.as_ref().is_ok_and(|a| a.host == "github.com") {
                return Err(
                    "That is GitHub itself: choose GitHub, not GitHub Enterprise Server.".into(),
                );
            }
            a
        }
        "gitea" | "forgejo" => Address::parse(Kind::Gitea, url),
        _ => return Err("Choose GitHub, GitHub Enterprise Server, Gitea or Forgejo.".into()),
    }
    .map_err(|e| e.to_string())
}

/// An id for a new server, from its host: `github`, `git-example-org`.
fn new_id(address: &Address, taken: &[Entry]) -> String {
    let base: String = if address.host == "github.com" {
        "github".into()
    } else {
        let mut id = String::new();
        for c in address.host.chars() {
            if c.is_ascii_alphanumeric() {
                id.push(c.to_ascii_lowercase());
            } else if !id.ends_with('-') && !id.is_empty() {
                id.push('-');
            }
        }
        let id = id.trim_end_matches('-');
        if id.is_empty() {
            "server".to_string()
        } else {
            id.chars().take(32).collect()
        }
    };
    let mut id = base.clone();
    let mut n = 2;
    while taken.iter().any(|e| e.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

/// Connect a server: check the policy and the address, ask the server
/// whose token this is, and only then keep the token (in the credential
/// store) and the server (in the list).
#[tauri::command]
pub async fn git_server_connect(app: AppHandle, form: ConnectForm) -> Result<ServerView, String> {
    allowed()?;
    let address = address_of(&form.kind, form.url.as_deref())?;
    host_allowed(&address.host)?;
    // Before anything is sent: a token that could not be kept is not asked
    // about either.
    tokens::available()?;
    let certificate = form
        .certificate
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(PathBuf::from);
    if let Some(c) = &certificate {
        if !c.is_absolute() {
            return Err("Give the certificate file's full path.".into());
        }
    }
    let server =
        Server::new(&address, &form.token, certificate.as_deref()).map_err(|e| e.to_string())?;
    server.check().await.map_err(|e| e.to_string())?;
    let me = server.whoami().await.map_err(|e| e.to_string())?;
    let token = form.token.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let _one = crate::settings::ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut list = load(&app)?;
        // Connecting a server again replaces its token and account.
        let at = list
            .iter()
            .position(|e| e.kind == address.kind && e.url == address.url);
        if at.is_none() && list.len() >= MAX_SERVERS {
            return Err(format!(
                "At most {MAX_SERVERS} servers can be connected. Remove one first."
            ));
        }
        let entry = Entry {
            id: match at {
                Some(i) => list[i].id.clone(),
                None => new_id(&address, &list),
            },
            kind: address.kind,
            url: address.url.clone(),
            user: me.login.clone(),
            certificate: certificate.map(|c| c.display().to_string()),
        };
        tokens::set(&entry.id, &token)?;
        match at {
            Some(i) => list[i] = entry.clone(),
            None => list.push(entry.clone()),
        }
        if let Err(e) = save(&app, &list) {
            // Not listed, so not kept either.
            if at.is_none() {
                let _ = tokens::delete(&entry.id);
            }
            return Err(e);
        }
        Ok(view(&entry))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove a server: its token is deleted from the credential store, and
/// it leaves the list. Projects linked to it keep their files and their
/// link; they sync again once the server is connected again.
#[tauri::command]
pub async fn git_server_remove(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !id_ok(&id) {
            return Err("not a server".into());
        }
        let _one = crate::settings::ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut list = load(&app)?;
        // The token first: a server is never left unlisted with its token
        // still kept.
        tokens::delete(&id)?;
        list.retain(|e| e.id != id);
        save(&app, &list)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Open the server's own page for making a token in the person's browser.
/// The address is built here from the kind and the server's address, never
/// taken from a page. Gives the address opened, to show beside the link.
#[tauri::command]
pub async fn git_server_token_page(kind: String, url: Option<String>) -> Result<String, String> {
    test_setup();
    let page = address_of(&kind, url.as_deref())?.token_page();
    crate::settings::open_browser(&page)
        .map_err(|e| format!("Your browser could not be opened ({e}). The page is {page}"))?;
    Ok(page)
}

/// Choose the company's certificate file for a server.
#[tauri::command]
pub async fn git_server_pick_certificate(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Choose your organization's certificate file")
        .add_filter("Certificate", &["pem", "crt", "cer"])
        .blocking_pick_file();
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: Kind, url: &str) -> Entry {
        Entry {
            id: id.into(),
            kind,
            url: url.into(),
            user: "ana".into(),
            certificate: None,
        }
    }

    #[test]
    fn the_list_is_written_and_read_back_and_holds_no_token() {
        let mut list = vec![
            entry("github", Kind::GitHub, "https://github.com"),
            entry(
                "git-example-org",
                Kind::Gitea,
                "https://git.example.org/gitea",
            ),
        ];
        list[1].certificate = Some("/etc/ssl/company-ca.pem".into());
        let text = to_text(&list).unwrap();
        assert!(
            text.starts_with("# Git servers connected in the Columbus app"),
            "{text}"
        );
        assert!(
            text.contains("kind: github") && text.contains("kind: gitea"),
            "{text}"
        );
        assert!(
            text.contains("user: ana") && text.contains("certificate: /etc/ssl/company-ca.pem")
        );
        // F-18's four fields, and the optional certificate: nothing else.
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let key = line
                .trim_start_matches("- ")
                .trim()
                .split(':')
                .next()
                .unwrap();
            assert!(
                ["id", "kind", "url", "user", "certificate"].contains(&key),
                "{line}"
            );
        }
        assert_eq!(parse(&text).unwrap(), list);
        assert_eq!(parse(&to_text(&[]).unwrap()).unwrap(), vec![]);
        assert_eq!(parse("").unwrap(), vec![]);
        // Unknown keys (a token someone pasted in, say) are refused, and so
        // is an address that is not https.
        assert!(
            parse("- {id: x, kind: gitea, url: 'https://a.example', user: u, token: abc}\n")
                .is_err()
        );
        assert!(parse("- {id: x, kind: gitea, url: 'http://a.example', user: u}\n").is_err());
        assert!(
            parse("- {id: 'Bad Id', kind: gitea, url: 'https://a.example', user: u}\n").is_err()
        );
        assert!(parse("- {id: x, kind: gitlab, url: 'https://a.example', user: u}\n").is_err());
    }

    #[test]
    fn ids_come_from_the_host() {
        let gh = Address::parse(Kind::GitHub, "github.com").unwrap();
        let gitea = Address::parse(Kind::Gitea, "https://Git.Example.org:8443/gitea").unwrap();
        assert_eq!(new_id(&gh, &[]), "github");
        assert_eq!(new_id(&gitea, &[]), "git-example-org");
        let taken = [entry(
            "git-example-org",
            Kind::Gitea,
            "https://git.example.org",
        )];
        assert_eq!(new_id(&gitea, &taken), "git-example-org-2");
        assert!(id_ok(&new_id(&gitea, &taken)));
    }

    #[test]
    fn the_kinds_a_person_can_choose() {
        let a = address_of("github", None).unwrap();
        assert_eq!(
            (a.kind, a.url.as_str()),
            (Kind::GitHub, "https://github.com")
        );
        // Whatever address is typed with "GitHub", it is GitHub.
        assert_eq!(
            address_of("github", Some("https://evil.example"))
                .unwrap()
                .host,
            "github.com"
        );
        let e = address_of("github-enterprise", Some("github.corp.example")).unwrap();
        assert_eq!(
            (e.kind, e.url.as_str()),
            (Kind::GitHub, "https://github.corp.example")
        );
        assert!(address_of("github-enterprise", Some("github.com")).is_err());
        assert!(address_of("github-enterprise", None).is_err());
        for k in ["gitea", "forgejo"] {
            let g = address_of(k, Some("https://git.example.org")).unwrap();
            assert_eq!(g.kind, Kind::Gitea);
        }
        assert!(address_of("gitlab", Some("https://gitlab.com")).is_err());
        assert!(address_of("gitea", Some("http://git.example.org")).is_err());
        assert_eq!(
            a.token_page(),
            "https://github.com/settings/personal-access-tokens/new?name=Columbus&contents=write"
        );
    }
}
