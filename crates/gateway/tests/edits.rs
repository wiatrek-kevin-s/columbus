//! Edits end to end on a GeoPackage: propose_edits writes nothing and
//! checks against the connection's rules; apply_edits to a copy leaves the
//! source alone; apply_edits to the source needs edits allowed, a
//! read-write connection and the person's approval, writes in one
//! transaction that GDAL can read back (spatial index included), and rolls
//! back when a row changed since the proposal or would leave the row
//! filter.
//!
//! Needs Python with geopandas, pyogrio and shapely; skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, ElicitRequestParams,
    ElicitResult, ElicitationAction, ElicitationCapability, FormElicitationCapability,
};
use rmcp::service::{RequestContext, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", "import geopandas, pyogrio, shapely"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

/// Five tracts in EPSG:3081; fid 4 is in district 12, outside the row
/// filter.
fn setup(py: &str, allow_edits: bool) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["A", "B", "C", "D", "E"], "district": [14, 14, 14, 12, 14]},
    geometry=[box(i * 1000, 0, i * 1000 + 900, 900) for i in range(5)], crs="EPSG:3081"
).to_file(os.path.join(sys.argv[1], "t.gpkg"), layer="tracts", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(&root)
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [gpkg]\nallow_edits: {allow_edits}\n"),
    )
    .unwrap();
    let gpkg = serde_json::to_string(&root.join("t.gpkg").display().to_string()).unwrap();
    std::fs::write(
        root.join("connections/tiger.yaml"),
        format!("id: tiger\ntype: gpkg\npath: {gpkg}\nmode: read-write\ndatasets:\n  allow: [\"*\"]\nrow_filter: \"district = 14\"\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/tiger_ro.yaml"),
        format!("id: tiger_ro\ntype: gpkg\npath: {gpkg}\ndatasets:\n  allow: [\"*\"]\n"),
    )
    .unwrap();
    (dir, root)
}

#[derive(Clone)]
struct Person {
    accept: Arc<AtomicBool>,
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
        if let ElicitRequestParams::FormElicitationParams { message, .. } = &request {
            *self.last.lock().unwrap() = message.clone();
        }
        Ok(if self.accept.load(Ordering::SeqCst) {
            ElicitResult::new(ElicitationAction::Accept)
                .with_content(json!({"decision": "allow_once"}))
        } else {
            ElicitResult::new(ElicitationAction::Decline)
        })
    }
}

async fn start(root: &Path, py: &str, person: Person) -> RunningService<RoleClient, Person> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = root.to_path_buf();
    let py = py.to_string();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_LIVE_DIR", root.join("live"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
        });
    person
        .serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap()
}

