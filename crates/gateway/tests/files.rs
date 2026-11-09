//! `files` connections (build brief Part 1, T-2): a folder of CSVs is
//! listed, described, queried under the row filter and hidden fields,
//! buffered and exported, all through the cache GeoPackage; files it can't
//! offer (lock files, `.xls`, links out of the folder) are listed under
//! `skipped` with the reason.
//!
//! T-3: one fixture per format (CSV, XLSX sheets, GeoJSON, GeoParquet, KML
//! layers, FlatGeobuf) lists, describes and queries; a deleted file's dataset
//! is gone; a policy listing the old type `file` allows files connections.
//!
//! Needs Python with geopandas, pyogrio, shapely and pyproj; skipped
//! otherwise.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python() -> Option<String> {
    support::python_for("import geopandas, pyogrio, shapely, pyproj")
}

struct Env {
    _dir: tempfile::TempDir,
    _outside: tempfile::TempDir,
    root: PathBuf,
    /// Whether the link to a file outside the folder could be made (Windows
    /// may need a privilege for symbolic links).
    linked: bool,
}

fn q(p: &Path) -> String {
    serde_json::to_string(&p.display().to_string()).unwrap()
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn setup() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in ["connections", "csvs", "drops", "share"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // Crashes as lon/lat points; CSJ codes keep their zeros, mileposts are
    // numbers.
    std::fs::write(
        root.join("csvs/crashes.csv"),
        "CRASH_ID,STATUS,OWNER,lon,lat,CSJ,MILEPOST\n\
         1,A,Ann,-97.7431,30.2672,000123456,12.500\n\
         2,R,Bo,-97.7500,30.2800,000123457,1.50\n\
         3,A,Cy,-97.7600,30.2900,000123458,3\n\
         4,R,Di,-97.7700,30.3000,000123459,40\n",
    )
    .unwrap();
    // Roads as WKT lines in Texas Central State Plane feet.
    std::fs::write(
        root.join("csvs/roads.csv"),
        "ROAD,STATUS,OWNER,geometry\n\
         R1,A,Ann,\"LINESTRING (3110000 10070000, 3111000 10071000)\"\n\
         R2,R,Bo,\"LINESTRING (3112000 10072000, 3113000 10073000)\"\n",
    )
    .unwrap();
    // The folder with things a files connection doesn't offer.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.csv"), "SECRET\nhush\n").unwrap();
    std::fs::write(
        root.join("drops/roads.v2.csv"),
        "ROAD,STATUS,lon,lat\nR9,A,-97.7,30.2\n",
    )
    .unwrap();
    std::fs::write(root.join("drops/~$book.xlsx"), b"lock").unwrap();
    std::fs::write(root.join("drops/book.xls"), b"\xd0\xcf\x11\xe0old excel").unwrap();
    let linked = match symlink(
        &outside.path().join("secret.csv"),
        &root.join("drops/elsewhere.csv"),
    ) {
        Ok(()) => true,
        Err(e) if cfg!(windows) => {
            eprintln!("link sub-case skipped: could not make a symbolic link ({e})");
            false
        }
        Err(e) => panic!("symlink: {e}"),
    };
    std::fs::write(
        root.join("connections/crash-csvs.yaml"),
        format!(
            "id: crash-csvs\ntype: files\npath: {}\ndatasets:\n  allow: [\"*\"]\nrow_filter: \"STATUS <> 'R'\"\n\
             description: Crash and road CSVs\n",
            q(&root.join("csvs"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/drops.yaml"),
        format!(
            "id: drops\ntype: files\npath: {}\ndatasets:\n  allow: [\"*\"]\n",
            q(&root.join("drops"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [files]\nhide_fields: [OWNER]\nallow_edits: true\napprovals: {{ page: false }}\n\
             exports:\n  - {{ id: share, path: {}, granted: true }}\n",
            q(&root.join("share"))
        ),
    )
    .unwrap();
    Env {
        _dir: dir,
        _outside: outside,
        root,
        linked,
    }
}

async fn start(root: &Path, py: &str) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = root.to_path_buf();
    let py = py.to_string();
    let transport = TokioChildProcess::new(
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

fn names(v: &Value) -> Vec<String> {
    let mut n: Vec<String> = v["result"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    n.sort();
    n
}

#[tokio::test]
async fn csv_folder_is_read_buffered_and_exported_under_the_rules() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup();
    let c = start(&env.root, &py).await;

    // Two datasets; the hidden field is not listed.
    let l = ok(&call(&c, "list_datasets", json!({"connection": "crash-csvs"})).await);
    assert_eq!(names(&l), ["crashes", "roads"], "{l}");
    assert!(!l.to_string().contains("OWNER"), "{l}");
    // The cache GeoPackage was made where the brief says.
    let cache = env.root.join("workspaces/cache/files/crash-csvs.gpkg");
    assert!(cache.is_file(), "no cache at {}", cache.display());

    // lon/lat points are in EPSG:4326; typing keeps CSJ text and mileposts
    // numbers (R-2, R-4).
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "crash-csvs", "dataset": "crashes"}),
    )
    .await);
    assert_eq!(d["result"]["crs"]["code"], "EPSG:4326", "{d}");
    assert_eq!(d["result"]["geometry_type"], "POINT", "{d}");
    assert_eq!(d["result"]["rows"], 2, "counted under the row filter: {d}");
    let ty = |f: &str| {
        d["result"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == f)
            .unwrap_or_else(|| panic!("no field {f}: {d}"))["type"]
            .clone()
    };
    assert_eq!(ty("CSJ"), "TEXT");
    assert_eq!(ty("MILEPOST"), "REAL");
    assert_eq!(ty("CRASH_ID"), "INTEGER");
    // WKT lines (R-1), no CRS on the connection: none.
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "crash-csvs", "dataset": "roads"}),
    )
    .await);
    assert_eq!(d["result"]["geometry_type"], "LINESTRING", "{d}");
    assert!(d["result"]["crs"].is_null(), "{d}");

    // The row filter keeps R rows out; mileposts compare as numbers.
    let r = ok(&call(
        &c,
        "query_features",
        json!({"connection": "crash-csvs", "dataset": "crashes", "where": "MILEPOST > 2"}),
    )
    .await);
    let text = r.to_string();
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 2, "{r}");
    assert!(!text.contains("\"Bo\"") && !text.contains("\"Di\""), "{r}");
    assert!(!text.contains("\"OWNER\""), "hidden field returned: {r}");
    // A bbox (T-13): the cache's R-tree serves it, beside the row filter,
    // and the rows are those meeting the box: crash 1 and 3 (2 is an R
    // row, 4 is outside). The roads have no CRS: a box in their own
    // coordinates.
    let rtree = StdCommand::new(&py)
        .args([
            "-c",
            "import sqlite3\nsqlite3.connect(':memory:').execute('CREATE VIRTUAL TABLE t USING rtree(id, a, b)')",
        ])
        .status()
        .is_ok_and(|s| s.success());
    for (args, want) in [
        (
            json!({"dataset": "crashes", "fields": ["CRASH_ID"], "bbox": [-97.765, 30.26, -97.74, 30.295], "bbox_crs": "EPSG:4326", "order_by": "CRASH_ID"}),
            json!([[1], [3]]),
        ),
        (
            json!({"dataset": "roads", "fields": ["ROAD"], "bbox": [3110500, 10070500, 3110600, 10070600]}),
            json!([["R1"]]),
        ),
    ] {
        let mut a = json!({"connection": "crash-csvs"});
        a.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        let b = ok(&call(&c, "query_features", a).await);
        let rows: Vec<Value> = b["result"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| json!([r[0]]))
            .collect();
        assert_eq!(json!(rows), want, "{b}");
        let note = b["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "query.bbox_index")
            .unwrap_or_else(|| panic!("{b}"))["message"]
            .to_string();
        assert_eq!(note.contains("GeoPackage R-tree"), rtree, "{note}");
        let sql = b["result"]["executed_sql"].as_str().unwrap();
        if rtree {
            assert!(
                sql.contains("(columbus_filtered.STATUS <> 'R') AND (columbus_filtered.rowid IN (SELECT \"id\" FROM \"rtree_"),
                "{sql}"
            );
        }
    }
    let s = ok(&call(
        &c,
        "sql_select",
        json!({"connection": "crash-csvs", "sql": "SELECT CRASH_ID, STATUS FROM crashes ORDER BY CRASH_ID"}),
    )
    .await);
    let rows = s["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{s}");
    assert!(rows.iter().all(|r| r[1] == "A"), "{s}");

    // The hidden field is refused wherever it is named.
    let e = err(&call(
        &c,
        "query_features",
        json!({"connection": "crash-csvs", "dataset": "crashes", "fields": ["OWNER"]}),
    )
    .await);
    assert!(e.contains("OWNER"), "{e}");
    err(&call(
        &c,
        "sql_select",
        json!({"connection": "crash-csvs", "sql": "SELECT OWNER FROM crashes"}),
    )
    .await);
    // The cache's own tables aren't datasets.
    for sql in [
        "SELECT * FROM columbus_sources",
        "SELECT * FROM gpkg_contents",
    ] {
        err(&call(
            &c,
            "sql_select",
            json!({"connection": "crash-csvs", "sql": sql}),
        )
        .await);
    }
    // Files connections are read only.
    let e = err(&call(
        &c,
        "propose_edits",
        json!({"connection": "crash-csvs", "dataset": "crashes",
               "changes": [{"op": "delete", "key": 1}]}),
    )
    .await);
    assert!(e.contains("not supported on files"), "{e}");

    // Buffer (after a reproject out of degrees) and export as gpkg.
    let pts = r["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let p = ok(&call(
        &c,
        "vector_op",
        json!({"op": "reproject", "inputs": {"layer": pts}, "params": {"crs": "EPSG:2277"}}),
    )
    .await);
    let b = ok(&call(
        &c,
        "vector_op",
        json!({"op": "buffer", "inputs": {"layer": p["result"]["layer"]["handle"]},
               "params": {"distance": 100, "unit": "ft"}}),
    )
    .await);
    assert_eq!(b["result"]["layer"]["summary"]["rows"], 2, "{b}");
    let x = ok(&call(
        &c,
        "export_output",
        json!({"handle": b["result"]["layer"]["handle"], "folder": "share", "name": "crash buffers", "format": "gpkg"}),
    )
    .await);
    assert_eq!(x["result"]["status"], "exported", "{x}");
    let exported: Vec<PathBuf> = std::fs::read_dir(env.root.join("share"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert!(
        exported
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "gpkg")),
        "{exported:?}"
    );
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn files_it_cannot_offer_are_skipped_with_the_reason() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup();
    let c = start(&env.root, &py).await;

    // F-1: dots become underscores; only the CSV is a dataset.
    let l = ok(&call(&c, "list_datasets", json!({"connection": "drops"})).await);
    assert_eq!(names(&l), ["roads_v2"], "{l}");
    let skipped = l["result"]["skipped"].as_array().unwrap().clone();
    let reason = |file: &str| -> String {
        skipped
            .iter()
            .find(|s| s["file"] == file)
            .unwrap_or_else(|| panic!("{file} not under skipped: {l}"))["reason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(reason("~$book.xlsx").contains("lock file"), "{l}");
    assert!(reason("book.xls").contains("save as .xlsx"), "{l}");
    if env.linked {
        assert_eq!(
            reason("elsewhere.csv"),
            "link leads outside the connection folder",
            "{l}"
        );
        assert!(!l.to_string().contains("SECRET"), "{l}");
    }

    // roads_v2 reads; points in EPSG:4326.
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "drops", "dataset": "roads_v2"}),
    )
    .await);
    assert_eq!(d["result"]["crs"]["code"], "EPSG:4326", "{d}");
    ok(&call(
        &c,
        "query_features",
        json!({"connection": "drops", "dataset": "roads_v2"}),
    )
    .await);

    // The .xls named directly is refused with the fix.
    for (tool, args) in [
        (
            "describe_dataset",
            json!({"connection": "drops", "dataset": "book"}),
        ),
        (
            "query_features",
            json!({"connection": "drops", "dataset": "book"}),
        ),
        (
            "sql_select",
            json!({"connection": "drops", "sql": "SELECT * FROM book"}),
        ),
    ] {
        let e = err(&call(&c, tool, args).await);
        assert!(e.contains("save as .xlsx"), "{tool}: {e}");
    }
    // The link's would-be dataset is refused too, and nothing of it is read.
    if env.linked {
        let e = err(&call(
            &c,
            "sql_select",
            json!({"connection": "drops", "sql": "SELECT * FROM elsewhere"}),
        )
        .await);
        assert!(e.contains("outside the connection folder"), "{e}");
        assert!(!e.contains("hush"), "{e}");
    }

    // Removing the connection deletes its cache, as for workspace files.
    let cache = env.root.join("workspaces/cache/files/drops.gpkg");
    assert!(cache.is_file());
    std::fs::remove_file(env.root.join("connections/drops.yaml")).unwrap();
    let mut gone = false;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let _ = call(&c, "list_connections", json!({})).await;
        if !cache.exists() {
            gone = true;
            break;
        }
    }
    assert!(gone, "cache still at {}", cache.display());
    c.cancel().await.unwrap();
}

