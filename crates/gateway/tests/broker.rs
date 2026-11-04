//! Connected MCP servers, end to end (design change 1). The gateway runs as
//! a real binary; the connected server is `examples/fake_mcp_server.rs`.
//!
//! Milestone 1 gates covered here: a changed tool description is disabled
//! automatically; a poisoned description is flagged and stays off. Also:
//! file handles (Rule 2), flow labels and organization flow rules (Rule 3),
//! untrusted output (Rule 4), approvals asked of the person in their own
//! client, and the fallback when the client cannot ask.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use columbus_broker::registry::Broker;
use columbus_broker::review::{self, FlagKind, ToolState};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ElicitRequestParams,
    ElicitResult, ElicitationAction, ElicitationCapability, FormElicitationCapability,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde_json::{json, Value};

// ---------- fixtures ----------

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn fake_server() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = if cfg!(windows) {
        "fake_mcp_server.exe"
    } else {
        "fake_mcp_server"
    };
    let p = dir.join(name);
    assert!(
        p.exists(),
        "build the example first: cargo build --example fake_mcp_server ({})",
        p.display()
    );
    p
}

fn make_gpkg(py: &str, path: &Path) {
    let script = r#"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("CREATE TABLE roads (fid INTEGER PRIMARY KEY, route_id TEXT, district INTEGER)")
db.executemany("INSERT INTO roads (route_id, district) VALUES (?, ?)", [(f"IH{i:04d}", 14 if i % 2 == 0 else 12) for i in range(40)])
db.commit()
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(path)
        .status()
        .unwrap()
        .success());
}

const RULES: &str = "tools:
  search_notes:  { risk: read }
  upload:        { risk: write, egress: true, args: { content: file_handle }, approval: never }
  export_report: { risk: read, output: to_workspace }
  delete_all:    { risk: destructive, approval: never }
  helper:        { risk: read }
  default:       { allow: false }
";

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn manifest(&self, id: &str, variant: &str) -> PathBuf {
        let path = self.root.join("servers").join(format!("{id}.yaml"));
        std::fs::write(
            &path,
            format!(
                "id: {id}\nname: Test vault {id}\ntransport: {{ type: stdio, command: {}, args: [{variant}] }}\ntrust: standard\n{RULES}",
                serde_json::to_string(&fake_server().display().to_string()).unwrap()
            ),
        )
        .unwrap();
        path
    }

    fn policy(&self) -> columbus_shared::policy::OrgPolicy {
        columbus_shared::policy::OrgPolicy::load(&self.root.join("policy.yaml")).unwrap()
    }

    /// What `columbus-mcp servers approve` does, minus the terminal prompt.
    async fn approve(&self, id: &str) {
        let ws = self.root.join("review");
        std::fs::create_dir_all(&ws).unwrap();
        let b = Broker::load(
            &self.policy(),
            &self.root.join("servers"),
            &self.root.join("personal"),
            &ws,
            Arc::new(|_: &str| Ok(None)),
            &[],
        )
        .unwrap();
        let p = b.get(id).unwrap().clone();
        let c = b.connected(&p).await.map_err(|s| format!("{s:?}")).unwrap();
        let pinned = review::pin(&c.review, None);
        let path = self.root.join("servers").join(format!("{id}.yaml"));
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&serde_yaml::to_string(&json!({ "pinned": pinned })).unwrap());
        std::fs::write(&path, text).unwrap();
    }

    /// Point a pinned manifest at another variant of the server.
    fn switch_variant(&self, id: &str, from: &str, to: &str) {
        let path = self.root.join("servers").join(format!("{id}.yaml"));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            text.replace(&format!("args: [{from}]"), &format!("args: [{to}]")),
        )
        .unwrap();
    }
}

