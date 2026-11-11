//! Gitea and Forgejo, through the API they share (`/api/v1`).
//!
//! Reading: `GET /version` (no token needed), `/user`, `/user/repos`,
//! `/repos/{o}/{r}`, its `/branches` and `/branches/{branch}`,
//! `/git/trees/{commit}?recursive=true` (a page at a time),
//! `/git/blobs/{id}` and `/commits`. Lists come 50 to a page at most.
//!
//! Sending is one call: `POST /repos/{o}/{r}/contents` carries every new,
//! changed and deleted file of a sync, and the server makes one commit of
//! them. Each changed or deleted file is sent with the id (`sha`) of the
//! version the sync read; the server refuses the whole call if any of
//! them is no longer that version, and refuses a new file whose name is
//! taken. So nothing is overwritten unseen, and nothing is forced (the
//! call's `force_push` and `new_branch` are never used). That call exists
//! since Gitea 1.20; an older server is refused, by its version (A-9).

use serde_json::{json, Value};

use super::http::Http;
use super::{base64, id_of, unbase64, Account, Remote, RepoInfo};
use crate::host::{Change, Commit, EntryKind, SendError, Tip, TreeEntry};
use crate::Error;

/// One call carries at most this many bytes of files; a larger sync is
/// sent as several commits, one after the other (rare: sync is for scripts
/// and notes).
const CALL_BYTES: usize = 20 * 1024 * 1024;

