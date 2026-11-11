//! The app's chat with any AI service (build brief 3, D-22; T-37): the
//! same governed session driven through each of the three request formats
//! (Anthropic Messages, OpenAI Responses, OpenAI Chat Completions), with
//! plain and with streamed answers, against stand-in services
//! (`ai_stand_ins`). No real service is contacted and no real key exists.
//!
//! What must be the same in every format is tested once, in a loop over
//! the formats: tools, approvals, an export, a map snapshot the model sees
//! as a picture, several tool calls in one step, a reply cut off at its
//! length limit, Stop, token counts and the caps, retries, redirects and
//! where the key goes. What a service sends is untrusted: the two new
//! formats are also fed damaged and odd answers.

mod ai_stand_ins;
mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};

use ai_stand_ins::{
    call, calls, conversation, error_body, result_text, results, say, stream, system_text, tools,
    End, Fmt, Next, Said, SaidCall, StandIn, ALL, NEW,
};
use columbus_chat::services::{self, added, builtin};
use columbus_chat::{
    Answer, Cancel, Chat, Config, Event, KeyHeader, Prompt, Service, Ui, MAX_CALLS_PER_STEP,
};
use serde_json::{json, Value};
use tokio::sync::{oneshot, Notify};

/// Made up for these tests: no service ever issued it.
const KEY: &str = "tk51Hq8ZpL0vXn3TdRy7WbQe2MfUaKc9Gj4sVhN6";

fn python() -> Option<String> {
    let found = support::python_for("import geopandas, pyogrio, shapely, pyproj");
    if found.is_none() {
        eprintln!("skipping: the GIS engine is not installed");
    }
    found
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

impl TestUi {
    fn new(answer: Answer) -> Arc<TestUi> {
        Arc::new(TestUi {
            events: Mutex::default(),
            prompts: Mutex::default(),
            withdrawn: Mutex::default(),
            answer,
        })
    }

    /// Everything the window was shown, as the transcript would hold it.
    fn shown(&self) -> String {
        let mut out = serde_json::to_string(&*self.events.lock().unwrap()).unwrap();
        for p in self.prompts.lock().unwrap().iter() {
            out.push_str(&format!("{p:?}"));
        }
        out
    }

    fn statuses(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::Status { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    fn texts(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Each tool call that finished: its name, whether it ran well, its summary.
    fn finished(&self) -> Vec<(String, bool, String)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                Event::ToolFinished {
                    name, ok, summary, ..
                } => Some((name.clone(), *ok, summary.clone())),
                _ => None,
            })
            .collect()
    }

    fn pictures(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::ToolFinished { image: Some(i), .. } if i.starts_with("data:image/png;base64,")))
            .count()
    }

    fn clear(&self) {
        self.events.lock().unwrap().clear();
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

fn config(root: &Path, py: &str, api_base: &str, model: &str) -> Config {
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
        api_key: KEY.into(),
        model: model.into(),
        effort: String::new(),
        api_base: api_base.into(),
    }
}

/// The service Columbus knows for each format, and its first model: the
/// stand-in only takes the place of its address.
fn known(fmt: Fmt) -> (Service, &'static str) {
    let id = match fmt {
        Fmt::Messages => "anthropic",
        Fmt::Responses => "openai",
        Fmt::Chat => "gemini",
    };
    let b = builtin(id).unwrap();
    (b.service(), b.models[0].0)
}

fn format_of(fmt: Fmt) -> columbus_chat::Format {
    match fmt {
        Fmt::Messages => columbus_chat::Format::Messages,
        Fmt::Responses => columbus_chat::Format::Responses,
        Fmt::Chat => columbus_chat::Format::Chat,
    }
}

/// A service the person added, in `fmt`.
fn own(fmt: Fmt, key_header: KeyHeader, pictures: bool) -> Service {
    added(
        "standin",
        "My gateway",
        format_of(fmt),
        key_header,
        pictures,
    )
}

/// One chat on a stand-in service, with its window.
struct Run {
    chat: Chat,
    ui: Arc<TestUi>,
    stand: StandIn,
    root: PathBuf,
    _dir: tempfile::TempDir,
    fmt: Fmt,
}

impl Run {
    async fn start(py: &str, fmt: Fmt, streamed: bool, script: Vec<Next>) -> Run {
        let (service, model) = known(fmt);
        Run::start_as(py, fmt, streamed, script, service, model, Answer::AllowOnce).await
    }

    async fn start_as(
        py: &str,
        fmt: Fmt,
        streamed: bool,
        script: Vec<Next>,
        service: Service,
        model: &str,
        answer: Answer,
    ) -> Run {
        let (dir, root) = setup(py);
        let stand = StandIn::start(fmt, streamed, script).await;
        let ui = TestUi::new(answer);
        let chat = Chat::start_with(
            config(&root, py, &stand.base, model),
            service,
            ui.clone(),
            Cancel::default(),
        )
        .await
        .unwrap();
        Run {
            chat,
            ui,
            stand,
            root,
            _dir: dir,
            fmt,
        }
    }

    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        self.chat.send(text, &Cancel::default()).await
    }

    /// What this run is, for a failed assertion.
    fn what(&self) -> String {
        format!(
            "{} ({})",
            self.fmt.name(),
            if self.stand.inner.lock().unwrap().streamed {
                "streamed"
            } else {
                "plain"
            }
        )
    }

    /// The stand-in took every conversation it was sent.
    fn assert_well_formed(&self) {
        assert_eq!(
            self.stand.refused(),
            Vec::<String>::new(),
            "{}: the service would have refused a conversation",
            self.what()
        );
    }

    async fn close(self) {
        self.chat.close().await;
    }
}

fn reply(s: Said) -> Next {
    Next::Reply(s)
}

/// Every format, with plain and with streamed answers.
fn every() -> Vec<(Fmt, bool)> {
    ALL.iter().flat_map(|f| [(*f, false), (*f, true)]).collect()
}

fn every_new() -> Vec<(Fmt, bool)> {
    NEW.iter().flat_map(|f| [(*f, false), (*f, true)]).collect()
}

/// No piece of the key, eight characters or longer, is in `text`.
fn assert_no_key(text: &str, what: &str) {
    for at in 0..=KEY.len() - 8 {
        let piece = &KEY[at..at + 8];
        assert!(
            !text.contains(piece),
            "{what}: a piece of the key ({piece}) is in: {text}"
        );
    }
}

