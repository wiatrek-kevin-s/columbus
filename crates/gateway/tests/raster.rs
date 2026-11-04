//! End to end for rasters: a `rasters` connection (a folder of GeoTIFFs),
//! describe_dataset on it, names that would leave the folder refused, and
//! raster_op clip (by a GeoPackage layer), slope and zonal_stats, through
//! the real `columbus-mcp` binary and Python worker.
//!
//! Needs Python with geopandas, pyogrio, shapely, pyproj and rasterio;
//! skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python(import: &str) -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", import])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

/// A DEM around Austin in EPSG:3081 (50 m cells) rising 0.05 m per metre
/// toward the east, so its slope is atan(0.05) = 2.862 degrees; a file the
/// connection denies; a ".tif" that is really VRT XML; one outside the
/// folder. A GeoPackage with the county (in lon/lat) and two zones, one
/// half outside the county.
fn make_fixture(py: &str, folder: &Path) {
    let script = r#"
import sys, os
import numpy as np, rasterio, geopandas as gpd
from rasterio.transform import from_origin
from shapely.geometry import Polygon, box
folder = sys.argv[1]
rasters = os.path.join(folder, "rasters")
for d in ("dem", "secret"):
    os.makedirs(os.path.join(rasters, d), exist_ok=True)
x0, y0, x1, y1 = gpd.GeoSeries([box(-97.9, 30.1, -97.7, 30.4)], crs="EPSG:4326").to_crs("EPSG:3081").total_bounds
x0, y1 = float(np.floor(x0 / 50) * 50), float(np.ceil(y1 / 50) * 50)
cols, rows = int((x1 - x0) // 50) + 1, int((y1 - y0) // 50) + 1
z = (150.0 + 0.05 * 50 * np.arange(cols, dtype="float64"))[np.newaxis, :].repeat(rows, axis=0).astype("float32")
def write(path, data):
    with rasterio.open(path, "w", driver="GTiff", height=rows, width=cols, count=1, dtype="float32",
                       crs="EPSG:3081", transform=from_origin(x0, y1, 50, 50), nodata=-9999) as d:
        d.write(data, 1)
write(os.path.join(rasters, "dem", "travis_dem.tif"), z)
write(os.path.join(rasters, "secret", "restricted.tif"), z)
write(os.path.join(folder, "outside.tif"), z)
with open(os.path.join(rasters, "fake.tif"), "w") as f:
    f.write('<VRTDataset rasterXSize="1" rasterYSize="1"><VRTRasterBand dataType="Byte" band="1">'
            '<SimpleSource><SourceFilename>/etc/passwd</SourceFilename></SimpleSource></VRTRasterBand></VRTDataset>')
gpkg = os.path.join(folder, "austin.gpkg")
gpd.GeoDataFrame({"name": ["Travis"]},
    geometry=[Polygon([(-97.85, 30.15), (-97.745, 30.15), (-97.745, 30.35), (-97.85, 30.35)])],
    crs="EPSG:4326").to_file(gpkg, layer="county", driver="GPKG", engine="pyogrio")
gpd.GeoDataFrame({"zone": ["inside", "half out"]},
    geometry=[box(-97.82, 30.20, -97.78, 30.24), box(-97.76, 30.20, -97.72, 30.24)],
    crs="EPSG:4326").to_crs("EPSG:2277").to_file(gpkg, layer="zones", driver="GPKG", engine="pyogrio")
"#;
    let ok = StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(folder)
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
    make_fixture(py, &root);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("outside.tif"), root.join("rasters/link.tif"))
            .unwrap();
        std::os::unix::fs::symlink(
            root.join("rasters/secret/restricted.tif"),
            root.join("rasters/dem/restricted.tif"),
        )
        .unwrap();
    }
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg, rasters]\nlimits: { rows: 1000, seconds: 120 }\n",
    )
    .unwrap();
    let q = |p: PathBuf| serde_json::to_string(&p.display().to_string()).unwrap();
    std::fs::write(
        root.join("connections").join("dem.yaml"),
        format!(
            "id: dem\ntype: rasters\npath: {}\ndescription: Elevation\n\
             datasets:\n  allow: [\"*\"]\n  deny: [\"secret/*\"]\n",
            q(root.join("rasters"))
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("austin-ref.yaml"),
        format!(
            "id: austin-ref\ntype: gpkg\npath: {}\ndatasets:\n  allow: [\"county\", \"zones\"]\n",
            q(root.join("austin.gpkg"))
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

fn check_ids(v: &Value) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

fn check<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check in {}", v["checks"]))
}

#[tokio::test]
async fn raster_session_on_a_folder_of_geotiffs() {
    let Some(py) = python("import geopandas, pyogrio, shapely, pyproj, rasterio") else {
        eprintln!("skipping: the GIS and raster engines (geopandas, rasterio) are not installed");
        return;
    };
    let env = setup(&py);
    let client = start(&env.root, &py).await;

    // The connection, and its files: denied ones and links are not listed.
    let c = ok(&call(&client, "describe_connection", json!({"connection": "dem"})).await);
    assert_eq!(c["result"]["engine_ready"], true);
    let l = ok(&call(&client, "list_datasets", json!({"connection": "dem"})).await);
    let names: Vec<&str> = l["result"]["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["dem/travis_dem.tif", "fake.tif"], "{l}");
    assert_eq!(l["result"]["not_allowed"], 1);

    // describe_dataset: bands, CRS facts, cell size, size and layout.
    let d = ok(&call(
        &client,
        "describe_dataset",
        json!({"connection": "dem", "dataset": "dem/travis_dem.tif"}),
    )
    .await);
    let r = &d["result"];
    assert_eq!(r["type"], "raster");
    assert_eq!(r["crs"]["code"], "EPSG:3081");
    assert_eq!(r["crs"]["geographic"], false);
    assert_eq!(r["bands"][0]["dtype"], "float32");
    assert_eq!(r["bands"][0]["nodata"], -9999);
    assert_eq!(r["resolution"], json!([50.0, 50.0]));
    assert!(r["width"].as_u64().unwrap() > 300, "{r}");
    assert_eq!(r["cog"], false);
    assert!(check_ids(&d).contains(&"crs.info".to_string()));
    assert!(!check_ids(&d).contains(&"raster.nodata".to_string()));

    // Names that leave the folder, denied files, links and fake TIFFs.
    for (bad, why) in [
        ("../outside.tif", "not a raster dataset name"),
        ("dem/../../outside.tif", "not a raster dataset name"),
        ("/etc/hosts.tif", "not a raster dataset name"),
        ("dem\\travis_dem.tif", "not a raster dataset name"),
        ("dem/travis_dem.tif:x", "not a raster dataset name"),
        (
            "secret/restricted.tif",
            "not in this connection's allowed datasets",
        ),
        ("fake.tif", "not TIFF"),
        ("DEM/travis_dem.tif", "no raster|spelled differently"),
    ] {
        let e = err(&call(
            &client,
            "describe_dataset",
            json!({"connection": "dem", "dataset": bad}),
        )
        .await);
        assert!(why.split('|').any(|w| e.contains(w)), "{bad}: {e}");
    }
    #[cfg(unix)]
    for link in ["link.tif", "dem/restricted.tif"] {
        let e = err(&call(
            &client,
            "describe_dataset",
            json!({"connection": "dem", "dataset": link}),
        )
        .await);
        assert!(e.contains("outside") || e.contains("link"), "{link}: {e}");
    }
    let e = err(&call(
        &client,
        "query_features",
        json!({"connection": "dem", "dataset": "dem/travis_dem.tif"}),
    )
    .await);
    assert!(e.contains("rasters connection"), "{e}");
    let e = err(&call(
        &client,
        "sql_select",
        json!({"connection": "dem", "sql": "SELECT 1"}),
    )
    .await);
    assert!(e.contains("rasters connection"), "{e}");

    // Clip by the county from the GeoPackage (in lon/lat: moved to the DEM's CRS).
    let clip = ok(&call(
        &client,
        "raster_op",
        json!({"op": "clip", "inputs": {"raster": "dem:dem/travis_dem.tif", "mask": "austin-ref:county"}}),
    )
    .await);
    let ids = check_ids(&clip);
    for id in ["crs.agreement", "clip.cells", "raster.cog", "crs.info"] {
        assert!(ids.contains(&id.to_string()), "{id} missing: {ids:?}");
    }
    let labels = &check(&clip, "flow.labels")["message"];
    assert!(
        labels.as_str().unwrap().contains("connection:dem")
            && labels.as_str().unwrap().contains("connection:austin-ref"),
        "{labels}"
    );
    let clipped = clip["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(clipped.ends_with(".tif"), "{clipped}");
    assert_eq!(clip["result"]["layer"]["summary"]["cog"], true);
    assert!(clip["provenance"].is_string());

    // Slope needs the elevation unit, and is refused over the size limit.
    let e = err(&call(
        &client,
        "raster_op",
        json!({"op": "slope", "inputs": {"raster": clipped}}),
    )
    .await);
    assert!(e.contains("z_unit is required"), "{e}");
    let e = err(&call(
        &client,
        "raster_op",
        json!({"op": "reproject", "inputs": {"raster": clipped}, "params": {"crs": "EPSG:2277", "resampling": "bilinear", "resolution": 1}}),
    )
    .await);
    assert!(
        e.contains("output.too_large") && e.contains("1,000,000"),
        "{e}"
    );
    let slope = ok(&call(
        &client,
        "raster_op",
        json!({"op": "slope", "inputs": {"raster": clipped}, "params": {"z_unit": "m"}}),
    )
    .await);
    assert_eq!(check(&slope, "terrain.z_factor")["detail"]["z_factor"], 1.0);
    let slope_h = slope["result"]["layer"]["handle"]
        .as_str()
        .unwrap()
        .to_string();
    let file = env.root.join("workspaces");
    let found: Vec<PathBuf> = walk(&file)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("slope-"))
        })
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    let probe = StdCommand::new(&py)
        .args(["-c", "import sys, rasterio, numpy as np\nwith rasterio.open(sys.argv[1]) as d:\n a = d.read(1, masked=True)\n print(round(float(a.mean()), 3), d.tags(ns='IMAGE_STRUCTURE').get('LAYOUT'))"])
        .arg(&found[0])
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&probe.stdout);
    assert!(printed.contains("2.862 COG"), "{printed}");

    // Zonal statistics of the slope by zones in another CRS (feet).
    let z = ok(&call(
        &client,
        "raster_op",
        json!({"op": "zonal_stats", "inputs": {"raster": slope_h, "zones": "austin-ref:zones"}, "params": {"stats": ["count", "mean"]}}),
    )
    .await);
    let c = check(&z, "zonal.outside");
    assert_eq!(c["severity"], "warn");
    assert_eq!(c["detail"]["partly_outside"], 1);
    let zh = z["result"]["layer"]["handle"].as_str().unwrap().to_string();
    assert!(zh.ends_with(".gpkg"), "{zh}");
    assert_eq!(z["result"]["layer"]["summary"]["crs"]["code"], "EPSG:2277");
    let rows = ok(&call(&client, "read_output", json!({"handle": zh})).await);
    let cols: Vec<&str> = rows["result"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    let mi = cols.iter().position(|c| *c == "mean").unwrap();
    let ci = cols.iter().position(|c| *c == "count").unwrap();
    for row in rows["result"]["rows"].as_array().unwrap() {
        assert!((row[mi].as_f64().unwrap() - 2.862).abs() < 0.01, "{row}");
        assert!(row[ci].as_u64().unwrap() > 50, "{row}");
    }

    // info: band statistics, no output.
    let i = ok(&call(
        &client,
        "raster_op",
        json!({"op": "info", "inputs": {"raster": "dem:dem/travis_dem.tif"}}),
    )
    .await);
    assert!(i["result"].get("layer").is_none());
    assert_eq!(i["result"]["info"]["computed_on"], "all cells");
    assert_eq!(i["result"]["info"]["bands"][0]["min"], 150);

    // A raster is not a vector layer, and a layer is not a raster.
    let e = err(&call(
        &client,
        "vector_op",
        json!({"op": "make_valid", "inputs": {"layer": slope_h}}),
    )
    .await);
    assert!(e.contains("is a raster"), "{e}");
    let e = err(&call(
        &client,
        "raster_op",
        json!({"op": "info", "inputs": {"raster": zh}}),
    )
    .await);
    assert!(e.contains("not a raster"), "{e}");
    let e = err(&call(
        &client,
        "raster_op",
        json!({"op": "info", "inputs": {"raster": "austin-ref:county"}}),
    )
    .await);
    assert!(e.contains("gpkg connection"), "{e}");
    let e = err(&call(
        &client,
        "raster_op",
        json!({"op": "info", "inputs": {"raster": "dem:secret/restricted.tif"}}),
    )
    .await);
    assert!(
        e.contains("not in this connection's allowed datasets"),
        "{e}"
    );
    let e = err(&call(
        &client,
        "raster_op",
        json!({"op": "info", "inputs": {"dem": "dem:dem/travis_dem.tif"}}),
    )
    .await);
    assert!(e.contains("not an input of raster_op"), "{e}");
    client.cancel().await.unwrap();

    // Every call was audited and the chain verifies.
    let log = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("\"raster_op\""));
    columbus_shared::audit::verify(&log).expect("audit chain intact");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
