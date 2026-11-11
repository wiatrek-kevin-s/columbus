//! GitHub and GitHub Enterprise Server, through the REST API (version
//! 2022-11-28, which every supported GitHub Enterprise Server also has).
//!
//! Reading: `GET /user`, `/user/repos`, `/repos/{o}/{r}`, its `/branches`,
//! `/git/ref/heads/{branch}`, `/git/trees/{commit}?recursive=1`,
//! `/git/blobs/{id}` and `/commits`.
//!
//! Sending is built from Git's own objects, so that any number of files
//! goes as one commit: files (`POST /git/blobs`, or as text inside the next
//! call), `POST /git/trees` on the tree that was read (`base_tree`),
//! `POST /git/commits` with the commit that was read as its parent, then
//! `PATCH /git/refs/heads/{branch}` with `force: false`. GitHub moves the
//! branch only if that is a fast-forward: if anyone else's commit landed in
//! between, it refuses, and the sync reads again (R-24).
//!
//! An empty repository answers 409 to all of that until it has a first
//! file, which is made through the contents API
//! (`PUT /contents/{path}`), as GitHub's guide says.

use reqwest::Method;
use serde_json::{json, Value};

use super::http::{Http, Reply};
use super::{base64, id_of, unbase64, Account, Remote, RepoInfo};
use crate::hash::is_text;
use crate::host::{Change, Commit, EntryKind, SendError, Tip, TreeEntry};
use crate::paths::SETTINGS_FILE;
use crate::Error;

/// A text file up to this size travels inside the tree call; a larger one,
/// and any file that is not text, is sent as a file of its own first.
const INLINE_MAX: usize = 256 * 1024;
/// One tree call carries at most this much text, and this many paths.
const TREE_BYTES: usize = 2 * 1024 * 1024;
const TREE_PATHS: usize = 400;