/// The version of Gitea whose API a server speaks, from what `/version`
/// says: `1.22.3`, `28.3.0` (Gitea), `9.0.3+gitea-1.22.0` (Forgejo).
/// `(major, minor)`, or `None` when it cannot be read.
pub(super) fn api_version(text: &str) -> Option<(u32, u32)> {
    let compatible = match text.split_once("+gitea-") {
        Some((_, gitea)) => gitea,
        None => text,
    };
    let mut parts = compatible
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

pub(super) async fn check_version(http: &Http) -> Result<(), Error> {
    let reply = http
        .call(reqwest::Method::GET, &["version"], &[], None, false)
        .await?;
    let not_gitea = || {
        Error::NotFound(format!(
            "{} did not answer as a Gitea or Forgejo server does. Check the address (for a server under a path, include the path).",
            http.host
        ))
    };
    if reply.status != 200 {
        return Err(not_gitea());
    }
    let v = reply.json(http).map_err(|_| not_gitea())?;
    let text = v["version"].as_str().ok_or_else(not_gitea)?;
    let shown: String = text.chars().filter(|c| !c.is_control()).take(40).collect();
    match api_version(text) {
        Some((major, minor)) if (major, minor) >= (1, 20) => Ok(()),
        Some(_) => Err(Error::TooOld(format!(
            "{} runs version {shown}. Columbus needs Gitea 1.20 or later (or a Forgejo of that age) to send several files as one change. Ask whoever runs the server to update it.",
            http.host
        ))),
        None => Err(Error::TooOld(format!(
            "{} says its version is {shown:?}, which Columbus cannot read. It needs Gitea 1.20 or later.",
            http.host
        ))),
    }
}

pub(super) async fn whoami(http: &Http) -> Result<Account, Error> {
    let reply = http.get(&["user"], &[]).await?;
    if reply.status != 200 {
        return Err(http.failed(&reply));
    }
    let v = reply.json(http)?;
    let login = v["login"].as_str().unwrap_or_default().to_string();
    if login.is_empty() {
        return Err(Error::Server(format!(
            "{} did not say whose token this is",
            http.host
        )));
    }
    Ok(Account {
        name: v["full_name"].as_str().unwrap_or_default().to_string(),
        login,
    })
}

fn repo_info(v: &Value) -> Option<RepoInfo> {
    Some(RepoInfo {
        owner: v["owner"]["login"].as_str()?.to_string(),
        name: v["name"].as_str()?.to_string(),
        private: v["private"].as_bool().unwrap_or(false),
        default_branch: v["default_branch"].as_str().unwrap_or("main").to_string(),
        can_send: v["permissions"]["push"].as_bool().unwrap_or(false),
        archived: v["archived"].as_bool().unwrap_or(false),
    })
}

pub(super) async fn repos(http: &Http) -> Result<Vec<RepoInfo>, Error> {
    let items = http.list(&["user", "repos"], &[], ("limit", 50)).await?;
    Ok(items.iter().filter_map(repo_info).collect())
}

async fn repo_json(http: &Http, owner: &str, name: &str) -> Result<Value, Error> {
    let reply = http.get(&["repos", owner, name], &[]).await?;
    match reply.status {
        200 => reply.json(http),
        404 => Err(Error::NotFound(format!(
            "There is no repository {owner}/{name} on {}, or the token may not see it.",
            http.host
        ))),
        _ => Err(http.failed(&reply)),
    }
}

pub(super) async fn repo(http: &Http, owner: &str, name: &str) -> Result<RepoInfo, Error> {
    repo_info(&repo_json(http, owner, name).await?)
        .ok_or_else(|| Error::Server(format!("{} did not describe {owner}/{name}", http.host)))
}

pub(super) async fn create(http: &Http, name: &str, description: &str) -> Result<RepoInfo, Error> {
    let body =
        json!({"name": name, "description": description, "private": true, "auto_init": false});
    let reply = http.post(&["user", "repos"], &body).await?;
    match reply.status {
        201 => repo_info(&reply.json(http)?).ok_or_else(|| {
            Error::Server(format!("{} made the repository but did not describe it", http.host))
        }),
        409 => Err(Error::Refused(format!(
            "You already have a repository named {name} on {}. Pick it from the list, or choose another name.",
            http.host
        ))),
        403 => Err(Error::Permission(format!(
            "The token may not create repositories on {}. It needs the scopes write:user and write:repository for that. Or create the repository on the server and pick it here.",
            http.host
        ))),
        422 => Err(Error::Refused(format!(
            "{} did not make the repository: {}",
            http.host,
            reply.message(http)
        ))),
        _ => Err(http.failed(&reply)),
    }
}

pub(super) async fn branches(http: &Http, owner: &str, name: &str) -> Result<Vec<String>, Error> {
    let items = http
        .list(&["repos", owner, name, "branches"], &[], ("limit", 50))
        .await?;
    Ok(items
        .iter()
        .filter_map(|b| b["name"].as_str().map(str::to_string))
        .collect())
}

fn repo_path<'a>(r: &'a Remote, rest: &[&'a str]) -> Vec<&'a str> {
    let mut p = vec!["repos", r.owner.as_str(), r.repo.as_str()];
    p.extend_from_slice(rest);
    p
}

pub(super) async fn tip(r: &Remote) -> Result<Tip, Error> {
    let mut path = repo_path(r, &["branches"]);
    path.extend(r.branch_parts());
    let reply = r.http.get(&path, &[]).await?;
    match reply.status {
        200 => {
            let v = reply.json(&r.http)?;
            Ok(Tip::At(id_of(
                &v["commit"]["id"],
                "the newest change",
                &r.http.host,
            )?))
        }
        404 => {
            // No such branch: because the repository is empty, or because
            // the branch (or the repository) is gone.
            let info = repo_json(&r.http, &r.owner, &r.repo).await?;
            if info["empty"].as_bool() == Some(true) {
                Ok(Tip::Empty)
            } else {
                Err(Error::NotFound(format!(
                    "The branch {} is not in {} (it may have been renamed or deleted). Disconnect this project and link it again.",
                    r.branch,
                    r.place()
                )))
            }
        }
        _ => Err(r.http.failed(&reply)),
    }
}

pub(super) async fn tree(r: &Remote, commit: &str) -> Result<Vec<TreeEntry>, Error> {
    let mut out = Vec::new();
    // A page at a time, until the server says nothing is left.
    for page in 1..=2000 {
        let q = [
            ("recursive", "true".to_string()),
            ("per_page", "1000".to_string()),
            ("page", page.to_string()),
        ];
        let reply = r
            .http
            .get(&repo_path(r, &["git", "trees", commit]), &q)
            .await?;
        if reply.status != 200 {
            return Err(r.http.failed(&reply));
        }
        let v = reply.json(&r.http)?;
        let items = v["tree"].as_array().cloned().unwrap_or_default();
        for e in &items {
            let kind = match (e["type"].as_str(), e["mode"].as_str()) {
                (Some("tree"), _) => continue,
                (Some("commit"), _) | (_, Some("160000")) => EntryKind::Submodule,
                (_, Some("120000")) => EntryKind::Link,
                (Some("blob"), mode) => EntryKind::File {
                    exec: mode == Some("100755"),
                },
                _ => continue,
            };
            let (Some(path), Some(id)) = (e["path"].as_str(), e["sha"].as_str()) else {
                continue;
            };
            out.push(TreeEntry {
                path: path.to_string(),
                blob: id.to_string(),
                size: e["size"].as_u64(),
                kind,
            });
        }
        if v["truncated"].as_bool() != Some(true) || items.is_empty() {
            return Ok(out);
        }
    }
    Err(Error::Refused(format!(
        "{} holds more files than Columbus can sync",
        r.place()
    )))
}

pub(super) async fn blob(r: &Remote, id: &str) -> Result<Vec<u8>, Error> {
    let reply = r
        .http
        .get(&repo_path(r, &["git", "blobs", id]), &[])
        .await?;
    if reply.status != 200 {
        return Err(r.http.failed(&reply));
    }
    let v = reply.json(&r.http)?;
    match (v["encoding"].as_str(), v["content"].as_str()) {
        (Some("base64"), Some(text)) => unbase64(text, "a file", &r.http.host),
        // An empty file has no content to encode.
        _ if v["size"].as_u64() == Some(0) => Ok(Vec::new()),
        _ => Err(Error::Server(format!(
            "{} did not send the file",
            r.http.host
        ))),
    }
}

fn operation(c: &Change) -> Value {
    match c {
        Change::Create { path, bytes, .. } => {
            json!({"operation": "create", "path": path, "content": base64(bytes)})
        }
        Change::Update {
            path, bytes, old, ..
        } => {
            json!({"operation": "update", "path": path, "content": base64(bytes), "sha": old})
        }
        Change::Delete { path, old } => json!({"operation": "delete", "path": path, "sha": old}),
    }
}

pub(super) async fn send(
    r: &Remote,
    tip_read: &Tip,
    changes: &[Change],
    message: &str,
) -> Result<String, SendError> {
    // Usually one call, so one commit.
    let mut calls: Vec<Vec<&Change>> = vec![Vec::new()];
    let mut bytes = 0usize;
    for c in changes {
        let n = c.bytes().map_or(0, <[u8]>::len);
        if bytes + n > CALL_BYTES && !calls.last().is_some_and(Vec::is_empty) {
            calls.push(Vec::new());
            bytes = 0;
        }
        bytes += n;
        calls.last_mut().expect("one call at least").push(c);
    }
    let mut commit = String::new();
    let mut was = tip_read.clone();
    for files in calls {
        let body = json!({
            "branch": r.branch,
            "message": message,
            "files": files.iter().map(|c| operation(c)).collect::<Vec<_>>(),
        });
        let reply = r.http.post(&repo_path(r, &["contents"]), &body).await?;
        match reply.status {
            201 => {
                commit = id_of(
                    &reply.json(&r.http)?["commit"]["sha"],
                    "the change it made",
                    &r.http.host,
                )?;
                was = Tip::At(commit.clone());
            }
            // The server refuses the whole call when a file is no longer
            // the version Columbus read (422), is gone (404), or when
            // another commit landed at the same moment (409, 500). If the
            // branch has moved, the sync reads again; if it has not, the
            // refusal has another cause, which the person is told.
            404 | 405 | 409 | 422 | 500 => {
                return Err(match tip(r).await {
                    Ok(now) if now != was => SendError::Moved,
                    _ => {
                        if matches!(reply.status, 404 | 405) {
                            // A server too old to have this call answers so.
                            if let Err(e @ Error::TooOld(_)) = check_version(&r.http).await {
                                return Err(SendError::Failed(e));
                            }
                        }
                        SendError::Failed(r.http.failed(&reply))
                    }
                });
            }
            423 => {
                return Err(SendError::Failed(Error::Permission(format!(
                    "{} is archived (read only) on the server, so nothing can be sent to it.",
                    r.place()
                ))))
            }
            _ => return Err(SendError::Failed(r.http.failed(&reply))),
        }
    }
    Ok(commit)
}

pub(super) async fn history(r: &Remote, limit: usize) -> Result<Vec<Commit>, Error> {
    let q = [
        ("sha", r.branch.clone()),
        ("limit", limit.clamp(1, 50).to_string()),
        ("stat", "false".to_string()),
        ("verification", "false".to_string()),
        ("files", "false".to_string()),
    ];
    let reply = r.http.get(&repo_path(r, &["commits"]), &q).await?;
    match reply.status {
        200 => {}
        // An empty repository has no history.
        409 => return Ok(Vec::new()),
        _ => return Err(r.http.failed(&reply)),
    }
    let Value::Array(items) = reply.json(&r.http)? else {
        return Ok(Vec::new());
    };
    Ok(items
        .iter()
        .take(limit)
        .filter_map(|c| {
            let author = &c["commit"]["author"];
            Some(Commit {
                id: c["sha"].as_str()?.to_string(),
                who: author["name"]
                    .as_str()
                    .or(c["author"]["login"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                when: author["date"]
                    .as_str()
                    .or(c["created"].as_str())
                    .unwrap_or_default()
                    .to_string(),
                note: c["commit"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::api_version;

    #[test]
    fn versions() {
        for (text, want) in [
            ("1.20.0", Some((1, 20))),
            ("1.19.4", Some((1, 19))),
            ("1.22.3+dev-12-gabcdef", Some((1, 22))),
            ("28.3.0", Some((28, 3))),
            ("v1.21.11", Some((1, 21))),
            // Forgejo says its own version, then the Gitea it matches.
            ("9.0.3+gitea-1.22.0", Some((1, 22))),
            ("7.0.0+gitea-1.22.0", Some((1, 22))),
            ("1.20.5-0", Some((1, 20))),
            ("1.18.5", Some((1, 18))),
            ("development", None),
            ("", None),
        ] {
            assert_eq!(api_version(text), want, "{text}");
        }
    }
}
