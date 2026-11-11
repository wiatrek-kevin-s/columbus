//! The Git servers Columbus syncs with, through their web APIs: GitHub and
//! GitHub Enterprise Server (kind `github`), Gitea and Forgejo (kind
//! `gitea`).
//!
//! A [`Server`] is one connected server with its token: who am I, my
//! repositories, a new private repository, a repository's branches. A
//! [`Remote`] is one branch of one repository there, and is what a sync
//! talks to ([`crate::Host`]).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::host::{Change, Commit, Host, SendError, Tip, TreeEntry};
use crate::Error;

mod gitea;
mod github;
mod http;

use http::Http;

/// Which API a server speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// github.com, or a GitHub Enterprise Server.
    GitHub,
    /// Gitea, or Forgejo (the same API).
    Gitea,
}

impl Kind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::GitHub => "GitHub",
            Kind::Gitea => "Gitea or Forgejo",
        }
    }
}

/// Tests only (debug builds): let a server's address be plain http on this
/// computer (`http://127.0.0.1:<port>`, `http://localhost:<port>`), so that
/// a stand-in server can play GitHub or Gitea. Release builds have no such
/// switch: every address is https.
#[cfg(debug_assertions)]
pub fn allow_loopback_http_for_tests() {
    LOOPBACK.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(debug_assertions)]
static LOOPBACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn loopback_http_allowed() -> bool {
    #[cfg(debug_assertions)]
    {
        LOOPBACK.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

/// A server's address, checked: where its API is, and the host the token
/// may go to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub kind: Kind,
    /// The address as it is kept in `git-servers.yaml`: `https://host`, or
    /// `https://host/path` for a server under a path. No user, no query.
    pub url: String,
    /// The host's name, in lower case, without a port.
    pub host: String,
    api: Url,
}

impl Address {
    /// Check an address the person gave (`github.com`, `git.example.org`,
    /// `https://example.org/gitea`).
    pub fn parse(kind: Kind, given: &str) -> Result<Address, Error> {
        let bad = |why: &str| Error::Refused(format!("That address cannot be used: {why}."));
        let given = given.trim();
        if given.is_empty() || given.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(bad(
                "give the server's address, such as https://git.example.org",
            ));
        }
        let with_scheme = if given.contains("://") {
            given.to_string()
        } else {
            format!("https://{given}")
        };
        let mut url = Url::parse(&with_scheme).map_err(|_| bad("it is not a web address"))?;
        let host = url
            .host_str()
            .map(str::to_lowercase)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| bad("it has no host name"))?;
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
        match url.scheme() {
            "https" => {}
            "http" if loopback && loopback_http_allowed() => {}
            "http" => {
                return Err(bad(
                    "it must start with https://, so that your token travels encrypted",
                ))
            }
            _ => return Err(bad("it must start with https://")),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad(
                "it has a user name or password in it; the token is entered separately",
            ));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(bad(
                "it has a ? or # part; give the server's own address only",
            ));
        }
        // GitHub itself: one address, whatever was typed.
        let github_com = kind == Kind::GitHub
            && matches!(
                host.as_str(),
                "github.com" | "www.github.com" | "api.github.com"
            );
        if github_com {
            return Ok(Address {
                kind,
                url: "https://github.com".into(),
                host: "github.com".into(),
                api: Url::parse("https://api.github.com/").expect("a fixed address"),
            });
        }
        let path = url.path().trim_end_matches('/').to_string();
        url.set_path(&path);
        let shown = url.as_str().trim_end_matches('/').to_string();
        let api = format!(
            "{shown}/{}/",
            match kind {
                Kind::GitHub => "api/v3",
                Kind::Gitea => "api/v1",
            }
        );
        Ok(Address {
            kind,
            url: shown,
            host,
            api: Url::parse(&api).map_err(|_| bad("it is not a web address"))?,
        })
    }

    pub(crate) fn is_loopback(&self) -> bool {
        matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    /// The page on the server where the person makes a token, to open in
    /// their browser.
    pub fn token_page(&self) -> String {
        match self.kind {
            Kind::GitHub if self.host == "github.com" => {
                "https://github.com/settings/personal-access-tokens/new?name=Columbus&contents=write".into()
            }
            Kind::GitHub => format!("{}/settings/tokens", self.url),
            Kind::Gitea => format!("{}/user/settings/applications", self.url),
        }
    }
}

