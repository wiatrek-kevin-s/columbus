//! Workflows end to end: an assistant proposes one with propose_workflow,
//! the person approves it (the same function the app calls after Touch ID),
//! and `columbus-mcp workflow run` runs it with no person there: its steps
//! read only the connections it lists, its results are exported to a
//! granted folder, and its record and report are written. A step that would
//! need an approval stops the run with "needs you"; a changed workflow does
//! not run; and the organization's switch stops them all.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj; skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use columbus_shared::workflows as wf;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", "import geopandas, pyogrio, shapely, pyproj"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    py: String,
}

impl Env {
    fn workflows(&self) -> PathBuf {
        self.root.join("workflows")
    }
    fn tools(&self) -> PathBuf {
        self.root.join("tools")
    }
    fn policy(&self, extra: &str) {
        let q = |p: PathBuf| serde_json::to_string(&p.display().to_string()).unwrap();
        std::fs::write(
            self.root.join("policy.yaml"),
            format!(
                "connection_types: [gpkg]\nlimits: {{ rows: 1000, seconds: 60 }}\napprovals: {{ page: true, wait_seconds: 0 }}\nexports:\n  - {{ id: outbox, path: {}, granted: true }}\n  - {{ id: review, path: {} }}\n{extra}",
                q(self.root.join("outbox")),
                q(self.root.join("review"))
            ),
        )
        .unwrap();
    }
    fn vars(&self) -> Vec<(String, String)> {
        let r = |p: &str| self.root.join(p).display().to_string();
        vec![
            ("COLUMBUS_POLICY".into(), r("policy.yaml")),
            ("COLUMBUS_CONNECTIONS".into(), r("connections")),
            ("COLUMBUS_SERVERS".into(), r("servers")),
            ("COLUMBUS_PLUGINS".into(), r("plugins")),
            ("COLUMBUS_PERSONAL_PLUGINS".into(), r("personal-plugins")),
            ("COLUMBUS_WORKSPACES".into(), r("workspaces")),
            ("COLUMBUS_TOOLS".into(), r("tools")),
            ("COLUMBUS_AUDIT_DIR".into(), r("audit")),
            ("COLUMBUS_LIVE_DIR".into(), r("live")),
            (
                "COLUMBUS_WORKER".into(),
                repo()
                    .join("workers/python/columbus_worker.py")
                    .display()
                    .to_string(),
            ),
            ("COLUMBUS_PYTHON".into(), self.py.clone()),
            ("COLUMBUS_APPROVALS_LAUNCH_FILE".into(), r("launch.txt")),
            ("COLUMBUS_COMPANION".into(), "off".into()),
            ("COLUMBUS_LOG".into(), "warn".into()),
        ]
    }
}

fn setup(py: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in ["connections", "outbox", "review"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let gpkg = root.join("crashes.gpkg");
    let script = r#"
import sys
import geopandas as gpd
from shapely.geometry import Point
gpd.GeoDataFrame({"crash_id": ["C1", "C2", "C3"], "district": [14, 14, 12]},
    geometry=[Point(-97.74, 30.27), Point(-97.70, 30.30), Point(-97.80, 30.20)],
    crs="EPSG:4326").to_file(sys.argv[1], layer="crashes", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(&gpkg)
        .status()
        .unwrap()
        .success());
    let q = |p: &Path| serde_json::to_string(&p.display().to_string()).unwrap();
    std::fs::write(
        root.join("connections").join("crashes.yaml"),
        format!("id: crashes\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"crashes\"]\nrow_filter: \"district = 14\"\n", q(&gpkg)),
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("other.yaml"),
        format!(
            "id: other\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"crashes\"]\n",
            q(&gpkg)
        ),
    )
    .unwrap();
    let env = Env {
        _dir: dir,
        root,
        py: py.to_string(),
    };
    env.policy("allow_workflows: true\n");
    env
}

async fn start(env: &Env) -> RunningService<RoleClient, ()> {
    let vars = env.vars();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            for (k, v) in &vars {
                cmd.env(k, v);
            }
        });
    ().serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap()
}

async fn call(
    c: &RunningService<RoleClient, ()>,
    tool: &'static str,
    args: Value,
) -> CallToolResult {
    let mut p = CallToolRequestParams::new(tool);
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    c.call_tool(p).await.unwrap()
}

fn err(r: &CallToolResult) -> String {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected an error, got {:?}",
        r.structured_content
    );
    serde_json::to_string(&r.content).unwrap()
}