fn setup(py: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in ["connections", "servers", "personal"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let gpkg = root.join("roads.gpkg");
    make_gpkg(py, &gpkg);
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nlimits: { rows: 1000, seconds: 30 }\n\
         servers: { allow_unconfined_local: true }\n\
         flows:\n  - { from: [\"connection:roads-gpkg\"], to: [\"server:vault2\"], action: deny }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("roads.yaml"),
        format!(
            "id: roads-gpkg\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"roads\"]\n",
            serde_json::to_string(&gpkg.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
}

// ---------- clients ----------

/// A client whose person answers approval prompts: accepts everything
/// except deletions. Counts how often they were asked.
#[derive(Clone)]
struct Person {
    asked: Arc<AtomicUsize>,
    last: Arc<std::sync::Mutex<String>>,
}

impl ClientHandler for Person {
    fn get_info(&self) -> ClientConfig {
        let mut c = ClientConfig::default();
        let mut caps = ClientCapabilities::default();
        let mut el = ElicitationCapability::default();
        el.form = Some(FormElicitationCapability::default());
        caps.elicitation = Some(el);
        c.capabilities = caps;
        c
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let message = match &request {
            ElicitRequestParams::FormElicitationParams { message, .. } => message.clone(),
            _ => String::new(),
        };
        *self.last.lock().unwrap() = message.clone();
        Ok(if message.contains("delete_all") {
            ElicitResult::new(ElicitationAction::Decline)
        } else {
            ElicitResult::new(ElicitationAction::Accept)
                .with_content(json!({"decision": "allow_once"}))
        })
    }
}

fn gateway_cmd(env: &Env, py: &str) -> tokio::process::Command {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = env.root.clone();
    let py = py.to_string();
    tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
        cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
            .env("COLUMBUS_CONNECTIONS", root.join("connections"))
            .env("COLUMBUS_SERVERS", root.join("servers"))
            .env("COLUMBUS_PERSONAL_SERVERS", root.join("personal"))
            .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
            .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
            .env("COLUMBUS_WORKER", &worker)
            .env("COLUMBUS_PYTHON", &py)
            .env("COLUMBUS_LOG", "warn");
    })
}

async fn call<H: ClientHandler>(
    c: &RunningService<RoleClient, H>,
    tool: &'static str,
    args: Value,
) -> CallToolResult {
    let mut p = CallToolRequestParams::new(tool);
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    c.call_tool(p).await.unwrap()
}

fn ok(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().expect("structured output")
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

fn checksum(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ *b as u64).wrapping_mul(0x100000001b3)
        })
        .to_string()
}

// ---------- tests ----------

#[tokio::test]
async fn unreviewed_servers_enable_nothing() {
    let Some(py) = python() else { return };
    let env = setup(&py);
    env.manifest("vault", "plain");
    let client = ().serve(TokioChildProcess::new(gateway_cmd(&env, &py)).unwrap()).await.unwrap();

    let lp = ok(&call(&client, "list_providers", json!({})).await);
    let vault = lp["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "vault")
        .unwrap()
        .clone();
    assert_eq!(vault["status"]["state"], "trial");
    assert_eq!(vault["enabled_tools"], 0);

    let found = ok(&call(
        &client,
        "search_tools",
        json!({"query": "upload", "provider": "vault"}),
    )
    .await);
    assert_eq!(
        found["result"],
        json!([]),
        "unreviewed tools are not discoverable"
    );
    let e = err(&call(
        &client,
        "run_tool",
        json!({"name": "vault.search_notes", "args": {"query": "x"}}),
    )
    .await);
    assert!(e.contains("has not been reviewed"), "{e}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn governed_session_with_a_person_answering() {
    let Some(py) = python() else { return };
    let env = setup(&py);
    env.manifest("vault", "plain");
    env.manifest("vault2", "plain");
    env.approve("vault").await;
    env.approve("vault2").await;

    let person = Person {
        asked: Arc::new(AtomicUsize::new(0)),
        last: Arc::new(Default::default()),
    };
    let client = person
        .clone()
        .serve(TokioChildProcess::new(gateway_cmd(&env, &py)).unwrap())
        .await
        .unwrap();

    // Discovery: namespaced, with the reviewer's risk, never the server's claims.
    let found = ok(&call(
        &client,
        "search_tools",
        json!({"query": "upload a file", "provider": "vault"}),
    )
    .await);
    let upload = found["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "vault.upload")
        .unwrap()
        .clone();
    assert_eq!(
        upload["risk"], "write",
        "the server's readOnlyHint is ignored"
    );
    assert_eq!(upload["approval"], "if_labelled");
    assert!(found["result"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["name"] != "vault.helper"));
    let d = ok(&call(&client, "describe_tool", json!({"name": "vault.upload"})).await);
    assert_eq!(d["result"]["read_only"], false);
    assert_eq!(d["result"]["argument_roles"]["content"], "file_handle");
    assert!(d["result"]["reviewed_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));

    // Rule 4: results are wrapped as untrusted data.
    let r = ok(&call(
        &client,
        "run_tool",
        json!({"name": "vault.search_notes", "args": {"query": "field"}}),
    )
    .await);
    let ext = &r["result"]["external"];
    assert_eq!(ext["untrusted"], true);
    assert!(
        ext["content"]
            .to_string()
            .contains("IGNORE PREVIOUS INSTRUCTIONS"),
        "returned, but as data"
    );
    assert!(r["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "provider.untrusted_output"));

    // Rule 2: file arguments take workspace handles only, not pasted text.
    let e = err(&call(
        &client,
        "run_tool",
        json!({"name": "vault.upload", "args": {"name": "a", "content": "pasted rows"}}),
    )
    .await);
    assert!(e.contains("not a workspace handle"), "{e}");

    // Output to the workspace: a handle back, not the content.
    let r = ok(&call(&client, "run_tool", json!({"name": "vault.export_report"})).await);
    let saved = r["result"]["saved"].as_array().unwrap();
    assert_eq!(saved.len(), 1);
    let report_handle = saved[0]["handle"].as_str().unwrap().to_string();
    assert!(r["result"]["external"].get("content").is_none());

    // No governed data yet: an outbound tool with approval: never runs unasked.
    let r = ok(&call(
        &client,
        "run_tool",
        json!({"name": "vault.upload", "args": {"name": "report.json", "content": report_handle}}),
    )
    .await);
    assert_eq!(r["result"]["status"], "done");
    assert_eq!(person.asked.load(Ordering::SeqCst), 0);

    // Read governed data: the session is labelled and the file carries it.
    let q = ok(&call(
        &client,
        "sql_select",
        json!({"connection": "roads-gpkg", "sql": "SELECT route_id, district FROM roads"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], 40);
    let who = ok(&call(&client, "whoami", json!({})).await);
    assert_eq!(who["result"]["labels"], json!(["connection:roads-gpkg"]));

    // Rule 3: with a label active, the same upload needs the person's yes,
    // whatever the manifest says, and the bytes arrive exactly.
    let r = ok(&call(
        &client,
        "run_tool",
        json!({"name": "vault.upload", "args": {"name": "report.json", "content": report_handle}}),
    )
    .await);
    assert_eq!(r["result"]["status"], "done");
    assert_eq!(person.asked.load(Ordering::SeqCst), 1, "asked once");
    let asked = person.last.lock().unwrap().clone();
    assert!(
        asked.contains("Test vault vault") && asked.contains("roads-gpkg"),
        "{asked}"
    );
    let report_path = {
        let lo = ok(&call(&client, "list_outputs", json!({})).await);
        let card = lo["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["handle"] == report_handle.as_str())
            .unwrap()
            .clone();
        assert_eq!(card["made_by"], "vault.export_report");
        let ws = PathBuf::from(who["result"]["workspace"].as_str().unwrap());
        let (_session, rel) = report_handle
            .strip_prefix("ws://")
            .unwrap()
            .split_once('/')
            .unwrap();
        ws.join(rel)
    };
    let bytes = std::fs::read(&report_path).unwrap();
    let stored = &r["result"]["external"]["structured"];
    assert_eq!(stored["bytes"], bytes.len());
    assert_eq!(
        stored["checksum"],
        checksum(&bytes),
        "file arrived byte for byte"
    );

    // An organization flow rule forbids roads-gpkg data reaching vault2.
    let e = err(&call(
        &client,
        "run_tool",
        json!({"name": "vault2.upload", "args": {"name": "r", "content": report_handle}}),
    )
    .await);
    assert!(
        e.contains("flow rule 1 forbids") && e.contains("roads-gpkg"),
        "{e}"
    );

    // Destructive tools always ask, even with approval: never; this person says no.
    let e = err(&call(&client, "run_tool", json!({"name": "vault.delete_all"})).await);
    assert!(e.contains("declined"), "{e}");
    assert_eq!(person.asked.load(Ordering::SeqCst), 2);

    client.cancel().await.unwrap();

    // The audit log records providers and flow decisions, and still verifies.
    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).expect("audit chain intact");
    let text = std::fs::read_to_string(&audit).unwrap();
    assert!(text.contains("\"provider\":\"vault\""));
    assert!(text.contains("\"flow_decision\":\"deny\""));
    assert!(text.contains("\"flow_decision\":\"approve\""));
    assert!(text.contains("\"tools_hash\":\"sha256:"));
}

#[tokio::test]
async fn clients_that_cannot_ask_get_approval_required() {
    let Some(py) = python() else { return };
    let env = setup(&py);
    env.manifest("vault", "plain");
    env.approve("vault").await;
    let client = ().serve(TokioChildProcess::new(gateway_cmd(&env, &py)).unwrap()).await.unwrap();

    let r = ok(&call(&client, "run_tool", json!({"name": "vault.export_report"})).await);
    let handle = r["result"]["saved"][0]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    ok(&call(
        &client,
        "sql_select",
        json!({"connection": "roads-gpkg", "sql": "SELECT * FROM roads"}),
    )
    .await);

    let args = json!({"name": "vault.upload", "args": {"name": "x", "content": handle}});
    let r = ok(&call(&client, "run_tool", args.clone()).await);
    assert_eq!(r["result"]["status"], "approval_required");
    let id = r["result"]["approval"]["id"].as_str().unwrap().to_string();
    assert!(r["result"]["approval"]["summary"]
        .as_str()
        .unwrap()
        .contains("roads-gpkg"));
    let st = ok(&call(&client, "approval_status", json!({"id": id})).await);
    assert_eq!(st["result"]["state"], "pending");

    // The agent cannot approve its own request by repeating it with the id.
    let mut again = args.clone();
    again["approval_id"] = json!(id);
    let e = err(&call(&client, "run_tool", again).await);
    assert!(e.contains("still waiting for a person"), "{e}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn changed_and_poisoned_tools_are_switched_off() {
    let Some(py) = python() else { return };
    let env = setup(&py);
    env.manifest("vault", "plain");
    env.approve("vault").await;

    // Gate: a changed tool description is disabled automatically.
    env.switch_variant("vault", "plain", "changed");
    let client = ().serve(TokioChildProcess::new(gateway_cmd(&env, &py)).unwrap()).await.unwrap();
    let lp = ok(&call(&client, "list_providers", json!({})).await);
    let vault = lp["result"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "vault")
        .unwrap()
        .clone();
    assert_eq!(vault["status"]["state"], "needs_review");
    assert_eq!(vault["status"]["changed"], json!(["search_notes"]));
    let e = err(&call(
        &client,
        "run_tool",
        json!({"name": "vault.search_notes", "args": {"query": "x"}}),
    )
    .await);
    assert!(
        e.contains("definition changed since it was reviewed"),
        "{e}"
    );
    let e = err(&call(
        &client,
        "describe_tool",
        json!({"name": "vault.search_notes"}),
    )
    .await);
    assert!(
        e.contains("changed since it was reviewed"),
        "never describe an unreviewed definition: {e}"
    );
    // Unchanged tools keep working.
    ok(&call(&client, "run_tool", json!({"name": "vault.export_report"})).await);
    client.cancel().await.unwrap();

    // Gate: a poisoned description is flagged, and the new tool stays off.
    env.switch_variant("vault", "changed", "poisoned");
    let ws = env.root.join("review");
    std::fs::create_dir_all(&ws).unwrap();
    let b = Broker::load(
        &env.policy(),
        &env.root.join("servers"),
        &env.root.join("personal"),
        &ws,
        Arc::new(|_: &str| Ok(None)),
        &[],
    )
    .unwrap();
    let p = b.get("vault").unwrap().clone();
    let tools = b.reviewed(&p).await.map_err(|s| format!("{s:?}")).unwrap();
    let helper = tools.iter().find(|t| t.name == "helper").unwrap();
    assert_eq!(helper.state, ToolState::New);
    let kinds: Vec<FlagKind> = helper.flags.iter().map(|f| f.kind).collect();
    assert!(kinds.contains(&FlagKind::ModelInstructions), "{kinds:?}");
    assert!(
        tools
            .iter()
            .filter(|t| t.name != "helper")
            .all(|t| t.flags.is_empty()),
        "clean tools are not flagged"
    );

    let client = ().serve(TokioChildProcess::new(gateway_cmd(&env, &py)).unwrap()).await.unwrap();
    let e = err(&call(
        &client,
        "run_tool",
        json!({"name": "vault.helper", "args": {"text": "x"}}),
    )
    .await);
    assert!(e.contains("appeared after the server was reviewed"), "{e}");
    client.cancel().await.unwrap();
}
