//! End to end against a real Oracle database.
//!
//! Runs only when `COLUMBUS_TEST_ORA_HOST` is set and the fixture in
//! `tests/fixtures/oracle.sql` is loaded (CI does both on Linux):
//!
//! ```text
//! COLUMBUS_TEST_ORA_HOST=localhost COLUMBUS_TEST_ORA_PORT=1521 \
//! COLUMBUS_TEST_ORA_SERVICE=FREEPDB1 cargo test --test oracle
//! ```
//!
//! The connection deliberately uses `columbus_rw`, a login that can write,
//! to show that writes are still refused: by the server's checks first, and
//! by the read-only session at the database underneath.

mod measures_chain;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const PASSWORD: &str = "columbus_rw_test";
const CONN: &str = "roads-ora";

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", "import oracledb, geopandas"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

/// With `exports`, a granted export folder `share` (and no approvals page).
fn setup(host: &str, port: &str, service: &str, exports: bool) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let mut extra = String::new();
    if exports {
        std::fs::create_dir_all(root.join("share")).unwrap();
        let share = serde_json::to_string(&root.join("share").display().to_string()).unwrap();
        extra = format!(
            "approvals: {{ page: false }}\nexports:\n  - {{ id: share, path: {share}, granted: true }}\n"
        );
    }
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [oracle]\nhosts: [{host:?}]\nhide_fields: [\"owner_*\"]\n\
             allow_edits: false\nlimits: {{ rows: 1000, seconds: 30 }}\n{extra}"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("roads-ora.yaml"),
        format!(
            "id: {CONN}\ntype: oracle\nhost: {host}\nport: {port}\ndatabase: {service}\n\
             user: columbus_rw\nmode: read-write\n\
             datasets:\n  allow: [\"ROADS.*\"]\n  deny: [\"ROADS.*_AUDIT\"]\n\
             row_filter: \"district = 14\"\n"
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
                .env("COLUMBUS_SECRET_ROADS_ORA", PASSWORD)
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

async fn sql(c: &RunningService<RoleClient, ()>, sql: &str) -> CallToolResult {
    call(c, "sql_select", json!({"connection": CONN, "sql": sql})).await
}

#[tokio::test]
async fn oracle_governed_session() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_ORA_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_ORA_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_ORA_PORT").unwrap_or_else(|_| "1521".into());
    let service = std::env::var("COLUMBUS_TEST_ORA_SERVICE").unwrap_or_else(|_| "FREEPDB1".into());
    let py = python().expect(
        "python with oracledb and geopandas is required when COLUMBUS_TEST_ORA_HOST is set",
    );
    let env = setup(&host, &port, &service, false);
    let client = start(&env, &py).await;

    let who = ok(&call(&client, "whoami", json!({})).await);
    let conn = &who["result"]["connections"][0];
    assert_eq!(conn["status"], "ok");
    assert_eq!(conn["mode"], "read", "edits are off in policy");
    assert_eq!(conn["engine_ready"], true);

    // Datasets: the schema the allow list names, the deny list, no
    // spatial index tables, hidden fields removed.
    let ds = ok(&call(&client, "list_datasets", json!({"connection": CONN})).await);
    let list = ds["result"]["datasets"].as_array().unwrap();
    let names: Vec<&str> = list.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"ROADS.ROADS"), "{names:?}");
    assert!(names.contains(&"ROADS.PARCELS"), "{names:?}");
    assert!(!names.contains(&"ROADS.ROADS_AUDIT"), "denied: {names:?}");
    assert!(!names.iter().any(|n| n.contains("MDRT")), "{names:?}");
    let roads = list.iter().find(|d| d["name"] == "ROADS.ROADS").unwrap();
    assert_eq!(roads["srs_id"], 4326);
    let fields: Vec<&str> = roads["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["ID", "ROUTE_CODE", "DISTRICT", "GEOM"]);

    // describe_dataset: geometry, CRS, and the count under the row filter.
    let d = ok(&call(
        &client,
        "describe_dataset",
        json!({"connection": CONN, "dataset": "ROADS.ROADS"}),
    )
    .await);
    assert_eq!(d["result"]["geometry_column"], "GEOM", "{d}");
    assert_eq!(d["result"]["rows"], 600, "{d}");
    assert_eq!(d["result"]["crs"]["code"], "EPSG:4326", "{d}");
    assert!(!d.to_string().contains("OWNER_NAME"), "{d}");
    // Esri-style ST_Geometry is reported, not read.
    let p = ok(&call(
        &client,
        "describe_dataset",
        json!({"connection": CONN, "dataset": "roads.parcels"}),
    )
    .await);
    assert!(
        p["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "geometry.unsupported"),
        "{p}"
    );

    // query_features: row filter applied, geometry read, bbox in lon/lat.
    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": CONN, "dataset": "ROADS.ROADS", "fields": ["ID", "DISTRICT"], "where": "id < 100", "order_by": "ID"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], 49, "{q}");
    assert!(q["result"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r[1] == 14));
    assert!(q["result"]["layer"]["handle"].is_string(), "{q}");
    assert!(
        q["result"]["executed_sql"]
            .as_str()
            .unwrap()
            .contains("ROWNUM > 0"),
        "{q}"
    );
    let b = ok(&call(
        &client,
        "query_features",
        json!({"connection": CONN, "dataset": "ROADS.ROADS", "fields": ["ID"], "bbox": [-97.75, 30.0, -97.70, 31.0], "bbox_crs": "EPSG:4326"}),
    )
    .await);
    let n = b["result"]["rows_read"].as_u64().unwrap();
    assert!(n > 100 && n < 600, "bbox keeps part of the rows: {b}");

    let s = ok(&call(
        &client,
        "sample_rows",
        json!({"connection": CONN, "dataset": "ROADS.ROADS", "n": 5}),
    )
    .await);
    assert_eq!(s["result"]["rows_returned"], 5, "{s}");

    // sql_select: the filter, hidden fields and geometry summaries.
    let q = ok(&sql(&client, "SELECT * FROM roads.roads ORDER BY id").await);
    let res = &q["result"];
    assert_eq!(res["rows_read"], 600, "{q}");
    assert_eq!(res["rows_returned"], 500);
    assert_eq!(
        res["columns"],
        json!(["ID", "ROUTE_CODE", "DISTRICT", "GEOM"])
    );
    assert_eq!(res["rows"][0][3]["geometry"], "ewkb", "{q}");
    assert!(res["rows"].as_array().unwrap().iter().all(|r| r[2] == 14));

    // The read limit, reported.
    let big = ok(&sql(
        &client,
        "SELECT a.id FROM roads.roads a CROSS JOIN roads.roads b",
    )
    .await);
    assert_eq!(big["result"]["rows_read"], 1000);
    assert!(big["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "query.read_limit"));

    // Oracle Spatial runs, called in MDSYS.
    let g = ok(&sql(
        &client,
        "SELECT id, SDO_GEOM.SDO_LENGTH(geom, 0.005) AS len FROM roads.roads WHERE id = 2",
    )
    .await);
    assert_eq!(g["result"]["rows_returned"], 1, "{g}");
    assert!(g["result"]["executed_sql"]
        .as_str()
        .unwrap()
        .contains("MDSYS.SDO_GEOM.SDO_LENGTH"));

    // Refusals by the server's checks.
    for (q, expect) in [
        (
            "SELECT * FROM all_tables",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM sys.dba_users",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM roads.roads_audit",
            "not in this connection's allowed datasets",
        ),
        ("DELETE FROM roads.roads", "only SELECT"),
        ("UPDATE roads.roads SET district = 9", "only SELECT"),
        (
            "SELECT owner_name FROM roads.roads",
            "hidden by organization policy",
        ),
        (
            "SELECT id FROM roads.roads WHERE TO_NUMBER(owner_name) = 1",
            "hidden by organization policy",
        ),
        ("SELECT roads.log_visit() FROM roads.roads", "not allowed"),
        // resolves to a public synonym for the database's own function
        (
            "SELECT split_part(route_code, '-', 1) FROM roads.roads",
            "not allowed",
        ),
        ("SELECT ST_Length(geom) FROM roads.roads", "Oracle Spatial"),
        ("SELECT r.geom.GET_WKT() FROM roads.roads r", "not allowed"),
    ] {
        let e = err(&sql(&client, q).await);
        assert!(e.contains(expect), "{q}: {e}");
        assert!(!e.contains("Owner "), "{q}: {e}");
    }

    // The barrier: the row filter runs before the caller's conditions, so a
    // conversion error cannot quote a filtered-out row's value. Odd ids are
    // district 9 with route codes like SECRET-1 that are not numbers.
    for q in [
        "SELECT id FROM roads.roads WHERE TO_NUMBER(route_code) = 1002",
        "SELECT id FROM roads.roads WHERE id = 1 OR TO_NUMBER(route_code) > 0",
        "SELECT r.id FROM roads.roads r WHERE TO_NUMBER(r.route_code) < 0",
    ] {
        let r = sql(&client, q).await;
        let text = serde_json::to_string(&r).unwrap();
        assert!(
            !text.contains("SECRET"),
            "{q} reached a filtered row: {text}"
        );
        assert_ne!(r.is_error, Some(true), "{q}: {text}");
    }
    let one = ok(&sql(
        &client,
        "SELECT id FROM roads.roads WHERE TO_NUMBER(route_code) = 1002",
    )
    .await);
    assert_eq!(one["result"]["rows"], json!([[2]]));

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

/// LRS routes (SDO_GTYPE 3302, measures third) keep their M values through
/// query_features, reproject, join, spatial_join, dissolve and exports to
/// gpkg, gdb and shp; buffer drops M with geometry.m_dropped (T-19,
/// REQ-24). The connection's row filter keeps district 14 (even ids).
#[tokio::test]
async fn oracle_lrs_measures_survive() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_ORA_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_ORA_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_ORA_PORT").unwrap_or_else(|_| "1521".into());
    let service = std::env::var("COLUMBUS_TEST_ORA_SERVICE").unwrap_or_else(|_| "FREEPDB1".into());
    let py = python().expect(
        "python with oracledb and geopandas is required when COLUMBUS_TEST_ORA_HOST is set",
    );
    let env = setup(&host, &port, &service, true);
    let client = start(&env, &py).await;
    measures_chain::run(
        &client,
        &py,
        &env.root.join("workspaces"),
        &env.root.join("share"),
        &measures_chain::Names {
            conn: CONN,
            routes: "ROADS.ROUTES",
            route_id: "ROUTE_ID",
            district: "DISTRICT",
            // The connection's row filter (district = 14) applies to every
            // dataset, so the join is to the routes' own ids.
            join: "ROADS.ROUTES",
            join_fields: Some(&["ROUTE_ID", "ID"]),
            join_left: "ROUTE_ID",
            join_right: "ROUTE_ID",
            other: "ROADS.ROADS",
        },
    )
    .await;
    client.cancel().await.unwrap();
}
