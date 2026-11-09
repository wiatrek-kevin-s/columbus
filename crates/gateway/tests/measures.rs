//! M (measure) values survive end to end through the real `columbus-mcp`
//! binary and Python worker (T-19, REQ-24), on a GeoPackage of LINESTRING
//! M routes: query_features, vector_op reproject, join, spatial_join and
//! dissolve, export_output to gpkg, gdb and shp; buffer drops M with a
//! `geometry.m_dropped` info check. `oracle.rs` runs the same chain on
//! Oracle LRS routes in CI. The streamed writer, plugin extracts and
//! personal tools are covered in `workers/python/tests/test_measures.py`.
//!
//! Needs Python with geopandas, pyogrio, shapely 2.1+ and pyproj; skipped
//! otherwise.

mod measures_chain;
mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};

fn python() -> Option<String> {
    support::python_for(
        "import geopandas, pyogrio, pyproj, shapely; assert hasattr(shapely, 'has_m')",
    )
}

/// 40 routes (route i: id R<i>, district 14 or 9, measures from
/// 1000 + 10 i), a districts table and two zone polygons, in EPSG:4326.
fn make_fixture(py: &str, path: &Path) {
    let script = r#"
import sys
import geopandas as gpd, pandas as pd, pyogrio, shapely
sys.path.insert(0, sys.argv[2])
import columbus_gis as g
path = sys.argv[1]
n = 40
routes = gpd.GeoDataFrame(
    {"route_id": [f"R{i:04d}" for i in range(n)], "district": [14 if i % 2 == 0 else 9 for i in range(n)]},
    geometry=[shapely.from_wkt(f"LINESTRING M (-97.8 {30.2 + i * 0.0001} {1000 + 10 * i}, "
                               f"-97.7999 {30.2001 + i * 0.0001} {1005 + 10 * i})") for i in range(n)],
    crs="EPSG:4326")
g.write_ogr(routes, path, "GPKG", layer="routes")
pyogrio.write_dataframe(pd.DataFrame({"id": [14, 9], "name": ["Austin", "Waco"]}), path, layer="districts",
                        driver="GPKG", append=True)
gpd.GeoDataFrame({"zone": ["north", "south"]},
                 geometry=[shapely.box(-97.9, 30.202, -97.6, 30.4), shapely.box(-97.9, 30.0, -97.6, 30.202)],
                 crs="EPSG:4326").to_file(path, layer="zones", driver="GPKG", engine="pyogrio", append=True)
"#;
    let workers = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/python");
    let ok = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(path)
        .arg(&workers)
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
    for d in ["connections", "share"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    make_fixture(py, &root.join("routes.gpkg"));
    let q = |p: PathBuf| serde_json::to_string(&p.display().to_string()).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!(
            "connection_types: [gpkg]\nlimits: {{ rows: 1000, seconds: 120 }}\n\
             approvals: {{ page: false }}\n\
             exports:\n  - {{ id: share, path: {}, granted: true }}\n",
            q(root.join("share"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/routes.yaml"),
        format!(
            "id: routes\ntype: gpkg\npath: {}\ndescription: Routes with measures\n\
             datasets:\n  allow: [\"*\"]\n",
            q(root.join("routes.gpkg"))
        ),
    )
    .unwrap();
    Env { _dir: dir, root }
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
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
        }),
    )
    .unwrap();
    ().serve(transport).await.unwrap()
}

#[tokio::test]
async fn measures_survive_on_a_geopackage() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(&py);
    let client = start(&env.root, &py).await;
    measures_chain::run(
        &client,
        &py,
        &env.root.join("workspaces"),
        &env.root.join("share"),
        &measures_chain::Names {
            conn: "routes",
            routes: "routes",
            route_id: "route_id",
            district: "district",
            join: "districts",
            join_fields: None,
            join_left: "district",
            join_right: "id",
            other: "zones",
        },
    )
    .await;
    client.cancel().await.unwrap();
}
