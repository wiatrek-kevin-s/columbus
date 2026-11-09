//! The companion window's data, end to end: the gateway announces its
//! session in `live/`, opens a window on request (the launch link is handed
//! off to a file in debug builds), and serves the session's outputs, a
//! layer as EPSG:4326 GeoJSON, table rows and provenance, only with the
//! page's session token. Needs the GIS engine; skipped without it.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
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
    root: PathBuf,
}

fn setup(py: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["A", "B", "C"], "Land Area": [10.0, 20.0, 30.0]},
    geometry=[box(-97.8 + i * 0.1, 30.2, -97.75 + i * 0.1, 30.25) for i in range(3)],
    crs="EPSG:4269").to_crs("EPSG:3081").to_file(os.path.join(sys.argv[1], "t.gpkg"),
    layer="Tribal Census Tract", driver="GPKG", engine="pyogrio")
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
        "connection_types: [gpkg]\napprovals: { page: true, wait_seconds: 0 }\n",
    )
    .unwrap();
    let gpkg = serde_json::to_string(&root.join("t.gpkg").display().to_string()).unwrap();
    std::fs::write(
        root.join("connections").join("tiger.yaml"),
        format!("id: tiger\ntype: gpkg\npath: {gpkg}\ndatasets:\n  allow: [\"*\"]\n"),
    )
    .unwrap();
    // A stand-in for the built window: the server only serves the files.
    let ui = root.join("ui");
    std::fs::create_dir_all(ui.join("assets")).unwrap();
    std::fs::write(
        ui.join("index.html"),
        "<!doctype html><script type=module src=/assets/app.js></script>",
    )
    .unwrap();
    std::fs::write(ui.join("assets").join("app.js"), "console.log(1)").unwrap();
    std::fs::write(root.join("secret.txt"), "not for the window").unwrap();
    Env { _dir: dir, root }
}