/// The account a token belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    /// The login name.
    pub login: String,
    /// The person's name as the server shows it, if they gave one.
    pub name: String,
}

/// A repository, as far as linking needs to know it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoInfo {
    pub owner: String,
    pub name: String,
    pub private: bool,
    /// The branch the repository opens on.
    pub default_branch: String,
    /// Whether the token may send to it.
    pub can_send: bool,
    /// Read only on the server (archived).
    pub archived: bool,
}

/// One connected server.
#[derive(Clone)]
pub struct Server {
    http: Http,
}

impl Server {
    /// Set up the connection (no request is made yet). `certificate` is the
    /// company's certificate file (.pem) for this server, if it needs one
    /// beyond the operating system's trust store.
    pub fn new(
        address: &Address,
        token: &str,
        certificate: Option<&Path>,
    ) -> Result<Server, Error> {
        Ok(Server {
            http: Http::new(address, token, certificate)?,
        })
    }

    pub fn kind(&self) -> Kind {
        self.http.kind
    }

    /// Whether the server is one Columbus can sync with. For Gitea and
    /// Forgejo this asks the server its version, without the token, and
    /// refuses one older than 1.20 with a message naming it (A-9). So a
    /// wrong address is found before the token is sent anywhere.
    pub async fn check(&self) -> Result<(), Error> {
        match self.http.kind {
            Kind::GitHub => Ok(()),
            Kind::Gitea => gitea::check_version(&self.http).await,
        }
    }

    /// Whose token this is.
    pub async fn whoami(&self) -> Result<Account, Error> {
        match self.http.kind {
            Kind::GitHub => github::whoami(&self.http).await,
            Kind::Gitea => gitea::whoami(&self.http).await,
        }
    }

    /// The repositories the account can pick from.
    pub async fn repos(&self) -> Result<Vec<RepoInfo>, Error> {
        let mut repos = match self.http.kind {
            Kind::GitHub => github::repos(&self.http).await?,
            Kind::Gitea => gitea::repos(&self.http).await?,
        };
        repos.sort_by_key(|r| (r.owner.to_lowercase(), r.name.to_lowercase()));
        Ok(repos)
    }

    /// One repository, by owner and name.
    pub async fn repo(&self, owner: &str, name: &str) -> Result<RepoInfo, Error> {
        check_name(owner)?;
        check_name(name)?;
        match self.http.kind {
            Kind::GitHub => github::repo(&self.http, owner, name).await,
            Kind::Gitea => gitea::repo(&self.http, owner, name).await,
        }
    }

    /// Make a new private repository in the account, empty.
    pub async fn create_repo(&self, name: &str, description: &str) -> Result<RepoInfo, Error> {
        check_name(name)?;
        let description: String = description
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        match self.http.kind {
            Kind::GitHub => github::create(&self.http, name, &description).await,
            Kind::Gitea => gitea::create(&self.http, name, &description).await,
        }
    }

    /// A repository's branches (none: it is empty).
    pub async fn branches(&self, owner: &str, name: &str) -> Result<Vec<String>, Error> {
        check_name(owner)?;
        check_name(name)?;
        match self.http.kind {
            Kind::GitHub => github::branches(&self.http, owner, name).await,
            Kind::Gitea => gitea::branches(&self.http, owner, name).await,
        }
    }

    /// One branch of one repository, to sync with.
    pub fn remote(&self, owner: &str, repo: &str, branch: &str) -> Result<Remote, Error> {
        check_name(owner)?;
        check_name(repo)?;
        check_branch(branch)?;
        Ok(Remote {
            http: self.http.clone(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            branch: branch.to_string(),
            trees: Arc::default(),
        })
    }
}

/// An owner's or a repository's name: letters, digits, `-`, `_` and `.`.
pub fn check_name(name: &str) -> Result<(), Error> {
    let ok = !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && !name.to_lowercase().ends_with(".git")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::Refused(format!(
            "{name:?} is not a name a repository or its owner can have (letters, digits, - _ and . only)"
        )))
    }
}

