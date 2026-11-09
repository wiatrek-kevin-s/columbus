//! End to end against a real SQL Server.
//!
//! Runs only when `COLUMBUS_TEST_MSSQL_HOST` is set and the fixture in
//! `tests/fixtures/mssql.sql` is loaded (CI does both on Linux):
//!
//! ```text
//! COLUMBUS_TEST_MSSQL_HOST=localhost COLUMBUS_TEST_MSSQL_PORT=1433 \
//! COLUMBUS_TEST_MSSQL_DB=gis cargo test --test mssql
//! ```
//!
//! The connection deliberately uses `columbus_rw`, a login that can write,
//! to show that writes are still refused by the server's checks, and that
//! nothing a statement does is kept (every query runs in a transaction that
//! is rolled back). SQL Server has no read-only session: a read-only login
//! is the real guarantee.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const PASSWORD: &str = "Columbus_rw_test1";
const CONN: &str = "roads-ms";

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", "import pyodbc, geopandas"])
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
            "connection_types: [sqlserver]\nhosts: [{host:?}]\nhide_fields: [\"owner_*\"]\n\
             allow_edits: false\nlimits: {{ rows: 1000, seconds: 30 }}\n"
        ),
    )
    .unwrap();
    // sslmode require: encrypted, but the container's certificate is self-signed.
    std::fs::write(
        root.join("connections").join("roads-ms.yaml"),
        format!(
            "id: {CONN}\ntype: sqlserver\nhost: {host}\nport: {port}\ndatabase: {db}\n\
             user: columbus_rw\nsslmode: require\nmode: read-write\n\
             datasets:\n  allow: [\"roads.*\"]\n  deny: [\"roads.*_audit\"]\n\
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
                .env("COLUMBUS_SECRET_ROADS_MS", PASSWORD)
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
async fn sql_server_governed_session() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_MSSQL_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_MSSQL_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_MSSQL_PORT").unwrap_or_else(|_| "1433".into());
    let db = std::env::var("COLUMBUS_TEST_MSSQL_DB").unwrap_or_else(|_| "gis".into());
    let py = python().expect(
        "python with pyodbc and geopandas is required when COLUMBUS_TEST_MSSQL_HOST is set",
    );
    let env = setup(&host, &port, &db);
    let client = start(&env, &py).await;

    let who = ok(&call(&client, "whoami", json!({})).await);
    let conn = &who["result"]["connections"][0];
    assert_eq!(conn["status"], "ok");
    assert_eq!(conn["mode"], "read", "edits are off in policy");
    assert_eq!(conn["engine_ready"], true);

    let ds = ok(&call(&client, "list_datasets", json!({"connection": CONN})).await);
    let list = ds["result"]["datasets"].as_array().unwrap();
    let names: Vec<&str> = list.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"roads.roads"), "{names:?}");
    assert!(names.contains(&"roads.places"), "{names:?}");
    assert!(!names.contains(&"roads.roads_audit"), "denied: {names:?}");
    let roads = list.iter().find(|d| d["name"] == "roads.roads").unwrap();
    assert_eq!(roads["srs_id"], 3081);
    let fields: Vec<&str> = roads["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["id", "route_code", "district", "geom"]);

    let d = ok(&call(
        &client,
        "describe_dataset",
        json!({"connection": CONN, "dataset": "roads.roads"}),
    )
    .await);
    assert_eq!(d["result"]["geometry_column"], "geom", "{d}");
    assert_eq!(d["result"]["rows"], 600, "{d}");
    assert_eq!(d["result"]["crs"]["code"], "EPSG:3081", "{d}");
    assert!(!d.to_string().contains("owner_name"), "{d}");

    // query_features: row filter, geometry, ORDER BY with the limit.
    let q = ok(&call(
        &client,
        "query_features",
        json!({"connection": CONN, "dataset": "roads.roads", "fields": ["id", "district"], "where": "id < 100", "order_by": "id"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], 49, "{q}");
    assert_eq!(q["result"]["rows"][0][0], 2, "ordered: {q}");
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
            .contains("TOP (9223372036854775807)"),
        "{q}"
    );
    // A bbox (unlimited read, filtered by the engine) that is also ordered.
    let b = ok(&call(
        &client,
        "query_features",
        json!({"connection": CONN, "dataset": "roads.roads", "fields": ["id"], "bbox": [1005000, 6999000, 1010000, 7001000], "order_by": "id"}),
    )
    .await);
    let n = b["result"]["rows_read"].as_u64().unwrap();
    assert!(n > 100 && n < 600, "bbox keeps part of the rows: {b}");
    // geography
    let g = ok(&call(
        &client,
        "query_features",
        json!({"connection": CONN, "dataset": "roads.places"}),
    )
    .await);
    assert_eq!(g["result"]["rows_read"], 1, "{g}");

    let s = ok(&call(
        &client,
        "sample_rows",
        json!({"connection": CONN, "dataset": "roads.roads", "n": 5}),
    )
    .await);
    assert_eq!(s["result"]["rows_returned"], 5, "{s}");

    let q = ok(&sql(&client, "SELECT * FROM roads.roads ORDER BY id").await);
    let res = &q["result"];
    assert_eq!(res["rows_read"], 600, "{q}");
    assert_eq!(res["rows_returned"], 500);
    assert_eq!(
        res["columns"],
        json!(["id", "route_code", "district", "geom"])
    );
    assert_eq!(res["rows"][0][3]["geometry"], "ewkb", "{q}");

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

    for (q, expect) in [
        (
            "SELECT * FROM sys.objects",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM INFORMATION_SCHEMA.TABLES",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM master.sys.sql_logins",
            "not in this connection's allowed datasets",
        ),
        (
            "SELECT * FROM roads.roads_audit",
            "not in this connection's allowed datasets",
        ),
        ("DELETE FROM roads.roads", "only SELECT"),
        ("SELECT * INTO roads.copy FROM roads.roads", "INTO"),
        (
            "SELECT owner_name FROM roads.roads",
            "hidden by organization policy",
        ),
        (
            "SELECT id FROM roads.roads WHERE CAST(owner_name AS INT) = 1",
            "hidden by organization policy",
        ),
        (
            "SELECT id FROM roads.roads WHERE CAST([owner_name] AS INT) = 1",
            "hidden by organization policy",
        ),
        (
            "SELECT roads.owner_list() FROM roads.roads",
            "query_features",
        ),
        ("SELECT geom.STLength() FROM roads.roads", "query_features"),
        (
            "SELECT id FROM roads.roads WHERE geom.STIntersects(geometry::Point(1, 2, 3081)) = 1",
            "query_features",
        ),
        ("SELECT id FROM roads.roads FOR JSON AUTO", "not allowed"),
    ] {
        let e = err(&sql(&client, q).await);
        assert!(e.contains(expect), "{q}: {e}");
        assert!(!e.contains("Owner "), "{q}: {e}");
    }

    // The barrier: a conversion error in the caller's condition would quote
    // the value ("Conversion failed when converting the varchar value
    // 'SECRET-1'") if the condition ever ran on a filtered-out row.
    for q in [
        "SELECT id FROM roads.roads WHERE CAST(route_code AS INT) = 1002",
        "SELECT id FROM roads.roads WHERE id = 1 OR CAST(route_code AS INT) > 0",
        "SELECT r.id, CAST(r.route_code AS INT) AS code FROM roads.roads r WHERE r.id < 10",
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
        "SELECT id FROM roads.roads WHERE CAST(route_code AS INT) = 1002",
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

/// Sorted first column of a query_features result, as integers.
fn ids(q: &Value) -> Vec<i64> {
    let mut v: Vec<i64> = q["result"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[0].as_i64().unwrap())
        .collect();
    v.sort();
    v
}

/// The query.bbox_index check's message.
fn bbox_note(q: &Value) -> String {
    q["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "query.bbox_index")
        .unwrap_or_else(|| panic!("no query.bbox_index check: {q}"))["message"]
        .as_str()
        .unwrap()
        .to_string()
}

/// T-13 (REQ-19): a bbox on a geometry column goes into `Filter()` inside
/// the filtered table, beside the row filter, and gives exactly the rows
/// meeting the box, in the layer's EPSG:3081 and in lon/lat; on a
/// geography column the engine keeps the rows meeting the box, as before.
#[tokio::test]
async fn sql_server_bbox_uses_the_spatial_index() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_MSSQL_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_MSSQL_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_MSSQL_PORT").unwrap_or_else(|_| "1433".into());
    let db = std::env::var("COLUMBUS_TEST_MSSQL_DB").unwrap_or_else(|_| "gis".into());
    let py = python().expect(
        "python with pyodbc and geopandas is required when COLUMBUS_TEST_MSSQL_HOST is set",
    );
    let env = setup(&host, &port, &db);
    let client = start(&env, &py).await;

    // Road i runs from x = 1,000,000 + 10 i for 5 m at y 7,000,000 to
    // 7,000,010; district 14 is the even ids.
    let native = [1_001_007.5, 6_999_990.0, 1_002_007.5, 7_000_020.0];
    // The same box in lon/lat, and back as the engine moves it (larger).
    let out = StdCommand::new(&py)
        .args([
            "-c",
            "import sys\nfrom pyproj import Transformer\nb = [float(v) for v in sys.argv[1:5]]\n\
             ll = Transformer.from_crs(3081, 4326, always_xy=True).transform_bounds(*b)\n\
             back = Transformer.from_crs(4326, 3081, always_xy=True).transform_bounds(*ll)\n\
             print(','.join(repr(v) for v in list(ll) + list(back)))",
        ])
        .args(native.map(|v| v.to_string()))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Vec<f64> = String::from_utf8_lossy(&out.stdout)
        .trim()
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let (lonlat, back) = (v[0..4].to_vec(), v[4..8].to_vec());
    // The roads meeting a box in EPSG:3081.
    let meeting = |b: &[f64]| -> Vec<i64> {
        (1..=1200)
            .filter(|i| i % 2 == 0)
            .filter(|i| {
                let x = 1_000_000.0 + 10.0 * *i as f64;
                x <= b[2] && x + 5.0 >= b[0] && 7_000_000.0 <= b[3] && 7_000_010.0 >= b[1]
            })
            .collect()
    };
    for (args, expected) in [
        (json!({"bbox": native}), meeting(&native)),
        (
            json!({"bbox": lonlat, "bbox_crs": "EPSG:4326"}),
            meeting(&back),
        ),
    ] {
        assert!(!expected.is_empty());
        let mut a = json!({"connection": CONN, "dataset": "roads.roads", "fields": ["id"], "order_by": "id"});
        a.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        let q = ok(&call(&client, "query_features", a).await);
        let sql = q["result"]["executed_sql"].as_str().unwrap();
        assert!(
            sql.contains("WHERE (columbus_filtered.district = 14) AND (columbus_filtered.[geom].Filter(geometry::STGeomFromText('POLYGON (("),
            "inside the filtered table, beside the row filter: {sql}"
        );
        assert!(sql.contains("', 3081)) = 1)"), "{sql}");
        assert!(!sql.contains("owner_name"), "{sql}");
        assert!(bbox_note(&q).contains("Filter()"), "{q}");
        assert_eq!(ids(&q), expected, "{args}: {q}");
    }
    assert_eq!(
        meeting(&native),
        (101..=200).filter(|i| i % 2 == 0).collect::<Vec<i64>>()
    );

    // geography: not pushed; the engine keeps the place meeting the box.
    let g = ok(&call(
        &client,
        "query_features",
        json!({"connection": CONN, "dataset": "roads.places", "fields": ["id"], "bbox": [-98.5, 30.0, -97.0, 31.5], "bbox_crs": "EPSG:4326"}),
    )
    .await);
    assert!(!g["result"]["executed_sql"]
        .as_str()
        .unwrap()
        .contains("Filter("));
    assert!(bbox_note(&g).contains("after reading"), "{g}");
    assert_eq!(ids(&g), vec![1], "{g}");
    client.cancel().await.unwrap();
}
