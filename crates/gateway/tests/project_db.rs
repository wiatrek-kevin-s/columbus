//! Project databases end to end: governed results loaded into a local
//! DuckDB file, keys, indexes and views added with no approval, joined with
//! SQL into a layer that carries the database's data labels; statements
//! that would reach other files, databases, extensions or settings refused
//! and audited; deleting a whole database asks the person; a database kept
//! for later sessions is withheld once the settings no longer cover what
//! it holds, and removed when they narrow during a session.
//!
//! Needs Python with geopandas, pyogrio, shapely and duckdb; skipped
//! otherwise.

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
                .args(["-c", "import geopandas, pyogrio, shapely, duckdb"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

#[derive(Clone, Default)]
struct Person {
    accept: Arc<AtomicBool>,
    asked: Arc<AtomicUsize>,
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
        _request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(if self.accept.load(Ordering::SeqCst) {
            ElicitResult::new(ElicitationAction::Accept)
                .with_content(json!({"decision": "allow_once"}))
        } else {
            ElicitResult::new(ElicitationAction::Decline)
        })
    }
}

/// Parcels (polygons in EPSG:2277, a hidden owner_ssn, district 14 or 12)
/// and an assessor table keyed by parcel id with zero-padded codes.
fn fixture(py: &str, root: &Path) -> PathBuf {
    let path = root.join("county.gpkg");
    let script = r#"
import sys
import geopandas as gpd, pandas as pd
from shapely.geometry import box
n = 2000
gpd.GeoDataFrame({"parcel_id": [f"P{i:05d}" for i in range(n)], "district": [14 if i % 10 else 12 for i in range(n)],
                  "owner_ssn": ["123-45-6789"] * n},
    geometry=[box(3_100_000 + (i % 50) * 100, 10_070_000 + (i // 50) * 100, 3_100_090 + (i % 50) * 100, 10_070_090 + (i // 50) * 100) for i in range(n)],
    crs="EPSG:2277").to_file(sys.argv[1], layer="parcels", driver="GPKG", engine="pyogrio")
import sqlite3
db = sqlite3.connect(sys.argv[1])
db.execute("CREATE TABLE assessor (parcel_id TEXT PRIMARY KEY, land_use TEXT, value INTEGER, district INTEGER)")
db.executemany("INSERT INTO assessor VALUES (?, ?, ?, 14)", [(f"P{i:05d}", f"{i % 7:03d}", i * 100) for i in range(0, n, 2)])
db.execute("INSERT INTO gpkg_contents (table_name, data_type, identifier) VALUES ('assessor', 'attributes', 'assessor')")
db.commit()
"#;
    let out = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

fn conn_yaml(path: &Path, filter: &str) -> String {
    let p = serde_json::to_string(&path.display().to_string()).unwrap();
    format!("id: county\ntype: gpkg\npath: {p}\ndatasets:\n  allow: [\"*\"]\nrow_filter: \"{filter}\"\n")
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
                .env("COLUMBUS_COMPANION", "off")
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

fn put(path: &Path, text: &str) {
    let tmp = path.with_extension("tmp-write");
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, path).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
}

fn audit_lines(root: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(root.join("audit")).unwrap().flatten() {
        for l in std::fs::read_to_string(e.path())
            .unwrap_or_default()
            .lines()
        {
            if let Ok(v) = serde_json::from_str::<Value>(l) {
                out.push(v);
            }
        }
    }
    out
}

fn layer(r: &CallToolResult) -> String {
    ok(r)["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn pdb(c: &RunningService<RoleClient, Person>, args: Value) -> CallToolResult {
    call(c, "project_db", args).await
}

fn setup(py: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = fixture(py, &root);
    put(
        &root.join("policy.yaml"),
        "connection_types: [gpkg]\nhide_fields: [\"*_ssn\"]\n",
    );
    put(
        &root.join("connections/county.yaml"),
        &conn_yaml(&gpkg, "district = 14"),
    );
    (dir, root, gpkg)
}

#[tokio::test]
async fn a_project_database_joins_governed_tables_and_keeps_their_labels() {
    let Some(py) = python() else {
        eprintln!("skipped: needs Python with geopandas, pyogrio, shapely and duckdb");
        return;
    };
    let (_dir, root, _gpkg) = setup(&py);
    let person = Person::default();
    person.accept.store(true, Ordering::SeqCst);
    let c = start(&root, &py, person.clone()).await;
    let who = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(who["result"]["policy"]["project_db"], true);

    let parcels = layer(
        &call(
            &c,
            "query_features",
            json!({"connection": "county", "dataset": "parcels"}),
        )
        .await,
    );
    let assessor = layer(
        &call(
            &c,
            "query_features",
            json!({"connection": "county", "dataset": "assessor"}),
        )
        .await,
    );

    let r = ok(&pdb(&c, json!({"action": "create", "db": "county", "description": "Parcels and assessor values for the 2026 review"})).await);
    assert_eq!(r["result"]["status"], "done");
    let r = ok(&pdb(&c, json!({"action": "load", "db": "county", "layer": parcels, "table": "parcels", "primary_key": ["parcel_id"]})).await);
    // The row filter applied when the layer was read: district 14 only.
    assert_eq!(r["result"]["result"]["rows"], 1800);
    assert_eq!(r["result"]["result"]["crs"], "EPSG:2277");
    assert_eq!(r["result"]["labels"], json!(["connection:county"]));
    let cols = r["result"]["result"]["columns"].to_string();
    assert!(!cols.contains("owner_ssn"), "{cols}");
    let r = ok(&pdb(&c, json!({"action": "load", "db": "county", "layer": assessor, "table": "assessor", "primary_key": ["parcel_id"]})).await);
    assert_eq!(r["result"]["result"]["rows"], 1000);

    // Schema work in one call, with no approval.
    let asked = person.asked.load(Ordering::SeqCst);
    let r = ok(&pdb(&c, json!({"action": "execute", "db": "county", "sql": "
        CREATE INDEX assessor_parcel ON assessor (parcel_id);
        COMMENT ON TABLE assessor IS 'Assessor values by parcel (land_use codes keep their padding)';
        CREATE TABLE land_use (code VARCHAR PRIMARY KEY, label VARCHAR);
        INSERT INTO land_use VALUES ('000', 'Residential'), ('001', 'Commercial'), ('002', 'Industrial'), ('003', 'Ag'), ('004', 'Park'), ('005', 'Vacant'), ('006', 'Other');
        CREATE VIEW parcel_values AS SELECT p.parcel_id, a.value, l.label, p.geom FROM parcels p JOIN assessor a USING (parcel_id) JOIN land_use l ON l.code = a.land_use;
    "})).await);
    assert_eq!(
        person.asked.load(Ordering::SeqCst),
        asked,
        "writing the project database asks nothing"
    );
    let types: Vec<&str> = r["result"]["result"]["statements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["CREATE", "ALTER", "CREATE", "INSERT", "CREATE"]);

    // A join, written as a layer that carries the database's labels.
    let q = ok(&pdb(&c, json!({"action": "query", "db": "county", "sql": "SELECT * FROM parcel_values WHERE label = 'Commercial'"})).await);
    let res = &q["result"]["result"];
    assert!(res["rows_read"].as_u64().unwrap() > 100, "{res}");
    let handle = res["layer"]["handle"].as_str().unwrap().to_string();
    assert!(handle.ends_with(".gpkg"));
    assert_eq!(res["layer"]["summary"]["crs"]["code"], "EPSG:2277");
    let outs = ok(&call(&c, "list_outputs", json!({})).await);
    let card = outs["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["handle"] == handle.as_str())
        .unwrap()
        .clone();
    assert_eq!(card["labels"], json!(["connection:county"]));
    // It is a layer like any other: the GIS tools open it.
    ok(&call(&c, "verify", json!({"layer": handle})).await);
    // Without geometry the result is a CSV layer.
    let q = ok(&pdb(&c, json!({"action": "query", "db": "county", "sql": "SELECT label, count(*) AS n, sum(value) AS total FROM parcel_values GROUP BY label ORDER BY label"})).await);
    assert_eq!(q["result"]["result"]["rows_read"], 7);
    assert!(q["result"]["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .ends_with(".csv"));

    let d = ok(&pdb(&c, json!({"action": "describe", "db": "county"})).await);
    let tables = d["result"]["result"]["tables"].to_string();
    assert!(
        tables.contains("parcel_values") && tables.contains("Assessor values by parcel"),
        "{tables}"
    );
    assert_eq!(
        d["result"]["result"]["indexes"][0]["name"],
        "assessor_parcel"
    );

    // Nothing outside the file: refused, audited, and nothing kept.
    let outside = root
        .join("county.gpkg")
        .display()
        .to_string()
        .replace('\'', "''");
    for sql in [
        format!("ATTACH '{outside}' AS src (TYPE sqlite)"),
        format!("COPY parcels TO '{outside}.csv'"),
        "INSTALL spatial".to_string(),
        "LOAD httpfs".to_string(),
        "SET enable_external_access = true".to_string(),
        format!("CREATE TABLE leak AS SELECT * FROM read_csv('{outside}')"),
        format!("CREATE TABLE leak AS SELECT * FROM '{outside}'"),
        "PRAGMA database_list".to_string(),
    ] {
        let e = err(&pdb(&c, json!({"action": "execute", "db": "county", "sql": sql})).await);
        assert!(
            e.contains("refused") || e.contains("Permission") || e.contains("failed"),
            "{sql}: {e}"
        );
    }
    assert!(!root.join("county.gpkg.csv").exists());
    let e = err(&pdb(&c, json!({"action": "query", "db": "county", "sql": format!("SELECT * FROM read_csv('{outside}')")})).await);
    assert!(e.contains("refused"), "{e}");
    let e = err(&pdb(&c, json!({"action": "load", "db": "county", "layer": "ws://elsewhere/results/x.gpkg", "table": "x"})).await);
    assert!(e.contains("another session"), "{e}");

    // Every statement is audited on its own line, never with its text.
    let lines = audit_lines(&root);
    let stmts: Vec<&Value> = lines
        .iter()
        .filter(|l| {
            l["tool"] == "project_db"
                && l["decision"] == "allowed"
                && l["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("statement "))
        })
        .collect();
    assert_eq!(stmts.len(), 5);
    assert!(stmts
        .iter()
        .all(|l| l["request"].is_string() && l["labels"] == json!(["connection:county"])));
    assert!(!serde_json::to_string(&lines)
        .unwrap()
        .contains("Residential"));
    let denied = lines
        .iter()
        .filter(|l| l["tool"] == "project_db" && l["decision"] != "allowed")
        .count();
    assert!(denied >= 9, "{denied}");

    // Deleting a whole database asks the person, every time.
    person.accept.store(false, Ordering::SeqCst);
    err(&pdb(&c, json!({"action": "delete", "db": "county"})).await);
    let listed = ok(&pdb(&c, json!({"action": "list"})).await);
    assert_eq!(listed["result"]["result"]["databases"][0]["db"], "county");
    person.accept.store(true, Ordering::SeqCst);
    let before = person.asked.load(Ordering::SeqCst);
    ok(&pdb(&c, json!({"action": "delete", "db": "county"})).await);
    assert_eq!(person.asked.load(Ordering::SeqCst), before + 1);
    let listed = ok(&pdb(&c, json!({"action": "list"})).await);
    assert_eq!(listed["result"]["result"]["databases"], json!([]));

    // The organization can turn project databases off.
    put(
        &root.join("policy.yaml"),
        "connection_types: [gpkg]\nhide_fields: [\"*_ssn\"]\nproject_db: { enabled: false }\n",
    );
    let e = err(&pdb(&c, json!({"action": "list"})).await);
    assert!(e.contains("turned project databases off"), "{e}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_project_database_kept_for_later_follows_the_settings() {
    let Some(py) = python() else {
        eprintln!("skipped: needs Python with geopandas, pyogrio, shapely and duckdb");
        return;
    };
    let (_dir, root, gpkg) = setup(&py);
    let c = start(&root, &py, Person::default()).await;
    let parcels = layer(
        &call(
            &c,
            "query_features",
            json!({"connection": "county", "dataset": "parcels", "fields": ["parcel_id"]}),
        )
        .await,
    );
    ok(&pdb(
        &c,
        json!({"action": "create", "db": "review", "scope": "project"}),
    )
    .await);
    ok(&pdb(&c, json!({"action": "load", "db": "review", "scope": "project", "layer": parcels, "table": "parcels"})).await);
    c.cancel().await.unwrap();
    let folder = root.join("workspaces/projects/review");
    assert!(
        folder.join("review.duckdb").is_file() && folder.join("columbus-project.json").is_file()
    );

    // A later session: the database is there, and reading it labels what
    // comes out (and the session) with the connection it came from.
    let c = start(&root, &py, Person::default()).await;
    let who = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(who["result"]["labels"], json!([]));
    let q = ok(&pdb(&c, json!({"action": "query", "db": "review", "scope": "project", "sql": "SELECT count(*) AS n FROM parcels"})).await);
    assert_eq!(q["result"]["labels"], json!(["connection:county"]));
    let who = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(who["result"]["labels"], json!(["connection:county"]));
    // Labels cannot be dropped by SQL: the record is outside the database.
    err(&pdb(&c, json!({"action": "execute", "db": "review", "scope": "project", "sql": "COPY (SELECT 1) TO 'columbus-project.json'"})).await);

    // The person narrows the connection while this session runs: the
    // database goes, as files read from the connection do.
    put(
        &root.join("connections/county.yaml"),
        &conn_yaml(&gpkg, "district = 14 AND parcel_id <> 'P00001'"),
    );
    let e = err(&pdb(
        &c,
        json!({"action": "describe", "db": "review", "scope": "project"}),
    )
    .await);
    assert!(
        e.contains("no project project database") || e.contains("create it first"),
        "{e}"
    );
    assert!(!folder.exists());
    c.cancel().await.unwrap();

    // Built again, then the settings change between sessions: withheld.
    let c = start(&root, &py, Person::default()).await;
    let parcels = layer(
        &call(
            &c,
            "query_features",
            json!({"connection": "county", "dataset": "parcels", "fields": ["parcel_id"]}),
        )
        .await,
    );
    ok(&pdb(
        &c,
        json!({"action": "create", "db": "review", "scope": "project"}),
    )
    .await);
    ok(&pdb(&c, json!({"action": "load", "db": "review", "scope": "project", "layer": parcels, "table": "parcels"})).await);
    c.cancel().await.unwrap();
    put(
        &root.join("policy.yaml"),
        "connection_types: [gpkg]\nhide_fields: [\"*_ssn\", \"parcel_*\"]\n",
    );
    let c = start(&root, &py, Person::default()).await;
    let e = err(&pdb(&c, json!({"action": "query", "db": "review", "scope": "project", "sql": "SELECT * FROM parcels"})).await);
    assert!(e.contains("withheld"), "{e}");
    let listed = ok(&pdb(&c, json!({"action": "list"})).await);
    assert!(listed["result"]["result"]["databases"][0]["withheld"].is_string());
    c.cancel().await.unwrap();
}