/// A branch's name, as Git allows them (the common part of its rules).
pub fn check_branch(branch: &str) -> Result<(), Error> {
    let ok = !branch.is_empty()
        && branch.len() <= 200
        && !branch.starts_with(['/', '-', '.'])
        && !branch.ends_with(['/', '.'])
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.contains("@{")
        && !branch.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        });
    if ok {
        Ok(())
    } else {
        Err(Error::Refused(format!("{branch:?} is not a branch name")))
    }
}

/// One branch of one repository on a server.
#[derive(Clone)]
pub struct Remote {
    http: Http,
    owner: String,
    repo: String,
    branch: String,
    /// GitHub: the id of each commit's list of files, as read, so that a
    /// commit can be built on it without asking again.
    trees: Arc<Mutex<HashMap<String, String>>>,
}

impl Remote {
    /// `owner/repo` on `host`, for messages.
    fn place(&self) -> String {
        format!("{}/{} on {}", self.owner, self.repo, self.http.host)
    }

    fn branch_parts(&self) -> impl Iterator<Item = &str> {
        self.branch.split('/')
    }
}

impl Host for Remote {
    async fn tip(&self) -> Result<Tip, Error> {
        match self.http.kind {
            Kind::GitHub => github::tip(self).await,
            Kind::Gitea => gitea::tip(self).await,
        }
    }

    async fn tree(&self, commit: &str) -> Result<Vec<TreeEntry>, Error> {
        match self.http.kind {
            Kind::GitHub => github::tree(self, commit).await,
            Kind::Gitea => gitea::tree(self, commit).await,
        }
    }

    async fn blob(&self, id: &str) -> Result<Vec<u8>, Error> {
        match self.http.kind {
            Kind::GitHub => github::blob(self, id).await,
            Kind::Gitea => gitea::blob(self, id).await,
        }
    }

    async fn send(
        &self,
        tip: &Tip,
        changes: &[Change],
        message: &str,
    ) -> Result<String, SendError> {
        match self.http.kind {
            Kind::GitHub => github::send(self, tip, changes, message).await,
            Kind::Gitea => gitea::send(self, tip, changes, message).await,
        }
    }

    async fn history(&self, limit: usize) -> Result<Vec<Commit>, Error> {
        match self.http.kind {
            Kind::GitHub => github::history(self, limit).await,
            Kind::Gitea => gitea::history(self, limit).await,
        }
    }
}

