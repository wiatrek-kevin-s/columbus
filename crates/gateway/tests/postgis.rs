//! End to end against a real PostGIS server.
//!
//! Runs only when `COLUMBUS_TEST_PG_HOST` is set and the fixture in
//! `tests/fixtures/postgis.sql` is loaded (CI does both on Linux):
//!
//! ```text
//! COLUMBUS_TEST_PG_HOST=localhost COLUMBUS_TEST_PG_PORT=5432 cargo test --test postgis
//! ```
//!
//! The connection deliberately uses `columbus_rw`, a login that can write,
//! to show that writes are still refused: by the server's checks first, and
//! by the read-only session at the database underneath.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const PASSWORD: &str = "columbus_rw_test";

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", "import psycopg"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(host: &str, port: &str, db: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [postgis]\nhosts: [{host:?}]\nhide_fields: [\"owner_*\"]\n\
             allow_edits: false\nlimits: {{ rows: 1000, seconds: 20 }}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("roads-pg.yaml"),
        format!(
            "id: roads-pg\ntype: postgis\nhost: {host}\nport: {port}\ndatabase: {db}\n\
             user: columbus_rw\nsslmode: disable\nmode: read-write\n\
             datasets:\n  allow: [\"roads.*\", \"*\"]\n  deny: [\"roads.*_audit\"]\n\
             row_filter: \"district_nbr = 14\"\n"
        ),
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
                .env("COLUMBUS_DEV_SECRETS", "1")
                .env("COLUMBUS_SECRET_ROADS_PG", PASSWORD)
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
        }),
    )
    .unwrap();
    ().serve(transport).await.unwrap()
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

#[tokio::test]
async fn postgis_governed_session() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_PG_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_PG_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_PG_PORT").unwrap_or_else(|_| "5432".into());
    let db = std::env::var("COLUMBUS_TEST_PG_DB").unwrap_or_else(|_| "gis".into());
    let py = python().expect("python with psycopg is required when COLUMBUS_TEST_PG_HOST is set");
    let env = setup(&host, &port, &db);
    let client = start(&env, &py).await;

    let who = ok(&call(&client, "whoami", json!({})).await);
    let r = &who["result"];
    assert!(r["secret_store"]
        .as_str()
        .unwrap()
        .starts_with("DEVELOPMENT MODE"));
    assert!(r["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("Development secrets")));
    let conn = &r["connections"][0];
    assert_eq!(conn["status"], "ok");
    assert_eq!(
        conn["mode"], "read",
        "edits are off in policy, so the read-write login is read"
    );
    assert_eq!(conn["engine_ready"], true);
    assert!(
        !serde_json::to_string(&who).unwrap().contains(PASSWORD),
        "never reveal the secret"
    );

    // Datasets: allow list, deny list, system catalogs and database grants all apply.
    let ds = ok(&call(&client, "list_datasets", json!({"connection": "roads-pg"})).await);
    let names: Vec<&str> = ds["result"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"roads.roadway_inventory"), "{names:?}");
    assert!(
        !names.contains(&"roads.roadway_audit"),
        "denied by the connection"
    );
    let inv = ds["result"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == "roads.roadway_inventory")
        .unwrap();
    assert_eq!(inv["srs_id"], 3081);
    let fields: Vec<&str> = inv["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"route_id") && !fields.contains(&"owner_name"),
        "{fields:?}"
    );

    // A query: the server adds the district filter, hides owner_name and
    // summarizes geometry.
    let q = ok(&call(
        &client,
        "sql_select",
        json!({"connection": "roads-pg", "sql": "SELECT * FROM roads.roadway_inventory ORDER BY route_id"}),
    )
    .await);
    let res = &q["result"];
    assert_eq!(res["rows_read"], 600);
    assert_eq!(res["rows_returned"], 500);
    assert_eq!(
        res["columns"],
        json!([
            "fid",
            "route_id",
            "district_nbr",
            "csj",
            "func_class",
            "geom"
        ])
    );
    let rows = res["rows"].as_array().unwrap();
    assert!(rows.iter().all(|row| row[2] == 14));
    assert_eq!(rows[0][5]["geometry"], "ewkb");
    assert!(res["executed_sql"]
        .as_str()
        .unwrap()
        .contains("district_nbr = 14"));
    assert!(res["workspace_file"].is_string());

    // Truncation at the read limit is reported.
    let big = ok(&call(
        &client,
        "sql_select",
        json!({"connection": "roads-pg", "sql": "SELECT a.fid FROM roads.roadway_inventory a CROSS JOIN roads.roadway_inventory b"}),
    )
    .await);
    assert_eq!(big["result"]["rows_read"], 1000);
    assert!(big["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "query.read_limit"));

    // Refusals by the server's checks.
    for (sql, expect) in [
        (
            "SELECT * FROM pg_roles",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM pg_catalog.pg_settings",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM information_schema.tables",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM roads.roadway_audit",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT pg_sleep(30) FROM roads.roadway_inventory",
            "not allowed",
        ),
        (
            "SELECT * FROM dblink('x', 'y') AS t(a int)",
            "are not allowed",
        ),
        ("DELETE FROM roads.roadway_inventory", "only SELECT"),
        // Hidden fields cannot be reached by renaming, filtering, casting
        // (whose error would echo the value) or reading a whole row.
        (
            "SELECT owner_name AS x FROM roads.roadway_inventory",
            "hidden by organization policy",
        ),
        (
            "SELECT fid FROM roads.roadway_inventory WHERE CAST(owner_name AS INTEGER) = 1",
            "hidden by organization policy",
        ),
        (
            "SELECT to_jsonb(r) FROM roads.roadway_inventory r",
            "whole row",
        ),
        (
            "SELECT row_to_json(r.*) FROM roads.roadway_inventory r",
            "name the columns",
        ),
        (
            "SELECT to_jsonb(roadway_inventory) FROM roads.roadway_inventory",
            "whole row",
        ),
    ] {
        let e = err(&call(
            &client,
            "sql_select",
            json!({"connection": "roads-pg", "sql": sql}),
        )
        .await);
        assert!(e.contains(expect), "{sql}: {e}");
    }

    // A SELECT that writes through a function passes the parser (the
    // function name is ordinary) and is stopped by the read-only session.
    let e = err(&call(&client, "sql_select", json!({"connection": "roads-pg", "sql": "SELECT roads.log_visit() FROM roads.roadway_inventory"})).await);
    assert!(e.contains("read-only"), "{e}");

    // Allowed by the server's list ("*"), but the login has no grant: the
    // database's own permissions still apply.
    let e = err(&call(
        &client,
        "sql_select",
        json!({"connection": "roads-pg", "sql": "SELECT * FROM hr.employees"}),
    )
    .await);
    assert!(e.contains("permission denied"), "{e}");
    assert!(!e.contains(PASSWORD));

    client.cancel().await.unwrap();

    let audit = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit).expect("audit chain intact");
    let text = std::fs::read_to_string(&audit).unwrap();
    assert!(
        !text.contains(PASSWORD),
        "the secret never reaches the audit log"
    );
}