/// One fixture per format, written by the engine's own fixture script
/// (`workers/python/tests/files_fixtures.py`), under a policy listing the
/// old type `file` (D-10).
fn formats_setup(py: &str, policy_type: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/tests/files_fixtures.py")
        .canonicalize()
        .unwrap();
    let made = StdCommand::new(py)
        .arg(&script)
        .arg(root.join("formats"))
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "fixtures: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    std::fs::write(
        root.join("connections/formats.yaml"),
        format!(
            "id: formats\ntype: files\npath: {}\ndatasets:\n  allow: [\"*\"]\n",
            q(&root.join("formats"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [{policy_type}]\napprovals: {{ page: false }}\n"),
    )
    .unwrap();
    (dir, root)
}

fn field_type(d: &Value, f: &str) -> String {
    d["result"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == f)
        .unwrap_or_else(|| panic!("no field {f}: {d}"))["type"]
        .as_str()
        .unwrap()
        .to_string()
}

fn check_ids(d: &Value) -> Vec<String> {
    d["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn every_format_lists_describes_and_queries() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let (_dir, root) = formats_setup(&py, "files");
    let c = start(&root, &py).await;

    let l = ok(&call(&c, "list_datasets", json!({"connection": "formats"})).await);
    assert_eq!(
        names(&l),
        [
            "crashes",
            "dupes",
            "empty",
            "inventory/2025",
            "inventory/2026",
            "legacy",
            "parcels",
            "places/Parks",
            "places/Schools",
            "roads"
        ],
        "{l}"
    );
    let skipped = l["result"]["skipped"].as_array().unwrap();
    let reason = |file: &str| {
        skipped
            .iter()
            .find(|s| s["file"] == file)
            .unwrap_or_else(|| panic!("{file} not skipped: {l}"))["reason"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(reason("settings.json").contains("isn't GeoJSON"), "{l}");
    assert!(reason("inventory.xlsx").contains("empty sheet"), "{l}");

    // Each format describes (geometry and CRS) and queries.
    for (name, gtype, code, rows) in [
        ("inventory/2025", Some("POINT"), Some("EPSG:4326"), 2),
        ("roads", Some("LINESTRING"), Some("EPSG:4326"), 2),
        ("crashes", Some("POINT"), Some("EPSG:4326"), 3),
        ("parcels", Some("POLYGON"), Some("EPSG:2277"), 1),
        ("places/Parks", Some("POINT"), Some("EPSG:4326"), 2),
        ("places/Schools", Some("POINT"), Some("EPSG:4326"), 1),
        ("legacy", None, None, 1),
        ("dupes", None, None, 1),
        ("empty", None, None, 0),
        ("inventory/2026", None, None, 0),
    ] {
        let d = ok(&call(
            &c,
            "describe_dataset",
            json!({"connection": "formats", "dataset": name}),
        )
        .await);
        assert_eq!(d["result"]["geometry_type"].as_str(), gtype, "{name}: {d}");
        assert_eq!(d["result"]["crs"]["code"].as_str(), code, "{name}: {d}");
        assert_eq!(d["result"]["rows"], rows, "{name}: {d}");
        let r = ok(&call(
            &c,
            "query_features",
            json!({"connection": "formats", "dataset": name}),
        )
        .await);
        assert_eq!(r["result"]["layer"]["summary"]["rows"], rows, "{name}: {r}");
    }

    // Typing (R-4, R-6, R-6b), encoding (R-5) and headers (R-6c).
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "formats", "dataset": "inventory/2025"}),
    )
    .await);
    assert_eq!(field_type(&d, "CSJ"), "TEXT");
    assert_eq!(field_type(&d, "Milepost"), "REAL");
    assert_eq!(field_type(&d, "Date"), "DATE");
    assert_eq!(field_type(&d, "Serial"), "INTEGER");
    assert_eq!(field_type(&d, "Name_2"), "TEXT");
    let s = ok(&call(
        &c,
        "sql_select",
        json!({"connection": "formats",
               "sql": "SELECT CSJ, \"Date\", Total FROM \"inventory/2025\" WHERE Milepost > 2"}),
    )
    .await);
    assert_eq!(
        s["result"]["rows"],
        json!([["000123456", "2026-11-08", 12.5]]),
        "{s}"
    );
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "formats", "dataset": "legacy"}),
    )
    .await);
    assert!(check_ids(&d).contains(&"file.encoding".to_string()), "{d}");
    let s = ok(&call(
        &c,
        "sql_select",
        json!({"connection": "formats", "sql": "SELECT Name FROM legacy"}),
    )
    .await);
    assert_eq!(s["result"]["rows"], json!([["Café"]]), "{s}");
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "formats", "dataset": "dupes"}),
    )
    .await);
    assert_eq!(field_type(&d, "Name_2"), "TEXT");
    assert_eq!(field_type(&d, "field_4"), "TEXT");

    // R-7c: a deleted file's dataset is gone; queries say there is none.
    std::fs::remove_file(root.join("formats/roads.geojson")).unwrap();
    let l = ok(&call(&c, "list_datasets", json!({"connection": "formats"})).await);
    assert!(!names(&l).contains(&"roads".to_string()), "{l}");
    for (tool, args) in [
        (
            "query_features",
            json!({"connection": "formats", "dataset": "roads"}),
        ),
        (
            "sql_select",
            json!({"connection": "formats", "sql": "SELECT * FROM roads"}),
        ),
        (
            "describe_dataset",
            json!({"connection": "formats", "dataset": "roads"}),
        ),
    ] {
        let e = err(&call(&c, tool, args).await);
        assert!(e.contains("no dataset roads"), "{tool}: {e}");
    }
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn the_old_file_type_in_policy_allows_files_connections() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    // D-10: `connection_types: [file]` allows a files connection; the
    // policy file is read as written, never rewritten.
    let (_dir, root) = formats_setup(&py, "file");
    let before = std::fs::read(root.join("policy.yaml")).unwrap();
    let c = start(&root, &py).await;
    let l = ok(&call(&c, "list_datasets", json!({"connection": "formats"})).await);
    assert!(names(&l).contains(&"crashes".to_string()), "{l}");
    ok(&call(
        &c,
        "query_features",
        json!({"connection": "formats", "dataset": "crashes"}),
    )
    .await);
    c.cancel().await.unwrap();
    assert_eq!(std::fs::read(root.join("policy.yaml")).unwrap(), before);
}