/// Decode what a server gives as base64 (GitHub breaks it into lines).
fn unbase64(text: &str, what: &str, host: &str) -> Result<Vec<u8>, Error> {
    use base64::Engine;
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|_| Error::Server(format!("{host} sent {what} in a form Columbus cannot read")))
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// An id as a server gives it: hex digits, as many as its format has.
fn id_of(v: &serde_json::Value, what: &str, host: &str) -> Result<String, Error> {
    v.as_str()
        .filter(|s| crate::hash::ObjectFormat::of_id(s).is_some())
        .map(str::to_string)
        .ok_or_else(|| Error::Server(format!("{host} did not give {what} as Columbus expects it")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let a = Address::parse(Kind::GitHub, "github.com").unwrap();
        assert_eq!(
            (a.url.as_str(), a.host.as_str(), a.api.as_str()),
            (
                "https://github.com",
                "github.com",
                "https://api.github.com/"
            )
        );
        for same in [
            "https://github.com/",
            "https://www.github.com",
            "HTTPS://GitHub.com",
            "api.github.com",
        ] {
            assert_eq!(Address::parse(Kind::GitHub, same).unwrap(), a, "{same}");
        }
        let e = Address::parse(Kind::GitHub, "https://github.corp.example/").unwrap();
        assert_eq!(e.url, "https://github.corp.example");
        assert_eq!(e.api.as_str(), "https://github.corp.example/api/v3/");
        let g = Address::parse(Kind::Gitea, "git.example.org").unwrap();
        assert_eq!(
            (g.url.as_str(), g.host.as_str()),
            ("https://git.example.org", "git.example.org")
        );
        assert_eq!(g.api.as_str(), "https://git.example.org/api/v1/");
        let sub = Address::parse(Kind::Gitea, "https://Example.org:8443/gitea/").unwrap();
        assert_eq!(sub.url, "https://example.org:8443/gitea");
        assert_eq!(sub.host, "example.org");
        assert_eq!(sub.api.as_str(), "https://example.org:8443/gitea/api/v1/");
        // The token pages.
        assert_eq!(
            a.token_page(),
            "https://github.com/settings/personal-access-tokens/new?name=Columbus&contents=write"
        );
        assert_eq!(
            e.token_page(),
            "https://github.corp.example/settings/tokens"
        );
        assert_eq!(
            sub.token_page(),
            "https://example.org:8443/gitea/user/settings/applications"
        );
    }

    #[test]
    fn addresses_that_are_refused() {
        for bad in [
            "",
            "   ",
            "http://git.example.org",
            "ftp://git.example.org",
            "https://user:pass@git.example.org",
            "https://user@git.example.org",
            "https://git.example.org/?x=1",
            "https://git.example.org/#frag",
            "https://",
            "git example org",
            "file:///etc/passwd",
        ] {
            let e = Address::parse(Kind::Gitea, bad).unwrap_err();
            assert!(matches!(e, Error::Refused(_)), "{bad:?}: {e:?}");
        }
        // Plain http to this computer is for the tests' stand-in servers
        // only, and only after a test build says so. (Whether it has been
        // said is shared by the tests of this file, so both are checked
        // here, in order.)
        if !loopback_http_allowed() {
            assert!(Address::parse(Kind::Gitea, "http://127.0.0.1:3000").is_err());
        }
        allow_loopback_http_for_tests();
        let local = Address::parse(Kind::Gitea, "http://127.0.0.1:3000").unwrap();
        assert_eq!(local.api.as_str(), "http://127.0.0.1:3000/api/v1/");
        assert!(local.is_loopback());
        // Even then, never plain http to anywhere else.
        assert!(Address::parse(Kind::Gitea, "http://git.example.org").is_err());
        assert!(Address::parse(Kind::GitHub, "http://10.0.0.5:3000").is_err());
    }

    #[test]
    fn names() {
        for ok in ["kevin", "roads-qc", "Roads_2027", "a.b"] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a b",
            "x.git",
            "é",
            "a?b",
            &"x".repeat(101),
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
        for ok in ["main", "feature/roads", "v1.2", "ümlaut"] {
            assert!(check_branch(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "/main", "main/", "a..b", "a b", "-x", "x.lock", "a//b", "a:b", "a@{b", "a\\b",
            ".hidden",
        ] {
            assert!(check_branch(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_token_is_one_plain_run_and_is_scrubbed_from_what_servers_say() {
        let a = Address::parse(Kind::GitHub, "github.com").unwrap();
        for bad in [
            "",
            "  ",
            "two words",
            "tab\there",
            "line\nbreak",
            "ünï",
            &"x".repeat(513),
        ] {
            assert!(Server::new(&a, bad, None).is_err(), "{bad:?}");
        }
        let s = Server::new(&a, " tok-0123456789 ", None).unwrap();
        assert_eq!(
            s.http
                .scrub("Bad credentials for tok-0123456789, twice tok-0123456789"),
            "Bad credentials for [token], twice [token]"
        );
    }

    #[test]
    fn a_certificate_file_must_hold_certificates_and_no_private_key() {
        let d = tempfile::tempdir().unwrap();
        let a = Address::parse(Kind::Gitea, "git.example.org").unwrap();
        let p = d.path().join("ca.pem");
        std::fs::write(&p, "not a certificate").unwrap();
        let e = Server::new(&a, "token-0123456789", Some(&p)).err().unwrap();
        assert!(e.to_string().contains("holds no certificate"), "{e}");
        std::fs::write(
            &p,
            format!(
                "-----BEGIN {} KEY-----\nMIIB\n-----END {} KEY-----\n",
                "PRIVATE", "PRIVATE"
            ),
        )
        .unwrap();
        let e = Server::new(&a, "token-0123456789", Some(&p)).err().unwrap();
        assert!(e.to_string().contains("private key"), "{e}");
        let e = Server::new(&a, "token-0123456789", Some(&d.path().join("missing.pem")))
            .err()
            .unwrap();
        assert!(matches!(e, Error::Io(_)), "{e:?}");
    }
}