async fn call(
    c: &RunningService<RoleClient, Person>,
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

/// The GeoPackage as GDAL reads it: fid -> (NAME, district, bounds), and
/// whether its spatial index agrees with its features.
fn read_back(py: &str, gpkg: &Path) -> Value {
    let script = r#"
import sys, json, sqlite3
import geopandas as gpd
df = gpd.read_file(sys.argv[1], layer="tracts", engine="pyogrio", fid_as_index=True)
rows = {str(int(i)): [r.NAME, int(r.district), [round(v) for v in r.geometry.bounds]] for i, r in df.iterrows()}
db = sqlite3.connect(sys.argv[1])
n = db.execute("SELECT COUNT(*) FROM rtree_tracts_geom").fetchone()[0]
ok = db.execute("PRAGMA integrity_check").fetchone()[0]
print(json.dumps({"rows": rows, "rtree": n, "integrity": ok}))
"#;
    let out = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(gpkg)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn person() -> Person {
    Person {
        accept: Arc::new(AtomicBool::new(true)),
        asked: Arc::default(),
        last: Arc::default(),
    }
}

fn square(x: f64) -> Value {
    json!({"type": "Polygon", "coordinates": [[[x, 0.0], [x + 500.0, 0.0], [x + 500.0, 500.0], [x, 500.0], [x, 0.0]]]})
}

#[tokio::test]
async fn edits_are_proposed_reviewed_and_written_within_the_rules() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let (_dir, root) = setup(&py, true);
    let gpkg = root.join("t.gpkg");
    let p = person();
    let c = start(&root, &py, p.clone()).await;
    let before = std::fs::read(&gpkg).unwrap();

    // Proposing writes nothing, and shows each change.
    let changes = json!([
        {"op": "update", "key": 1, "values": {"NAME": "One"}},
        {"op": "update", "key": 2, "geometry": square(1000.0)},
        {"op": "delete", "key": 3},
        {"op": "insert", "values": {"NAME": "New", "district": 14}, "geometry": square(9000.0)},
    ]);
    let r = ok(&call(
        &c,
        "propose_edits",
        json!({"connection": "tiger", "dataset": "tracts", "changes": changes, "crs": "EPSG:3081"}),
    )
    .await);
    let res = &r["result"];
    assert_eq!(
        res["counts"],
        json!({"update": 2, "insert": 1, "delete": 1}),
        "{res}"
    );
    assert_eq!(res["key"], "fid");
    let lines = res["changes"].to_string();
    assert!(lines.contains("NAME \\\"A\\\" → \\\"One\\\""), "{lines}");
    assert!(lines.contains("Delete fid 3"), "{lines}");
    assert!(
        res["preview"].as_str().unwrap().starts_with("ws://"),
        "{res}"
    );
    assert_eq!(
        std::fs::read(&gpkg).unwrap(),
        before,
        "proposing wrote nothing"
    );
    let change = res["change"].as_str().unwrap().to_string();

    // Rows outside the row filter, hidden or unknown fields, and the key
    // itself cannot be edited.
    for (bad, want) in [
        (
            json!([{"op": "update", "key": 4, "values": {"NAME": "x"}}]),
            "no row with fid = 4",
        ),
        (
            json!([{"op": "update", "key": 1, "values": {"NOPE": "x"}}]),
            "no field 'NOPE'",
        ),
        (
            json!([{"op": "update", "key": 1, "values": {"fid": 9}}]),
            "no field 'fid'",
        ),
        // The geometry only through "geometry", which is checked.
        (
            json!([{"op": "update", "key": 1, "values": {"geom": "00"}}]),
            "no field 'geom'",
        ),
        (
            json!([{"op": "update", "key": 1, "values": {"district": "x"}}]),
            "cannot take",
        ),
        (
            json!([{"op": "insert", "values": {"NAME": "x"}, "geometry": {"type": "Point", "coordinates": [1, 2]}}]),
            "Point cannot go in a",
        ),
    ] {
        let e = err(&call(
            &c,
            "propose_edits",
            json!({"connection": "tiger", "dataset": "tracts", "changes": bad, "crs": "EPSG:3081"}),
        )
        .await);
        assert!(e.contains(want), "{want}: {e}");
    }

    // A copy: every row this connection may read, edited; the source is
    // untouched and nobody is asked.
    let r = ok(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "copy"}),
    )
    .await);
    assert_eq!(r["result"]["status"], "applied_to_copy", "{r}");
    assert_eq!(
        r["result"]["summary"]["rows"], 4,
        "4 in district 14, minus 1, plus 1: {r}"
    );
    assert_eq!(std::fs::read(&gpkg).unwrap(), before);
    assert_eq!(p.asked.load(Ordering::SeqCst), 0);

    // Declined: nothing written.
    p.accept.store(false, Ordering::SeqCst);
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert!(e.to_lowercase().contains("declined"), "{e}");
    assert_eq!(std::fs::read(&gpkg).unwrap(), before);
    let asked = p.last.lock().unwrap().clone();
    assert!(
        asked.contains("Write 2 updates, 1 insertion and 1 deletion to tracts on tiger"),
        "{asked}"
    );
    assert!(asked.contains("NAME \"A\" → \"One\""), "{asked}");

    // Allowed: written in one go, readable by GDAL, index in step.
    p.accept.store(true, Ordering::SeqCst);
    let r = ok(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert_eq!(r["result"]["status"], "written", "{r}");
    assert_eq!(
        r["result"]["written"],
        json!({"update": 2, "insert": 1, "delete": 1})
    );
    let back = read_back(&py, &gpkg);
    let rows = &back["rows"];
    assert_eq!(rows["1"][0], "One", "{back}");
    assert_eq!(rows["2"][2], json!([1000, 0, 1500, 500]), "{back}");
    assert!(rows.get("3").is_none(), "{back}");
    assert_eq!(rows["4"][0], "D", "outside the filter, untouched: {back}");
    let new = rows
        .as_object()
        .unwrap()
        .values()
        .find(|v| v[0] == "New")
        .expect("inserted");
    assert_eq!(new[2], json!([9000, 0, 9500, 500]));
    assert_eq!(
        back["rtree"], 5,
        "spatial index matches the 5 features: {back}"
    );
    assert_eq!(back["integrity"], "ok");
    // A change set is used once.
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert!(e.contains("not a change set"), "{e}");

    // A row changed by someone else after the proposal: nothing written.
    let r = ok(&call(&c, "propose_edits", json!({"connection": "tiger", "dataset": "tracts",
        "changes": [{"op": "update", "key": 5, "values": {"NAME": "Five"}}, {"op": "delete", "key": 1}]})).await);
    let stale = r["result"]["change"].as_str().unwrap().to_string();
    let db = rusqlite_free_update(
        &py,
        &gpkg,
        "UPDATE tracts SET NAME = 'Elsewhere' WHERE fid = 5",
    );
    assert!(db);
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": stale, "target": "source"}),
    )
    .await);
    assert!(
        e.contains("changed") && e.contains("nothing was written"),
        "{e}"
    );
    let back = read_back(&py, &gpkg);
    assert_eq!(back["rows"]["5"][0], "Elsewhere");
    assert_eq!(back["rows"]["1"][0], "One", "the deletion rolled back too");

    // A change that would take a row outside the row filter: rolled back.
    let r = ok(&call(&c, "propose_edits", json!({"connection": "tiger", "dataset": "tracts",
        "changes": [{"op": "update", "key": 2, "values": {"NAME": "Two"}}, {"op": "update", "key": 5, "values": {"district": 12}}]})).await);
    let escape = r["result"]["change"].as_str().unwrap().to_string();
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": escape, "target": "source"}),
    )
    .await);
    assert!(e.contains("outside the connection's row filter"), "{e}");
    let back = read_back(&py, &gpkg);
    assert_eq!(back["rows"]["5"][1], 14);
    assert_ne!(
        back["rows"]["2"][0], "Two",
        "the first update rolled back too"
    );

    // A read-only connection cannot be written, even with approval.
    let r = ok(&call(
        &c,
        "propose_edits",
        json!({"connection": "tiger_ro", "dataset": "tracts",
        "changes": [{"op": "update", "key": 2, "values": {"NAME": "Two"}}]}),
    )
    .await);
    let ro = r["result"]["change"].as_str().unwrap().to_string();
    let e = err(&call(&c, "apply_edits", json!({"change": ro, "target": "source"})).await);
    assert!(e.contains("read-only"), "{e}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn without_allow_edits_the_source_is_never_written() {
    let Some(py) = python() else { return };
    let (_dir, root) = setup(&py, false);
    let gpkg = root.join("t.gpkg");
    let p = person();
    let c = start(&root, &py, p.clone()).await;
    let before = std::fs::read(&gpkg).unwrap();
    let r = ok(&call(
        &c,
        "propose_edits",
        json!({"connection": "tiger", "dataset": "tracts",
        "changes": [{"op": "update", "key": 1, "values": {"NAME": "One"}}]}),
    )
    .await);
    let change = r["result"]["change"].as_str().unwrap().to_string();
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert!(e.contains("does not allow edits"), "{e}");
    assert_eq!(p.asked.load(Ordering::SeqCst), 0, "not even asked");
    assert_eq!(std::fs::read(&gpkg).unwrap(), before);
    // A copy still works.
    let r = ok(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "copy"}),
    )
    .await);
    assert_eq!(r["result"]["status"], "applied_to_copy");
    c.cancel().await.unwrap();
}

