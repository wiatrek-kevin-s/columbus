//! Workflows end to end: an assistant proposes one with propose_workflow,
//! the person approves it (the same function the app calls after Touch ID),
//! and `columbus-mcp workflow run` runs it with no person there: its steps
//! read only the connections it lists, its results are exported to a
//! granted folder, and its record and report are written. A step that would
//! need an approval stops the run with "needs you"; a changed workflow does
//! not run; and the organization's switch stops them all.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj; skipped otherwise.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use columbus_shared::workflows as wf;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python() -> Option<String> {
    support::python_for("import geopandas, pyogrio, shapely, pyproj")
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

    // The organization lists the AI services allowed, and Anthropic's is
    // not among them: a review is one more use of an AI service. Nothing
    // is sent, the run says why, and a new one is refused when proposed.
    env.policy("allow_workflows: true\nassistant:\n  hosts: [\"*.openai.azure.com\", localhost]\n");
    let (rec, n) = run_review(&env, 200, good.clone()).await;
    assert_eq!(rec["review"]["status"], "did_not_run", "{rec}");
    assert_eq!(
        rec["review"]["reason"],
        "organization policy does not allow an AI service at api.anthropic.com, where an AI review is sent (assistant.hosts)",
        "{rec}"
    );
    assert_eq!(n, 0, "nothing was sent to the AI");
    assert_eq!(rec["deliveries"][0]["status"], "done", "{rec}");
    let c = start(&env).await;
    let r = call(
        &c,
        "propose_workflow",
        json!({"workflow": text.replace("crash-check", "crash-check-2")}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(
        err(&r).contains("it has an AI review, and organization policy does not allow an AI service at api.anthropic.com, where a review is sent (assistant.hosts)"),
        "{}",
        err(&r)
    );
    c.cancel().await.unwrap();
    // Listed, the review runs as before.
    env.policy("allow_workflows: true\nassistant:\n  hosts: [api.anthropic.com]\n");
    let (rec, n) = run_review(&env, 200, good.clone()).await;
    assert_eq!(rec["review"]["status"], "done", "{rec}");
    assert_eq!(n, 1, "{rec}");

    // No key where the review looks for one (the Anthropic key saved in
    // the Columbus app): the run says a review needs that key, whichever
    // service the chat uses. (Where there is a credential store the run
    // would read the person's own, so this is run only where there is none.)
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        env.policy("allow_workflows: true\n");
        let (base, seen) = ai_stand_in(200, good.clone()).await;
        let vars = env.vars();
        let w = env.workflows();
        let rec = tokio::task::spawn_blocking(move || {
            StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
                .args(["workflow", "run", "crash-check"])
                .envs(vars)
                .env("COLUMBUS_WORKFLOW_API_BASE", base.as_str())
                .env_remove("COLUMBUS_WORKFLOW_API_KEY")
                .output()
                .unwrap();
            let latest = wf::list_runs(&w, "crash-check", 1)[0]["run"]
                .as_str()
                .unwrap()
                .to_string();
            wf::read_run(&w, "crash-check", &latest).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(rec.0["review"]["status"], "did_not_run", "{}", rec.0);
        assert!(
            rec.1.contains("AI review did not run:** an AI review needs an Anthropic API key, whichever AI service the app's chat uses; the Columbus app keeps it in the system's credential store, which this system does not have"),
            "{}",
            rec.1
        );
        assert_eq!(seen.lock().unwrap().len(), 0, "nothing was sent to the AI");
    }

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

/// Every file under these folders, with its SHA-256.
fn hashes(dirs: &[PathBuf]) -> std::collections::BTreeMap<PathBuf, String> {
    fn walk(d: &Path, out: &mut std::collections::BTreeMap<PathBuf, String>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                let (h, _) = columbus_shared::canon::sha256_file(&p).unwrap();
                out.insert(p, h);
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    for d in dirs {
        walk(d, &mut out);
    }
    out
}

async fn start_with(env: &Env, extra: &[(&str, String)]) -> RunningService<RoleClient, ()> {
    let mut vars = env.vars();
    vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
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

const ROUTES: &str = "id: route-check\nname: Route check\npurpose: Check the routes every Friday.\nschedule: { days: [fri], at: \"06:00\" }\nscope: { connections: [routes], run_minutes: 5 }\nsteps:\n  - id: routes\n    tool: query_features\n    args: { connection: routes, dataset: routes }\n  - id: projected\n    tool: vector_op\n    args: { op: reproject, inputs: { layer: \"${routes}\" }, params: { crs: \"EPSG:3081\" } }\noutputs:\n  folder: { export: outbox, steps: [routes] }\n";

/// REQ-27, R-20, REQ-29: a finished run with two layers opens in a chat as
/// copies, with the run's labels, fingerprints, provenance and report; the
/// chat can filter them (a new layer each time), but can't read the run's
/// own files or replace its exports; and the run's folder, workspace and
/// exports are byte-for-byte unchanged afterwards. A run whose output
/// changed on disk refuses to open, naming the file.
#[tokio::test]
async fn a_run_opens_in_a_chat_as_copies_and_the_run_never_changes() {
    let Some(py) = python() else {
        eprintln!("skipped: no Python with geopandas");
        return;
    };
    let env = setup(&py);
    env.policy("allow_workflows: true\nhide_fields: [OWNER]\n");
    let gpkg = env.root.join("routes.gpkg");
    let script = r#"
import sys
import geopandas as gpd
from shapely.geometry import LineString
gpd.GeoDataFrame({"ROUTE_NAME": ["A", "B", "C"], "OWNER": ["x", "y", "z"]},
    geometry=[LineString([(-97.74, 30.27), (-97.73, 30.28)]), LineString([(-97.70, 30.30), (-97.69, 30.31)]),
              LineString([(-97.80, 30.20), (-97.79, 30.21)])],
    crs="EPSG:4326").to_file(sys.argv[1], layer="routes", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(&py)
        .arg("-c")
        .arg(script)
        .arg(&gpkg)
        .status()
        .unwrap()
        .success());
    std::fs::write(
        env.root.join("connections").join("routes.yaml"),
        format!(
            "id: routes\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"routes\"]\n",
            serde_json::to_string(&gpkg.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    let c = start(&env).await;
    let r = call(&c, "propose_workflow", json!({"workflow": ROUTES})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    c.cancel().await.unwrap();
    approve(&env, "route-check");
    let (code, rec, _) = run(&env, "route-check");
    assert_eq!(code, 0, "{rec}");
    let run_id = rec["run"].as_str().unwrap().to_string();
    let outputs = rec["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 2, "both layers are fingerprinted: {rec}");
    assert!(outputs
        .iter()
        .all(|o| o["sha256"].as_str().unwrap().len() == 64));
    let delivered = rec["deliveries"][0]["name"].as_str().unwrap().to_string();

    // The run's files: its folder, its workspace and its exports.
    let run_files = vec![
        wf::runs_dir(&env.workflows(), "route-check").join(&run_id),
        PathBuf::from(rec["workspace"].as_str().unwrap()),
        env.root.join("outbox"),
    ];
    let before = hashes(&run_files);
    assert!(before.len() >= 6, "{before:?}");

    let open = [("COLUMBUS_OPEN_RUN", format!("route-check/{run_id}"))];
    let c = start_with(&env, &open).await;
    let who = call(&c, "whoami", json!({}))
        .await
        .structured_content
        .unwrap();
    let opened = &who["result"]["opened_run"];
    assert_eq!(opened["workflow"], "route-check", "{who}");
    assert!(
        opened["report"].as_str().unwrap().contains("# Route check"),
        "{opened}"
    );
    let layers = opened["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 2, "{opened}");
    let ws = PathBuf::from(who["result"]["workspace"].as_str().unwrap());
    for (l, o) in layers.iter().zip(outputs) {
        assert_eq!(l["labels"], json!(["connection:routes"]), "{l}");
        assert_eq!(
            l["sha256"], o["sha256"],
            "the copy is the run's file, byte for byte"
        );
        let prov: Value = serde_json::from_str(
            &std::fs::read_to_string(
                ws.join("provenance")
                    .join(format!("{}.prov.json", l["provenance"].as_str().unwrap())),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(prov["run_provenance"]["id"], o["provenance"], "{prov}");
        assert_eq!(prov["inputs"][0]["run"], json!(run_id), "{prov}");
    }
    let routes = layers.iter().find(|l| l["step"] == "routes").unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_string();

    // Read-only: the run's own export is never replaced, overwrite or not.
    let e = err(&call(
        &c,
        "export_output",
        json!({"handle": routes, "folder": "outbox", "name": delivered, "overwrite": true}),
    )
    .await);
    assert!(e.contains("never changed"), "{e}");
    // ... and the run's own files can't be named from here.
    let theirs = rec["results"]["routes"].as_str().unwrap();
    let e = err(
        &call(
            &c,
            "vector_op",
            json!({"op": "filter", "inputs": {"layer": theirs}, "params": {"where": "ROUTE_NAME <> 'B'"}}),
        )
        .await,
    );
    assert!(e.contains("another session"), "{e}");

    // REQ-29: filter makes a new layer; the original keeps its 3 routes.
    let f = call(
        &c,
        "vector_op",
        json!({"op": "filter", "inputs": {"layer": routes}, "params": {"where": "ROUTE_NAME <> 'B'"}}),
    )
    .await;
    assert_ne!(f.is_error, Some(true), "{:?}", f.content);
    let f = f.structured_content.unwrap();
    assert_eq!(f["result"]["layer"]["summary"]["rows"], 2, "{f}");
    let filtered = f["result"]["layer"]["handle"].as_str().unwrap().to_string();
    assert_ne!(filtered, routes);
    assert!(
        f["checks"].to_string().contains("connection:routes"),
        "the run's labels carry over: {f}"
    );
    let all = call(
        &c,
        "vector_op",
        json!({"op": "filter", "inputs": {"layer": routes}, "params": {"where": "ROUTE_NAME IS NOT NULL"}}),
    )
    .await
    .structured_content
    .unwrap();
    assert_eq!(all["result"]["layer"]["summary"]["rows"], 3, "{all}");
    // A hidden field, or SQL outside the where subset, is refused.
    let e = err(&call(
        &c,
        "vector_op",
        json!({"op": "filter", "inputs": {"layer": routes}, "params": {"where": "OWNER = 'x'"}}),
    )
    .await);
    assert!(e.contains("hidden"), "{e}");
    let e = err(
        &call(
            &c,
            "vector_op",
            json!({"op": "filter", "inputs": {"layer": routes}, "params": {"where": "ROUTE_NAME IN (SELECT 1)"}}),
        )
        .await,
    );
    assert!(e.contains("filter refused"), "{e}");
    // An export from the chat is a new file, and asks as usual (review is
    // not a granted folder).
    let x = call(
        &c,
        "export_output",
        json!({"handle": filtered, "folder": "review", "name": "approved-routes"}),
    )
    .await;
    let xs = format!("{:?} {:?}", x.content, x.structured_content);
    assert!(xs.contains("approval"), "{xs}");
    assert!(!env
        .root
        .join("review")
        .join("approved-routes.gpkg")
        .exists());
    c.cancel().await.unwrap();

    // R-20: nothing of the run changed.
    assert_eq!(hashes(&run_files), before, "the run's files are unchanged");

    // A run whose output changed on disk refuses to open, naming the file.
    let changed = PathBuf::from(rec["workspace"].as_str().unwrap())
        .join(outputs[1]["file"].as_str().unwrap());
    let mut bytes = std::fs::read(&changed).unwrap();
    bytes.push(0);
    std::fs::write(&changed, bytes).unwrap();
    let e = wf::open_check(&env.workflows(), None, "route-check", &run_id).unwrap_err();
    assert!(e.contains(&changed.display().to_string()), "{e}");
    let out = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"))
        .args(["workflow", "open", "route-check", &run_id])
        .envs(env.vars())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("changed on disk") && stderr.contains(&changed.display().to_string()),
        "{stderr}"
    );
}