#[tokio::test]
async fn a_chat_drives_a_governed_session_in_every_format() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every() {
        let mut run = Run::start(
            &py,
            fmt,
            streamed,
            vec![
                reply(call("t1", "whoami", json!({})).usage(900, 0)),
                reply(call(
                    "t2",
                    "query_features",
                    json!({"connection": "tiger", "dataset": "tracts"}),
                )),
                Next::ExportLast,
                reply(say("Exported 2 tracts to your outbox.")),
            ],
        )
        .await;
        let what = run.what();
        assert!(run.chat.tool_count() > 10);
        run.send("Export the tracts to my outbox.").await.unwrap();
        run.assert_well_formed();

        // The key went in the one header its service takes, to the
        // format's own path, and nowhere else.
        let seen = run.stand.seen();
        assert_eq!(seen.len(), 4, "{what}");
        for r in &seen {
            assert_eq!(r.path, fmt.chat_path(), "{what}");
            let (with, without) = match fmt {
                Fmt::Messages => (("x-api-key", KEY.to_string()), "authorization"),
                _ => (("authorization", format!("Bearer {KEY}")), "x-api-key"),
            };
            assert_eq!(r.header(with.0), Some(with.1.as_str()), "{what}");
            assert_eq!(r.header(without), None, "{what}");
            assert_eq!(r.header("api-key"), None, "{what}");
            assert_eq!(
                r.header("anthropic-version").is_some(),
                fmt == Fmt::Messages,
                "{what}"
            );
            assert_eq!(r.body["stream"], true, "{what}: asked for a stream");
        }

        // The model was given the session's tools and nothing else.
        let first = &seen[0].body;
        let given = tools(fmt, first);
        assert_eq!(given.len(), run.chat.tool_count(), "{what}");
        let names: Vec<&str> = given.iter().map(|t| t.0.as_str()).collect();
        assert!(
            names.contains(&"whoami") && names.contains(&"export_output"),
            "{what}: {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| ["bash", "computer", "web_fetch", "web_search"].contains(n)),
            "{what}: only the session's tools"
        );
        let system = system_text(fmt, first);
        assert!(system.contains("whoami"), "{what}");
        for k in ["web_search", "code_interpreter", "tool_choice"] {
            assert!(first.get(k).is_none(), "{what}: {k}");
        }
        match fmt {
            // As the server wrote them: nothing changed for Anthropic.
            Fmt::Messages => {
                assert!(given.iter().any(|t| t.2.get("$schema").is_some()), "{what}");
                assert!(given.iter().any(|t| t.1.chars().count() > 1024), "{what}");
            }
            // Every tool, made plain: a name the format takes, a schema
            // every service reads, a description within the limit, and
            // the whole of a cut one in the instructions.
            _ => {
                let mut cut = 0;
                for (name, description, schema) in &given {
                    assert!(columbus_chat::schema::name_ok(name), "{what}: {name}");
                    assert_eq!(
                        columbus_chat::schema::not_plain(schema),
                        Vec::<String>::new(),
                        "{what}: {name}: {schema:#}"
                    );
                    assert!(!description.is_empty(), "{what}: {name}");
                    assert!(description.chars().count() <= 1024, "{what}: {name}");
                    if description.ends_with("under this tool's name.)") {
                        cut += 1;
                        assert!(system.contains(&format!("### {name}\n")), "{what}: {name}");
                    }
                }
                assert!(
                    cut >= 1,
                    "{what}: no description needed cutting any more; drop this check"
                );
                // (run_python's is the longest of them.)
                assert!(system.contains("### run_python\n"), "{what}");
            }
        }

        // The approval came to the window, and the export happened.
        {
            let prompts = run.ui.prompts.lock().unwrap();
            assert_eq!(prompts.len(), 1, "{what}: {prompts:?}");
            assert!(
                prompts[0].summary.contains("tracts"),
                "{what}: {:?}",
                prompts[0]
            );
            assert!(
                prompts[0].code.is_none(),
                "{what}: an export carries no code"
            );
        }
        assert!(run.ui.withdrawn.lock().unwrap().is_empty(), "{what}");
        assert!(
            run.root.join("outbox").join("tracts.gpkg").exists(),
            "{what}"
        );
        let finished: Vec<(String, bool)> = run
            .ui
            .finished()
            .into_iter()
            .map(|(n, ok, _)| (n, ok))
            .collect();
        assert_eq!(
            finished,
            vec![
                ("whoami".to_string(), true),
                ("query_features".to_string(), true),
                ("export_output".to_string(), true)
            ],
            "{what}"
        );
        assert!(
            run.ui
                .texts()
                .iter()
                .any(|t| t.contains("Exported 2 tracts")),
            "{what}"
        );
        assert!(
            matches!(run.ui.events.lock().unwrap().last(), Some(Event::Done)),
            "{what}"
        );
        assert_no_key(&run.ui.shown(), &what);
        // Each result went back under its own call's id.
        let ids: Vec<String> = results(fmt, &seen[3].body)
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(ids, ["t1", "t2", "t3"], "{what}");
        run.close().await;
    }
}

#[tokio::test]
async fn a_declined_approval_stops_the_export_in_every_format() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every() {
        let (service, model) = known(fmt);
        let mut run = Run::start_as(
            &py,
            fmt,
            streamed,
            vec![
                reply(call(
                    "q",
                    "query_features",
                    json!({"connection": "tiger", "dataset": "tracts"}),
                )),
                Next::ExportLast,
                reply(say("The export was declined.")),
            ],
            service,
            model,
            Answer::Decline,
        )
        .await;
        let what = run.what();
        run.send("export it").await.unwrap();
        run.assert_well_formed();
        assert_eq!(run.ui.prompts.lock().unwrap().len(), 1, "{what}");
        assert!(
            !run.root.join("outbox").join("tracts.gpkg").exists(),
            "{what}"
        );
        let last = run.stand.requests().pop().unwrap();
        let (id, result) = results(fmt, &last).pop().unwrap();
        assert_eq!(id, "t3", "{what}");
        let text = result_text(fmt, &result);
        assert!(text.contains("declined"), "{what}: {result}");
        match fmt {
            Fmt::Messages => assert_eq!(result["is_error"], true, "{what}: {result}"),
            // No mark for failure in these formats: the words say it.
            _ => assert!(
                text.starts_with("The tool call failed or was refused"),
                "{what}: {text}"
            ),
        }
        assert_eq!(
            run.ui.finished().last().map(|f| (f.0.as_str(), f.1)),
            Some(("export_output", false)),
            "{what}"
        );
        run.close().await;
    }
}