/// Another program changing the GeoPackage (SQLite with the functions a
/// GeoPackage's index triggers call, as GDAL provides them).
fn rusqlite_free_update(py: &str, gpkg: &Path, sql: &str) -> bool {
    StdCommand::new(py)
        .args([
            "-c",
            "import sqlite3, sys; sys.path.insert(0, sys.argv[3]); import columbus_edits as e; \
             d = sqlite3.connect(sys.argv[1]); e._gpkg_functions(d); d.execute(sys.argv[2]); d.commit()",
        ])
        .arg(gpkg)
        .arg(sql)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/python"))
        .status()
        .unwrap()
        .success()
}

/// The same rules on PostGIS (CI loads tests/fixtures/postgis.sql and sets
/// COLUMBUS_TEST_PG_HOST): a write in one transaction, refused for a hidden
/// field and outside the row filter, and rolled back when a change would
/// leave it.
#[tokio::test]
async fn postgis_edits_follow_the_same_rules() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_PG_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_PG_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_PG_PORT").unwrap_or_else(|_| "5432".into());
    let db = std::env::var("COLUMBUS_TEST_PG_DB").unwrap_or_else(|_| "gis".into());
    let py = python().expect("python with geopandas is required when COLUMBUS_TEST_PG_HOST is set");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [postgis]\nhosts: [{host:?}]\nhide_fields: [\"owner_*\"]\nallow_edits: true\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/parcels.yaml"),
        format!(
            "id: parcels\ntype: postgis\nhost: {host}\nport: {port}\ndatabase: {db}\nuser: columbus_rw\n\
             sslmode: disable\nmode: read-write\ndatasets:\n  allow: [\"roads.edit_parcels\"]\n\
             row_filter: \"district_nbr = 14\"\n"
        ),
    )
    .unwrap();
    let p = person();
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let r2 = root.clone();
    let py2 = py.clone();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", r2.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", r2.join("connections"))
                .env("COLUMBUS_WORKSPACES", r2.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", r2.join("audit"))
                .env("COLUMBUS_LIVE_DIR", r2.join("live"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py2)
                .env("COLUMBUS_DEV_SECRETS", "1")
                .env("COLUMBUS_SECRET_PARCELS", "columbus_rw_test")
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
        });
    let c = p
        .clone()
        .serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap();
    let ds = "roads.edit_parcels";
    let sq = |x: f64| json!({"type": "Polygon", "coordinates": [[[x, 7000000.0], [x + 500.0, 7000000.0], [x + 500.0, 7000500.0], [x, 7000500.0], [x, 7000000.0]]]});

    for (bad, want) in [
        (
            json!([{"op": "update", "key": 1, "values": {"owner_name": "x"}}]),
            "no field 'owner_name'",
        ),
        (
            json!([{"op": "update", "key": 4, "values": {"name": "x"}}]),
            "no row with fid = 4",
        ),
    ] {
        let e = err(&call(
            &c,
            "propose_edits",
            json!({"connection": "parcels", "dataset": ds, "changes": bad}),
        )
        .await);
        assert!(e.contains(want), "{want}: {e}");
    }
    let r = ok(&call(&c, "propose_edits", json!({"connection": "parcels", "dataset": ds, "crs": "EPSG:3081", "changes": [
        {"op": "update", "key": 1, "values": {"name": "One"}},
        {"op": "update", "key": 2, "geometry": sq(1002000.0)},
        {"op": "delete", "key": 3},
        {"op": "insert", "values": {"name": "New", "district_nbr": 14}, "geometry": sq(1009000.0)},
    ]})).await);
    let change = r["result"]["change"].as_str().unwrap().to_string();
    let w = ok(&call(
        &c,
        "apply_edits",
        json!({"change": change, "target": "source"}),
    )
    .await);
    assert_eq!(
        w["result"]["written"],
        json!({"update": 2, "insert": 1, "delete": 1}),
        "{w}"
    );
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "parcels", "dataset": ds, "order_by": "name"}),
    )
    .await);
    let names: Vec<&str> = q["result"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r.get("name").and_then(Value::as_str))
        .collect();
    // District 14 now: B, E, the new row and One; C deleted, D outside.
    assert_eq!(names, vec!["B", "E", "New", "One"], "{q}");

    // Moving a row out of district 14 rolls the whole set back.
    let r = ok(&call(
        &c,
        "propose_edits",
        json!({"connection": "parcels", "dataset": ds, "changes": [
            {"op": "update", "key": 1, "values": {"name": "Uno"}},
            {"op": "update", "key": 5, "values": {"district_nbr": 12}},
        ]}),
    )
    .await);
    let escape = r["result"]["change"].as_str().unwrap().to_string();
    let e = err(&call(
        &c,
        "apply_edits",
        json!({"change": escape, "target": "source"}),
    )
    .await);
    assert!(e.contains("outside the connection's row filter"), "{e}");
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "parcels", "dataset": ds}),
    )
    .await);
    let rows = q["result"]["rows"].to_string();
    assert!(
        rows.contains("\"One\"") && !rows.contains("\"Uno\"") && rows.contains("\"E\""),
        "{rows}"
    );
    c.cancel().await.unwrap();
}
