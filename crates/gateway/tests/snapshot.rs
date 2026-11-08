//! map_snapshot: a session's layers drawn as a PNG the assistant can see,
//! from this session's own layers only, geometry only, nothing fetched.
//!
//! Needs Python with geopandas, pyogrio, shapely, pyproj and rasterio;
//! skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use base64::Engine as _;
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
                .args(["-c", "import geopandas, pyogrio, shapely, pyproj, rasterio"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

async fn start(root: &Path, py: &str) -> RunningService<RoleClient, ()> {
    let root = root.to_path_buf();
    let py = py.to_string();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_TOOLS", root.join("tools"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_LIVE_DIR", root.join("live"))
                .env("COLUMBUS_COMPANION", "off")
                .env(
                    "COLUMBUS_WORKER",
                    repo().join("workers/python/columbus_worker.py"),
                )
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_APPROVALS_BROWSER", "none")
                .env("COLUMBUS_LOG", "warn");
        });
    ().serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap()
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

#[tokio::test]
async fn the_assistant_sees_its_layers_as_a_picture() {
    let Some(py) = python() else {
        eprintln!("skipped: no Python with geopandas and rasterio");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let gpkg = root.join("ak.gpkg");
    // Polygons on both sides of the 180th meridian, as the Aleutians are.
    let script = r#"
import sys
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["West", "East", "Main"], "SECRET": ["a", "b", "c"]},
    geometry=[box(172, 51, 179.9, 53), box(-179.9, 51, -170, 53), box(-160, 55, -140, 65)],
    crs="EPSG:4269").to_file(sys.argv[1], layer="regions", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(&py)
        .arg("-c")
        .arg(script)
        .arg(&gpkg)
        .status()
        .unwrap()
        .success());
    std::fs::write(
        root.join("connections/ak.yaml"),
        format!(
            "id: ak\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"*\"]\n",
            serde_json::to_string(&gpkg.display().to_string()).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(root.join("policy.yaml"), "connection_types: [gpkg]\n").unwrap();
    let c = start(root, &py).await;

    let q = call(
        &c,
        "query_features",
        json!({"connection": "ak", "dataset": "regions"}),
    )
    .await;
    assert_ne!(q.is_error, Some(true), "{:?}", q.content);
    let h = q.structured_content.unwrap()["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string();

    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [h], "width": 800, "height": 600}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    let img = r
        .content
        .iter()
        .find_map(|c| c.as_image())
        .expect("a picture for the assistant");
    assert_eq!(img.mime_type, "image/png");
    let png = base64::engine::general_purpose::STANDARD
        .decode(&img.data)
        .unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    let out = r.structured_content.unwrap();
    let res = &out["result"];
    assert_eq!(res["layers"][0]["features"], 3, "{out}");
    assert_eq!(
        res["wrapped_antimeridian"], true,
        "drawn in one piece: {out}"
    );
    assert!(
        res["width"].as_u64().unwrap() <= 800 && res["height"].as_u64().unwrap() <= 600,
        "{out}"
    );
    let handle = res["handle"].as_str().unwrap();
    assert!(
        handle.starts_with("ws://") && handle.ends_with(".png"),
        "{out}"
    );
    // Geometry only: no attribute value reaches the assistant in words either.
    assert!(!out.to_string().contains("SECRET"), "{out}");

    // Only this session's own layers, and 1 to 8 of them.
    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": ["ws://other/results/x.gpkg"]}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    let nine: Vec<&str> = std::iter::repeat_n(h.as_str(), 9).collect();
    let r = call(&c, "map_snapshot", json!({"layers": nine})).await;
    assert_eq!(r.is_error, Some(true));
    let r = call(&c, "map_snapshot", json!({"layers": [handle]})).await;
    assert_eq!(r.is_error, Some(true), "a picture is not a layer to draw");

    // Labels by a field of the layer's, when asked: a different picture,
    // with the count drawn; a field the layer hasn't, or a layer not drawn,
    // is refused.
    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [h], "width": 800, "height": 600, "labels": {h.clone(): "NAME"}}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    let labelled = r
        .content
        .iter()
        .find_map(|c| c.as_image())
        .unwrap()
        .data
        .clone();
    assert_ne!(labelled, img.data, "the labels are drawn");
    let out = r.structured_content.unwrap();
    assert_eq!(out["result"]["layers"][0]["label_field"], "NAME", "{out}");
    assert!(
        out["result"]["layers"][0]["labels_drawn"].as_u64().unwrap() >= 1,
        "{out}"
    );
    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [h], "labels": {h.clone(): "NOPE"}}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [h], "labels": {"ws://x/y.gpkg": "NAME"}}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));

    // Audited with the rows drawn.
    let audit: String = std::fs::read_dir(root.join("audit"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect();
    assert!(audit.contains("map_snapshot"), "audited");
    c.cancel().await.unwrap();

    // An organization can turn it off.
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\ndeny_tools: [map_snapshot]\n",
    )
    .unwrap();
    let c = start(root, &py).await;
    let r = call(&c, "map_snapshot", json!({"layers": ["ws://x/y.gpkg"]})).await;
    assert_eq!(r.is_error, Some(true));
    c.cancel().await.unwrap();

    // Or keep it to geometry only.
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\nsnapshot_labels: false\n",
    )
    .unwrap();
    let c = start(root, &py).await;
    let q = call(
        &c,
        "query_features",
        json!({"connection": "ak", "dataset": "regions"}),
    )
    .await;
    let h = q.structured_content.unwrap()["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [h], "labels": {h.clone(): "NAME"}}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(format!("{:?}", r.content).contains("snapshot_labels"));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_raster_of_the_session_is_drawn_underneath() {
    let Some(py) = python() else {
        eprintln!("skipped: no Python with geopandas and rasterio");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::create_dir_all(root.join("rasters")).unwrap();
    let gpkg = root.join("tx.gpkg");
    let script = r#"
import sys
import numpy as np
import geopandas as gpd
import rasterio
from rasterio.transform import from_bounds
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["Tract"]}, geometry=[box(-97.8, 30.7, -97.7, 30.8)],
    crs="EPSG:4269").to_file(sys.argv[1], layer="tracts", driver="GPKG", engine="pyogrio")