async fn start(env: &Env, py: &str) -> RunningService<RoleClient, ()> {
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = env.root.clone();
    let py = py.to_string();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_LIVE_DIR", root.join("live"))
                .env("COLUMBUS_UI_DIR", root.join("ui"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_APPROVALS_LAUNCH_FILE", root.join("launch.txt"))
                .env("COLUMBUS_LOG", "warn");
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
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().expect("structured output")
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

#[tokio::test]
async fn companion_window_shows_the_sessions_outputs() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(&py);
    let client = start(&env, &py).await;
    let w = call(&client, "whoami", json!({})).await;
    let session = w["result"]["session"].as_str().unwrap().to_string();

    // The session announces itself, with its port and nothing secret.
    let live: Value = serde_json::from_str(&wait_for(
        &env.root.join("live").join(format!("{session}.json")),
    ))
    .unwrap();
    let port = live["port"].as_u64().unwrap();
    let text = live.to_string();
    assert!(!text.contains("#t=") && !text.contains("token"), "{text}");
    // whoami was a tool call: the session says when it was last used, for
    // the app's "used 2 minutes ago".
    assert!(
        live["last_used"]
            .as_str()
            .is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok()),
        "{text}"
    );
    let base = format!("http://127.0.0.1:{port}");
    let origin = base.clone();
    let http = reqwest::Client::new();

    // Opening a window needs the custom header, and no other site's Origin.
    let r = http.post(format!("{base}/api/open")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = http
        .post(format!("{base}/api/open"))
        .header("x-columbus-open", "1")
        .header("origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = http
        .post(format!("{base}/api/open"))
        .header("x-columbus-open", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let link = wait_for(&env.root.join("launch.txt"));
    let launch = link.split("#t=").nth(1).unwrap().trim().to_string();

    // Nothing without the session token.
    let r = http
        .get(format!("{base}/api/outputs"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = http
        .post(format!("{base}/api/session"))
        .header("origin", &origin)
        .json(&json!({"launch": launch}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let tok = r.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let get = |path: String| {
        let http = http.clone();
        let tok = tok.clone();
        async move {
            let r = http.get(path).bearer_auth(tok).send().await.unwrap();
            let code = r.status();
            (code, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };

    // The window itself, with its policy, and its files by plain name only.
    let r = http.get(&base).send().await.unwrap();
    let csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        csp.contains("script-src 'self'") && csp.contains("worker-src 'self' blob:"),
        "{csp}"
    );
    assert!(r.text().await.unwrap().contains("/assets/app.js"));
    let r = http
        .get(format!("{base}/assets/app.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // Scripts carry the window's policy: the map's worker, which fetches
    // the basemap tiles, follows the policy its script came with.
    let worker_csp = r.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(worker_csp, csp, "a script has the window's policy");
    assert!(
        csp.contains("connect-src 'self' https://tiles.openfreemap.org"),
        "{csp}"
    );
    for bad in ["..%2Fsecret.txt", "%2E%2E%2F..%2Fsecret.txt", ".hidden"] {
        let r = http
            .get(format!("{base}/assets/{bad}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{bad}");
    }
    let r = http
        .get(format!("{base}/api/state"))
        .bearer_auth(&tok)
        .header("host", "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "wrong Host is refused");

    // An output from a query appears, opens on the map in lon/lat, and in
    // the table.
    let (_, before) = get(format!("{base}/api/state")).await;
    let q = call(
        &client,
        "query_features",
        json!({"connection": "tiger", "dataset": "Tribal Census Tract"}),
    )
    .await;
    let handle = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let (_, after) = get(format!("{base}/api/state")).await;
    assert_ne!(
        before["version"], after["version"],
        "the window can tell something changed"
    );

    let (code, o) = get(format!("{base}/api/outputs")).await;
    assert_eq!(code, StatusCode::OK, "{o}");
    let card = o["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["handle"] == handle.as_str())
        .unwrap_or_else(|| panic!("{o}"))
        .clone();
    assert_eq!(card["kind"], "layer");
    assert_eq!(card["viewable"], true);
    assert_eq!(card["rows"], 3);

    let enc = |h: &str| h.replace(':', "%3A").replace('/', "%2F");
    let (code, g) = get(format!("{base}/api/layer?handle={}", enc(&handle))).await;
    assert_eq!(code, StatusCode::OK, "{g}");
    assert_eq!(g["type"], "FeatureCollection");
    assert_eq!(g["shown"], 3);
    let bbox: Vec<f64> = serde_json::from_value(g["bbox"].clone()).unwrap();
    assert!(
        bbox[0] > -98.0 && bbox[2] < -97.0 && bbox[1] > 30.0,
        "in degrees: {bbox:?}"
    );
    assert_eq!(g["features"][0]["properties"]["Land Area"], 10.0);

    let (code, t) = get(format!("{base}/api/rows?handle={}&limit=2", enc(&handle))).await;
    assert_eq!(code, StatusCode::OK, "{t}");
    assert_eq!(t["rows"].as_array().unwrap().len(), 2);
    assert_eq!(t["total"], 3);

    let prov = card["provenance"].as_str().unwrap();
    let (code, p) = get(format!("{base}/api/provenance/{prov}")).await;
    assert_eq!(code, StatusCode::OK, "{p}");
    assert_eq!(p["id"], prov);

    let (code, ov) = get(format!("{base}/api/overview")).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(ov["session"], session.as_str());
    assert_eq!(ov["connections"][0]["id"], "tiger");

    // Only Columbus's own, unchanged outputs open.
    let path = PathBuf::from(
        q["result"]["layer"]["path"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| {
                env.root
                    .join("workspaces")
                    .join(&session)
                    .join(handle.split(&format!("{session}/")).nth(1).unwrap())
                    .display()
                    .to_string()
            }),
    );
    std::fs::write(&path, b"tampered").unwrap();
    let (code, e) = get(format!("{base}/api/layer?handle={}", enc(&handle))).await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    assert!(e["error"].as_str().unwrap().contains("changed"), "{e}");
    let (code, _) = get(format!(
        "{base}/api/layer?handle={}",
        enc(&format!("ws://{session}/../secret.txt"))
    ))
    .await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY);

    // A window opened for the app may look but not decide.
    let r = http
        .post(format!("{base}/api/approvals/x"))
        .bearer_auth(&tok)
        .header("origin", &origin)
        .json(&json!({"decision": "allow_once"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::FORBIDDEN,
        "view windows cannot decide"
    );
    let (_, st) = get(format!("{base}/api/state")).await;
    assert_eq!(st["scope"], "view");

    // While the app runs, its windows go to the app by a columbus:// link
    // (carrying a view token only).
    std::fs::write(env.root.join("live").join("app.json"), "{}").unwrap();
    std::fs::remove_file(env.root.join("launch.txt")).unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let r = http
        .post(format!("{base}/api/open"))
        .header("x-columbus-open", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let deep = wait_for(&env.root.join("launch.txt"));
    assert!(
        deep.starts_with(&format!("columbus://open?port={port}#t=")),
        "{deep}"
    );

    // The announcement goes when the session ends.
    let live_file = env.root.join("live").join(format!("{session}.json"));
    client.cancel().await.unwrap();
    let t = Instant::now();
    while live_file.exists() && t.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!live_file.exists(), "live file removed at exit");
}
