//! End to end: start the real `columbus-mcp` binary as an MCP client would,
//! with a policy, a GeoPackage connection and the Python worker, and check
//! that the rules hold over the wire.
//!
//! Needs `python3` (or `python` on Windows) on PATH; skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python() -> Option<String> {
    for p in ["python3", "python"] {
        if StdCommand::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some(p.into());
        }
    }
    None
}

/// A small GeoPackage-shaped SQLite file: 1,200 road segments across three
/// districts (600 in district 14), a field the policy hides, and an audit
/// table the connection denies.
fn make_fixture(py: &str, path: &Path) {
    let script = r#"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.executescript('''
CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT,
  last_change TEXT, min_x REAL, min_y REAL, max_x REAL, max_y REAL, srs_id INTEGER);
CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT,
  srs_id INTEGER, z INTEGER, m INTEGER);
CREATE TABLE roads (fid INTEGER PRIMARY KEY, route_id TEXT, district INTEGER, csj TEXT, owner_ssn TEXT, geom BLOB);
CREATE TABLE roads_audit (id INTEGER PRIMARY KEY, who TEXT);
INSERT INTO gpkg_contents VALUES ('roads','features','Roads','Test roads','2026-11-03',-106.6,25.8,-93.5,36.5,3081);
INSERT INTO gpkg_geometry_columns VALUES ('roads','geom','LINESTRING',3081,0,0);
INSERT INTO roads_audit (who) VALUES ('someone');
''')
rows = []
for i in range(1200):
    district = 14 if i % 2 == 0 else (12 if i % 4 == 1 else 15)
    rows.append((f"IH{i:04d}", district, f"{i:09d}", "123-45-6789", b"GP\x00\x01" + b"\x00" * 20))
db.executemany("INSERT INTO roads (route_id, district, csj, owner_ssn, geom) VALUES (?,?,?,?,?)", rows)
db.commit()
"#;
    let ok = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(path)
        .status()
        .unwrap();
    assert!(ok.success(), "fixture script failed");
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(py: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let gpkg = root.join("roads.gpkg");
    make_fixture(py, &gpkg);
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nhide_fields: [\"*ssn*\"]\nlimits: { rows: 1000, seconds: 30 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("roads.yaml"),
        format!(
            "id: roads-gpkg\ntype: gpkg\npath: {}\ndescription: Test roads\nmode: read-write\n\
             datasets:\n  allow: [\"roads\"]\n  deny: [\"*audit*\"]\nrow_filter: \"district = 14\"\n",
            serde_json::to_string(&gpkg.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    // A second connection of a type the policy does not allow.
    std::fs::write(
        root.join("connections").join("hr.yaml"),
        "id: hr-prod\ntype: oracle\ndsn: HRPROD\ndatasets:\n  allow: [\"HR.*\"]\n",
    )
    .unwrap();
    Env { _dir: dir, root }
}

async fn start(env: &Env, py: &str) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = env.root.clone();
    let py = py.to_string();
    let transport = TokioChildProcess::new(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
        }),
    )
    .unwrap();
    ().serve(transport).await.unwrap()
}

async fn call(
    client: &RunningService<RoleClient, ()>,
    tool: &'static str,
    args: Value,
) -> CallToolResult {
    let mut params = CallToolRequestParams::new(tool);
    if let Value::Object(m) = args {
        params = params.with_arguments(m);
    }
    client.call_tool(params).await.unwrap()
}

fn structured(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().expect("structured output")
}

fn error_text(r: &CallToolResult) -> String {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected an error, got {:?}",
        r.structured_content
    );
    serde_json::to_string(&r.content).unwrap()
}

#[tokio::test]
async fn governed_session_end_to_end() {
    let Some(py) = python() else {
        eprintln!("skipping: no python on PATH");
        return;
    };
    let env = setup(&py);
    let client = start(&env, &py).await;

    // Tool list.
    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for t in [
        "whoami",
        "search_tools",
        "describe_tool",
        "list_connections",
        "describe_connection",
        "list_datasets",
        "sql_select",
        "approval_status",
    ] {
        assert!(names.contains(&t), "missing tool {t}");
    }
    assert!(
        tools.iter().all(|t| t.output_schema.is_some()),
        "every tool has an output schema"
    );

    // whoami: policy found, edits off so the read-write connection is read.
    let who = structured(&call(&client, "whoami", json!({})).await);
    let r = &who["result"];
    assert_eq!(r["policy"]["source"]["found"], true);
    assert_eq!(r["policy"]["edits_allowed"], false);
    assert_eq!(r["policy"]["rows_read_per_query"], 1000);
    let conns = r["connections"].as_array().unwrap();
    let roads = conns.iter().find(|c| c["id"] == "roads-gpkg").unwrap();
    assert_eq!(roads["mode"], "read");
    assert_eq!(roads["status"], "ok");
    let hr = conns.iter().find(|c| c["id"] == "hr-prod").unwrap();
    assert!(hr["status"].as_str().unwrap().contains("not allowed"));
    assert!(
        r["engines"]["engines"]["sqlite"].is_string(),
        "worker answered ping: {}",
        r["engines"]
    );
    let workspace = PathBuf::from(r["workspace"].as_str().unwrap());

    // search_tools finds the query tool.
    let found = structured(
        &call(
            &client,
            "search_tools",
            json!({"query": "run a SELECT query"}),
        )
        .await,
    );
    assert_eq!(found["result"][0]["name"], "sql_select");

    // list_datasets: only allowed tables, hidden fields removed.
    let ds = structured(
        &call(
            &client,
            "list_datasets",
            json!({"connection": "roads-gpkg"}),
        )
        .await,
    );
    let datasets = ds["result"]["datasets"].as_array().unwrap();
    assert_eq!(datasets.len(), 1);
    assert_eq!(datasets[0]["name"], "roads");
    assert_eq!(datasets[0]["srs_id"], 3081);
    let fields: Vec<&str> = datasets[0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(!fields.contains(&"owner_ssn"));
    assert!(ds["result"]["not_allowed"].as_u64().unwrap() >= 1);

    // A hidden field cannot be named anywhere in a query: not selected, not
    // renamed, not filtered on, not sorted by, not read as part of a whole row.
    for sql in [
        "SELECT route_id, owner_ssn FROM roads",
        "SELECT route_id, owner_ssn AS x FROM roads",
        "SELECT route_id FROM roads WHERE owner_ssn LIKE '1%'",
        "SELECT route_id FROM roads ORDER BY OWNER_SSN",
        "SELECT route_id FROM roads r WHERE r.\"owner_ssn\" IS NULL",
        "SELECT * FROM roads ORDER BY 5",
        "SELECT x FROM (SELECT owner_ssn AS x FROM roads) AS t",
    ] {
        let r = call(
            &client,
            "sql_select",
            json!({"connection": "roads-gpkg", "sql": sql}),
        )
        .await;
        assert_eq!(r.is_error, Some(true), "{sql} should be refused");
        let text = serde_json::to_string(&r.content).unwrap();
        assert!(
            text.contains("hidden") || text.contains("position"),
            "{sql}: {text}"
        );
    }

    // sql_select: row filter applied by the server, hidden field stripped
    // from SELECT *, 600 rows read, 500 returned, full result saved in the
    // workspace.
    let q = structured(
        &call(
            &client,
            "sql_select",
            json!({"connection": "roads-gpkg", "sql": "SELECT * FROM roads"}),
        )
        .await,
    );
    let res = &q["result"];
    assert_eq!(res["rows_read"], 600);
    assert_eq!(res["rows_returned"], 500);
    assert_eq!(
        res["columns"],
        json!(["fid", "route_id", "district", "csj", "geom"])
    );
    assert!(res["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row[2] == 14));
    let file = PathBuf::from(res["workspace_file"].as_str().unwrap());
    assert!(file.starts_with(workspace.canonicalize().unwrap()) && file.exists());
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(
        !saved.contains("123-45-6789") && !saved.contains("owner_ssn"),
        "hidden fields never reach the workspace file, which can be sent on as a handle"
    );
    let ids: Vec<&str> = q["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"query.row_filter") && ids.contains(&"policy.hidden_fields"));
    let prov = q["provenance"].as_str().unwrap();
    assert!(workspace
        .join("provenance")
        .join(format!("{prov}.prov.json"))
        .exists());

    // Read limit: 1,000 rows from policy. A query that reads past it is cut and flagged.
    // (With the row filter, only 600 rows exist; a self-join produces more.)
    let big = structured(
        &call(
            &client,
            "sql_select",
            json!({"connection": "roads-gpkg", "sql": "SELECT a.route_id FROM roads a JOIN roads b ON a.district = b.district"}),
        )
        .await,
    );
    assert_eq!(big["result"]["rows_read"], 1000);
    assert!(big["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "query.read_limit"));

    // Refusals.
    for (sql, expect) in [
        (
            "SELECT * FROM roads_audit",
            "not in this connection's allowed datasets",
        ),
        ("DELETE FROM roads", "only SELECT"),
        (
            "SELECT 1 FROM roads; DROP TABLE roads",
            "exactly one statement",
        ),
        ("SELECT load_extension('x') FROM roads", "not allowed"),
    ] {
        let e = error_text(
            &call(
                &client,
                "sql_select",
                json!({"connection": "roads-gpkg", "sql": sql}),
            )
            .await,
        );
        assert!(e.contains(expect), "{sql}: {e}");
    }
    let e = error_text(
        &call(
            &client,
            "sql_select",
            json!({"connection": "hr-prod", "sql": "SELECT * FROM HR.EMPLOYEES"}),
        )
        .await,
    );
    assert!(e.contains("not allowed by organization policy"), "{e}");
    let e = error_text(
        &call(
            &client,
            "describe_connection",
            json!({"connection": "nope"}),
        )
        .await,
    );
    assert!(e.contains("no connection named"), "{e}");

    // The agent cannot approve anything: unknown ids report unknown.
    let ap = structured(&call(&client, "approval_status", json!({"id": "ap_123"})).await);
    assert_eq!(ap["result"]["state"], "unknown");

    client.cancel().await.unwrap();

    // Audit log: one line per call, chain intact, refusals recorded.
    let audit_dir = env.root.join("audit");
    let file = std::fs::read_dir(&audit_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let n = columbus_shared::audit::verify(&file).expect("audit chain intact");
    assert_eq!(n, 19, "one audit line per tool call");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"decision\":\"denied\""));
    assert!(
        !text.contains("123-45-6789"),
        "row data never reaches the audit log"
    );
}