#[tokio::test]
async fn the_model_sees_a_map_snapshot_as_a_picture_in_every_format() {
    let Some(py) = python() else { return };
    let rasterio = std::process::Command::new(&py)
        .args(["-c", "import rasterio"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !rasterio {
        return;
    }
    let script = || {
        vec![
            reply(call(
                "q",
                "query_features",
                json!({"connection": "tiger", "dataset": "tracts"}),
            )),
            Next::SnapshotLast,
            reply(say("They line up.")),
        ]
    };
    for (fmt, streamed) in every() {
        // A service whose models are sent pictures.
        let mut run = Run::start(&py, fmt, streamed, script()).await;
        let what = run.what();
        run.send("show me").await.unwrap();
        run.assert_well_formed();
        let last = run.stand.requests().pop().unwrap();
        let (id, result) = results(fmt, &last).pop().unwrap();
        assert_eq!(id, "s1", "{what}");
        let png = |url: &Value| {
            url.as_str()
                .is_some_and(|u| u.starts_with("data:image/png;base64,iVBOR"))
        };
        match fmt {
            // Inside the tool result, as before.
            Fmt::Messages => {
                let blocks = result["content"].as_array().expect("text and picture");
                assert_eq!(blocks[0]["type"], "text", "{what}");
                assert_eq!(blocks[1]["type"], "image", "{what}");
                assert_eq!(blocks[1]["source"]["media_type"], "image/png", "{what}");
            }
            // Inside the tool result.
            Fmt::Responses => {
                let parts = result["output"].as_array().expect("text and picture");
                assert_eq!(parts[0]["type"], "input_text", "{what}");
                assert_eq!(parts[1]["type"], "input_image", "{what}");
                assert!(png(&parts[1]["image_url"]), "{what}: {}", parts[1]);
            }
            // In a message of its own, after the step's tool messages.
            Fmt::Chat => {
                assert!(
                    result["content"]
                        .as_str()
                        .unwrap()
                        .contains("follows in the next message"),
                    "{what}: {result}"
                );
                let msgs = conversation(fmt, &last);
                let at = msgs.iter().rposition(|m| m["role"] == "tool").unwrap();
                let after = &msgs[at + 1];
                assert_eq!(after["role"], "user", "{what}: {after}");
                let parts = after["content"].as_array().unwrap();
                assert_eq!(parts[0]["type"], "text", "{what}");
                assert!(
                    parts[0]["text"].as_str().unwrap().contains("map_snapshot"),
                    "{what}: one line saying which tool made it"
                );
                assert_eq!(parts[1]["type"], "image_url", "{what}");
                assert!(png(&parts[1]["image_url"]["url"]), "{what}");
                assert_eq!(at + 2, msgs.len(), "{what}: nothing after the pictures");
            }
        }
        assert_eq!(run.ui.pictures(), 1, "{what}: the window shows it too");
        run.close().await;

        // A service set up without pictures: a line of text instead, and
        // the window does not show as seen what the model was not sent.
        let mut run = Run::start_as(
            &py,
            fmt,
            streamed,
            script(),
            own(fmt, KeyHeader::Bearer, false),
            "any-model:latest",
            Answer::AllowOnce,
        )
        .await;
        let what = format!("{} without pictures", run.what());
        run.send("show me").await.unwrap();
        run.assert_well_formed();
        let last = run.stand.requests().pop().unwrap();
        let (_, result) = results(fmt, &last).pop().unwrap();
        let text = result_text(fmt, &result);
        assert!(
            text.contains("not set up to be sent pictures"),
            "{what}: {text}"
        );
        assert!(
            text.contains("\"result\""),
            "{what}: the tool's words are still there"
        );
        let sent = last.to_string();
        assert!(
            !sent.contains("data:image/png") && !sent.contains("iVBOR"),
            "{what}: no picture was sent"
        );
        assert_eq!(run.ui.pictures(), 0, "{what}");
        assert_eq!(run.ui.finished().last().map(|f| f.1), Some(true), "{what}");
        run.close().await;
    }
}

#[tokio::test]
async fn several_tool_calls_in_one_step_each_get_their_result_in_every_format() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every() {
        let many: Vec<(String, &str, Value)> = (0..MAX_CALLS_PER_STEP + 2)
            .map(|n| (format!("m{n}"), "whoami", json!({})))
            .collect();
        let many: Vec<(&str, &str, Value)> = many
            .iter()
            .map(|(a, b, c)| (a.as_str(), *b, c.clone()))
            .collect();
        let mut run = Run::start(
            &py,
            fmt,
            streamed,
            vec![
                reply(calls(&[
                    ("a", "whoami", json!({})),
                    ("b", "list_connections", json!({})),
                ])),
                reply(calls(&many)),
                reply(say("Done.")),
            ],
        )
        .await;
        let what = run.what();
        run.send("look around").await.unwrap();
        run.assert_well_formed();
        let reqs = run.stand.requests();
        assert_eq!(reqs.len(), 3, "{what}");
        // Two calls, two results, in order, each under its own id.
        let two = results(fmt, &reqs[1]);
        assert_eq!(
            two.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            ["a", "b"],
            "{what}"
        );
        assert!(
            result_text(fmt, &two[0].1).contains("\"session\""),
            "{what}"
        );
        assert!(result_text(fmt, &two[1].1).contains("tiger"), "{what}");
        // More than a step may hold: the first ten run, the rest are told why not.
        let all = results(fmt, &reqs[2]);
        assert_eq!(all.len(), 2 + MAX_CALLS_PER_STEP + 2, "{what}");
        for (n, (id, result)) in all.iter().skip(2).enumerate() {
            assert_eq!(id, &format!("m{n}"), "{what}");
            let text = result_text(fmt, result);
            if n < MAX_CALLS_PER_STEP {
                assert!(text.contains("\"session\""), "{what}: call {n}: {text}");
            } else {
                assert!(
                    text.contains("at most 10 tool calls per step"),
                    "{what}: call {n}: {text}"
                );
            }
        }
        assert_eq!(run.ui.finished().len(), 2 + MAX_CALLS_PER_STEP, "{what}");
        assert_eq!(
            run.ui.texts().last().map(String::as_str),
            Some("Done."),
            "{what}"
        );
        run.close().await;
    }
}

#[tokio::test]
async fn a_reply_cut_off_at_its_length_limit_runs_nothing_in_every_format() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every() {
        // The tool call in it is cut off in the middle of its arguments
        // (the Anthropic stand-in sends it whole: that format's stream is
        // read as before, and gives up on a call it cannot read).
        let cut = call(
            "c1",
            "export_output",
            json!({"handle": "ws://x/y.gpkg", "folder": "outbox", "name": "tracts"}),
        )
        .end(End::Length);
        let cut = if fmt == Fmt::Messages {
            cut
        } else {
            cut.raw_arguments(0, "{\"handle\": \"ws://x/y.gpkg\", \"fol")
        };
        let mut run = Run::start(
            &py,
            fmt,
            streamed,
            vec![reply(cut), reply(say("Shorter this time."))],
        )
        .await;
        let what = run.what();
        run.send("write a long one").await.unwrap();
        assert!(
            run.ui
                .statuses()
                .iter()
                .any(|s| s == "The reply reached its length limit."),
            "{what}: {:?}",
            run.ui.statuses()
        );
        assert!(run.ui.finished().is_empty(), "{what}: nothing was run");
        assert!(
            run.ui.prompts.lock().unwrap().is_empty(),
            "{what}: nothing was asked"
        );
        assert_eq!(run.ui.texts(), ["Calling export_output."], "{what}");
        // The conversation is still one the service takes: the cut-off
        // call is not in it.
        run.send("again, shorter").await.unwrap();
        run.assert_well_formed();
        let second = run.stand.requests().pop().unwrap();
        assert!(!second.to_string().contains("c1"), "{what}: {second}");
        assert!(
            second.to_string().contains("Calling export_output."),
            "{what}: its words stay"
        );
        assert_eq!(
            run.ui.texts().last().map(String::as_str),
            Some("Shorter this time."),
            "{what}"
        );
        run.close().await;
    }
}

#[tokio::test]
async fn stop_while_the_model_is_asked_again_keeps_the_conversation_whole_in_every_format() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every() {
        // A tool step, then Stop during the request that follows it.
        let hold = Arc::new(Notify::new());
        let mut run = Run::start(
            &py,
            fmt,
            streamed,
            vec![
                reply(call("t1", "whoami", json!({}))),
                Next::Hold(hold.clone()),
            ],
        )
        .await;
        let what = run.what();
        let cancel = Cancel::default();
        let (stand, stop) = (run.stand.clone(), cancel.clone());
        let stopper = tokio::spawn(async move {
            stand.until_requests(2).await;
            stop.cancel();
        });
        let e = run.chat.send("who am I?", &cancel).await.unwrap_err();
        stopper.await.unwrap();
        assert_eq!(e.to_string(), "stopped", "{what}");
        assert!(
            matches!(run.ui.events.lock().unwrap().last(), Some(Event::Error { text }) if text == "stopped"),
            "{what}"
        );
        // A new message must go through: the tool call still has its result.
        run.stand.push(reply(say("Fresh answer.")));
        run.send("and now?").await.unwrap();
        run.assert_well_formed();
        let third = run.stand.requests().pop().unwrap();
        let kept = results(fmt, &third);
        assert_eq!(
            kept.len(),
            1,
            "{what}: the tool's result is still there: {third}"
        );
        assert_eq!(kept[0].0, "t1", "{what}");
        let said = third.to_string();
        assert!(
            said.contains("who am I?") && said.contains("and now?"),
            "{what}: {said}"
        );
        assert_eq!(
            run.ui.texts().last().map(String::as_str),
            Some("Fresh answer."),
            "{what}"
        );
        run.close().await;

        // Stop during the first request: the person's unanswered message
        // is forgotten, as before.
        let hold = Arc::new(Notify::new());
        let mut run = Run::start(&py, fmt, streamed, vec![Next::Hold(hold.clone())]).await;
        let cancel = Cancel::default();
        let (stand, stop) = (run.stand.clone(), cancel.clone());
        let stopper = tokio::spawn(async move {
            stand.until_requests(1).await;
            stop.cancel();
        });
        assert_eq!(
            run.chat
                .send("never answered", &cancel)
                .await
                .unwrap_err()
                .to_string(),
            "stopped",
            "{what}"
        );
        stopper.await.unwrap();
        run.stand.push(reply(say("Hello.")));
        run.send("hello").await.unwrap();
        run.assert_well_formed();
        let second = run.stand.requests().pop().unwrap();
        assert!(!second.to_string().contains("never answered"), "{what}");
        assert_eq!(conversation(fmt, &second).len(), 1, "{what}");
        run.close().await;
    }
}

