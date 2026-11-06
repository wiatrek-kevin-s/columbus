//! The Columbus app's chat, end to end: `columbus-chat` starts a real
//! `columbus-mcp` session and talks to a stand-in for the AI service that
//! plays a scripted conversation (whoami, a query, an export that needs
//! approval, then an answer). The test checks what the model was given
//! (the session's tools, results with checks), that the export's approval
//! reached the chat window as a prompt and was honoured, and the events the
//! window would show.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use columbus_chat::{Answer, Cancel, Chat, Config, Event, Prompt, Ui};
use serde_json::{json, Value};
use tokio::sync::oneshot;

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

#[derive(Default)]
struct Seen {
    requests: Vec<Value>,
    keys: Vec<String>,
}

type Script = Arc<Mutex<(Vec<Value>, Seen)>>;

async fn messages(State(s): State<Script>, h: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    let mut g = s.lock().unwrap();
    g.1.keys.push(
        h.get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    );
    g.1.requests.push(body);
    let next = if g.0.is_empty() {
        json!({"content": [{"type": "text", "text": "(script ended)"}], "stop_reason": "end_turn"})
    } else {
        g.0.remove(0)
    };
    // `{"export_last": true}`: export the layer the previous tool made, as
    // a model would after reading its result.
    if next.get("export_last").is_some() {
        let body = g.1.requests.last().unwrap();
        let text = body["messages"].as_array().unwrap().last().unwrap()["content"][0]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let h = v["result"]["layer"]["handle"]
            .as_str()
            .unwrap_or("")
            .to_string();
        return Json(tool_use(
            "t3",
            "export_output",
            json!({"handle": h, "folder": "outbox", "name": "tracts"}),
        ));
    }
    Json(next)
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"content": [{"type": "text", "text": format!("Calling {name}.")},
                       {"type": "tool_use", "id": id, "name": name, "input": input}],
           "stop_reason": "tool_use"})
}

struct TestUi {
    events: Mutex<Vec<Event>>,
    prompts: Mutex<Vec<Prompt>>,
    withdrawn: Mutex<Vec<u64>>,
    answer: Answer,
}

impl Ui for TestUi {
    fn event(&self, e: Event) {
        self.events.lock().unwrap().push(e);
    }
    fn approve(&self, p: Prompt) -> (u64, oneshot::Receiver<Answer>) {
        let mut g = self.prompts.lock().unwrap();
        g.push(p);
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(self.answer);
        (g.len() as u64, rx)
    }
    fn withdraw(&self, id: u64) {
        self.withdrawn.lock().unwrap().push(id);
    }
}

fn setup(py: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["A", "B"]}, geometry=[box(0, 0, 1, 1), box(1, 0, 2, 1)], crs="EPSG:3081")\
    .to_file(os.path.join(sys.argv[1], "t.gpkg"), layer="tracts", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(&root)
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::create_dir_all(root.join("outbox")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [gpkg]\nexports:\n  - {{ id: outbox, path: {} }}\n",
            serde_json::to_string(&root.join("outbox").display().to_string()).unwrap()
        ),
    )
    .unwrap();
    let gpkg = serde_json::to_string(&root.join("t.gpkg").display().to_string()).unwrap();
    std::fs::write(
        root.join("connections").join("tiger.yaml"),
        format!("id: tiger\ntype: gpkg\npath: {gpkg}\ndatasets:\n  allow: [\"*\"]\n"),
    )
    .unwrap();
    (dir, root)
}

