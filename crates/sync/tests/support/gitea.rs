//! A stand-in for Gitea's API (which Forgejo shares), served under
//! `/api/v1`, written from Gitea's published API description
//! (`templates/swagger/v1-swagger.generated.json` at v28.3.0) and, for
//! what the description leaves open, the handlers it documents
//! (`routers/api/v1/repo/file.go`, `services/repository/files/update.go`
//! and `tree.go`, `modules/setting/api.go`), read 2026-11-11.
//!
//! The endpoints Columbus uses, with the answers and refusals found there:
//!
//! * `GET /version` (no token needed), `GET /user`, `GET|POST /user/repos`,
//!   `GET /repos/{o}/{r}`, `GET /repos/{o}/{r}/branches` and
//!   `/branches/{branch}`, `GET /repos/{o}/{r}/commits`;
//! * `GET /repos/{o}/{r}/git/trees/{sha}?recursive=true&page=&per_page=`
//!   (at most 1000 a page; `truncated` while more pages follow) and
//!   `GET /repos/{o}/{r}/git/blobs/{sha}`; the `git/*` endpoints only read;
//! * `POST /repos/{o}/{r}/contents`: every created, updated and deleted
//!   file in one commit.
//!
//! Refusals: 401 without a valid `token` in `Authorization`; 404 for an
//! unknown repository or branch; lists 50 to a page at most
//! (`MAX_RESPONSE_ITEMS`); for the files call, 422 "sha does not match"
//! when a changed or deleted file is not the version given, 422
//! "repository file already exists" for a new file whose name is taken,
//! 422 when `sha` is missing, 404 for a file that is not there; 409 from
//! the commit list of an empty repository; 409 when a repository's name is
//! taken; and no files call at all on a server older than 1.20.

use axum::extract::Request;
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use base64::Engine;
use serde_json::{json, Value};

use super::http::{answer, arrive, page_of, percent_decode, query_value, Shared, World};
use super::{Entry, Repo};

const SWAGGER: &str = "https://gitea.example/api/swagger";

fn error(status: u16, message: &str) -> Response {
    answer(status, json!({"message": message, "url": SWAGGER}))
}

fn not_found() -> Response {
    answer(
        404,
        json!({"errors": [], "message": "The target couldn't be found.", "url": SWAGGER}),
    )
}

/// The router: the API under `/api/v1`.
pub fn router(world: Shared) -> Router {
    at(world, "/api/v1")
}

/// The router with the API under `prefix`.
pub fn at(world: Shared, prefix: &'static str) -> Router {
    Router::new().fallback(any(move |req: Request| respond(world.clone(), prefix, req)))
}

/// What `/version` says, as (major, minor), read the way Gitea's own
/// numbers go.
fn version(w: &World) -> (u32, u32) {
    let text = w.knobs.version.split("+gitea-").last().unwrap_or("0.0");
    let mut parts = text.split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor)
}

fn repo_json(owner: &str, name: &str, repo: &Repo, can_push: bool) -> Value {
    json!({
        "id": 1, "name": name, "full_name": format!("{owner}/{name}"),
        "owner": {"id": 1, "login": owner, "full_name": ""},
        "private": repo.private, "empty": repo.is_empty(), "archived": false, "mirror": false,
        "default_branch": repo.default_branch,
        "object_format_name": if repo.format == columbus_sync::hash::ObjectFormat::Sha256 { "sha256" } else { "sha1" },
        "html_url": format!("https://gitea.example/{owner}/{name}"),
        "clone_url": format!("https://gitea.example/{owner}/{name}.git"),
        "permissions": {"admin": can_push, "push": can_push, "pull": true},
    })
}

