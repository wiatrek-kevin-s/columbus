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
}