#[tokio::test]
async fn token_counts_mean_the_same_in_every_format() {
    let Some(py) = python() else { return };
    for fmt in ALL {
        // Each step: 300,000 fresh input tokens and 1,000,000 read from
        // the cache, which count a tenth: 400,000. The cap is 1,500,000,
        // so the fifth step is not asked for.
        let step = || reply(call("w", "whoami", json!({})).usage(300_000, 1_000_000));
        let script: Vec<Next> = (0..8).map(|_| step()).collect();
        let mut run = Run::start(&py, fmt, false, script).await;
        let what = run.what();
        run.send("keep going").await.unwrap();
        // (The stand-in's calls reuse one id: Columbus makes them unique.)
        run.assert_well_formed();
        assert_eq!(run.stand.requests().len(), 4, "{what}");
        let said = run.ui.statuses();
        let stopped = said
            .iter()
            .find(|s| {
                s.starts_with("Stopped: this message has used about 1600 thousand input tokens")
            })
            .unwrap_or_else(|| panic!("{what}: {said:?}"));
        assert!(
            !stopped.contains("worked that out"),
            "{what}: the service's own count: {stopped}"
        );
        assert!(
            matches!(run.ui.events.lock().unwrap().last(), Some(Event::Done)),
            "{what}"
        );
        run.close().await;
    }
}

#[tokio::test]
async fn a_service_that_sends_no_token_counts_is_estimated_and_the_cap_holds() {
    let Some(py) = python() else { return };
    for fmt in ALL {
        // No reply carries a count. Each step reads the dataset again, so
        // the conversation grows; the cap must stop it well before the
        // limit on steps does.
        let step = || {
            reply(
                calls(&[
                    (
                        "r",
                        "describe_dataset",
                        json!({"connection": "tiger", "dataset": "tracts"}),
                    ),
                    (
                        "s",
                        "sample_rows",
                        json!({"connection": "tiger", "dataset": "tracts"}),
                    ),
                    ("w", "whoami", json!({})),
                ])
                .no_text(),
            )
        };
        let script: Vec<Next> = (0..columbus_chat::MAX_STEPS).map(|_| step()).collect();
        let mut run = Run::start(&py, fmt, false, script).await;
        let what = run.what();
        run.send("read it again and again").await.unwrap();
        run.assert_well_formed();
        let asked = run.stand.requests().len();
        assert!(
            asked > 3 && asked < columbus_chat::MAX_STEPS,
            "{what}: {asked} requests"
        );
        let said = run.ui.statuses();
        let stopped = said
            .iter()
            .find(|s| s.starts_with("Stopped: this message has used about"))
            .unwrap_or_else(|| panic!("{what}: {said:?}"));
        let name = &run.chat.service().name;
        assert!(
            stopped.contains(&format!("Columbus worked that out from the size of what it sent, because {name} gave no count")),
            "{what}: {stopped}"
        );
        // The estimate is of the right size: about a token for every
        // three characters sent.
        let sent: usize = run
            .stand
            .requests()
            .iter()
            .map(|r| r.to_string().len())
            .sum();
        assert!(
            sent / 3 > 1_200_000 && sent / 3 < 2_200_000,
            "{what}: {sent} characters sent"
        );
        eprintln!("{what}: stopped after {asked} requests, {sent} characters sent");
        run.close().await;
    }
}

#[tokio::test]
async fn damaged_tool_calls_are_not_run_and_the_model_is_told() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every_new() {
        let odd = Said {
            text: Some("Four calls.".into()),
            calls: vec![
                // Arguments cut off in the middle.
                SaidCall {
                    id: "d1".into(),
                    name: "export_output".into(),
                    arguments: "{\"handle\": \"ws://x/y.gpkg\", \"folder\": \"outbox\", \"na"
                        .into(),
                    extra: Value::Null,
                },
                // A tool Columbus does not have.
                SaidCall {
                    id: "d2".into(),
                    name: "delete_everything".into(),
                    arguments: "{}".into(),
                    extra: Value::Null,
                },
                // No id at all, then an id used before.
                SaidCall {
                    id: "".into(),
                    name: "whoami".into(),
                    arguments: "{}".into(),
                    extra: Value::Null,
                },
                SaidCall {
                    id: "d1".into(),
                    name: "list_connections".into(),
                    arguments: "".into(),
                    extra: Value::Null,
                },
                // Arguments that are JSON, but not an object.
                SaidCall {
                    id: "d5".into(),
                    name: "whoami".into(),
                    arguments: "[1, 2]".into(),
                    extra: Value::Null,
                },
            ],
            end: End::Tools,
            thinking: false,
            usage: Some((10, 0)),
        };
        let mut run = Run::start(
            &py,
            fmt,
            streamed,
            vec![reply(odd), reply(say("Understood."))],
        )
        .await;
        let what = run.what();
        run.send("do things").await.unwrap();
        // What went back is a conversation the service takes: every call
        // has an id of its own and arguments it can read.
        run.assert_well_formed();
        let second = run.stand.requests().pop().unwrap();
        let back = results(fmt, &second);
        assert_eq!(back.len(), 5, "{what}");
        let ids: Vec<&str> = back.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(ids[0], "d1", "{what}");
        assert_eq!(ids[1], "d2", "{what}");
        assert!(
            ids[2].starts_with("call_columbus_") && ids[3].starts_with("call_columbus_"),
            "{what}: {ids:?}"
        );
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            5,
            "{what}: {ids:?}"
        );
        let text = |n: usize| result_text(fmt, &back[n].1);
        assert!(
            text(0).contains("a tool call arrived incomplete, so it was not run"),
            "{what}: {}",
            text(0)
        );
        assert!(
            text(1).contains("Columbus has no tool named \"delete_everything\""),
            "{what}: {}",
            text(1)
        );
        assert!(
            text(2).contains("\"session\""),
            "{what}: a call with a new id runs: {}",
            text(2)
        );
        assert!(
            text(3).contains("tiger"),
            "{what}: no arguments is no arguments: {}",
            text(3)
        );
        assert!(
            text(4).contains("arrived incomplete"),
            "{what}: {}",
            text(4)
        );
        // The window says what was not run, and why.
        let finished = run.ui.finished();
        assert_eq!(
            finished.iter().map(|f| f.1).collect::<Vec<_>>(),
            [false, false, true, true, false],
            "{what}"
        );
        assert!(finished[0].2.contains("arrived incomplete"), "{what}");
        assert!(finished[1].2.contains("no tool named"), "{what}");
        // Nothing was exported, and nothing was asked.
        assert!(run.ui.prompts.lock().unwrap().is_empty(), "{what}");
        assert!(
            !run.root.join("outbox").join("tracts.gpkg").exists(),
            "{what}"
        );
        assert_eq!(
            run.ui.texts().last().map(String::as_str),
            Some("Understood."),
            "{what}"
        );
        run.close().await;
    }
}