pub(super) async fn whoami(http: &Http) -> Result<Account, Error> {
    let reply = http.get(&["user"], &[]).await?;
    if reply.status != 200 {
        return Err(http.failed(&reply));
    }
    let v = reply.json(http)?;
    let login = v["login"].as_str().unwrap_or_default().to_string();
    if login.is_empty() {
        return Err(Error::Server(format!(
            "{} did not say whose token this is. Check the server's address in Settings > Git servers.",
            http.host
        )));
    }
    Ok(Account {
        name: v["name"].as_str().unwrap_or_default().to_string(),
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
    let items = http
        .list(
            &["user", "repos"],
            &[("sort", "full_name".to_string())],
            ("per_page", 100),
        )
        .await?;
    Ok(items.iter().filter_map(repo_info).collect())
}

pub(super) async fn repo(http: &Http, owner: &str, name: &str) -> Result<RepoInfo, Error> {
    let reply = http.get(&["repos", owner, name], &[]).await?;
    match reply.status {
        200 => repo_info(&reply.json(http)?)
            .ok_or_else(|| Error::Server(format!("{} did not describe {owner}/{name}", http.host))),
        404 => Err(Error::NotFound(format!(
            "There is no repository {owner}/{name} on {}, or the token may not see it.",
            http.host
        ))),
        _ => Err(http.failed(&reply)),
    }
}

pub(super) async fn create(http: &Http, name: &str, description: &str) -> Result<RepoInfo, Error> {
    let body =
        json!({"name": name, "description": description, "private": true, "auto_init": false});
    let reply = http.post(&["user", "repos"], &body).await?;
    match reply.status {
        201 => repo_info(&reply.json(http)?).ok_or_else(|| {
            Error::Server(format!("{} made the repository but did not describe it", http.host))
        }),
        403 | 404 => Err(Error::Permission(format!(
            "The token may not create repositories on {}. A fine-grained token needs the account permission Administration: read and write for that. Or create the repository on the server and pick it here.",
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
        .list(&["repos", owner, name, "branches"], &[], ("per_page", 100))
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

/// GitHub's answer for a repository with no commit yet.
fn is_empty_repo(reply: &Reply) -> bool {
    reply.status == 409
}

pub(super) async fn tip(r: &Remote) -> Result<Tip, Error> {
    let mut path = repo_path(r, &["git", "ref", "heads"]);
    path.extend(r.branch_parts());
    let reply = r.http.get(&path, &[]).await?;
    match reply.status {
        200 => {
            let v = reply.json(&r.http)?;
            // GitHub answers with a list when several branches start with
            // the name: then there is none of exactly that name.
            if v.is_object()
                && v["ref"].as_str() == Some(format!("refs/heads/{}", r.branch).as_str())
            {
                return Ok(Tip::At(id_of(
                    &v["object"]["sha"],
                    "the branch's commit",
                    &r.http.host,
                )?));
            }
            Err(no_branch(r))
        }
        _ if is_empty_repo(&reply) => Ok(Tip::Empty),
        404 => {
            // No such branch, or no such repository, or (on some servers)
            // an empty one.
            let info = repo(&r.http, &r.owner, &r.repo).await?;
            let any = branches(&r.http, &r.owner, &r.repo).await?;
            if any.is_empty() && info.default_branch == r.branch {
                Ok(Tip::Empty)
            } else {
                Err(no_branch(r))
            }
        }
        _ => Err(r.http.failed(&reply)),
    }
}

fn no_branch(r: &Remote) -> Error {
    Error::NotFound(format!(
        "The branch {} is not in {} (it may have been renamed or deleted). Disconnect this project and link it again.",
        r.branch,
        r.place()
    ))
}

pub(super) async fn tree(r: &Remote, commit: &str) -> Result<Vec<TreeEntry>, Error> {
    let reply = r
        .http
        .get(
            &repo_path(r, &["git", "trees", commit]),
            &[("recursive", "1".to_string())],
        )
        .await?;
    if reply.status != 200 {
        return Err(r.http.failed(&reply));
    }
    let v = reply.json(&r.http)?;
    if v["truncated"].as_bool() == Some(true) {
        return Err(Error::Refused(format!(
            "{} holds more files than GitHub lists at once (over 100,000). Columbus cannot sync a repository that large.",
            r.place()
        )));
    }
    if let Ok(id) = id_of(&v["sha"], "the list of files", &r.http.host) {
        r.trees
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(commit.to_string(), id);
    }
    let Some(items) = v["tree"].as_array() else {
        return Err(Error::Server(format!(
            "{} did not list the files of {}",
            r.http.host,
            r.place()
        )));
    };
    let mut out = Vec::new();
    for e in items {
        let kind = match (e["type"].as_str(), e["mode"].as_str()) {
            (Some("tree"), _) => continue,
            (Some("commit"), _) => EntryKind::Submodule,
            (Some("blob"), Some("120000")) => EntryKind::Link,
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
    Ok(out)
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
        (Some("utf-8"), Some(text)) => Ok(text.as_bytes().to_vec()),
        _ => Err(Error::Server(format!(
            "{} did not send the file",
            r.http.host
        ))),
    }
}

/// The id of a commit's list of files (its tree).
async fn tree_id(r: &Remote, commit: &str) -> Result<String, Error> {
    if let Some(id) = r
        .trees
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(commit)
    {
        return Ok(id.clone());
    }
    let reply = r
        .http
        .get(&repo_path(r, &["git", "commits", commit]), &[])
        .await?;
    if reply.status != 200 {
        return Err(r.http.failed(&reply));
    }
    id_of(
        &reply.json(&r.http)?["tree"]["sha"],
        "the commit's list of files",
        &r.http.host,
    )
}

/// Whether the branch is still where the sync read it. When it is, a
/// refusal has another cause, which the person is told.
async fn moved_or(r: &Remote, was: &Tip, reply: &Reply) -> SendError {
    match tip(r).await {
        Ok(now) if now != *was => SendError::Moved,
        _ => SendError::Failed(r.http.failed(reply)),
    }
}

/// The first file of an empty repository, through the contents API. Gives
/// the commit it made.
async fn first_file(
    r: &Remote,
    path: &str,
    bytes: &[u8],
    message: &str,
) -> Result<String, SendError> {
    let mut segments = repo_path(r, &["contents"]);
    segments.extend(path.split('/'));
    // No branch is named: the first commit makes the repository's default
    // branch, which is the one the project was linked to.
    let body = json!({"message": message, "content": base64(bytes)});
    let reply = r
        .http
        .call(Method::PUT, &segments, &[], Some(&body), true)
        .await?;
    match reply.status {
        200 | 201 => {
            let commit = id_of(
                &reply.json(&r.http)?["commit"]["sha"],
                "the first commit",
                &r.http.host,
            )?;
            // It must be on the linked branch.
            match tip(r).await? {
                Tip::At(now) if now == commit => Ok(commit),
                Tip::At(_) => Err(SendError::Moved),
                Tip::Empty => Err(SendError::Failed(Error::Refused(format!(
                    "{} was empty, and its first file went to its default branch, which is not {}. Disconnect this project and link it again, choosing that branch.",
                    r.place(),
                    r.branch
                )))),
            }
        }
        // Someone else's first file came first, or this one is there already.
        409 | 422 => Err(moved_or(r, &Tip::Empty, &reply).await),
        _ => Err(SendError::Failed(r.http.failed(&reply))),
    }
}

async fn post_tree(r: &Remote, base: &str, entries: &mut Vec<Value>) -> Result<String, SendError> {
    let body = json!({"base_tree": base, "tree": std::mem::take(entries)});
    let reply = r.http.post(&repo_path(r, &["git", "trees"]), &body).await?;
    if reply.status != 201 {
        return Err(SendError::Failed(r.http.failed(&reply)));
    }
    Ok(id_of(
        &reply.json(&r.http)?["sha"],
        "the new list of files",
        &r.http.host,
    )?)
}

pub(super) async fn send(
    r: &Remote,
    tip_read: &Tip,
    changes: &[Change],
    message: &str,
) -> Result<String, SendError> {
    let mut rest: Vec<&Change> = changes.iter().collect();
    let parent = match tip_read {
        Tip::At(c) => c.clone(),
        Tip::Empty => {
            // The project's shared settings first, if they are among the
            // files; else the first new file.
            let first = rest
                .iter()
                .position(|c| matches!(c, Change::Create { path, .. } if path == SETTINGS_FILE))
                .or_else(|| rest.iter().position(|c| matches!(c, Change::Create { .. })));
            let Some(Change::Create { path, bytes, .. }) = first.map(|i| rest.remove(i)) else {
                return Err(SendError::Failed(Error::Refused(
                    "there is no new file to start the repository with".into(),
                )));
            };
            let commit = first_file(r, path, bytes, message).await?;
            if rest.is_empty() {
                return Ok(commit);
            }
            commit
        }
    };

    let mut base = tree_id(r, &parent).await?;
    let mut entries: Vec<Value> = Vec::new();
    let mut inline = 0usize;
    for c in rest {
        let (path, bytes, exec) = match c {
            Change::Delete { path, .. } => {
                entries.push(json!({"path": path, "mode": "100644", "type": "blob", "sha": null}));
                continue;
            }
            Change::Create { path, bytes, exec }
            | Change::Update {
                path, bytes, exec, ..
            } => (path, bytes, *exec),
        };
        let mode = if exec { "100755" } else { "100644" };
        let text = std::str::from_utf8(bytes)
            .ok()
            .filter(|_| bytes.len() <= INLINE_MAX && is_text(bytes));
        match text {
            Some(text) => {
                inline += bytes.len();
                entries.push(json!({"path": path, "mode": mode, "type": "blob", "content": text}));
            }
            None => {
                let body = json!({"content": base64(bytes), "encoding": "base64"});
                let reply = r.http.post(&repo_path(r, &["git", "blobs"]), &body).await?;
                if reply.status != 201 {
                    return Err(SendError::Failed(r.http.failed(&reply)));
                }
                let id = id_of(&reply.json(&r.http)?["sha"], "the file's id", &r.http.host)?;
                entries.push(json!({"path": path, "mode": mode, "type": "blob", "sha": id}));
            }
        }
        if inline >= TREE_BYTES || entries.len() >= TREE_PATHS {
            base = post_tree(r, &base, &mut entries).await?;
            inline = 0;
        }
    }
    if !entries.is_empty() {
        base = post_tree(r, &base, &mut entries).await?;
    }

    let body = json!({"message": message, "tree": base, "parents": [parent]});
    let reply = r
        .http
        .post(&repo_path(r, &["git", "commits"]), &body)
        .await?;
    if reply.status != 201 {
        return Err(SendError::Failed(r.http.failed(&reply)));
    }
    let commit = id_of(&reply.json(&r.http)?["sha"], "the new commit", &r.http.host)?;

    // Move the branch, only if that loses nobody's commit.
    let mut segments = repo_path(r, &["git", "refs", "heads"]);
    segments.extend(r.branch_parts());
    let body = json!({"sha": commit, "force": false});
    let reply = r
        .http
        .call(Method::PATCH, &segments, &[], Some(&body), true)
        .await?;
    match reply.status {
        200 => Ok(commit),
        409 | 422 => Err(moved_or(r, &Tip::At(parent), &reply).await),
        _ => Err(SendError::Failed(r.http.failed(&reply))),
    }
}

pub(super) async fn history(r: &Remote, limit: usize) -> Result<Vec<Commit>, Error> {
    let q = [
        ("sha", r.branch.clone()),
        ("per_page", limit.clamp(1, 100).to_string()),
    ];
    let reply = r.http.get(&repo_path(r, &["commits"]), &q).await?;
    match reply.status {
        200 => {}
        _ if is_empty_repo(&reply) => return Ok(Vec::new()),
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
                when: author["date"].as_str().unwrap_or_default().to_string(),
                note: c["commit"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}