z = (np.arange(100 * 100, dtype="float32").reshape(1, 100, 100) + 100.0)
with rasterio.open(sys.argv[2], "w", driver="GTiff", width=100, height=100, count=1, dtype="float32",
                   crs="EPSG:3081", transform=from_bounds(1_190_000, 930_000, 1_240_000, 980_000, 100, 100)) as d:
    d.write(z)
"#;
    assert!(StdCommand::new(&py)
        .arg("-c")
        .arg(script)
        .arg(&gpkg)
        .arg(root.join("rasters/dem.tif"))
        .status()
        .unwrap()
        .success());
    let q = |p: PathBuf| serde_json::to_string(&p.display().to_string()).unwrap();
    std::fs::write(
        root.join("connections/tx.yaml"),
        format!(
            "id: tx\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"*\"]\n",
            q(gpkg.clone())
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections/dem.yaml"),
        format!(
            "id: dem\ntype: rasters\npath: {}\ndatasets:\n  allow: [\"*\"]\n",
            q(root.join("rasters"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg, rasters]\n",
    )
    .unwrap();
    let c = start(root, &py).await;

    let q = call(
        &c,
        "query_features",
        json!({"connection": "tx", "dataset": "tracts"}),
    )
    .await;
    assert_ne!(q.is_error, Some(true), "{:?}", q.content);
    let layer = q.structured_content.unwrap()["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let r = call(
        &c,
        "raster_op",
        json!({"op": "reproject", "inputs": {"raster": "dem:dem.tif"}, "params": {"crs": "EPSG:4269", "resampling": "bilinear"}}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    let out = r.structured_content.unwrap();
    let raster = out["result"]["layer"]["handle"]
        .as_str()
        .or_else(|| out["result"]["handle"].as_str())
        .unwrap_or_else(|| panic!("a raster handle: {out}"))
        .to_string();

    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [layer], "imagery": raster, "width": 600, "height": 600}),
    )
    .await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    let out = r.structured_content.unwrap();
    assert_eq!(out["result"]["imagery"]["bands"], "one band, grey", "{out}");
    assert_eq!(out["result"]["imagery"]["covers"], true, "{out}");

    // A layer of the session's that is not a GeoTIFF is not imagery.
    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [layer], "imagery": layer}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(
        format!("{:?}", r.content).contains("not a GeoTIFF"),
        "{:?}",
        r.content
    );

    // Only this session's own rasters.
    let r = call(
        &c,
        "map_snapshot",
        json!({"layers": [layer], "imagery": "ws://other/results/x.tif"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    c.cancel().await.unwrap();
}
