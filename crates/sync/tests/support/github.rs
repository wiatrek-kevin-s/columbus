//! A stand-in for GitHub's REST API (and a GitHub Enterprise Server's,
//! which serves it under `/api/v3`), written from GitHub's published
//! OpenAPI description (github/rest-api-description, `api.github.com`,
//! read 2026-11-11) and its guide to the Git database API.
//!
//! The endpoints Columbus uses, with the answers and refusals GitHub
//! documents for them:
//!
//! * `GET /user`, `GET|POST /user/repos`, `GET /repos/{o}/{r}`,
//!   `GET /repos/{o}/{r}/branches`, `GET /repos/{o}/{r}/commits`;
//! * the Git database: `GET /git/ref/heads/{branch}`,
//!   `GET /git/trees/{sha}?recursive=1`, `GET /git/blobs/{sha}`,
//!   `GET /git/commits/{sha}`, `POST /git/blobs`, `POST /git/trees`
//!   (with `base_tree`; a `sha` of null deletes), `POST /git/commits`,
//!   `PATCH /git/refs/heads/{branch}`;
//! * `PUT /repos/{o}/{r}/contents/{path}`.
//!
//! Refusals: 401 "Bad credentials" without a valid `Bearer` token; 404 for
//! a repository the token cannot see; 409 "Git Repository is empty." from
//! the Git database and the commit list until a first file exists; 422
//! "Update is not a fast forward" when a branch update would lose a
//! commit (and it would be accepted with `force: true`, which a test
//! checks Columbus never sends); 422 when a repository's name is taken;
//! 403 for a token that may not write; a list of files cut off
//! (`truncated`) beyond a limit; lists 100 to a page at most, with a
//! `Link` header.

use axum::extract::Request;
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use base64::Engine;
use serde_json::{json, Value};

use super::http::{answer, arrive, page_of, percent_decode, query_value, Shared, World};
use super::{Entry, Repo};

const DOCS: &str = "https://docs.github.com/rest";

fn error(status: u16, message: &str) -> Response {
    answer(
        status,
        json!({"message": message, "documentation_url": DOCS, "status": status.to_string()}),
    )
}

fn not_found() -> Response {
    error(404, "Not Found")
}

fn empty() -> Response {
    error(409, "Git Repository is empty.")
}

/// The router of a GitHub Enterprise Server: the API under `/api/v3`.
pub fn enterprise(world: Shared) -> Router {
    at(world, "/api/v3")
}

/// The router with the API under `prefix`.
pub fn at(world: Shared, prefix: &'static str) -> Router {
    Router::new().fallback(any(move |req: Request| respond(world.clone(), prefix, req)))
}

fn repo_json(owner: &str, name: &str, repo: &Repo, can_push: bool) -> Value {
    json!({
        "id": 1, "node_id": "R_1", "name": name, "full_name": format!("{owner}/{name}"),
        "owner": {"login": owner, "id": 1, "type": "User"},
        "private": repo.private, "description": null, "fork": false,
        "html_url": format!("https://github.example/{owner}/{name}"),
        "clone_url": format!("https://github.example/{owner}/{name}.git"),
        "default_branch": repo.default_branch, "archived": false, "disabled": false,
        "size": if repo.is_empty() { 0 } else { 1 },
        "visibility": if repo.private { "private" } else { "public" },
        "permissions": {"admin": can_push, "push": can_push, "pull": true},
    })
}

fn commit_json(id: &str, repo: &Repo) -> Value {
    let c = &repo.commits[id];
    let who = json!({"name": c.who, "email": format!("{}@example.org", c.who.to_lowercase()), "date": c.when});
    json!({
        "sha": id, "node_id": "C_1",
        "author": who, "committer": who, "message": c.message,
        "tree": {"sha": c.tree, "url": ""},
        "parents": c.parents.iter().map(|p| json!({"sha": p, "url": ""})).collect::<Vec<_>>(),
        "verification": {"verified": false, "reason": "unsigned", "signature": null, "payload": null, "verified_at": null},
        "html_url": "",
    })
}