fn commit_json(id: &str, repo: &Repo) -> Value {
    let c = &repo.commits[id];
    let who = json!({"name": c.who, "email": format!("{}@example.org", c.who.to_lowercase()), "date": c.when});
    json!({
        "sha": id, "url": "", "html_url": "", "created": c.when,
        "commit": {"message": c.message, "author": who, "committer": who, "tree": {"sha": c.tree, "url": "", "created": c.when}, "url": ""},
        "author": {"login": c.who.to_lowercase(), "id": 1, "full_name": c.who},
        "committer": {"login": c.who.to_lowercase(), "id": 1, "full_name": c.who},
        "parents": c.parents.iter().map(|p| json!({"sha": p, "url": "", "created": ""})).collect::<Vec<_>>(),
    })
}

/// Answer one request to the API under `prefix`.
pub async fn respond(world: Shared, prefix: &str, req: Request) -> Response {
    let method = req.method().to_string();
    let (headers, path, query, body) = match arrive(&world, prefix, req).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut w = world.lock().unwrap();
    let parts: Vec<String> = path
        .trim_matches('/')
        .split('/')
        .map(percent_decode)
        .collect();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();

    // The version is public.
    if (method.as_str(), parts.as_slice()) == ("GET", &["version"][..]) {
        return answer(200, json!({"version": w.knobs.version}));
    }

    // "API tokens must be prepended with "token" followed by a space."
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("token "))
        .map(str::to_string);
    let Some(login) = token.as_ref().and_then(|t| w.tokens.get(t).cloned()) else {
        return error(401, "invalid username, password or token");
    };
    let may_write = !token
        .as_ref()
        .is_some_and(|t| w.knobs.read_only.contains(t));
    let who = w
        .names
        .get(&login)
        .cloned()
        .unwrap_or_else(|| login.clone());

    let page = query_value(&query, "page")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1usize)
        .max(1);
    // DEFAULT_PAGING_NUM 30, MAX_RESPONSE_ITEMS 50.
    let limit = match query_value(&query, "limit").and_then(|p| p.parse::<usize>().ok()) {
        None | Some(0) => 30,
        Some(n) => n.min(w.knobs.page_max.min(50)),
    };

    match (method.as_str(), parts.as_slice()) {
        ("GET", ["user"]) => answer(
            200,
            json!({"id": 1, "login": login, "login_name": "", "full_name": who, "email": "", "is_admin": false}),
        ),
        ("GET", ["user", "repos"]) => {
            let all: Vec<Value> = w
                .repos
                .iter()
                .filter(|((owner, _), _)| *owner == login)
                .map(|((o, n), r)| repo_json(o, n, r, may_write))
                .collect();
            listed(&all, page, limit)
        }
        ("POST", ["user", "repos"]) => {
            if !may_write {
                return error(403, "token does not have at least one of required scope(s), required=[write:user write:repository]");
            }
            let Some(name) = body["name"].as_str().filter(|n| !n.is_empty()) else {
                return error(422, "[Name]: Required");
            };
            if w.get(&login, name).is_some() {
                return error(409, "The repository with the same name already exists.");
            }
            let private = body["private"].as_bool().unwrap_or(false);
            let init = body["auto_init"].as_bool().unwrap_or(false);
            let branch = body["default_branch"]
                .as_str()
                .filter(|b| !b.is_empty())
                .unwrap_or("main")
                .to_string();
            let repo = w.repo(&login, name);
            repo.private = private;
            repo.default_branch = branch.clone();
            if init {
                repo.commit(
                    &branch,
                    &[super::put("README.md", format!("# {name}\n").as_bytes())],
                    "Initial commit",
                    &who,
                );
            }
            let made = repo_json(&login, name, w.get(&login, name).unwrap(), true);
            answer(201, made)
        }
        (_, ["repos", owner, name, rest @ ..]) => {
            let key = (owner.to_string(), name.to_string());
            if *owner != login || !w.repos.contains_key(&key) {
                return not_found();
            }
            repo_route(
                &mut w, &method, key, &who, rest, &query, &body, may_write, page, limit,
            )
        }
        _ => not_found(),
    }
}

fn listed(all: &[Value], page: usize, limit: usize) -> Response {
    let mut resp = answer(200, Value::Array(page_of(all, page, limit)));
    resp.headers_mut()
        .insert("x-total-count", all.len().to_string().parse().unwrap());
    resp
}