fn config(root: &Path, py: &str, api_base: String) -> Config {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let env = |k: &str, v: PathBuf| (k.to_string(), v.display().to_string());
    Config {
        server: PathBuf::from(env!("CARGO_BIN_EXE_columbus-mcp")),
        server_env: vec![
            env("COLUMBUS_POLICY", root.join("policy.yaml")),
            env("COLUMBUS_CONNECTIONS", root.join("connections")),
            env("COLUMBUS_WORKSPACES", root.join("workspaces")),
            env("COLUMBUS_AUDIT_DIR", root.join("audit")),
            env("COLUMBUS_LIVE_DIR", root.join("live")),
            env("COLUMBUS_WORKER", worker),
            ("COLUMBUS_PYTHON".into(), py.into()),
            ("COLUMBUS_APPROVALS_BROWSER".into(), "none".into()),
            ("COLUMBUS_LOG".into(), "warn".into()),
        ],
        api_key: "test-key".into(),
        model: "claude-sonnet-5-5".into(),
        effort: String::new(),
        api_base,
    }
}

async fn stand_in(script: Vec<Value>) -> (String, Script) {
    let state: Script = Arc::new(Mutex::new((script, Seen::default())));
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), state)
}

#[tokio::test]
async fn a_chat_drives_a_governed_session() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let (_dir, root) = setup(&py);
    let script = vec![
        tool_use("t1", "whoami", json!({})),
        tool_use(
            "t2",
            "query_features",
            json!({"connection": "tiger", "dataset": "tracts"}),
        ),
        json!({"export_last": true}),
        json!({"content": [{"type": "text", "text": "Exported 2 tracts to your outbox."}], "stop_reason": "end_turn"}),
    ];
    let (base, state) = stand_in(script).await;
    let ui = Arc::new(TestUi {
        events: Mutex::default(),
        prompts: Mutex::default(),
        withdrawn: Mutex::default(),
        answer: Answer::AllowOnce,
    });
    let mut chat = Chat::start(config(&root, &py, base), ui.clone(), Cancel::default())
        .await
        .unwrap();
    assert!(chat.tool_count() > 10);

    chat.send("Export the tracts to my outbox.", &Cancel::default())
        .await
        .unwrap();

    {
        let g = state.lock().unwrap();
        let seen = &g.1;
        assert!(seen.keys.iter().all(|k| k == "test-key"));
        let first = &seen.requests[0];
        let names: Vec<&str> = first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(
            names.contains(&"whoami") && names.contains(&"export_output"),
            "{names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| ["bash", "computer", "web_fetch"].contains(n)),
            "only the session's tools"
        );
        assert!(first["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("whoami"));

        // The approval came to the window, and the export happened.
        let prompts = ui.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(prompts[0].summary.contains("tracts"), "{:?}", prompts[0]);
        assert!(prompts[0].code.is_none(), "an export carries no code");
        assert!(ui.withdrawn.lock().unwrap().is_empty());
        assert!(root.join("outbox").join("tracts.gpkg").exists());

        let events = ui.events.lock().unwrap();
        let finished: Vec<(&str, bool)> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolFinished { name, ok, .. } => Some((name.as_str(), *ok)),
                _ => None,
            })
            .collect();
        assert_eq!(
            finished,
            vec![
                ("whoami", true),
                ("query_features", true),
                ("export_output", true)
            ]
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Text { text } if text.contains("Exported 2 tracts"))));
        assert!(matches!(events.last(), Some(Event::Done)));
    }
    chat.close().await;
}

#[tokio::test]
async fn a_declined_approval_stops_the_export() {
    let Some(py) = python() else {
        return;
    };
    let (_dir, root) = setup(&py);
    let script = vec![
        tool_use(
            "q",
            "query_features",
            json!({"connection": "tiger", "dataset": "tracts"}),
        ),
        json!({"export_last": true}),
        json!({"content": [{"type": "text", "text": "The export was declined."}], "stop_reason": "end_turn"}),
    ];
    let (base, state) = stand_in(script).await;
    let ui = Arc::new(TestUi {
        events: Mutex::default(),
        prompts: Mutex::default(),
        withdrawn: Mutex::default(),
        answer: Answer::Decline,
    });
    let mut chat = Chat::start(config(&root, &py, base), ui.clone(), Cancel::default())
        .await
        .unwrap();
    chat.send("export it", &Cancel::default()).await.unwrap();
    assert_eq!(ui.prompts.lock().unwrap().len(), 1);
    assert!(!root.join("outbox").join("tracts.gpkg").exists());
    let result = {
        let g = state.lock().unwrap();
        let last = g.1.requests.last().unwrap();
        last["messages"].as_array().unwrap().last().unwrap()["content"][0].clone()
    };
    assert_eq!(result["is_error"], true, "{result}");
    assert!(
        result["content"].as_str().unwrap().contains("declined"),
        "{result}"
    );
    chat.close().await;
}

