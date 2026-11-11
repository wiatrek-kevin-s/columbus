//! Stand-in Git servers for the desktop app's end-to-end script
//! (`app/desktop/e2e/sync.py`): a GitHub Enterprise Server and a Gitea on
//! one port of 127.0.0.1, each with one account. They are the same
//! stand-ins the crate's own tests run against (`tests/support`), written
//! from the two servers' published API descriptions. Nothing here is a
//! real server, account or token.
//!
//! Prints `listening http://127.0.0.1:<port>` and serves until stopped:
//!
//! * `/github/api/v3/…`: GitHub's REST API. Connect it in the app as a
//!   GitHub Enterprise Server at `http://127.0.0.1:<port>/github`.
//! * `/gitea/api/v1/…`: Gitea's API (version `FAKE_GITEA_VERSION`, by
//!   default 1.22.3). Connect it at `http://127.0.0.1:<port>/gitea`.
//!
//! The one account on each is `ana` (Ana Example). Its token is the value
//! of `FAKE_GIT_TOKEN`, which the script makes up.
//!
//! For the script, without a token, under `/test/<github|gitea>/`:
//!
//! * `GET state`: every repository (its branches, the files and commits of
//!   each branch) and every request the server was sent (method, path, and
//!   whether it carried the token: never the token itself);
//! * `POST commit` `{repo, branch?, who?, message?, put: {path: text},
//!   delete: [path]}`: a colleague's commit, as if made with Git;
//! * `POST land` (the same body): that commit lands while the next send is
//!   under way.

#[path = "../tests/support/mod.rs"]
mod support;

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use support::http::{shared, Shared};
use support::{del, put, Edit};

#[derive(Clone)]
struct Both {
    github: Shared,
    gitea: Shared,
    token: String,
}

impl Both {
    fn world(&self, kind: &str) -> Option<&Shared> {
        match kind {
            "github" => Some(&self.github),
            "gitea" => Some(&self.gitea),
            _ => None,
        }
    }
}

fn edits(body: &Value) -> Vec<Edit> {
    let mut out = Vec::new();
    if let Some(files) = body["put"].as_object() {
        for (path, text) in files {
            out.push(put(path, text.as_str().unwrap_or_default().as_bytes()));
        }
    }
    for path in body["delete"].as_array().into_iter().flatten() {
        out.push(del(path.as_str().unwrap_or_default()));
    }
    out
}

async fn state(State(both): State<Both>, Path(kind): Path<String>) -> Json<Value> {
    let Some(world) = both.world(&kind) else {
        return Json(json!({"error": "no such server"}));
    };
    let w = world.lock().unwrap();
    let mut repos = BTreeMap::new();
    for ((owner, name), repo) in &w.repos {
        let mut branches = BTreeMap::new();
        for branch in repo.branches.keys() {
            let files: BTreeMap<String, String> = repo
                .files(branch)
                .into_iter()
                .map(|(p, b)| (p, String::from_utf8_lossy(&b).into_owned()))
                .collect();
            let log: Vec<Value> = repo
                .log(branch)
                .into_iter()
                .map(|(id, c)| json!({"id": id, "who": c.who, "message": c.message}))
                .collect();
            branches.insert(branch.clone(), json!({"files": files, "log": log}));
        }
        repos.insert(
            format!("{owner}/{name}"),
            json!({"private": repo.private, "default_branch": repo.default_branch, "branches": branches}),
        );
    }
    let expected = [
        format!("Bearer {}", both.token),
        format!("token {}", both.token),
    ];
    let seen: Vec<Value> = w
        .seen
        .iter()
        .map(|s| {
            let token = match &s.auth {
                None => "none",
                Some(a) if expected.contains(a) => "ours",
                Some(_) => "other",
            };
            json!({"method": s.method, "path": s.path, "token": token, "host": s.host, "agent": s.agent,
                "forced": s.body.contains("\"force\":true") || s.body.contains("force_push")})
        })
        .collect();
    Json(json!({"repos": repos, "seen": seen}))
}

async fn commit(
    State(both): State<Both>,
    Path(kind): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let Some(world) = both.world(&kind) else {
        return Json(json!({"error": "no such server"}));
    };
    let repo = body["repo"].as_str().unwrap_or_default().to_string();
    let branch = body["branch"].as_str().unwrap_or("main").to_string();
    let who = body["who"].as_str().unwrap_or("Cy").to_string();
    let message = body["message"]
        .as_str()
        .unwrap_or("A colleague's change")
        .to_string();
    let id =
        world
            .lock()
            .unwrap()
            .repo("ana", &repo)
            .commit(&branch, &edits(&body), &message, &who);
    Json(json!({"commit": id}))
}

async fn land(
    State(both): State<Both>,
    Path(kind): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let Some(world) = both.world(&kind) else {
        return Json(json!({"error": "no such server"}));
    };
    let repo = body["repo"].as_str().unwrap_or_default().to_string();
    let branch = body["branch"].as_str().unwrap_or("main").to_string();
    let who = body["who"].as_str().unwrap_or("Cy").to_string();
    let message = body["message"]
        .as_str()
        .unwrap_or("A colleague's change, meanwhile")
        .to_string();
    let edits = edits(&body);
    world
        .lock()
        .unwrap()
        .knobs
        .before_landing
        .push(Box::new(move |w| {
            w.repo("ana", &repo).commit(&branch, &edits, &message, &who);
        }));
    Json(json!({"ok": true}))
}

#[tokio::main]
async fn main() {
    let token = std::env::var("FAKE_GIT_TOKEN").unwrap_or_else(|_| support::TOKEN.to_string());
    let both = Both {
        github: shared(),
        gitea: shared(),
        token: token.clone(),
    };
    for world in [&both.github, &both.gitea] {
        let mut w = world.lock().unwrap();
        w.user("ana", "Ana Example", &token);
        if let Ok(v) = std::env::var("FAKE_GITEA_VERSION") {
            w.knobs.version = v;
        }
    }
    let for_the_script = Router::new()
        .route("/test/{kind}/state", get(state))
        .route("/test/{kind}/commit", post(commit))
        .route("/test/{kind}/land", post(land))
        .with_state(both.clone());
    let (github, gitea) = (both.github.clone(), both.gitea.clone());
    let app = for_the_script.fallback(axum::routing::any(move |req: axum::extract::Request| {
        let (github, gitea) = (github.clone(), gitea.clone());
        async move {
            if req.uri().path().starts_with("/gitea/") {
                support::gitea::respond(gitea, "/gitea/api/v1", req).await
            } else {
                support::github::respond(github, "/github/api/v3", req).await
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port");
    println!(
        "listening http://{}",
        listener.local_addr().expect("an address")
    );
    axum::serve(listener, app).await.expect("serving");
}