#[allow(clippy::too_many_arguments)]
fn repo_route(
    w: &mut World,
    method: &str,
    key: (String, String),
    who: &str,
    rest: &[&str],
    query: &str,
    body: &Value,
    may_write: bool,
    page: usize,
    limit: usize,
) -> Response {
    let (owner, name) = (key.0.clone(), key.1.clone());
    match (method, rest) {
        ("GET", []) => answer(200, repo_json(&owner, &name, &w.repos[&key], may_write)),
        ("GET", ["branches"]) => {
            let repo = &w.repos[&key];
            let all: Vec<Value> = repo
                .branches
                .iter()
                .map(|(b, c)| json!({"name": b, "commit": {"id": c, "message": repo.commits[c].message, "timestamp": repo.commits[c].when}, "protected": false}))
                .collect();
            listed(&all, page, limit)
        }
        ("GET", ["branches", branch @ ..]) => {
            let repo = &w.repos[&key];
            let branch = branch.join("/");
            match repo.head(&branch) {
                Some(c) => answer(
                    200,
                    json!({"name": branch, "commit": {"id": c, "message": repo.commits[c].message, "timestamp": repo.commits[c].when},
                        "protected": false, "user_can_push": may_write, "user_can_merge": may_write}),
                ),
                None => not_found(),
            }
        }
        ("GET", ["commits"]) => {
            let repo = &w.repos[&key];
            if repo.is_empty() {
                return error(409, "Git Repository is empty.");
            }
            let branch = query_value(query, "sha").unwrap_or_else(|| repo.default_branch.clone());
            if repo.head(&branch).is_none() {
                return not_found();
            }
            let all: Vec<Value> = repo
                .log(&branch)
                .iter()
                .map(|(id, _)| commit_json(id, repo))
                .collect();
            listed(&all, page, limit)
        }
        ("GET", ["git", "trees", id]) => {
            let repo = &w.repos[&key];
            // "sha of the commit": a commit's id, a tree's own, or a branch.
            let commit = repo
                .head(id)
                .map(str::to_string)
                .unwrap_or_else(|| id.to_string());
            let tree_id = repo
                .commits
                .get(&commit)
                .map(|c| c.tree.clone())
                .unwrap_or(commit);
            let Some(tree) = repo.trees.get(&tree_id) else {
                return error(400, &format!("sha not found [{id}]"));
            };
            let recursive = matches!(
                query_value(query, "recursive").as_deref(),
                Some("true" | "1")
            );
            let mut items = Vec::new();
            let mut folders = std::collections::BTreeSet::new();
            for (p, e) in tree {
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
                let size = repo.blobs.get(&e.id).map_or(0, Vec::len);
                items.push(json!({"path": p, "mode": e.mode, "type": kind, "size": size, "sha": e.id, "url": ""}));
            }
            for f in folders {
                if recursive || !f.contains('/') {
                    items.push(json!({"path": f, "mode": "040000", "type": "tree", "size": 0, "sha": repo.subtree_id(tree, &f), "url": ""}));
                }
            }
            items.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            // DEFAULT_GIT_TREES_PER_PAGE.
            let max = w.knobs.tree_page_max.max(1);
            let per_page =
                match query_value(query, "per_page").and_then(|p| p.parse::<usize>().ok()) {
                    Some(n) if n > 0 && n <= max => n,
                    _ => max,
                };
            let total = items.len();
            let start = per_page * (page - 1);
            let slice: Vec<Value> = items.into_iter().skip(start).take(per_page).collect();
            let truncated = start + slice.len() < total;
            answer(
                200,
                json!({"sha": tree_id, "url": "", "tree": slice, "truncated": truncated, "page": page, "total_count": total}),
            )
        }
        ("GET", ["git", "blobs", id]) => match w.repos[&key].blobs.get(*id) {
            Some(b) => answer(
                200,
                json!({"content": base64::engine::general_purpose::STANDARD.encode(b), "encoding": "base64", "sha": id, "size": b.len(), "url": ""}),
            ),
            None => error(
                400,
                &format!("object does not exist [id: {id}, rel_path: ]"),
            ),
        },
        ("POST", ["contents"]) => {
            // Before 1.20 there is no such call: only /contents/{filepath}.
            if version(w) < (1, 20) {
                return answer(
                    405,
                    json!({"message": "Method Not Allowed", "url": SWAGGER}),
                );
            }
            if !may_write {
                return error(403, "token does not have at least one of required scope(s), required=[write:repository]");
            }
            if let Some(hook) = w.knobs.before_landing.pop() {
                hook(w);
            }
            let repo = w.repos.get_mut(&key).unwrap();
            let branch = body["branch"]
                .as_str()
                .filter(|b| !b.is_empty())
                .unwrap_or(&repo.default_branch)
                .to_string();
            let Some(files) = body["files"].as_array().filter(|f| !f.is_empty()) else {
                return error(422, "[Files]: Required");
            };
            let head = repo.head(&branch).map(str::to_string);
            // "oldBranch must exist for this operation", unless the
            // repository is empty.
            if head.is_none() && !repo.is_empty() {
                return error(404, &format!("branch does not exist [name: {branch}]"));
            }
            let mut tree = head.as_deref().map(|h| repo.tree_at(h)).unwrap_or_default();
            for f in files {
                let (Some(op), Some(path)) = (f["operation"].as_str(), f["path"].as_str()) else {
                    return error(422, "[Files.Operation]: Required");
                };
                let given = f["sha"].as_str().unwrap_or("");
                let now = tree.get(path).map(|e| e.id.clone());
                if matches!(op, "update" | "delete") {
                    if head.is_none() && op == "delete" {
                        return error(404, &format!("branch does not exist [name: {branch}]"));
                    }
                    let Some(now) = &now else {
                        return error(
                            404,
                            &format!("object does not exist [id: , rel_path: {path}]"),
                        );
                    };
                    if given.is_empty() {
                        return error(
                            422,
                            "a SHA or commit ID must be provided when updating a file",
                        );
                    }
                    if given != now {
                        return error(
                            422,
                            &format!("sha does not match [given: {given}, expected: {now}]"),
                        );
                    }
                }
                match op {
                    "create" | "update" => {
                        if op == "create" && now.is_some() {
                            return error(422, &format!("repository file already exists [path: {path}]"));
                        }
                        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(f["content"].as_str().unwrap_or("")) else {
                            return error(422, "illegal base64 data");
                        };
                        // An updated file keeps its mark as a program.
                        let mode = tree.get(path).map(|e| e.mode.clone()).filter(|m| m == "100755").unwrap_or_else(|| "100644".into());
                        let id = repo.blob(&bytes);
                        tree.insert(path.to_string(), Entry { mode, id });
                    }
                    "delete" => {
                        tree.remove(path);
                    }
                    other => return error(500, &format!("invalid file operation: {other} {path}, supported operations are create, update, delete")),
                }
            }
            if body["force_push"].as_bool() == Some(true)
                || body["new_branch"].as_str().is_some_and(|b| !b.is_empty())
            {
                // Columbus never asks for either; a test checks the requests.
                return error(422, "force_push and new_branch are not used by Columbus");
            }
            let message = body["message"]
                .as_str()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or("Update files")
                .trim()
                .to_string();
            let tree_id = repo.tree(tree);
            let parents: Vec<String> = head.into_iter().collect();
            let commit = repo.commit_tree(&tree_id, &parents, &message, who);
            repo.branches.insert(branch.clone(), commit.clone());
            if repo.branches.len() == 1 {
                repo.default_branch = branch;
            }
            let c = &repo.commits[&commit];
            answer(
                201,
                json!({"commit": {"sha": commit, "message": c.message, "created": c.when, "url": "", "html_url": "",
                        "parents": c.parents.iter().map(|p| json!({"sha": p})).collect::<Vec<_>>(), "tree": {"sha": c.tree}},
                    "files": [], "verification": {"verified": false}}),
            )
        }
        _ => not_found(),
    }
}