/// A stream cut off before its last event (the one that says the reply is whole).
fn cut_stream(fmt: Fmt, said: &Said) -> String {
    let whole = stream(fmt, said);
    let last = match fmt {
        Fmt::Messages => "event: message_delta",
        Fmt::Responses => "event: response.completed",
        Fmt::Chat => "data: {\"choices\":[{\"delta\":{},\"finish_reason\"",
    };
    let at = whole
        .rfind(last)
        .unwrap_or_else(|| panic!("no {last} in {whole}"));
    whole[..at].to_string()
}

#[tokio::test]
async fn a_stream_that_ends_early_is_not_a_reply_in_every_format() {
    let Some(py) = python() else { return };
    for fmt in ALL {
        // Everything of a tool call arrived except the end of the reply.
        let said = call(
            "e1",
            "export_output",
            json!({"handle": "ws://x/y.gpkg", "folder": "outbox", "name": "tracts"}),
        );
        let mut run = Run::start(
            &py,
            fmt,
            true,
            vec![
                Next::Stream(cut_stream(fmt, &said)),
                reply(say("Here now.")),
            ],
        )
        .await;
        let what = run.what();
        let e = run.send("export").await.unwrap_err().to_string();
        assert_eq!(e, "the reply was cut off before it ended", "{what}");
        assert!(
            run.ui.finished().is_empty() && run.ui.prompts.lock().unwrap().is_empty(),
            "{what}: nothing ran"
        );
        // And a stream with nothing in it.
        run.stand
            .inner
            .lock()
            .unwrap()
            .script
            .push_front(Next::Stream(String::new()));
        let e = run.send("export").await.unwrap_err().to_string();
        assert_eq!(e, "the AI service sent no reply", "{what}");
        // The chat goes on.
        run.send("try again").await.unwrap();
        run.assert_well_formed();
        assert_eq!(
            run.ui.texts().last().map(String::as_str),
            Some("Here now."),
            "{what}"
        );
        run.close().await;
    }
}

#[tokio::test]
async fn what_a_tool_call_arrived_with_goes_back_unchanged() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every_new() {
        // Gemini's endpoint sends a signature with each tool call, which
        // it wants back (A-12); the stand-in sends one in each format.
        let extra = json!({"extra_content": {"google": {"thought_signature": "c2lnbmF0dXJlLW9mLXRoZS1maXJzdC1jYWxs"}}});
        let said = calls(&[
            ("x1", "whoami", json!({})),
            ("x2", "list_connections", json!({})),
        ])
        .extra(0, extra.clone())
        .thinking();
        let mut run = Run::start(&py, fmt, streamed, vec![reply(said), reply(say("Done."))]).await;
        let what = run.what();
        run.send("go").await.unwrap();
        run.assert_well_formed();
        let second = run.stand.requests().pop().unwrap();
        let conv = conversation(fmt, &second);
        match fmt {
            Fmt::Chat => {
                let tc = conv[1]["tool_calls"].as_array().unwrap();
                assert_eq!(
                    tc[0]["extra_content"], extra["extra_content"],
                    "{what}: {}",
                    tc[0]
                );
                assert!(
                    tc[1].get("extra_content").is_none(),
                    "{what}: only on the call it came with"
                );
                assert!(tc[0].get("index").is_none(), "{what}");
                assert_eq!(tc[0]["function"]["arguments"], "{}", "{what}");
            }
            _ => {
                // Reasoning, message and calls, as they came.
                let kinds: Vec<&str> = conv
                    .iter()
                    .skip(1)
                    .map(|i| i["type"].as_str().unwrap_or(""))
                    .collect();
                assert_eq!(
                    kinds,
                    [
                        "reasoning",
                        "message",
                        "function_call",
                        "function_call",
                        "function_call_output",
                        "function_call_output"
                    ],
                    "{what}"
                );
                assert_eq!(
                    conv[1]["encrypted_content"], "gAAAAB-encrypted-reasoning",
                    "{what}: the finished item's, not the part sent first"
                );
                assert_eq!(conv[2]["id"], "msg_1", "{what}");
                assert_eq!(conv[3]["extra_content"], extra["extra_content"], "{what}");
                assert_eq!(conv[3]["id"], "fc_0", "{what}");
            }
        }
        run.close().await;
    }
}

#[tokio::test]
async fn no_key_or_part_of_one_reaches_the_window_or_a_file() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every() {
        // The service refuses the key and quotes it, whole and in part.
        let quoting = format!(
            "Incorrect API key provided: {KEY}. You passed {}***{} (find your key at the provider's site).",
            &KEY[..14],
            &KEY[KEY.len() - 4..]
        );
        // Then a service that sends the key back in its words, in a tool
        // call's arguments and as a tool's name.
        let hostile = Said {
            text: Some(format!("Your key is {KEY}, ending {}.", &KEY[20..])),
            calls: vec![
                SaidCall {
                    id: "k1".into(),
                    name: "sample_rows".into(),
                    arguments:
                        json!({"connection": "tiger", "dataset": format!("tracts-{}", &KEY[5..30])})
                            .to_string(),
                    extra: Value::Null,
                },
                SaidCall {
                    id: "k2".into(),
                    name: format!("tool-{}", &KEY[8..24]),
                    arguments: "{}".into(),
                    extra: Value::Null,
                },
            ],
            end: End::Tools,
            thinking: false,
            usage: None,
        };
        let mut run = Run::start(
            &py,
            fmt,
            streamed,
            vec![
                Next::Status(
                    401,
                    vec![],
                    error_body(fmt, "authentication_error", &quoting),
                ),
                reply(hostile),
                reply(say("Done.")),
            ],
        )
        .await;
        let what = run.what();
        let e = run.send("hello").await.unwrap_err().to_string();
        let name = run.chat.service().name.clone();
        assert!(
            e.starts_with(&format!("{name} refused the key")),
            "{what}: {e}"
        );
        assert!(
            e.contains("Incorrect API key provided") && e.contains("[key removed]"),
            "{what}: {e}"
        );
        assert!(e.contains("Settings › Assistant"), "{what}: {e}");
        assert_no_key(&e, &what);
        assert_no_key(&run.ui.shown(), &what);
        run.send("hello again").await.unwrap();
        run.assert_well_formed();
        let shown = run.ui.shown();
        assert_no_key(&shown, &what);
        assert!(
            shown.contains("Your key is [key removed]"),
            "{what}: {shown}"
        );
        // Neither call ran: one carried the key, the other names no tool.
        let finished = run.ui.finished();
        assert_eq!(
            finished.iter().map(|f| f.1).collect::<Vec<_>>(),
            [false, false],
            "{what}"
        );
        assert!(
            finished[0].2.contains("carried the AI service's key"),
            "{what}: {finished:?}"
        );
        // Nothing Columbus wrote holds it: workspace, audit log, session files.
        let mut files = 0;
        let mut stack = vec![run.root.clone()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(bytes) = std::fs::read(&p) {
                    files += 1;
                    assert_no_key(
                        &String::from_utf8_lossy(&bytes),
                        &format!("{what}: {}", p.display()),
                    );
                }
            }
        }
        assert!(files > 3, "{what}: the session's files were looked at");
        // (The service itself is sent its own words back, as any service is.)
        run.close().await;
    }
}

/// A busy answer, a refusal for lack of credit and a refusal of the
/// request, in each format's own shape.
fn busy(fmt: Fmt) -> Value {
    match fmt {
        Fmt::Messages => {
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})
        }
        _ => {
            json!({"error": {"message": "Rate limit reached for requests", "type": "rate_limit_error", "param": null, "code": "rate_limit_exceeded"}})
        }
    }
}