/// Run a workflow as the scheduler would: (exit code, run.json, report).
fn run(env: &Env, id: &str) -> (i32, Value, String) {
    let out = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
        .args(["workflow", "run", id, "--scheduled"])
        .envs(env.vars())
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let runs = wf::list_runs(&env.workflows(), id, 1);
    let latest = runs.first().unwrap_or_else(|| {
        panic!(
            "no run recorded (exit {code}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let (rec, report) =
        wf::read_run(&env.workflows(), id, latest["run"].as_str().unwrap()).unwrap();
    (code, rec, report)
}

const WORKFLOW: &str = "id: crash-check\nname: Crash check\npurpose: Export district 14's crashes every weekday.\nschedule: { days: weekdays, at: \"06:00\" }\nscope: { connections: [crashes], run_minutes: 5 }\nsteps:\n  - id: crashes\n    tool: query_features\n    args: { connection: crashes, dataset: crashes }\n  - id: checked\n    tool: verify\n    args: { layer: \"${crashes}\" }\noutputs:\n  folder: { export: outbox, steps: [crashes] }\n";

fn approve(env: &Env, id: &str) {
    let p = wf::read_proposal(&env.workflows(), &env.tools(), id).unwrap();
    wf::install(&env.workflows(), &env.tools(), id, &p.fingerprint, "now").unwrap();
}

#[tokio::test]
async fn a_workflow_is_proposed_approved_and_runs_without_a_person() {
    let Some(py) = python() else {
        eprintln!("skipped: no Python with geopandas");
        return;
    };
    let env = setup(&py);
    let c = start(&env).await;

    // What a workflow may not be is refused when it is proposed.
    let bad = WORKFLOW.replace("tool: verify", "tool: run_python");
    let e = err(&call(&c, "propose_workflow", json!({"workflow": bad})).await);
    assert!(e.contains("cannot be a step"), "{e}");
    let bad = WORKFLOW.replace("export: outbox", "export: nowhere");
    let e = err(&call(&c, "propose_workflow", json!({"workflow": bad})).await);
    assert!(e.contains("export folders"), "{e}");
    let bad = WORKFLOW.replace("connections: [crashes]", "connections: [crashes, nope]");
    let e = err(&call(&c, "propose_workflow", json!({"workflow": bad})).await);
    assert!(e.contains("not one of the person's connections"), "{e}");

    let r = call(&c, "propose_workflow", json!({"workflow": WORKFLOW})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    let out = r.structured_content.unwrap();
    assert_eq!(out["result"]["status"], "waiting_for_person", "{out}");
    c.cancel().await.unwrap();

    // Not before the person approves it.
    let (code, rec, _) = run(&env, "crash-check");
    assert_eq!(
        (code, rec["status"].as_str()),
        (3, Some("needs_reapproval")),
        "{rec}"
    );

    approve(&env, "crash-check");
    let (code, rec, report) = run(&env, "crash-check");
    assert_eq!(code, 0, "{rec}");
    assert!(
        rec["status"].as_str().unwrap().starts_with("succeeded"),
        "{rec}"
    );
    let steps = rec["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2, "{rec}");
    assert!(steps.iter().all(|s| s["status"] == "done"), "{rec}");
    // The row filter applies: two of the three crashes.
    assert!(
        steps[0]["summary"].as_str().unwrap().contains("2 rows"),
        "{rec}"
    );
    assert_eq!(rec["deliveries"][0]["status"], "done", "{rec}");
    let exported: Vec<String> = std::fs::read_dir(env.root.join("outbox"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        exported
            .iter()
            .any(|n| n.starts_with("crash-check-crashes-") && n.ends_with(".gpkg")),
        "{exported:?}"
    );
    assert!(report.contains("# Crash check"), "{report}");
    assert!(report.contains("| crashes | query_features |"), "{report}");
    assert!(report.contains("exported to the outbox folder"), "{report}");
    // Audited as the runner's own calls.
    let audit: String = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect();
    assert!(
        audit.contains("columbus-runner"),
        "the runner's calls are audited"
    );

    // An export that would need an approval: the run stops, nothing waits.
    let review = WORKFLOW
        .replace("id: crash-check", "id: crash-review")
        .replace("export: outbox", "export: review");
    let c = start(&env).await;
    let r = call(&c, "propose_workflow", json!({"workflow": review})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    c.cancel().await.unwrap();
    approve(&env, "crash-review");
    let started = std::time::Instant::now();
    let (code, rec, report) = run(&env, "crash-review");
    assert_eq!(
        (code, rec["status"].as_str()),
        (2, Some("needs_you")),
        "{rec}"
    );
    assert!(
        started.elapsed().as_secs() < 60,
        "a run never waits for a person"
    );
    assert_eq!(rec["deliveries"][0]["status"], "needs_you", "{rec}");
    assert!(std::fs::read_dir(env.root.join("review"))
        .unwrap()
        .next()
        .is_none());
    assert!(report.contains("not exported"), "{report}");

    // A step that reads a connection outside the workflow's scope is
    // refused by the session itself, even in a file the checks never saw.
    let installed = env.workflows().join("crash-check").join(wf::FILE);
    std::fs::write(&installed, WORKFLOW.replace("06:00", "07:00")).unwrap();
    let (code, rec, _) = run(&env, "crash-check");
    assert_eq!(
        (code, rec["status"].as_str()),
        (3, Some("needs_reapproval")),
        "{rec}"
    );
    std::fs::write(&installed, WORKFLOW).unwrap();

    // The organization's switch stops them all.
    env.policy("");
    let (code, rec, _) = run(&env, "crash-check");
    assert_eq!((code, rec["status"].as_str()), (3, Some("off")), "{rec}");
    let c = start(&env).await;
    let e = err(&call(&c, "propose_workflow", json!({"workflow": WORKFLOW})).await);
    assert!(e.contains("allow_workflows"), "{e}");
    c.cancel().await.unwrap();
}

/// A stand-in for the AI service: answers each review with `answer` (or
/// fails with `status`), and keeps what it was sent.
async fn ai_stand_in(
    status: u16,
    answer: Value,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::routing::post;
    let seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let s2 = seen.clone();
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move |axum::Json(body): axum::Json<Value>| {
            let s2 = s2.clone();
            let answer = answer.clone();
            async move {
                s2.lock().unwrap().push(body);
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    axum::Json(answer),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

#[tokio::test]
async fn an_ai_review_reads_its_rows_and_nothing_else() {
    let Some(py) = python() else {
        return;
    };
    let env = setup(&py);
    let text = WORKFLOW.replace(
        "outputs:",
        "review:\n  reads: [crashes]\n  fields: [crash_id]\n  max_rows: 50\n  model: claude-haiku-5-5\n  effort: low\n  criteria: Flag crash C2.\noutputs:",
    );
    let c = start(&env).await;
    let r = call(&c, "propose_workflow", json!({"workflow": text})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    c.cancel().await.unwrap();
    let p = wf::read_proposal(&env.workflows(), &env.tools(), "crash-check").unwrap();
    assert!(p.plain.join(" ").contains("AI review: sends at most 50 rows of crashes (only crash_id) to Anthropic (claude-haiku-5-5, thinking low)"), "{:?}", p.plain);
    approve(&env, "crash-check");

    let answer = json!({"content": [{"type": "text", "text": "{\"summary\": \"One crash matches.\", \"flagged\": [{\"layer\": \"crashes\", \"row\": 2, \"why\": \"it is C2\"}]}"}],
        "usage": {"input_tokens": 120, "output_tokens": 30}, "stop_reason": "end_turn"});
    let (base, seen) = ai_stand_in(200, answer).await;
    let vars = [
        ("COLUMBUS_WORKFLOW_API_BASE", base.as_str()),
        ("COLUMBUS_WORKFLOW_API_KEY", "sk-test"),
    ];
    let (code, rec, report) = tokio::task::spawn_blocking({
        let env_vars = env.vars();
        let root = env.root.clone();
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        move || {
            let out = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
                .args(["workflow", "run", "crash-check"])
                .envs(env_vars)
                .envs(vars)
                .output()
                .unwrap();
            let w = root.join("workflows");
            let latest = wf::list_runs(&w, "crash-check", 1)[0]["run"]
                .as_str()
                .unwrap()
                .to_string();
            let (rec, report) = wf::read_run(&w, "crash-check", &latest).unwrap();
            (out.status.code().unwrap_or(-1), rec, report)
        }
    })
    .await
    .unwrap();
    assert_eq!(code, 0, "{rec}");
    assert_eq!(rec["review"]["status"], "done", "{rec}");
    assert_eq!(rec["review"]["flagged"][0]["row"], 2, "{rec}");
    assert!(report.contains("One crash matches."), "{report}");
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let body = &sent[0];
    assert_eq!(body["model"], "claude-haiku-5-5");
    assert_eq!(body["output_config"]["effort"], "low");
    assert!(body.get("tools").is_none(), "the review has no tools");
    let prompt = body["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("Flag crash C2."), "{prompt}");
    assert!(prompt.contains("C1") && prompt.contains("C2"), "{prompt}");
    // Only the field asked for, and only rows the connection allows.
    assert!(!prompt.contains("district"), "{prompt}");
    assert!(!prompt.contains("C3"), "{prompt}");
    let audit: String = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect();
    assert!(
        audit.contains("workflow_review"),
        "the model call is audited"
    );

    // A review that cannot run leaves the run's results, marked.
    let (base, _) = ai_stand_in(529, json!({"error": {"message": "overloaded"}})).await;
    let env2 = env.vars();
    let (code, rec, report) = tokio::task::spawn_blocking(move || {
        let out = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
            .args(["workflow", "run", "crash-check"])
            .envs(env2)
            .envs([
                ("COLUMBUS_WORKFLOW_API_BASE", base.as_str()),
                ("COLUMBUS_WORKFLOW_API_KEY", "sk-test"),
            ])
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1),)
    })
    .await
    .map(|(c,)| {
        let w = env.workflows();
        let latest = wf::list_runs(&w, "crash-check", 1)[0]["run"]
            .as_str()
            .unwrap()
            .to_string();
        let (rec, report) = wf::read_run(&w, "crash-check", &latest).unwrap();
        (c, rec, report)
    })
    .unwrap();
    assert_eq!(code, 0, "{rec}");
    assert_eq!(rec["status"], "succeeded_with_warnings", "{rec}");
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    assert!(report.contains("AI review did not run"), "{report}");
    assert_eq!(
        rec["deliveries"][0]["status"], "done",
        "results are still delivered: {rec}"
    );
}

/// A stand-in for the AI service that streams its answer (server-sent
/// events), as the real one does when asked to.
async fn ai_stream_stand_in(text: &str) -> String {
    use axum::routing::post;
    let t = serde_json::to_string(text).unwrap();
    let body = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"role\":\"assistant\",\"content\":[],\"usage\":{{\"input_tokens\":5}}}}}}\n\n\
event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{t}}}}}\n\n\
event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":7}}}}\n\n\
event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    );
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move || {
            let body = body.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    body,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Run crash-check against a stand-in AI that answers `answer` with
/// `status`; the run's record and how many calls the stand-in had.
async fn run_review(env: &Env, status: u16, answer: Value) -> (Value, usize) {
    let (base, seen) = ai_stand_in(status, answer).await;
    let vars = env.vars();
    let w = env.workflows();
    let rec = tokio::task::spawn_blocking(move || {
        StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
            .args(["workflow", "run", "crash-check"])
            .envs(vars)
            .envs([
                ("COLUMBUS_WORKFLOW_API_BASE", base.as_str()),
                ("COLUMBUS_WORKFLOW_API_KEY", "sk-test"),
            ])
            .output()
            .unwrap();
        let latest = wf::list_runs(&w, "crash-check", 1)[0]["run"]
            .as_str()
            .unwrap()
            .to_string();
        wf::read_run(&w, "crash-check", &latest).unwrap().0
    })
    .await
    .unwrap();
    let n = seen.lock().unwrap().len();
    (rec, n)
}

#[tokio::test]
async fn a_review_that_is_not_whole_or_not_allowed_never_reads_as_an_all_clear() {
    let Some(py) = python() else {
        return;
    };
    let env = setup(&py);
    let text = WORKFLOW.replace(
        "outputs:",
        "review:\n  reads: [crashes]\n  criteria: Flag crash C2.\noutputs:",
    );
    let c = start(&env).await;
    let r = call(&c, "propose_workflow", json!({"workflow": text})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    c.cancel().await.unwrap();
    approve(&env, "crash-check");

    // Prose instead of the form asked for: did not run.
    let prose = json!({"content": [{"type": "text", "text": "Everything looks fine to me."}],
        "usage": {"input_tokens": 1, "output_tokens": 1}, "stop_reason": "end_turn"});
    let (rec, _) = run_review(&env, 200, prose).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    assert_eq!(rec["status"], "succeeded_with_warnings", "{rec}");
    // Cut off at its token limit, even with valid-looking JSON: did not run.
    let cut = json!({"content": [{"type": "text", "text": "{\"summary\": \"ok\", \"flagged\": []}"}],
        "usage": {"input_tokens": 1, "output_tokens": 1}, "stop_reason": "max_tokens"});
    let (rec, _) = run_review(&env, 200, cut).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    // flagged not a list: did not run.
    let odd = json!({"content": [{"type": "text", "text": "{\"summary\": \"ok\", \"flagged\": \"none\"}"}],
        "usage": {"input_tokens": 1, "output_tokens": 1}, "stop_reason": "end_turn"});
    let (rec, _) = run_review(&env, 200, odd).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    // The whole answer, in form: done.
    let good = json!({"content": [{"type": "text", "text": "{\"summary\": \"ok\", \"flagged\": []}"}],
        "usage": {"input_tokens": 1, "output_tokens": 1}, "stop_reason": "end_turn"});
    let (rec, _) = run_review(&env, 200, good.clone()).await;
    assert_eq!(rec["review"]["status"], "done", "{rec}");

    // A streamed answer is read whole.
    let base = ai_stream_stand_in("{\"summary\": \"streamed ok\", \"flagged\": []}").await;
    let vars = env.vars();
    let w = env.workflows();
    let rec = tokio::task::spawn_blocking(move || {
        StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
            .args(["workflow", "run", "crash-check"])
            .envs(vars)
            .envs([
                ("COLUMBUS_WORKFLOW_API_BASE", base.as_str()),
                ("COLUMBUS_WORKFLOW_API_KEY", "sk-test"),
            ])
            .output()
            .unwrap();
        let latest = wf::list_runs(&w, "crash-check", 1)[0]["run"]
            .as_str()
            .unwrap()
            .to_string();
        wf::read_run(&w, "crash-check", &latest).unwrap().0
    })
    .await
    .unwrap();
    assert_eq!(rec["review"]["status"], "done", "{rec}");
    assert_eq!(rec["review"]["summary"], "streamed ok", "{rec}");
    assert_eq!(rec["review"]["tokens"]["output"], 7, "{rec}");

    // The organization turns the review off after it was approved: nothing is sent.
    env.policy("allow_workflows: true\ndeny_tools: [workflow_review]\n");
    let (rec, n) = run_review(&env, 200, good.clone()).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    assert_eq!(n, 0, "nothing was sent to the AI");
    assert_eq!(rec["deliveries"][0]["status"], "done", "{rec}");
    // ... and a new one with a review is refused when proposed.
    let c = start(&env).await;
    let r = call(
        &c,
        "propose_workflow",
        json!({"workflow": text.replace("crash-check", "crash-check-2")}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(err(&r).contains("workflow_review"), "{}", err(&r));
    c.cancel().await.unwrap();

    // A flow rule forbids this connection's data going to the AI.
    env.policy("allow_workflows: true\nflows:\n  - { from: [\"connection:crashes\"], to: [\"ai:anthropic\"], action: deny }\n");
    let (rec, n) = run_review(&env, 200, good.clone()).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    assert!(
        rec["review"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("forbids"),
        "{rec}"
    );
    assert_eq!(n, 0, "nothing was sent to the AI");
    let c = start(&env).await;
    let r = call(
        &c,
        "propose_workflow",
        json!({"workflow": text.replace("crash-check", "crash-check-2")}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(err(&r).contains("forbids"), "{}", err(&r));
    c.cancel().await.unwrap();
    // A rule written for every outside service covers the AI review too.
    env.policy("allow_workflows: true\nflows:\n  - { from: [\"connection:crashes\"], to: [\"server:*\"], action: deny }\n");
    let (rec, n) = run_review(&env, 200, good.clone()).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    assert_eq!(n, 0, "nothing was sent to the AI");
    let audit: String = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect();
    assert!(
        audit
            .lines()
            .any(|l| l.contains("workflow_review") && l.contains("\"denied\"")),
        "a refusal is audited as denied"
    );
    // A rule asking for an approval each time: a run cannot wait for one.
    env.policy("allow_workflows: true\nflows:\n  - { from: [\"connection:*\"], to: [\"ai:*\"], action: approve }\n");
    let (rec, n) = run_review(&env, 200, good).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    assert_eq!(n, 0, "nothing was sent to the AI");
}

#[test]
fn the_runner_reads_only_its_scope() {
    // The scope the runner gives its session: listed connections, or none.
    let w = wf::check(
        &WORKFLOW
            .replace("connections: [crashes]", "connections: []")
            .replace(
                "    args: { connection: crashes, dataset: crashes }\n",
                "    args: { dataset: crashes }\n",
            ),
    )
    .unwrap();
    assert!(w.scope.connections.is_empty());
}
