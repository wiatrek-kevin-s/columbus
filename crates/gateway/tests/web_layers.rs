//! Web map layers, server side (T-24; build brief part 2, Part 3: AF-22,
//! REQ-31, REQ-32, REQ-34, D-14, F-13, F-14), end to end: the real
//! gateway's map-page proxy and `columbus-mcp map-source …` against
//! `examples/fake_oauth_server.rs`, which plays the ArcGIS portal and one
//! fake service of each kind (XYZ, WMTS, WMS, MapServer tiled and dynamic,
//! ImageServer, FeatureServer, a secured MapServer, a redirect).
//!
//! The fake listens on 127.0.0.1 only; test builds let a few made-up host
//! names stand for it (`COLUMBUS_WEB_TEST_HOSTS`), so host policy can be
//! tested: `imagery.fake.test` and `other.fake.test` are on `map.hosts`,
//! `cdn.fake.test` (a WMTS's tile host) and `blocked.fake.test` are not.
//! The portal is `http://127.0.0.1:<port>` and vouches for its own host
//! only, so a secured MapServer on 127.0.0.1 gets the token and its twin
//! on `other.fake.test` does not (R-18).
//!
//! Fake tokens contain `zq7k`; no answer to the page may.

mod support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const MARK: &str = "zq7k";
const TEST_HOSTS: &str = "imagery.fake.test,cdn.fake.test,other.fake.test,blocked.fake.test";

fn example(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let p = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.exists(),
        "build the example first: cargo build --example {name}"
    );
    p
}

struct Fake(Child, u16);
impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