fn no_credit(fmt: Fmt) -> (u16, Value) {
    match fmt {
        Fmt::Messages => (
            400,
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}}),
        ),
        _ => (
            429,
            json!({"error": {"message": "You exceeded your current quota, please check your plan and billing details.", "type": "insufficient_quota", "param": null, "code": "insufficient_quota"}}),
        ),
    }
}

#[tokio::test]
async fn a_busy_service_is_asked_again_and_an_empty_account_is_not_in_every_format() {
    let Some(py) = python() else { return };
    for (fmt, streamed) in every() {
        let mut run = Run::start(
            &py,
            fmt,
            streamed,
            vec![
                // A server error (asked again after two seconds, as before),
                // then a rate limit that says how long to wait.
                Next::Status(500, vec![], json!({"error": {"message": "The server had an error", "type": "server_error"}})),
                Next::Status(429, vec![("retry-after", "0".into())], busy(fmt)),
                reply(say("Here.")),
            ],
        )
        .await;
        let what = run.what();
        let name = run.chat.service().name.clone();
        let began = std::time::Instant::now();
        run.send("hello").await.unwrap();
        assert_eq!(run.stand.requests().len(), 3, "{what}");
        assert_eq!(
            run.ui.statuses(),
            vec![format!("{name} is busy; trying again…"); 2],
            "{what}"
        );
        assert_eq!(run.ui.texts(), ["Here."], "{what}");
        let took = began.elapsed().as_secs_f32();
        assert!((1.9..6.0).contains(&took), "{what}: waited {took} s");

        // No credit left: said plainly, and not asked again.
        run.ui.clear();
        let (code, body) = no_credit(fmt);
        run.stand.push(Next::Status(code, vec![], body));
        let e = run.send("hello").await.unwrap_err().to_string();
        assert!(
            e.starts_with(&format!("your account at {name} has no credit left")),
            "{what}: {e}"
        );
        assert_eq!(run.stand.requests().len(), 4, "{what}: not asked again");
        assert!(run.ui.statuses().is_empty(), "{what}");

        // A wait longer than a minute is told to the person, not sat out.
        run.stand.push(Next::Status(
            429,
            vec![("retry-after", "3600".into())],
            busy(fmt),
        ));
        let e = run.send("hello").await.unwrap_err().to_string();
        assert!(
            e.starts_with(&format!(
                "{name} is limiting how much you may send right now"
            )),
            "{what}: {e}"
        );
        assert_eq!(run.stand.requests().len(), 5, "{what}");
        // Busy every time: given up after four tries.
        for _ in 0..4 {
            run.stand.push(Next::Status(
                429,
                vec![("retry-after-ms", "10".into())],
                busy(fmt),
            ));
        }
        let e = run.send("hello").await.unwrap_err().to_string();
        assert!(e.contains("limiting how much you may send"), "{what}: {e}");
        assert_eq!(run.stand.requests().len(), 9, "{what}");
        // The service says not to ask again.
        run.stand.push(Next::Status(
            503,
            vec![("x-should-retry", "false".into())],
            busy(fmt),
        ));
        let e = run.send("hello").await.unwrap_err().to_string();
        assert!(
            e.starts_with(&format!("{name} could not answer (503)")),
            "{what}: {e}"
        );
        assert_eq!(run.stand.requests().len(), 10, "{what}");
        // Other refusals, in plain words that name the service.
        let model = run.chat.model().0.to_string();
        for (code, kind, message, expect) in [
            (403, "permission_error", "Your key does not have access", format!("{name} says the key may not use the model {model} (Your key does not have access)")),
            (404, "not_found_error", "The model does not exist or you do not have access to it", format!("{name} does not know the model {model}")),
            (404, "not_found_error", "Unknown path", format!("{name} has nothing at the address Columbus was given for it")),
            (418, "odd", "I'm a teapot", format!("{name} answered 418 (I'm a teapot)")),
        ] {
            run.stand.push(Next::Status(code, vec![], error_body(fmt, kind, message)));
            let e = run.send("hello").await.unwrap_err().to_string();
            assert!(e.starts_with(&expect), "{what}: {e}");
        }
        run.close().await;
    }
}

#[tokio::test]
async fn a_redirect_is_never_followed_in_any_format() {
    let Some(py) = python() else { return };
    for (n, fmt) in ALL.into_iter().enumerate() {
        // Another address, which must never be sent anything.
        let elsewhere = StandIn::start(fmt, false, vec![reply(say("From elsewhere."))]).await;
        let code = [307, 302, 308][n];
        let location = format!(
            "{}{}",
            elsewhere.base.trim_end_matches("/v1"),
            fmt.chat_path()
        );
        let mut run = Run::start(
            &py,
            fmt,
            false,
            vec![Next::Status(code, vec![("location", location)], json!({}))],
        )
        .await;
        let what = run.what();
        let name = run.chat.service().name.clone();
        let e = run.send("hello").await.unwrap_err().to_string();
        assert!(
            e.starts_with(&format!(
                "{name} answered with a redirect to another address. Columbus does not follow it"
            )),
            "{what}: {e}"
        );
        assert_eq!(run.stand.requests().len(), 1, "{what}");
        assert!(
            elsewhere.seen().is_empty(),
            "{what}: nothing went to the other address: {:?}",
            elsewhere.seen()
        );
        assert!(run.ui.texts().is_empty(), "{what}");
        run.close().await;
    }
}

#[tokio::test]
async fn a_refusal_inside_a_stream_is_told_apart_from_a_busy_service() {
    let Some(py) = python() else { return };
    use ai_stand_ins::event;
    for fmt in ALL {
        // Busy, said inside a stream that began well: asked again.
        let busy_stream = match fmt {
            Fmt::Messages => event(
                Some("error"),
                &json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            ),
            Fmt::Responses => {
                event(
                    Some("response.created"),
                    &json!({"type": "response.created", "response": {"status": "in_progress", "output": []}}),
                ) + &event(
                    Some("response.failed"),
                    &json!({"type": "response.failed", "response": {"status": "failed", "output": [],
                        "error": {"code": "server_error", "message": "The server had an error while processing your request."}}}),
                )
            }
            Fmt::Chat => {
                event(
                    None,
                    &json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": "Let"}, "finish_reason": null}]}),
                ) + &event(
                    None,
                    &json!({"error": {"code": 503, "message": "The upstream model is overloaded"}, "choices": []}),
                )
            }
        };
        // A refusal that asking again will not change.
        let refusal_stream = match fmt {
            Fmt::Messages => event(
                Some("error"),
                &json!({"type": "error", "error": {"type": "invalid_request_error", "message": format!("Bad request for key {KEY}")}}),
            ),
            Fmt::Responses => event(
                Some("error"),
                &json!({"type": "error", "code": "invalid_prompt", "message": format!("Bad request for key {KEY}"), "param": null}),
            ),
            Fmt::Chat => event(
                None,
                &json!({"error": {"code": 400, "message": format!("Bad request for key {KEY}")}}),
            ),
        };
        let mut run = Run::start(
            &py,
            fmt,
            true,
            vec![Next::Stream(busy_stream), reply(say("Here."))],
        )
        .await;
        let what = run.what();
        let name = run.chat.service().name.clone();
        run.send("hello").await.unwrap();
        assert_eq!(
            run.ui.statuses(),
            [format!("{name} is busy; trying again…")],
            "{what}"
        );
        assert_eq!(run.stand.requests().len(), 2, "{what}");
        assert_eq!(
            run.ui.texts(),
            ["Here."],
            "{what}: nothing of the failed stream is shown or kept"
        );
        run.stand.push(Next::Stream(refusal_stream));
        let e = run.send("hello").await.unwrap_err().to_string();
        assert_eq!(run.stand.requests().len(), 3, "{what}: not asked again");
        match fmt {
            // (This format's stream is read as before, in the words it had.)
            Fmt::Messages => assert_eq!(e, "the AI service answered (invalid_request_error): Bad request for key [key removed]", "{what}"),
            _ => assert_eq!(e, format!("{name} answered with an error: Bad request for key [key removed]"), "{what}"),
        }
        assert_no_key(&run.ui.shown(), &what);
        // An error in place of a reply, with a status that says all is well
        // (OpenRouter answers so): the service's refusal all the same.
        if fmt != Fmt::Messages {
            run.stand.push(Next::Raw(
                json!({"error": {"message": "No endpoints found for this model", "code": 404}}),
            ));
            let e = run.send("hello").await.unwrap_err().to_string();
            assert_eq!(
                e,
                format!("{name} answered with an error: No endpoints found for this model"),
                "{what}"
            );
        }
        run.close().await;
    }
}

