//! Esri shapefiles and file geodatabases: a `shapefile` connection (a folder
//! of shapefiles) and a `filegdb` connection (a `.gdb`) list their layers,
//! read them under the same rules as any connection (row filters, hidden
//! fields, the SQL checks, bounding boxes), and can't be written to.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
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

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(py: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::create_dir_all(root.join("shp")).unwrap();
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box, Point
root = sys.argv[1]
parcels = gpd.GeoDataFrame(
    {"PARCEL_ID": ["P1", "P2", "P3"], "STATUS": ["A", "R", "A"], "OWNER": ["Ann", "Bo", "Cy"], "ACRES": [1.5, 2.0, 3.0]},
    geometry=[box(0, 0, 1, 1), box(1, 1, 2, 2), box(10, 10, 11, 11)], crs="EPSG:2277")
parcels.to_file(os.path.join(root, "shp", "parcels.shp"), engine="pyogrio")
gpd.GeoDataFrame({"ROAD": ["R1"]}, geometry=[Point(0, 0)], crs="EPSG:2277").to_file(
    os.path.join(root, "shp", "roads.shp"), engine="pyogrio")
# A dot in the name: not offered (SQL would read it as a file and a layer).
parcels.to_file(os.path.join(root, "shp", "parcels.2024.shp"), engine="pyogrio")
# A Date field, which GDAL's SQLite dialect compares as text YYYY/MM/DD.
import numpy as np, shapely
from pyogrio.raw import write
os.makedirs(os.path.join(root, "dated"))
write(os.path.join(root, "dated", "visits.shp"),
      geometry=np.array([shapely.to_wkb(Point(0, 0)), shapely.to_wkb(Point(1, 1))], dtype=object),
      field_data=[np.array(["2024-01-02", "2024-05-06"], dtype="datetime64[D]")], fields=["SEEN"],
      geometry_type="Point", crs="EPSG:2277", driver="ESRI Shapefile")
gdb = os.path.join(root, "county.gdb")
parcels.to_file(gdb, layer="parcels", driver="OpenFileGDB", engine="pyogrio")
gpd.GeoDataFrame({"ROAD": ["R1"]}, geometry=[Point(0, 0)], crs="EPSG:2277").to_file(
    gdb, layer="roads", driver="OpenFileGDB", engine="pyogrio")
"#;
    assert!(StdCommand::new(py)
        .args(["-c", script])
        .arg(&root)
        .status()
        .unwrap()
        .success());
    let q = |p: PathBuf| serde_json::to_string(&p.display().to_string()).unwrap();
    std::fs::write(
        root.join("connections/shp.yaml"),
        format!(
            "id: shp\ntype: shapefile\npath: {}\ndatasets:\n  allow: [\"*\"]\nrow_filter: \"STATUS = 'A'\"\n",
            q(root.join("shp"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/gdb.yaml"),
        format!(
            "id: gdb\ntype: filegdb\npath: {}\ndatasets:\n  allow: [\"*\"]\n",
            q(root.join("county.gdb"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/dated.yaml"),
        format!(
            "id: dated\ntype: shapefile\npath: {}\ndatasets:\n  allow: [\"*\"]\nrow_filter: \"SEEN >= '2024-03-01'\"\n",
            q(root.join("dated"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/dated-open.yaml"),
        format!(
            "id: dated-open\ntype: shapefile\npath: {}\ndatasets:\n  allow: [\"*\"]\n",
            q(root.join("dated"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [shapefile, filegdb]\nhide_fields: [OWNER]\nallow_edits: true\n",
    )
    .unwrap();
    Env { _dir: dir, root }
}

fn command(root: &Path, py: &str) -> tokio::process::Command {
    let worker =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/python/columbus_worker.py");
    let root = root.to_path_buf();
    let py = py.to_string();
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
    })
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

fn names(v: &Value) -> Vec<String> {
    v["result"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn shapefiles_and_file_geodatabases_read_under_the_rules() {
    let Some(py) = python() else {
        eprintln!("skipped: needs Python with geopandas, pyogrio and shapely");
        return;
    };
    let env = setup(&py);
    let c = ().serve(TokioChildProcess::new(command(&env.root, &py)).unwrap()).await.unwrap();

    // Each layer is a dataset.
    for conn in ["shp", "gdb"] {
        let l = ok(&call(&c, "list_datasets", json!({"connection": conn})).await);
        let mut n = names(&l);
        n.sort();
        assert_eq!(n, ["parcels", "roads"], "{conn}: {l}");
        // The hidden field is not listed.
        assert!(!l.to_string().contains("OWNER"), "{conn}: {l}");
    }

    // The shapefile connection's row filter keeps only STATUS 'A'.
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "shp", "dataset": "parcels"}),
    )
    .await);
    assert_eq!(q["result"]["layer"]["summary"]["rows"], 2, "{q}");
    // The geodatabase has none: all three, in its CRS.
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "gdb", "dataset": "parcels"}),
    )
    .await);
    assert_eq!(q["result"]["layer"]["summary"]["rows"], 3, "{q}");
    assert!(q.to_string().contains("2277"), "{q}");
    // A bounding box narrows it.
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "gdb", "dataset": "parcels", "bbox": [0, 0, 2.5, 2.5]}),
    )
    .await);
    assert_eq!(q["result"]["layer"]["summary"]["rows"], 2, "{q}");

    // Free SQL, checked as for any SQLite-dialect connection.
    let s = ok(&call(&c, "sql_select", json!({"connection": "gdb", "sql": "SELECT PARCEL_ID, ACRES FROM parcels WHERE ACRES > 1.7 ORDER BY PARCEL_ID"})).await);
    let text = s.to_string();
    assert!(
        text.contains("P2") && text.contains("P3") && !text.contains("\"P1\""),
        "{s}"
    );
    for sql in [
        // The hidden field.
        "SELECT OWNER FROM parcels",
        // Another file, by path: GDAL's SQLite dialect would open it.
        "SELECT * FROM \"/etc/passwd\".\"x\"",
        // GDAL's own functions.
        "SELECT ogr_version() FROM parcels",
        // The geodatabase's own tables.
        "SELECT * FROM GDB_Items",
    ] {
        let r = call(&c, "sql_select", json!({"connection": "gdb", "sql": sql})).await;
        assert_eq!(r.is_error, Some(true), "{sql}: {:?}", r.content);
    }
    // The row filter holds in free SQL on the shapefiles too.
    let s = ok(&call(
        &c,
        "sql_select",
        json!({"connection": "shp", "sql": "SELECT PARCEL_ID FROM parcels"}),
    )
    .await);
    assert!(!s.to_string().contains("P2"), "{s}");

    // describe_dataset: fields, CRS, the row count under the filter.
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "shp", "dataset": "parcels"}),
    )
    .await);
    assert_eq!(d["result"]["rows"], 2, "{d}");
    assert!(
        d["result"]["extent"].is_null(),
        "a filtered dataset's stored extent: {d}"
    );

    // A row filter on a shapefile date is refused: it would compare text
    // "2024/01/02" with "2024-03-01" and keep both rows.
    let r = call(
        &c,
        "query_features",
        json!({"connection": "dated", "dataset": "visits"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true), "{:?}", r.content);
    assert!(
        format!("{:?}", r.content).contains("date"),
        "{:?}",
        r.content
    );
    // Without a filter it reads, and describe_dataset warns.
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "dated-open", "dataset": "visits"}),
    )
    .await);
    assert!(d.to_string().contains("fields.text_dates"), "{d}");

    // Read only, whatever the policy allows.
    let r = call(
        &c,
        "propose_edits",
        json!({"connection": "gdb", "dataset": "parcels", "changes": [{"op": "delete", "key": 1}]}),
    )
    .await;
    assert_eq!(r.is_error, Some(true), "{:?}", r.content);
    let why = format!("{:?}", r.content);
    assert!(
        why.contains("not supported on filegdb") || why.contains("read"),
        "{why}"
    );
    c.cancel().await.unwrap();
}