/// Answer one request to the API under `prefix`.
pub async fn respond(world: Shared, prefix: &str, req: Request) -> Response {
    let method = req.method().to_string();
    let (headers, path, query, body) = match arrive(&world, prefix, req).await {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut w = world.lock().unwrap();

    // Who is asking.
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let Some(login) = token.as_ref().and_then(|t| w.tokens.get(t).cloned()) else {
        return error(401, "Bad credentials");
    };
    let may_write = !token
        .as_ref()
        .is_some_and(|t| w.knobs.read_only.contains(t));

    let parts: Vec<String> = path
        .trim_matches('/')
        .split('/')
        .map(percent_decode)
        .collect();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    let page = query_value(&query, "page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1usize);
    let per_page = query_value(&query, "per_page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(30usize)
        .clamp(1, w.knobs.page_max);

    match (method.as_str(), parts.as_slice()) {
        ("GET", ["user"]) => {
            let name = w.names.get(&login).cloned().unwrap_or_default();
            answer(
                200,
                json!({"login": login, "id": 1, "name": name, "type": "User"}),
            )
        }
        ("GET", ["user", "repos"]) => {
            let all: Vec<Value> = w
                .repos
                .iter()
                .filter(|((owner, _), _)| *owner == login)
                .map(|((o, n), r)| repo_json(o, n, r, may_write))
                .collect();
            paged(&all, page, per_page, &path)
        }
        ("POST", ["user", "repos"]) => {
            if !may_write {
                return error(403, "Resource not accessible by personal access token");
            }
            let Some(name) = body["name"].as_str() else {
                return error(422, "Repository creation failed.");
            };
            if w.get(&login, name).is_some() {
                return answer(
                    422,
                    json!({"message": "Repository creation failed.", "documentation_url": DOCS, "status": "422",
                        "errors": [{"resource": "Repository", "code": "custom", "field": "name", "message": "name already exists on this account"}]}),
                );
            }
            let private = body["private"].as_bool().unwrap_or(false);
            let init = body["auto_init"].as_bool().unwrap_or(false);
            let repo = w.repo(&login, name);
            repo.private = private;
            if init {
                repo.commit(
                    "main",
                    &[super::put("README.md", format!("# {name}\n").as_bytes())],
                    "Initial commit",
                    &login,
                );
            }
            let made = repo_json(&login, name, w.get(&login, name).unwrap(), true);
            answer(201, made)
        }
        (_, ["repos", owner, name, rest @ ..]) => {
            let (owner, name) = (owner.to_string(), name.to_string());
            // A repository the token cannot see is "not found".
            if owner != login || w.get(&owner, &name).is_none() {
                return not_found();
            }
            let who = w
                .names
                .get(&login)
                .cloned()
                .unwrap_or_else(|| login.clone());
            repo_route(
                &mut w, &method, &owner, &name, &who, rest, &query, &body, may_write, page,
                per_page, &path,
            )
        }
        _ => not_found(),
    }
}

fn paged(all: &[Value], page: usize, per_page: usize, path: &str) -> Response {
    let items = page_of(all, page, per_page);
    let mut resp = answer(200, Value::Array(items));
    let last = all.len().div_ceil(per_page).max(1);
    let mut link = Vec::new();
    if page < last {
        link.push(format!(
            "<https://github.example{path}?per_page={per_page}&page={}>; rel=\"next\"",
            page + 1
        ));
    }
    link.push(format!(
        "<https://github.example{path}?per_page={per_page}&page={last}>; rel=\"last\""
    ));
    resp.headers_mut()
        .insert("link", link.join(", ").parse().unwrap());
    resp
}

#[allow(clippy::too_many_arguments)]
fn repo_route(
    w: &mut World,
    method: &str,
    owner: &str,
    name: &str,
    who: &str,
    rest: &[&str],
    query: &str,
    body: &Value,
    may_write: bool,
    page: usize,
    per_page: usize,
    path: &str,
) -> Response {
    let key = (owner.to_string(), name.to_string());
    let forbidden = || error(403, "Resource not accessible by personal access token");
    match (method, rest) {
        ("GET", []) => {
            let repo = &w.repos[&key];
            answer(200, repo_json(owner, name, repo, may_write))
        }
        ("GET", ["branches"]) => {
            let repo = &w.repos[&key];
            let all: Vec<Value> = repo
                .branches
                .iter()
                .map(|(b, c)| json!({"name": b, "commit": {"sha": c, "url": ""}, "protected": false}))
                .collect();
            paged(&all, page, per_page, path)
        }
        ("GET", ["commits"]) => {
            let repo = &w.repos[&key];
            if repo.is_empty() {
                return empty();
            }
            let branch = query_value(query, "sha").unwrap_or_else(|| repo.default_branch.clone());
            if repo.head(&branch).is_none() {
                return not_found();
            }
            let all: Vec<Value> = repo
                .log(&branch)
                .iter()
                .map(|(id, c)| {
                    json!({"sha": id, "node_id": "C", "html_url": "", "url": "", "comments_url": "",
                        "commit": commit_json(id, repo),
                        "author": {"login": c.who.to_lowercase(), "id": 1},
                        "committer": {"login": c.who.to_lowercase(), "id": 1},
                        "parents": c.parents.iter().map(|p| json!({"sha": p})).collect::<Vec<_>>()})
                })
                .collect();
            paged(&all, page, per_page, path)
        }
        ("GET", ["git", "ref", "heads", branch @ ..]) => {
            let repo = &w.repos[&key];
            if repo.is_empty() {
                return empty();
            }
            let branch = branch.join("/");
            match repo.head(&branch) {
                Some(c) => answer(
                    200,
                    json!({"ref": format!("refs/heads/{branch}"), "node_id": "REF", "url": "",
                        "object": {"type": "commit", "sha": c, "url": ""}}),
                ),
                None => not_found(),
            }
        }
        ("GET", ["git", "trees", id]) => {
            let repo = &w.repos[&key];
            if repo.is_empty() {
                return empty();
            }
            // A commit's id or a tree's own.
            let tree_id = repo
                .commits
                .get(*id)
                .map(|c| c.tree.clone())
                .unwrap_or_else(|| id.to_string());
            let Some(tree) = repo.trees.get(&tree_id) else {
                return not_found();
            };
            let recursive = query_value(query, "recursive").is_some();
            let mut items = Vec::new();
            let mut folders = std::collections::BTreeSet::new();
            for (p, e) in tree {
                // Every folder on the way is listed too, as GitHub does.
                let mut at = 0;
                while let Some(i) = p[at..].find('/') {
                    at += i;
                    folders.insert(p[..at].to_string());
                    at += 1;
                }
                if !recursive && p.contains('/') {
                    continue;
                }
                let kind = if e.mode == "160000" { "commit" } else { "blob" };
                let mut item =
                    json!({"path": p, "mode": e.mode, "type": kind, "sha": e.id, "url": ""});
                if let Some(b) = repo.blobs.get(&e.id) {
                    item["size"] = json!(b.len());
                }
                items.push(item);
            }
            for f in folders {
                if recursive || !f.contains('/') {
                    items.push(json!({"path": f, "mode": "040000", "type": "tree", "sha": repo.subtree_id(tree, &f), "url": ""}));
                }
            }
            items.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            let truncated = items.len() > w.knobs.tree_limit;
            items.truncate(w.knobs.tree_limit);
            answer(
                200,
                json!({"sha": tree_id, "url": "", "tree": items, "truncated": truncated}),
            )
        }
        ("GET", ["git", "blobs", id]) => {
            let repo = &w.repos[&key];
            if repo.is_empty() {
                return empty();
            }
            match repo.blobs.get(*id) {
                // Broken into lines of 60, as GitHub sends it.
                Some(b) => {
                    let text = base64::engine::general_purpose::STANDARD.encode(b);
                    let lines: Vec<&str> = text
                        .as_bytes()
                        .chunks(60)
                        .map(|c| std::str::from_utf8(c).unwrap())
                        .collect();
                    answer(
                        200,
                        json!({"content": format!("{}\n", lines.join("\n")), "encoding": "base64", "sha": id, "size": b.len(), "url": "", "node_id": "B"}),
                    )
                }
                None => not_found(),
            }
        }
        ("GET", ["git", "commits", id]) => {
            let repo = &w.repos[&key];
            if repo.is_empty() {
                return empty();
            }
            if repo.commits.contains_key(*id) {
                answer(200, commit_json(id, repo))
            } else {
                not_found()
            }
        }
        ("POST", ["git", "blobs"]) => {
            if !may_write {
                return forbidden();
            }
            let repo = w.repos.get_mut(&key).unwrap();
            if repo.is_empty() {
                return empty();
            }
            let Some(content) = body["content"].as_str() else {
                return error(422, "Invalid request.");
            };
            let bytes = match body["encoding"].as_str().unwrap_or("utf-8") {
                "base64" => match base64::engine::general_purpose::STANDARD.decode(content) {
                    Ok(b) => b,
                    Err(_) => return error(422, "Invalid request."),
                },
                _ => content.as_bytes().to_vec(),
            };
            let id = repo.blob(&bytes);
            answer(201, json!({"sha": id, "url": ""}))
        }
        ("POST", ["git", "trees"]) => {
            if !may_write {
                return forbidden();
            }
            let repo = w.repos.get_mut(&key).unwrap();
            if repo.is_empty() {
                return empty();
            }
            let mut tree = match body["base_tree"].as_str() {
                Some(base) => match repo.trees.get(base) {
                    Some(t) => t.clone(),
                    None => return error(422, "base_tree is not a valid tree oid"),
                },
                None => Default::default(),
            };
            for e in body["tree"].as_array().cloned().unwrap_or_default() {
                let Some(path) = e["path"].as_str() else {
                    return error(422, "Invalid request.");
                };
                let mode = e["mode"].as_str().unwrap_or("100644").to_string();
                if let Some(text) = e["content"].as_str() {
                    let id = repo.blob(text.as_bytes());
                    tree.insert(path.to_string(), Entry { mode, id });
                } else if e["sha"].is_null() {
                    // "Returns an error if you try to delete a file that
                    // does not exist."
                    if tree.remove(path).is_none() {
                        return error(422, "GitRPC::BadObjectState");
                    }
                } else if let Some(id) = e["sha"].as_str() {
                    if !repo.blobs.contains_key(id) {
                        return error(422, "tree.sha is not a valid blob");
                    }
                    tree.insert(
                        path.to_string(),
                        Entry {
                            mode,
                            id: id.to_string(),
                        },
                    );
                } else {
                    return error(422, "Invalid request.");
                }
            }
            let id = repo.tree(tree);
            answer(
                201,
                json!({"sha": id, "url": "", "tree": [], "truncated": false}),
            )
        }
        ("POST", ["git", "commits"]) => {
            if !may_write {
                return forbidden();
            }
            let repo = w.repos.get_mut(&key).unwrap();
            if repo.is_empty() {
                return empty();
            }
            let (Some(message), Some(tree)) = (body["message"].as_str(), body["tree"].as_str())
            else {
                return error(422, "Invalid request.");
            };
            let parents: Vec<String> = body["parents"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if !repo.trees.contains_key(tree)
                || parents.iter().any(|p| !repo.commits.contains_key(p))
            {
                return error(422, "Tree SHA or parent is not valid");
            }
            // The author is the token's account when none is given.
            let id = repo.commit_tree(tree, &parents, message, who);
            answer(201, commit_json(&id, repo))
        }
        ("PATCH", ["git", "refs", "heads", branch @ ..]) => {
            if !may_write {
                return forbidden();
            }
            // A colleague's commit that lands just now.
            if let Some(hook) = w.knobs.before_landing.pop() {
                hook(w);
            }
            let repo = w.repos.get_mut(&key).unwrap();
            let branch = branch.join("/");
            let Some(head) = repo.head(&branch).map(str::to_string) else {
                return error(422, "Reference does not exist");
            };
            let Some(to) = body["sha"]
                .as_str()
                .filter(|s| repo.commits.contains_key(*s))
            else {
                return error(422, "Object does not exist");
            };
            let force = body["force"].as_bool().unwrap_or(false);
            // A fast-forward: the branch's commit is the new one or one of
            // its ancestors.
            let mut at = Some(to.to_string());
            let mut forward = false;
            while let Some(c) = at {
                if c == head {
                    forward = true;
                    break;
                }
                at = repo
                    .commits
                    .get(&c)
                    .and_then(|c| c.parents.first().cloned());
            }
            if !forward && !force {
                return error(422, "Update is not a fast forward");
            }
            repo.branches.insert(branch.clone(), to.to_string());
            answer(
                200,
                json!({"ref": format!("refs/heads/{branch}"), "node_id": "REF", "url": "", "object": {"type": "commit", "sha": to, "url": ""}}),
            )
        }
        ("PUT", ["contents", file @ ..]) => {
            if !may_write {
                return forbidden();
            }
            if let Some(hook) = w.knobs.before_landing.pop() {
                hook(w);
            }
            let repo = w.repos.get_mut(&key).unwrap();
            let file = file.join("/");
            let (Some(message), Some(content)) =
                (body["message"].as_str(), body["content"].as_str())
            else {
                return error(422, "Invalid request.");
            };
            let Ok(bytes) =
                base64::engine::general_purpose::STANDARD.decode(content.replace('\n', ""))
            else {
                return error(422, "content is not valid Base64");
            };
            let branch = body["branch"]
                .as_str()
                .unwrap_or(&repo.default_branch)
                .to_string();
            let head = repo.head(&branch).map(str::to_string);
            if head.is_none() && !repo.is_empty() {
                return error(404, &format!("Branch {branch} not found"));
            }
            let mut tree = head.as_deref().map(|h| repo.tree_at(h)).unwrap_or_default();
            match (tree.get(&file), body["sha"].as_str()) {
                (None, _) => {}
                (Some(_), None) => {
                    return error(422, "Invalid request.\n\n\"sha\" wasn't supplied.")
                }
                (Some(e), Some(given)) if e.id != given => {
                    return error(409, &format!("{file} does not match {given}"));
                }
                _ => {}
            }
            let id = repo.blob(&bytes);
            tree.insert(
                file.clone(),
                Entry {
                    mode: "100644".into(),
                    id: id.clone(),
                },
            );
            let tree_id = repo.tree(tree);
            let parents: Vec<String> = head.into_iter().collect();
            let commit = repo.commit_tree(&tree_id, &parents, message, who);
            repo.branches.insert(branch, commit.clone());
            answer(
                201,
                json!({"content": {"name": file.rsplit('/').next(), "path": file, "sha": id, "size": bytes.len(), "type": "file"},
                    "commit": commit_json(&commit, repo)}),
            )
        }
        _ => not_found(),
    }
}