/// The fields each format can do without, as its requests carry them.
fn optional_fields(fmt: Fmt) -> &'static [&'static str] {
    match fmt {
        Fmt::Messages => &["output_config"],
        Fmt::Responses => &["include", "max_output_tokens", "reasoning"],
        Fmt::Chat => &["stream_options", "max_completion_tokens"],
    }
}

#[tokio::test]
async fn a_request_refused_for_an_optional_field_is_tried_once_more_without_them() {
    let Some(py) = python() else { return };
    for fmt in ALL {
        let refuse =
            |m: &str| Next::Status(400, vec![], error_body(fmt, "invalid_request_error", m));
        let (service, model) = known(fmt);
        // A thinking level, where the service has them.
        let effort = if service.effort_ok("high") {
            "high"
        } else {
            ""
        };
        let mut run = Run::start(
            &py,
            fmt,
            false,
            vec![
                reply(call("o1", "whoami", json!({})).thinking()),
                refuse("Unknown parameter: the service does not take this field."),
                reply(say("Fine.")),
                reply(say("Again.")),
                refuse("This request is not valid for another reason."),
            ],
        )
        .await;
        let what = run.what();
        run.chat.set_model(model, effort).unwrap();
        let name = run.chat.service().name.clone();
        run.send("hello").await.unwrap();
        let reqs = run.stand.requests();
        assert_eq!(reqs.len(), 3, "{what}");
        let carried = |r: &Value| {
            optional_fields(fmt)
                .iter()
                .filter(|k| r.get(**k).is_some())
                .count()
        };
        assert_eq!(
            carried(&reqs[0]),
            optional_fields(fmt).len(),
            "{what}: {}",
            reqs[0]
        );
        assert_eq!(carried(&reqs[1]), optional_fields(fmt).len(), "{what}");
        // The second try: the same conversation, without them.
        assert_eq!(carried(&reqs[2]), 0, "{what}: {}", reqs[2]);
        assert_eq!(reqs[2]["model"], reqs[1]["model"], "{what}");
        assert_eq!(reqs[2]["stream"], true, "{what}");
        if fmt == Fmt::Responses {
            assert_eq!(
                reqs[2]["store"], false,
                "{what}: still nothing kept at the service"
            );
        }
        let said = run.ui.statuses();
        assert_eq!(said.len(), 1, "{what}: {said:?}");
        assert!(
            said[0].starts_with(&format!(
                "{name} refused a setting Columbus sends by choice"
            )) && said[0].contains("This chat goes on without those settings"),
            "{what}: {said:?}"
        );
        assert_eq!(
            run.ui.texts().last().map(String::as_str),
            Some("Fine."),
            "{what}"
        );
        // It stays that way for the chat.
        run.send("more").await.unwrap();
        run.assert_well_formed();
        assert_eq!(carried(&run.stand.requests()[3]), 0, "{what}");
        // Never more than once: a later refusal is reported as it is.
        let e = run.send("more").await.unwrap_err().to_string();
        assert_eq!(
            e,
            format!("{name} refused the request (This request is not valid for another reason.)"),
            "{what}"
        );
        assert_eq!(run.stand.requests().len(), 5, "{what}");
        run.close().await;

        // The second try is refused too: its answer is the one reported,
        // and the fields were not what the service minded, so they stay.
        let mut run = Run::start(
            &py,
            fmt,
            false,
            vec![
                refuse("The conversation is too long."),
                refuse("The conversation is still too long."),
                reply(say("Shorter is fine.")),
                refuse("No."),
            ],
        )
        .await;
        run.chat.set_model(model, effort).unwrap();
        let e = run.send("hello").await.unwrap_err().to_string();
        assert_eq!(
            e,
            format!("{name} refused the request (The conversation is still too long.)"),
            "{what}"
        );
        assert!(run.ui.statuses().is_empty(), "{what}");
        run.send("hello").await.unwrap();
        let reqs = run.stand.requests();
        assert_eq!(reqs.len(), 3, "{what}");
        assert_eq!(carried(&reqs[1]), 0, "{what}");
        assert_eq!(
            carried(&reqs[2]),
            optional_fields(fmt).len(),
            "{what}: they are sent again"
        );
        let e = run.send("hello").await.unwrap_err().to_string();
        assert_eq!(e, format!("{name} refused the request (No.)"), "{what}");
        assert_eq!(
            run.stand.requests().len(),
            4,
            "{what}: no second try a second time"
        );
        run.close().await;
    }
}

#[tokio::test]
async fn each_message_uses_the_chats_model_and_thinking_level_in_the_new_formats() {
    let Some(py) = python() else { return };
    // OpenAI (Responses): reasoning goes back as it came while the model
    // is the same, and leaves with the model.
    let mut run = Run::start(
        &py,
        Fmt::Responses,
        true,
        vec![
            reply(call("m1", "whoami", json!({})).thinking()),
            reply(say("First.").thinking()),
            reply(say("Second.")),
            reply(say("Third.")),
        ],
    )
    .await;
    run.send("one").await.unwrap();
    let reqs = run.stand.requests();
    assert_eq!(reqs[0]["model"], "gpt-6.1-sol");
    assert!(
        reqs[0].get("reasoning").is_none(),
        "the model's own level: {}",
        reqs[0]
    );
    assert_eq!(reqs[0]["include"], json!(["reasoning.encrypted_content"]));
    let kinds = |r: &Value| -> Vec<String> {
        conversation(Fmt::Responses, r)
            .iter()
            .map(|i| i["type"].as_str().unwrap_or("user").to_string())
            .collect()
    };
    assert_eq!(
        kinds(&reqs[1]),
        [
            "user",
            "reasoning",
            "message",
            "function_call",
            "function_call_output"
        ]
    );
    run.send("two").await.unwrap();
    let third = run.stand.requests().pop().unwrap();
    assert_eq!(
        third
            .to_string()
            .matches("gAAAAB-encrypted-reasoning")
            .count(),
        2,
        "both replies' reasoning went back"
    );
    // Only models and levels this service takes; a typed id is one.
    assert!(run.chat.set_model("gpt 9", "").is_err());
    assert!(
        run.chat.set_model("gpt-6-astra", "xhigh").is_err(),
        "not a level OpenAI is offered with"
    );
    run.chat.set_model("gpt-6-astra", "high").unwrap();
    run.send("three").await.unwrap();
    run.assert_well_formed();
    let fourth = run.stand.requests().pop().unwrap();
    assert_eq!(fourth["model"], "gpt-6-astra");
    assert_eq!(fourth["reasoning"], json!({"effort": "high"}));
    let conv = conversation(Fmt::Responses, &fourth);
    assert!(
        conv.iter().all(|i| i["type"] != "reasoning"),
        "the other model's reasoning is not sent: {conv:?}"
    );
    assert!(
        conv.iter().all(|i| i.get("id").is_none()),
        "nothing points at an item that is gone: {conv:?}"
    );
    let sent = fourth.to_string();
    assert!(
        sent.contains("First.") && sent.contains("Second.") && sent.contains("\"m1\""),
        "its answers and calls are: {sent}"
    );
    run.chat.set_model("gpt-7-not-listed-yet", "").unwrap();
    run.close().await;

    // Gemini (Chat Completions): the model changes, the conversation stays.
    let mut run = Run::start(
        &py,
        Fmt::Chat,
        true,
        vec![reply(say("First.")), reply(say("Second."))],
    )
    .await;
    run.send("one").await.unwrap();
    assert!(
        run.chat.set_model("gemini-4-argon", "high").is_err(),
        "Columbus sets no level for this service"
    );
    run.chat.set_model("gemini-4-argon", "").unwrap();
    run.send("two").await.unwrap();
    run.assert_well_formed();
    let reqs = run.stand.requests();
    assert_eq!(reqs[0]["model"], "gemini-3.8-flash");
    assert_eq!(reqs[1]["model"], "gemini-4-argon");
    assert!(reqs[1].get("reasoning_effort").is_none());
    assert!(reqs[1].to_string().contains("First."));
    run.close().await;
}