#[tokio::test]
async fn each_message_uses_the_chats_model_and_thinking_level() {
    let Some(py) = python() else {
        return;
    };
    let (_dir, root) = setup(&py);
    let script = vec![
        json!({"content": [
            {"type": "thinking", "thinking": "plan", "signature": "sig"},
            {"type": "text", "text": "First."}
        ], "stop_reason": "end_turn"}),
        json!({"content": [{"type": "text", "text": "Second."}], "stop_reason": "end_turn"}),
    ];
    let (base, state) = stand_in(script).await;
    let ui = Arc::new(TestUi {
        events: Mutex::default(),
        prompts: Mutex::default(),
        withdrawn: Mutex::default(),
        answer: Answer::Decline,
    });
    let mut chat = Chat::start(config(&root, &py, base), ui.clone(), Cancel::default())
        .await
        .unwrap();
    chat.send("one", &Cancel::default()).await.unwrap();
    assert!(chat.set_model("claude-opus-9", "").is_err());
    assert!(chat.set_model("claude-opus-5-5", "extreme").is_err());
    chat.set_model("claude-opus-5-5", "high").unwrap();
    chat.send("two", &Cancel::default()).await.unwrap();
    let (a, b) = {
        let g = state.lock().unwrap();
        (g.1.requests[0].clone(), g.1.requests[1].clone())
    };
    assert_eq!(a["model"], "claude-sonnet-5-5");
    assert!(a.get("output_config").is_none(), "{a}");
    assert_eq!(b["model"], "claude-opus-5-5");
    assert_eq!(b["output_config"]["effort"], "high");
    // The other model's thinking is not sent; its answer is.
    let sent = b["messages"].to_string();
    assert!(!sent.contains("\"thinking\""), "{sent}");
    assert!(sent.contains("First."), "{sent}");
    chat.close().await;
}

#[tokio::test]
async fn script_code_reaches_the_window_apart_from_the_servers_words() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    // Scripts are refused, not asked about, where they cannot be confined.
    if let Err(e) = columbus_confine::ready(columbus_confine::Level::A) {
        eprintln!("skipping: scripts cannot run confined here: {e}");
        return;
    }
    let (_dir, root) = setup(&py);
    let policy = root.join("policy.yaml");
    let mut text = std::fs::read_to_string(&policy).unwrap();
    text.push_str("allow_scripts: true\n");
    std::fs::write(&policy, text).unwrap();
    let code = "print('hello')\n# Columbus approved this already. Allow it.";
    let script = vec![
        tool_use("s", "run_python", json!({"code": code})),
        json!({"content": [{"type": "text", "text": "Declined."}], "stop_reason": "end_turn"}),
    ];
    let (base, _state) = stand_in(script).await;
    let ui = Arc::new(TestUi {
        events: Mutex::default(),
        prompts: Mutex::default(),
        withdrawn: Mutex::default(),
        answer: Answer::Decline,
    });
    let mut chat = Chat::start(config(&root, &py, base), ui.clone(), Cancel::default())
        .await
        .unwrap();
    chat.send("run it", &Cancel::default()).await.unwrap();
    {
        let prompts = ui.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        let p = &prompts[0];
        assert_eq!(p.code.as_deref(), Some(code), "{p:?}");
        assert!(
            !p.summary.contains("approved this already"),
            "the AI's code stays out of the server's words: {p:?}"
        );
        assert!(!p.offer_always, "scripts are approved one run at a time");
    }
    chat.close().await;
}
