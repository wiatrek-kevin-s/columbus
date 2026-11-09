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
            // Refused as a table the connection does not list, before the
            // table-function check would refuse it.
            "allowed",
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
    ] {
        let e = err(&call(
            &client,
            "sql_select",
            json!({"connection": "roads-pg", "sql": sql}),
        )
        .await);
        assert!(e.contains(expect), "{sql}: {e}");
    }

    // Real PostGIS functions work, called by the extension's own schema.
    let g = ok(&call(&client, "sql_select", json!({"connection": "roads-pg", "sql": "SELECT route_id, ST_Length(geom) AS len, ST_AsText(ST_StartPoint(geom)) AS p FROM roads.roadway_inventory ORDER BY route_id LIMIT 2"})).await);
    assert_eq!(g["result"]["rows_returned"], 2, "{g}");
    assert!(g["result"]["executed_sql"]
        .as_str()
        .unwrap()
        .contains("\"public\".st_length"));

    // Forms an independent review used to get hidden values past name
    // checks. The table each one reads holds only visible columns, so they
    // run (or fail) without ever returning an owner name.
    for sql in [
        "SELECT fid, route_id, '' AS a FROM roads.roadway_inventory WHERE false UNION ALL SELECT fid, route_id, csj FROM roads.roadway_inventory",
        "SELECT x.f FROM roads.roadway_inventory AS x(a, b, c, d, e, f)",
        "SELECT t.f FROM (SELECT * FROM roads.roadway_inventory) AS t(a, b, c, d, e, f)",
        "SELECT x.* FROM roads.roadway_inventory p, LATERAL (SELECT p.*) AS x",
        "SELECT p.fid FROM roads.roadway_inventory p JOIN (SELECT 'Owner 2' AS owner_name) v USING (owner_name)",
        "SELECT p.fid FROM roads.roadway_inventory p NATURAL JOIN (SELECT 'Owner 2' AS owner_name) v",
        "SELECT to_jsonb(r) FROM roads.roadway_inventory r",
        "SELECT row_to_json(r.*) FROM roads.roadway_inventory r",
        "SELECT * FROM roads.roadway_inventory ORDER BY 6",
    ] {
        let r = call(&client, "sql_select", json!({"connection": "roads-pg", "sql": sql})).await;
        // What came back from the table: the rows, or the error text (the
        // SQL echoed in executed_sql contains the query's own literals).
        let returned = match r.is_error {
            Some(true) => serde_json::to_string(&r.content).unwrap(),
            _ => r.structured_content.unwrap()["result"]["rows"].to_string(),
        };
        assert!(!returned.contains("Owner "), "{sql} leaked: {}", &returned[..returned.len().min(400)]);
    }

    // The row filter is applied before the caller's conditions run, so an
    // error from them cannot describe a row outside it. Row fid 2 is in
    // district 12: casting its route_id to a table name would fail with the
    // value in the message if the condition ever saw it.
    let r = call(&client, "sql_select", json!({"connection": "roads-pg", "sql": "SELECT fid FROM roads.roadway_inventory WHERE fid = 2 AND CAST(route_id AS regclass) IS NOT NULL"})).await;
    let text = serde_json::to_string(&r).unwrap();
    assert!(
        !text.contains("IH0001"),
        "a row outside the filter was evaluated: {text}"
    );

    // Only ordinary and spatial functions may be called. One of the
    // database's own (here one that writes) is refused by the server; and
    // functions that run SQL given as text, or read a table by name, would
    // read outside every check, so they are refused too.
    for sql in [
        "SELECT roads.log_visit() FROM roads.roadway_inventory",
        "SELECT query_to_xml('SELECT * FROM roads.roadway_inventory', true, false, '') FROM roads.roadway_inventory",
        "SELECT table_to_xml('roads.roadway_inventory', true, false, '') FROM roads.roadway_inventory",
        "SELECT ts_stat('SELECT to_tsvector(owner_name) FROM roads.roadway_inventory') FROM roads.roadway_inventory",
        // Named like PostGIS, but not installed by it.
        "SELECT st_owner_summary() FROM roads.roadway_inventory",
        "SELECT public.st_owner_summary() FROM roads.roadway_inventory",
    ] {
        let e = err(&call(&client, "sql_select", json!({"connection": "roads-pg", "sql": sql})).await);
        assert!(e.contains("not allowed"), "{sql}: {e}");
    }

    // An allowed name the database could resolve to its own function: the
    // server names pg_catalog's in full, which does not exist, so it fails.
    let r = call(
        &client,
        "sql_select",
        json!({"connection": "roads-pg", "sql": "SELECT ifnull(route_id, 'x') FROM roads.roadway_inventory"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true), "{:?}", r.structured_content);
    let text = serde_json::to_string(&r.content).unwrap();
    assert!(!text.contains("Owner"), "{text}");

    // Allowed by the server's list ("*"), but the login has no grant: the
    // database's own permissions still apply.
    let e = err(&call(
        &client,
        "sql_select",
        json!({"connection": "roads-pg", "sql": "SELECT * FROM hr.employees"}),
    )
    .await);
    // The server reads only tables it can list, and the login cannot see
    // this one, so it is refused before the database is asked.
    assert!(
        e.contains("permission denied") || e.contains("not in this connection's allowed datasets"),
        "{e}"
    );
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

/// Runs `script` with the PostGIS connection details as argv[1..5] and
/// `extra` after them; its stdout.
fn pg_script(py: &str, host: &str, port: &str, db: &str, script: &str, extra: &[&str]) -> String {
    let out = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .args([host, port, db, "columbus_rw", PASSWORD])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// T-13 (REQ-19): a bbox goes inside the filtered table, beside the row
/// filter, as `geom && ST_MakeEnvelope(...)`. With sequential scans off for
/// the session, PostgreSQL's plan for the exact SQL the gateway ran uses
/// the GiST index; and the rows are those of the unpushed query (the row
/// filter, then the rows whose geometry meets the box), for a box in the
/// layer's EPSG:3081 and one in lon/lat.
#[tokio::test]
async fn postgis_bbox_uses_the_spatial_index() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_PG_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_PG_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_PG_PORT").unwrap_or_else(|_| "5432".into());
    let db = std::env::var("COLUMBUS_TEST_PG_DB").unwrap_or_else(|_| "gis".into());
    let py = python().expect("python with psycopg is required when COLUMBUS_TEST_PG_HOST is set");
    let env = setup(&host, &port, &db);
    let client = start(&env, &py).await;

    const EXPLAIN: &str = "import sys, psycopg\n\
        h, p, d, u, pw, sql = sys.argv[1:7]\n\
        with psycopg.connect(host=h, port=p, dbname=d, user=u, password=pw) as c:\n\
        \x20   c.execute('SET enable_seqscan = off')\n\
        \x20   for (line,) in c.execute('EXPLAIN ' + sql).fetchall():\n\
        \x20       print(line)\n";
    // The unpushed query: the row filter, then the rows meeting the box,
    // which is moved to EPSG:3081 the way the engine moves it.
    const REFERENCE: &str = "import sys, json, psycopg\n\
        from pyproj import Transformer\n\
        h, p, d, u, pw, crs = sys.argv[1:7]\n\
        b = [float(v) for v in sys.argv[7:11]]\n\
        if crs != 'EPSG:3081':\n\
        \x20   b = list(Transformer.from_crs(crs, 'EPSG:3081', always_xy=True).transform_bounds(*b))\n\
        with psycopg.connect(host=h, port=p, dbname=d, user=u, password=pw) as c:\n\
        \x20   rows = c.execute('SELECT fid FROM roads.roadway_inventory WHERE district_nbr = 14 AND ST_Intersects(geom, ST_MakeEnvelope(%s, %s, %s, %s, 3081)) ORDER BY fid', b).fetchall()\n\
        print(json.dumps([r[0] for r in rows]))\n";
    const TO_LONLAT: &str = "import sys\n\
        from pyproj import Transformer\n\
        b = [float(v) for v in sys.argv[6:10]]\n\
        print(','.join(repr(v) for v in Transformer.from_crs('EPSG:3081', 'EPSG:4326', always_xy=True).transform_bounds(*b)))\n";

    // Segment i runs from x = 1,000,000 + 10 i to 5 m further: the box's
    // edges fall in the gaps, around segments 101 to 200; district 14 is
    // the even ones, fid i + 1.
    let native = [1_001_007.5, 6_999_990.0, 1_002_007.5, 7_000_020.0];
    let expected: Vec<i64> = (101..=200).filter(|i| i % 2 == 0).map(|i| i + 1).collect();
    let lonlat: Vec<f64> = pg_script(
        &py,
        &host,
        &port,
        &db,
        TO_LONLAT,
        &native
            .map(|v| v.to_string())
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
    .trim()
    .split(',')
    .map(|v| v.parse().unwrap())
    .collect();
    for (bbox, crs) in [(native.to_vec(), "EPSG:3081"), (lonlat, "EPSG:4326")] {
        let q = ok(&call(
            &client,
            "query_features",
            json!({"connection": "roads-pg", "dataset": "roads.roadway_inventory", "fields": ["fid", "route_id"],
                   "bbox": bbox, "bbox_crs": crs, "order_by": "fid"}),
        )
        .await);
        let sql = q["result"]["executed_sql"].as_str().unwrap().to_string();
        assert!(
            sql.contains("WHERE (columbus_filtered.district_nbr = 14) AND (columbus_filtered.\"geom\" && \"public\".st_makeenvelope("),
            "inside the filtered table, beside the row filter: {sql}"
        );
        assert!(sql.contains(", 3081)) OFFSET 0)"), "{sql}");
        assert!(!sql.contains("owner_name"), "{sql}");
        assert!(
            q["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "query.bbox_index"
                    && c["message"].as_str().unwrap().contains("PostGIS")),
            "{}",
            q["checks"]
        );
        let plan = pg_script(&py, &host, &port, &db, EXPLAIN, &[&sql]);
        assert!(
            plan.contains("roadway_inventory_geom_gist"),
            "the plan uses the spatial index:\n{plan}"
        );
        let got: Vec<i64> = q["result"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r[0].as_i64().unwrap())
            .collect();
        let b: Vec<String> = bbox.iter().map(|v| v.to_string()).collect();
        let mut args = vec![crs];
        args.extend(b.iter().map(String::as_str));
        let reference: Vec<i64> =
            serde_json::from_str(pg_script(&py, &host, &port, &db, REFERENCE, &args).trim())
                .unwrap();
        assert_eq!(got, reference, "{crs}: same rows as the unpushed query");
        assert_eq!(q["result"]["rows_read"], json!(reference.len()));
        if crs == "EPSG:3081" {
            assert_eq!(got, expected);
        } else {
            // Moving the box to lon/lat and back can only make it larger.
            assert!(expected.iter().all(|f| got.contains(f)), "{got:?}");
        }
    }
    client.cancel().await.unwrap();
}