#[tokio::test]
async fn a_key_goes_only_where_the_address_rule_allows_and_in_the_form_the_service_takes() {
    let Some(py) = python() else { return };
    let (_dir, root) = setup(&py);
    // Plain HTTP to a host that is not this computer, a user name in the
    // address, a key in the address: refused before anything is started
    // or sent.
    for (address, why) in [
        ("http://example.org/v1", "https://"),
        ("http://192.168.7.20:11434/v1", "is not this computer"),
        (
            "https://user:secret@example.org/v1",
            "user name or a password",
        ),
        ("https://example.org/v1?api-key=abc", "nothing after a ?"),
        ("ftp://example.org", "https://"),
        ("", "address"),
    ] {
        for fmt in ALL {
            let e = Chat::start_with(
                config(&root, &py, address, "any-model"),
                own(fmt, KeyHeader::Bearer, false),
                TestUi::new(Answer::Decline),
                Cancel::default(),
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("{address} was taken"))
            .to_string();
            assert!(
                e.starts_with("My gateway: ") && e.contains(why),
                "{address}: {e}"
            );
        }
    }
    assert!(
        !root.join("workspaces").exists(),
        "no session was started for a refused address"
    );

    // The key rides in the one header the service was set up with.
    for fmt in ALL {
        for (header, name, value) in [
            (KeyHeader::Bearer, "authorization", format!("Bearer {KEY}")),
            (KeyHeader::ApiKey, "api-key", KEY.to_string()),
            (KeyHeader::XApiKey, "x-api-key", KEY.to_string()),
        ] {
            let mut run = Run::start_as(
                &py,
                fmt,
                false,
                vec![reply(say("Hi."))],
                own(fmt, header, false),
                "deployment-1",
                Answer::Decline,
            )
            .await;
            run.send("hello").await.unwrap();
            let seen = run.stand.seen();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].path, fmt.chat_path());
            for h in ["authorization", "api-key", "x-api-key"] {
                assert_eq!(
                    seen[0].header(h),
                    (h == name).then_some(value.as_str()),
                    "{}: {h}",
                    fmt.name()
                );
            }
            assert_eq!(seen[0].body["model"], "deployment-1");
            run.close().await;
        }
    }

    // A service that takes no key is sent none, even if one is at hand;
    // one that may take a key is sent none when none is saved.
    let ollama = builtin("ollama").unwrap().service();
    let mut run = Run::start_as(
        &py,
        Fmt::Chat,
        true,
        vec![reply(say("Hi."))],
        ollama,
        "llama3.3:70b",
        Answer::Decline,
    )
    .await;
    run.send("hello").await.unwrap();
    let seen = run.stand.seen();
    for h in ["authorization", "api-key", "x-api-key"] {
        assert_eq!(seen[0].header(h), None, "{h}");
    }
    assert_eq!(seen[0].body["model"], "llama3.3:70b");
    run.close().await;
    let stand = StandIn::start(Fmt::Chat, false, vec![reply(say("Hi."))]).await;
    let mut cfg = config(&root, &py, &stand.base, "local-model");
    cfg.api_key = String::new();
    let ui = TestUi::new(Answer::Decline);
    let mut chat = Chat::start_with(
        cfg.clone(),
        builtin("lmstudio").unwrap().service(),
        ui,
        Cancel::default(),
    )
    .await
    .unwrap();
    chat.send("hello", &Cancel::default()).await.unwrap();
    assert_eq!(stand.seen()[0].header("authorization"), None);
    chat.close().await;
    // One that needs a key does not start without it.
    let e = Chat::start_with(
        cfg,
        builtin("openai").unwrap().service(),
        TestUi::new(Answer::Decline),
        Cancel::default(),
    )
    .await
    .err()
    .unwrap()
    .to_string();
    assert_eq!(e, "no API key is set");
}

#[tokio::test]
async fn a_services_own_list_of_models_is_taken_as_data() {
    // (No session is needed for this.)
    for fmt in ALL {
        let stand = StandIn::start(fmt, false, vec![]).await;
        let (service, _) = known(fmt);
        let got = columbus_chat::list_models(&stand.base, KEY, &service)
            .await
            .unwrap();
        assert_eq!(got, ["model-a", "model-b"], "{}", fmt.name());
        let seen = stand.seen();
        assert_eq!(
            (seen[0].method.as_str(), seen[0].path.as_str()),
            ("GET", "/v1/models")
        );
        match fmt {
            Fmt::Messages => assert_eq!(seen[0].header("x-api-key"), Some(KEY)),
            _ => assert_eq!(
                seen[0].header("authorization"),
                Some(format!("Bearer {KEY}").as_str())
            ),
        }
        // Ids are data from outside: only plain ones are taken, each once,
        // and no more than the cap.
        let long = "m".repeat(201);
        let mut data = vec![
            json!({"id": "models/gemini-3.8-flash"}),
            json!({"id": "llama3.3:70b-instruct-q4_K_M"}),
            json!({"id": "<img src=x onerror=alert(1)>"}),
            json!({"id": "two words"}),
            json!({"id": long}),
            json!({"id": 7}),
            json!({"name": "by-name"}),
            json!("a-bare-string"),
            json!({"id": "models/gemini-3.8-flash"}),
            json!(null),
        ];
        data.extend((0..2000).map(|n| json!({"id": format!("model-{n}")})));
        stand.inner.lock().unwrap().models = json!({"object": "list", "data": data});
        let got = columbus_chat::list_models(&stand.base, KEY, &service)
            .await
            .unwrap();
        assert_eq!(got.len(), columbus_chat::MAX_LISTED_MODELS);
        assert_eq!(
            &got[..4],
            [
                "models/gemini-3.8-flash",
                "llama3.3:70b-instruct-q4_K_M",
                "by-name",
                "a-bare-string"
            ]
        );
        assert!(got.iter().all(|id| services::model_id_ok(id)));
        // Nothing usable, and an answer that is not a list.
        stand.inner.lock().unwrap().models = json!({"data": [{"id": "not ok"}]});
        let e = columbus_chat::list_models(&stand.base, KEY, &service)
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            format!("{} listed no models Columbus can use", service.name)
        );
        // An address the rule refuses is never asked.
        let e = columbus_chat::list_models("http://example.org/v1", KEY, &service)
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("https://"), "{e}");
    }
}