impl Fake {
    fn start() -> Fake {
        let mut child = Command::new(example("fake_oauth_server"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("FAKE_PORTAL_SERVERS", "")
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let port = line.trim().rsplit(':').next().unwrap().parse().unwrap();
        Fake(child, port)
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.1)
    }

    async fn log(&self) -> Vec<Value> {
        reqwest::get(self.url("/test/log"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

fn sent_token(e: &Value) -> Option<String> {
    ["authorization", "x_esri_authorization"]
        .iter()
        .find_map(|k| e[*k].as_str())
        .map(String::from)
}

fn host(e: &Value) -> String {
    e["host"].as_str().unwrap_or("").to_string()
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(port: u16, py: Option<&str>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::create_dir_all(root.join("map-sources")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [arcgis, gpkg]\nhosts: [\"127.0.0.1\"]\nhide_fields: [\"*SSN*\"]\n\
         approvals: { page: false }\n\
         map:\n  hosts: [\"127.0.0.1\", \"imagery.fake.test\", \"other.fake.test\"]\n",
    )
    .unwrap();
    // Web layers stay on with no `map.hosts` at all: each source's own
    // host is the person's to approve when they save it.
    std::fs::write(
        root.join("policy-open.yaml"),
        "connection_types: [arcgis, gpkg]\nhosts: [\"127.0.0.1\"]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("connections").join("svc.yaml"),
        format!(
            "id: svc\ntype: arcgis\nurl: http://127.0.0.1:{port}/arcgis/rest/services/Secure/FeatureServer\n\
             auth: {{type: oauth, portal: \"http://127.0.0.1:{port}\", client_id: FakeClientId123}}\n\
             description: Secured assets (fake)\ndatasets:\n  allow: [\"*\"]\n"
        ),
    )
    .unwrap();
    if let Some(py) = py {
        let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["A", "B"]},
    geometry=[box(-97.8, 30.2, -97.75, 30.25), box(-97.7, 30.2, -97.65, 30.25)],
    crs="EPSG:4326").to_file(os.path.join(sys.argv[1], "t.gpkg"), layer="tracts",
    driver="GPKG", engine="pyogrio")
"#;
        assert!(Command::new(py)
            .arg("-c")
            .arg(script)
            .arg(&root)
            .status()
            .unwrap()
            .success());
        let gpkg = serde_json::to_string(&root.join("t.gpkg").display().to_string()).unwrap();
        std::fs::write(
            root.join("connections").join("tiger.yaml"),
            format!("id: tiger\ntype: gpkg\npath: {gpkg}\ndatasets:\n  allow: [\"*\"]\n"),
        )
        .unwrap();
    }
    let ui = root.join("ui");
    std::fs::create_dir_all(ui.join("assets")).unwrap();
    std::fs::write(ui.join("index.html"), "<!doctype html><p>map</p>").unwrap();
    Env { _dir: dir, root }
}

fn envs(root: &Path, py: Option<&str>) -> Vec<(String, String)> {
    let p = |s: &str| root.join(s).display().to_string();
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let mut v = vec![
        ("COLUMBUS_POLICY".into(), p("policy.yaml")),
        ("COLUMBUS_CONNECTIONS".into(), p("connections")),
        ("COLUMBUS_MAP_SOURCES".into(), p("map-sources")),
        ("COLUMBUS_SERVERS".into(), p("servers")),
        ("COLUMBUS_PERSONAL_SERVERS".into(), p("personal-servers")),
        ("COLUMBUS_PLUGINS".into(), p("plugins")),
        ("COLUMBUS_WORKSPACES".into(), p("workspaces")),
        ("COLUMBUS_AUDIT_DIR".into(), p("audit")),
        ("COLUMBUS_LIVE_DIR".into(), p("live")),
        ("COLUMBUS_UI_DIR".into(), p("ui")),
        ("COLUMBUS_APPROVALS_LAUNCH_FILE".into(), p("launch.txt")),
        ("COLUMBUS_DEV_SECRETS".into(), "1".into()),
        ("COLUMBUS_DEV_SECRETS_DIR".into(), p("secrets")),
        ("COLUMBUS_FETCH_TEST_LOOPBACK".into(), "1".into()),
        ("COLUMBUS_WEB_TEST_HOSTS".into(), TEST_HOSTS.into()),
        ("COLUMBUS_WORKER".into(), worker.display().to_string()),
        ("COLUMBUS_LOG".into(), "warn".into()),
    ];
    if let Some(py) = py {
        v.push(("COLUMBUS_PYTHON".into(), py.to_string()));
    }
    v
}

/// `columbus-mcp map-source …` (or any CLI), as the Columbus app runs it.
fn cli(root: &Path, args: &[&str], extra: &[(&str, String)]) -> (bool, Value, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(args).stdin(Stdio::null());
    for (k, v) in envs(root, None) {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let o = cmd.output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let all = format!("{out}{}", String::from_utf8_lossy(&o.stderr));
    (
        o.status.success(),
        serde_json::from_str(&out).unwrap_or(Value::Null),
        all,
    )
}

fn save(root: &Path, src: Value) -> Value {
    let (ok, v, all) = cli(root, &["map-source", "save", &src.to_string()], &[]);
    assert!(ok, "saving {src}: {all}");
    v
}

async fn start(root: &Path, py: Option<&str>) -> RunningService<RoleClient, ()> {
    let vars = envs(root, py);
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            for (k, v) in &vars {
                cmd.env(k, v);
            }
        });
    ().serve(TokioChildProcess::new(cmd).unwrap())
        .await
        .unwrap()
}

async fn call(c: &RunningService<RoleClient, ()>, tool: &'static str, args: Value) -> Value {
    let mut p = CallToolRequestParams::new(tool);
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    let r: CallToolResult = c.call_tool(p).await.unwrap();
    assert_ne!(r.is_error, Some(true), "{tool}: {:?}", r.content);
    r.structured_content
        .clone()
        .unwrap_or_else(|| json!(r.content))
}

fn wait_for(path: &Path) -> String {
    let t = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if !s.is_empty() {
                return s;
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The map page: its base address and session token, got as the Columbus
/// app gets them (`POST /api/open`, then the launch link's exchange).
async fn page(root: &Path, session: &str) -> (String, String) {
    let live: Value = serde_json::from_str(&wait_for(
        &root.join("live").join(format!("{session}.json")),
    ))
    .unwrap();
    let base = format!("http://127.0.0.1:{}", live["port"]);
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{base}/api/open"))
        .header("x-columbus-open", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let link = wait_for(&root.join("launch.txt"));
    let launch = link.split("#t=").nth(1).unwrap().trim().to_string();
    let r = http
        .post(format!("{base}/api/session"))
        .header("origin", &base)
        .json(&json!({"launch": launch}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let tok = r.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    (base, tok)
}

/// Every answer the page got: headers and body, for the token search.
#[derive(Default)]
struct Seen(String);

impl Seen {
    async fn get(&mut self, url: &str, tok: Option<&str>) -> (StatusCode, String, Vec<u8>) {
        let mut b = reqwest::Client::new().get(url);
        if let Some(t) = tok {
            b = b.bearer_auth(t);
        }
        let r = b.send().await.unwrap();
        let code = r.status();
        let ct = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        for (k, v) in r.headers() {
            self.0
                .push_str(&format!("{k}: {}\n", v.to_str().unwrap_or("")));
        }
        let body = r.bytes().await.unwrap().to_vec();
        self.0.push_str(&String::from_utf8_lossy(&body));
        self.0.push('\n');
        (code, ct, body)
    }

    async fn json(&mut self, url: &str, tok: &str) -> (StatusCode, Value) {
        let (code, _, body) = self.get(url, Some(tok)).await;
        (code, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }
}

fn is_png(b: &[u8]) -> bool {
    b.starts_with(&[137, 80, 78, 71])
}

#[tokio::test]
async fn web_layers_go_through_the_gateway_only() {
    let py = support::python_for("import geopandas, pyogrio, shapely, pyproj");
    if py.is_none() {
        eprintln!("skipping map_snapshot only: the GIS engine is not installed");
    }
    let fake = Fake::start();
    let port = fake.1;
    let env = setup(port, py.as_deref());
    let root = env.root.clone();

    // A signed-in arcgis connection (svc), its F-3b entry as sign-in
    // stores it.
    let t: Value = reqwest::Client::new()
        .post(fake.url("/test/issue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    std::fs::create_dir_all(root.join("secrets")).unwrap();
    let entry = root.join("secrets").join("COLUMBUS_SECRET_ARCGIS_SVC");
    std::fs::write(
        &entry,
        json!({
            "access_token": t["access_token"], "refresh_token": t["refresh_token"],
            "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "portal": format!("http://127.0.0.1:{port}"),
        })
        .to_string(),
    )
    .unwrap();

    let img = |p: &str| format!("http://imagery.fake.test:{port}{p}");
    let svc = |name: &str| img(&format!("/web/arcgis/rest/services/{name}"));

    // --- The app's side: read capabilities, offer layers, save (F-13).
    let (ok, v, all) = cli(&root, &["map-source", "inspect", &img("/web/wms")], &[]);
    assert!(ok, "{all}");
    assert_eq!(v["kind"], "wms");
    let offered: Vec<&str> = v["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["layer"].as_str().unwrap())
        .collect();
    assert_eq!(offered, ["parcels"], "only layers in Web Mercator: {v}");
    assert_eq!(
        v["confirm"], false,
        "map.hosts is listed: no keyboard confirmation"
    );
    let (ok, v, _) = cli(
        &root,
        &[
            "map-source",
            "inspect",
            &svc("Dynamic/MapServer").replace("imagery", "blocked"),
        ],
        &[],
    );
    assert!(!ok);
    assert!(
        v["message"].as_str().unwrap().contains("blocked.fake.test"),
        "a host off map.hosts is refused, named: {v}"
    );
    // With no map.hosts, saving is the person's approval (the app asks at
    // the keyboard first, because `confirm` is true): a capability host is
    // recorded as approved with the source.
    let open = [(
        "COLUMBUS_POLICY",
        root.join("policy-open.yaml").display().to_string(),
    )];
    let wmts_cdn = img("/web/wmts/cdn/WMTSCapabilities.xml");
    let (ok, v, all) = cli(&root, &["map-source", "inspect", &wmts_cdn], &open);
    assert!(ok, "{all}");
    assert_eq!(v["confirm"], true);
    assert_eq!(v["layers"][0]["hosts"], json!(["cdn.fake.test"]));
    let (ok, v, all) = cli(
        &root,
        &[
            "map-source",
            "save",
            &json!({"id": "wl-open", "title": "Open-policy orthophoto", "kind": "wmts", "url": wmts_cdn, "layer": "ortho"}).to_string(),
        ],
        &open,
    );
    assert!(ok, "{all}");
    assert_eq!(v["hosts"], json!(["cdn.fake.test"]));
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("map-sources").join(".hosts.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(rec["wl-open"]["hosts"], json!(["cdn.fake.test"]));
    let (ok, _, _) = cli(&root, &["map-source", "remove", "wl-open"], &[]);
    assert!(ok);
    assert!(!root.join("map-sources").join("wl-open.yaml").exists());
    assert!(!root.join("map-sources").join(".hosts.json").exists());
    // Under the listed policy, the same layer is refused: its tiles come
    // from cdn.fake.test, which is not on map.hosts.
    let (ok, v, _) = cli(
        &root,
        &[
            "map-source",
            "save",
            &json!({"id": "wl-cdn", "title": "CDN orthophoto", "kind": "wmts", "url": wmts_cdn, "layer": "ortho"}).to_string(),
        ],
        &[],
    );
    assert!(!ok);
    assert!(
        v["message"].as_str().unwrap().contains("cdn.fake.test"),
        "{v}"
    );

    let sources = [
        json!({"id": "wl-xyz", "title": "Fake XYZ streets", "kind": "xyz", "url": img("/web/xyz/{z}/{x}/{y}.png"), "attribution": "Fake tiles"}),
        json!({"id": "wl-wmts", "title": "Fake orthophoto WMTS", "kind": "wmts", "url": img("/web/wmts/own/WMTSCapabilities.xml"), "layer": "ortho"}),
        json!({"id": "wl-wms", "title": "Fake parcels WMS", "kind": "wms", "url": img("/web/wms"), "layer": "parcels"}),
        json!({"id": "wl-tiled", "title": "Fake cached MapServer", "kind": "arcgis-map", "url": svc("Imagery/MapServer")}),
        json!({"id": "wl-dynamic", "title": "Fake dynamic MapServer", "kind": "arcgis-map", "url": svc("Dynamic/MapServer"), "layer": "1"}),
        json!({"id": "wl-image", "title": "Fake elevation ImageServer", "kind": "arcgis-image", "url": svc("Elevation/ImageServer")}),
        json!({"id": "wl-parcels", "title": "Fake parcel features", "kind": "arcgis-features", "url": svc("Parcels/FeatureServer/0"), "auth_from": "svc"}),
        json!({"id": "wl-secure", "title": "Fake secured imagery", "kind": "arcgis-map", "url": format!("http://127.0.0.1:{port}/web/arcgis/rest/services/SecureMap/MapServer"), "auth_from": "svc"}),
        json!({"id": "wl-redirect", "title": "Fake moved tiles", "kind": "xyz", "url": img("/web/redirect/{z}/{x}/{y}.png")}),
    ];
    for s in &sources {
        save(&root, s.clone());
    }
    // Files the person wrote by hand (the app would refuse to save them):
    // a host off map.hosts, a WMTS whose tiles are on another host, and the
    // secured service on a host the portal does not vouch for.
    let by_hand = [
        ("wl-blocked", format!("id: wl-blocked\ntitle: Fake blocked tiles\nkind: xyz\nurl: \"http://blocked.fake.test:{port}/web/xyz/{{z}}/{{x}}/{{y}}.png\"\n")),
        ("wl-cdn", format!("id: wl-cdn\ntitle: Fake CDN orthophoto\nkind: wmts\nurl: \"{wmts_cdn}\"\nlayer: ortho\n")),
        ("wl-unvouched", format!("id: wl-unvouched\ntitle: Fake unvouched imagery\nkind: arcgis-map\nurl: \"http://other.fake.test:{port}/web/arcgis/rest/services/SecureMap/MapServer\"\nauth_from: svc\n")),
    ];
    for (id, text) in &by_hand {
        std::fs::write(root.join("map-sources").join(format!("{id}.yaml")), text).unwrap();
    }
    let (ok, v, all) = cli(&root, &["map-source", "test", "wl-wms"], &[]);
    assert!(ok && v["ok"] == true, "Test: {all}");
    let (ok, v, _) = cli(&root, &["map-source", "list"], &[]);
    assert!(ok);
    assert_eq!(v["sources"].as_array().unwrap().len(), 12, "{v}");

    // --- The map page.
    let client = start(&root, py.as_deref()).await;
    let w = call(&client, "whoami", json!({})).await;
    let session = w["result"]["session"].as_str().unwrap().to_string();
    let (base, tok) = page(&root, &session).await;
    let mut seen = Seen::default();

    // No page token: refused, and nothing is asked of any service.
    let before = fake.log().await.len();
    for path in [
        "/api/web/sources",
        "/api/web/source/wl-xyz",
        "/api/web/tile/wl-xyz/3/1/3",
        "/api/web/features/wl-parcels?bbox=-98,30,-97,31",
    ] {
        let (code, _, _) = seen.get(&format!("{base}{path}"), None).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{path}");
        let (code, _, _) = seen
            .get(&format!("{base}{path}"), Some("not-the-token"))
            .await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{path}");
    }
    // A page request naming an address: refused, and never fetched.
    let raw = img("/web/xyz/3/1/3.png");
    for path in [
        format!("/api/web/tile?url={raw}"),
        format!(
            "/api/web/proxy?url={}",
            raw.replace(':', "%3A").replace('/', "%2F")
        ),
        format!("/api/web/tile/wl-xyz/3/1/3?url={raw}"),
        format!(
            "/api/web/source/{}",
            raw.replace(':', "%3A").replace('/', "%2F")
        ),
        format!("/api/web/{raw}"),
    ] {
        let (code, _, body) = seen.get(&format!("{base}{path}"), Some(&tok)).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{path}");
        assert!(
            String::from_utf8_lossy(&body).contains("never by an address"),
            "{path}"
        );
    }
    assert_eq!(fake.log().await.len(), before, "nothing reached a service");

    // The list: ids and titles, no addresses.
    let (code, list) = seen.json(&format!("{base}/api/web/sources"), &tok).await;
    assert_eq!(code, StatusCode::OK, "{list}");
    assert_eq!(list["enabled"], true);
    assert_eq!(list["sources"].as_array().unwrap().len(), 12, "{list}");
    let text = list.to_string();
    assert!(
        !text.contains("fake.test") && !text.contains("http"),
        "{text}"
    );
    // Each carries a short revision (T-25), so the page redraws an edited
    // layer: a hash, the same on every list, different between sources.
    let revs: Vec<String> = list["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["rev"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        revs.iter()
            .all(|r| r.len() == 12 && r.chars().all(|c| c.is_ascii_hexdigit())),
        "{revs:?}"
    );
    let unique: std::collections::HashSet<_> = revs.iter().collect();
    assert_eq!(unique.len(), revs.len(), "{revs:?}");
    let (_, again) = seen.json(&format!("{base}/api/web/sources"), &tok).await;
    assert_eq!(
        again["sources"], list["sources"],
        "the same sources, the same revisions"
    );

    // Every allowed source draws through the proxy.
    for id in [
        "wl-xyz",
        "wl-wmts",
        "wl-wms",
        "wl-tiled",
        "wl-dynamic",
        "wl-image",
        "wl-secure",
    ] {
        let (code, info) = seen
            .json(&format!("{base}/api/web/source/{id}"), &tok)
            .await;
        assert_eq!(code, StatusCode::OK, "{id}: {info}");
        let tiles = info["tiles"].as_str().unwrap();
        assert_eq!(tiles, format!("/api/web/tile/{id}/{{z}}/{{x}}/{{y}}"));
        let (code, ct, body) = seen
            .get(&format!("{base}/api/web/tile/{id}/3/1/3"), Some(&tok))
            .await;
        assert_eq!(
            code,
            StatusCode::OK,
            "{id}: {}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(ct, "image/png", "{id}");
        assert!(is_png(&body), "{id}");
        assert!(
            root.join("workspaces/cache/tiles")
                .join(id)
                .join("3_1_3.png")
                .is_file(),
            "{id} is cached"
        );
    }
    let (_, info) = seen
        .json(&format!("{base}/api/web/source/wl-wmts"), &tok)
        .await;
    assert_eq!(info["extent"], json!([-98.0, 30.0, -97.0, 31.0]));
    let log = fake.log().await;
    let targets: Vec<String> = log
        .iter()
        .map(|e| e["target"].as_str().unwrap().to_string())
        .collect();
    for want in [
        "/web/xyz/3/1/3.png",
        "/web/wmts/tile/GoogleMapsCompatible/3/3/1.png",
        "/web/arcgis/rest/services/Imagery/MapServer/tile/3/3/1",
        "/web/arcgis/rest/services/SecureMap/MapServer/tile/3/3/1",
    ] {
        assert!(targets.iter().any(|t| t == want), "{want} in {targets:?}");
    }
    assert!(targets.iter().any(|t| t.starts_with("/web/wms?")
        && t.contains("REQUEST=GetMap")
        && t.contains("CRS=EPSG%3A3857")));
    assert!(targets
        .iter()
        .any(|t| t.contains("/Dynamic/MapServer/export?") && t.contains("show%3A1")));
    assert!(targets
        .iter()
        .any(|t| t.contains("/Elevation/ImageServer/exportImage?")));
    // A second look comes from the cache.
    let n = fake.log().await.len();
    let (code, _, _) = seen
        .get(&format!("{base}/api/web/tile/wl-xyz/3/1/3"), Some(&tok))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(fake.log().await.len(), n, "served from the cache");

    // Features as vectors, hidden fields removed (it comes through svc).
    let (code, info) = seen
        .json(&format!("{base}/api/web/source/wl-parcels"), &tok)
        .await;
    assert_eq!(code, StatusCode::OK, "{info}");
    assert_eq!(info["features"], "/api/web/features/wl-parcels");
    assert_eq!(info["fields"], json!(["OBJECTID", "OWNER"]));
    let (code, fc) = seen
        .json(
            &format!("{base}/api/web/features/wl-parcels?bbox=-98,30,-97,31"),
            &tok,
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{fc}");
    assert_eq!(fc["type"], "FeatureCollection");
    assert_eq!(fc["features"].as_array().unwrap().len(), 30);
    assert_eq!(fc["total"], 30);
    assert_eq!(fc["exceeded"], false);
    assert!(
        !fc.to_string().contains("SSN") && !fc.to_string().contains("900-00"),
        "{fc}"
    );
    assert!(
        fake.log()
            .await
            .iter()
            .all(|e| !e["target"].as_str().unwrap().contains("SSN")),
        "the hidden field is not even asked for"
    );

    // Refused: a host off map.hosts, a WMTS tile host off it, a redirect.
    let before = fake.log().await;
    let (code, _, body) = seen
        .get(&format!("{base}/api/web/tile/wl-blocked/3/1/3"), Some(&tok))
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert!(String::from_utf8_lossy(&body).contains("blocked.fake.test"));
    let (code, _, body) = seen
        .get(&format!("{base}/api/web/tile/wl-cdn/3/1/3"), Some(&tok))
        .await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains("cdn.fake.test"));
    let (code, _, body) = seen
        .get(
            &format!("{base}/api/web/tile/wl-redirect/3/1/3"),
            Some(&tok),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_GATEWAY);
    assert!(
        String::from_utf8_lossy(&body).contains("redirect"),
        "{}",
        String::from_utf8_lossy(&body)
    );
    let after = fake.log().await;
    assert!(
        after
            .iter()
            .all(|e| !host(e).starts_with("blocked.") && !host(e).starts_with("cdn.")),
        "no request reached a host off map.hosts"
    );
    assert!(
        !after[before.len()..]
            .iter()
            .any(|e| e["target"].as_str().unwrap().starts_with("/web/xyz/")),
        "the redirect was not followed"
    );

    // The secured service on a host the portal does not vouch for: no
    // token goes there, and its tiles are refused.
    let (code, _, body) = seen
        .get(
            &format!("{base}/api/web/tile/wl-unvouched/3/1/3"),
            Some(&tok),
        )
        .await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains("does not vouch for other.fake.test"));
    let log = fake.log().await;
    assert!(
        log.iter().any(|e| host(e).starts_with("other.fake.test")),
        "it was asked"
    );
    for e in &log {
        if sent_token(e).is_some() {
            assert!(
                host(e) == format!("127.0.0.1:{port}"),
                "a token went only to the vouched host: {e}"
            );
        }
    }
    assert!(
        log.iter().any(|e| sent_token(e).is_some()
            && e["target"]
                .as_str()
                .unwrap()
                .starts_with("/web/arcgis/rest/services/SecureMap/")),
        "the vouched host got the token"
    );

    // --- No agent access (D-14, REQ-34).
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.len() > 20);
    for t in &tools {
        let name = t.name.to_lowercase();
        let desc = t.description.as_deref().unwrap_or("").to_lowercase();
        let input = serde_json::to_string(&t.input_schema)
            .unwrap()
            .to_lowercase();
        assert!(
            !name.contains("web") && !name.contains("map_source") && !name.contains("basemap"),
            "{name}"
        );
        assert!(
            !desc.contains("web layer") && !desc.contains("web map layer"),
            "{name}: {desc}"
        );
        assert!(
            !input.contains("map_source")
                && !input.contains("web_layer")
                && !input.contains("auth_from"),
            "{name}: {input}"
        );
    }
    let titles: Vec<String> = sources
        .iter()
        .map(|s| s["title"].as_str().unwrap().to_string())
        .chain(
            [
                "Fake blocked tiles",
                "Fake CDN orthophoto",
                "Fake unvouched imagery",
            ]
            .map(String::from),
        )
        .collect();
    let w = call(&client, "whoami", json!({})).await;
    let mut listed: Vec<String> =
        serde_json::from_value(w["result"]["web_layers"].clone()).unwrap();
    listed.sort();
    let mut want = titles.clone();
    want.sort();
    assert_eq!(listed, want, "whoami names every web layer");
    let wt = w.to_string();
    for bad in ["fake.test", "wl-", "/web/", "zq7k", "ortho\"", "parcels\""] {
        assert!(!wt.contains(bad), "whoami shows titles only, not {bad}");
    }
    let lo = call(&client, "list_outputs", json!({})).await.to_string();
    let mut outs = vec![("list_outputs", lo)];
    if py.is_some() {
        let q = call(
            &client,
            "query_features",
            json!({"connection": "tiger", "dataset": "tracts"}),
        )
        .await;
        let handle = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
        let snap = call(&client, "map_snapshot", json!({"layers": [handle]})).await;
        let st = snap.to_string();
        assert!(st.contains("No web basemap"), "{st}");
        outs.push(("map_snapshot", st));
        outs.push((
            "list_outputs",
            call(&client, "list_outputs", json!({})).await.to_string(),
        ));
    }
    // The assistant's view of the person's map (T-30): the page says what
    // its map shows, and nothing of a web layer is in what `map_view`
    // answers; a report that carries one (its id as a layer that is on, or
    // anything this server does not list) is refused whole.
    let say = |report: Value| {
        let (url, origin, tok) = (format!("{base}/api/view"), base.clone(), tok.clone());
        async move {
            reqwest::Client::new()
                .post(url)
                .bearer_auth(tok)
                .header("origin", origin)
                .json(&report)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    let view = json!({"extent": [-97.9, 30.1, -97.6, 30.4], "zoom": 11.0, "layers": [], "table": false, "streets": false});
    let with = |change: &dyn Fn(&mut Value)| {
        let mut r = json!({"showing": true, "view": view});
        change(&mut r);
        r
    };
    for bad in [
        with(&|r| r["web_layers"] = json!(sources)),
        with(&|r| r["view"]["layers"] = json!(["wl-xyz"])),
        with(&|r| r["view"]["web"] = json!({"on": ["wl-wmts"], "base": "wl-tiled"})),
    ] {
        assert_eq!(say(bad.clone()).await, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(
        say(json!({"showing": true, "view": view})).await,
        StatusCode::OK
    );
    let mv = call(&client, "map_view", json!({})).await;
    assert_eq!(mv["result"]["status"], "open", "{mv}");
    outs.push(("map_view", mv.to_string()));
    for (tool, text) in &outs {
        for t in &titles {
            assert!(!text.contains(t.as_str()), "{tool} mentions {t}");
        }
        for bad in ["fake.test", "wl-", "/web/"] {
            assert!(!text.contains(bad), "{tool} mentions {bad}");
        }
    }

    // No answer to the page carried a token.
    assert!(!seen.0.is_empty());
    assert!(!seen.0.contains(MARK), "a token reached the page");

    // --- The cache: removing a source drops its folder.
    let tiles = root.join("workspaces/cache/tiles");
    assert!(tiles.join("wl-xyz").is_dir());
    let (ok, _, all) = cli(&root, &["map-source", "remove", "wl-xyz"], &[]);
    assert!(ok, "{all}");
    assert!(!tiles.join("wl-xyz").exists(), "its cache is gone");
    assert!(!root.join("map-sources/wl-xyz.yaml").exists());
    let (code, _, _) = seen
        .get(&format!("{base}/api/web/tile/wl-xyz/3/1/3"), Some(&tok))
        .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    // A source removed behind the app's back loses its cache at the next
    // listing.
    std::fs::remove_file(root.join("map-sources/wl-image.yaml")).unwrap();
    assert!(tiles.join("wl-image").is_dir());
    let (code, _) = seen.json(&format!("{base}/api/web/sources"), &tok).await;
    assert_eq!(code, StatusCode::OK);
    assert!(!tiles.join("wl-image").exists());

    // Signing out the arcgis connection drops the secured source's cache,
    // and its tiles are not served from anywhere after.
    assert!(tiles.join("wl-secure").is_dir());
    let (ok, _, all) = cli(&root, &["arcgis", "signout", "svc"], &[]);
    assert!(ok, "{all}");
    assert!(!entry.exists());
    assert!(!tiles.join("wl-secure").exists(), "its cache is gone");
    assert!(tiles.join("wl-wms").is_dir(), "other caches stay");
    let (code, _, body) = seen
        .get(&format!("{base}/api/web/tile/wl-secure/3/1/3"), Some(&tok))
        .await;
    assert_eq!(
        code,
        StatusCode::UNAUTHORIZED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(String::from_utf8_lossy(&body).contains("arcgis signin svc"));
    assert!(!tiles.join("wl-secure").exists());

    // A sign-in that went away without a sign-out (the credential deleted
    // elsewhere) is caught before a secured cache is reused.
    let t: Value = reqwest::Client::new()
        .post(fake.url("/test/issue"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    std::fs::write(
        &entry,
        json!({
            "access_token": t["access_token"], "refresh_token": t["refresh_token"],
            "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "portal": format!("http://127.0.0.1:{port}"),
        })
        .to_string(),
    )
    .unwrap();
    let (code, _, _) = seen
        .get(&format!("{base}/api/web/tile/wl-secure/3/1/3"), Some(&tok))
        .await;
    assert_eq!(code, StatusCode::OK);
    assert!(tiles.join("wl-secure").is_dir());
    std::fs::remove_file(&entry).unwrap();
    let (code, _, _) = seen
        .get(&format!("{base}/api/web/tile/wl-secure/3/1/3"), Some(&tok))
        .await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "not served from the cache");
    assert!(!tiles.join("wl-secure").exists());
    assert!(!seen.0.contains(MARK), "a token reached the page");

    // The organization can turn web layers off.
    let policy = std::fs::read_to_string(root.join("policy.yaml")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        policy.replace("map:\n", "map:\n  allow_web_layers: false\n"),
    )
    .unwrap();
    // (A settings change is used once the file has been still a moment.)
    let t0 = Instant::now();
    let l = loop {
        let (code, l) = seen.json(&format!("{base}/api/web/sources"), &tok).await;
        assert_eq!(code, StatusCode::OK);
        if l["enabled"] == false || t0.elapsed() > Duration::from_secs(15) {
            break l;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(l["enabled"], false);
    assert_eq!(l["sources"], json!([]));
    let (code, _, _) = seen
        .get(&format!("{base}/api/web/tile/wl-wms/3/1/3"), Some(&tok))
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let w = call(&client, "whoami", json!({})).await;
    assert_eq!(w["result"]["web_layers"], json!([]));

    client.cancel().await.unwrap();
}
