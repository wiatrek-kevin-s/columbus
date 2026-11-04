//! registry_fetch (design section 6): download open data an entry
//! describes, through a local test server (debug builds may use plain HTTP
//! on 127.0.0.1 when COLUMBUS_FETCH_TEST_LOOPBACK is set; release builds
//! never do). Paging, redirects refused, private addresses refused, and
//! hosts not pre-approved need a person.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

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
            Command::new(p)
                .args(["-c", "import geopandas, shapely, pyproj"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn feature(i: u32) -> Value {
    json!({"type": "Feature", "geometry": {"type": "Point", "coordinates": [-97.7 + i as f64 * 0.001, 30.27]},
           "properties": {"name": format!("site {i}"), "nested": {"a": i}}})
}

/// A tiny HTTP server: an OGC API collection in two pages, an ArcGIS layer
/// in two pages, and a redirect.
fn serve() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap_or(0);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
            }
            let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, body) = if target.starts_with("/ogc/collections/sites/items") {
                if target.contains("page=2") {
                    (
                        "200 OK",
                        json!({"type": "FeatureCollection", "features": [feature(3)], "links": []}),
                    )
                } else {
                    (
                        "200 OK",
                        json!({"type": "FeatureCollection", "features": [feature(1), feature(2)],
                        "links": [{"rel": "next", "href": "/ogc/collections/sites/items?page=2"},
                                  {"rel": "alternate", "href": "https://elsewhere.example/x"}]}),
                    )
                }
            } else if target.starts_with("/arcgis/rest/services/Sites/FeatureServer/0/query") {
                if target.contains("resultOffset=0") {
                    (
                        "200 OK",
                        json!({"type": "FeatureCollection", "features": [feature(10), feature(11)], "properties": {"exceededTransferLimit": true}}),
                    )
                } else {
                    (
                        "200 OK",
                        json!({"type": "FeatureCollection", "features": [feature(12)]}),
                    )
                }
            } else if target.starts_with("/moved.geojson") {
                ("302 Found", json!({}))
            } else {
                ("404 Not Found", json!({}))
            };
            let text = body.to_string();
            let mut w = s;
            let extra = if status.starts_with("302") {
                "Location: http://169.254.169.254/latest\r\n"
            } else {
                ""
            };
            let _ = write!(w, "HTTP/1.1 {status}\r\nContent-Type: application/geo+json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
        }
    });
    port
}

fn make_index(py: &str, dir: &Path, port: u16) {
    std::fs::create_dir_all(dir).unwrap();
    let script = r#"
import json, sqlite3, sys, hashlib, os
d, port = sys.argv[1], sys.argv[2]
db = sqlite3.connect(os.path.join(d, "registry.sqlite"))
db.executescript('''
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE entries (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, status TEXT, card TEXT NOT NULL, entry TEXT NOT NULL);
CREATE VIRTUAL TABLE entries_fts USING fts5(id UNINDEXED, name, summary, category, themes, notes);
''')
base = f"http://127.0.0.1:{port}"
entries = [
  {"id": "sites-ogc", "kind": "data", "name": "Sites (OGC)", "publisher": "Test", "license": "CC0-1.0", "status": "active",
   "review": {"state": "reviewed"}, "access": [{"type": "wms", "url": base + "/wms", "auth": "none"},
                                              {"type": "ogc-api-features", "url": base + "/ogc", "index": "sites", "auth": "none"}]},
  {"id": "sites-arcgis", "kind": "data", "name": "Sites (ArcGIS)", "publisher": "Test", "license": "NOASSERTION", "status": "active",
   "review": {"state": "seeded"}, "access": [{"type": "arcgis-rest", "url": base + "/arcgis/rest/services/Sites/FeatureServer/0", "auth": "none"}]},
  {"id": "moved", "kind": "data", "name": "Moved", "publisher": "Test", "license": "CC0-1.0", "status": "active",
   "review": {"state": "reviewed"}, "access": [{"type": "download", "url": base + "/moved.geojson", "auth": "none"}]},
  {"id": "internal", "kind": "data", "name": "Internal", "publisher": "Test", "license": "CC0-1.0", "status": "active",
   "review": {"state": "reviewed"}, "access": [{"type": "download", "url": "https://10.0.0.1/x.geojson", "auth": "none"}]},
]
for e in entries:
    card = {k: e[k] for k in ("id", "kind", "name")}
    db.execute("INSERT INTO entries VALUES (?,?,?,?,?,?)", (e["id"], e["kind"], e["name"], e["status"], json.dumps(card), json.dumps(e)))
db.commit(); db.close()
h = hashlib.sha256(open(os.path.join(d, "registry.sqlite"), "rb").read()).hexdigest()
open(os.path.join(d, "index.sha256"), "w").write(h + "  registry.sqlite\n")
"#;
    assert!(Command::new(py)
        .arg("-c")
        .arg(script)
        .arg(dir)
        .arg(port.to_string())
        .status()
        .unwrap()
        .success());
}

async fn start(root: &Path, py: &str, policy: &str) -> RunningService<RoleClient, ()> {
    std::fs::write(root.join("policy.yaml"), policy).unwrap();
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap();
    let root = root.to_path_buf();
    let py = py.to_string();
    ().serve(
        TokioChildProcess::new(
            tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(
                move |cmd| {
                    cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                        .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                        .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                        .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                        .env("COLUMBUS_REGISTRY", root.join("registry"))
                        .env("COLUMBUS_WORKER", &worker)
                        .env("COLUMBUS_PYTHON", &py)
                        .env("COLUMBUS_FETCH_TEST_LOOPBACK", "1")
                        .env("COLUMBUS_APPROVALS_BROWSER", "none")
                        .env("COLUMBUS_LOG", "warn");
                },
            ),
        )
        .unwrap(),
    )
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

fn ok(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().unwrap()
}

fn err(r: &CallToolResult) -> String {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected an error: {:?}",
        r.structured_content
    );
    serde_json::to_string(&r.content).unwrap()
}

fn ids(v: &Value) -> Vec<String> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn fetch_open_data_from_registry_entries() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let port = serve();
    make_index(&py, &root.join("registry"), port);

    let c = start(
        &root,
        &py,
        "connection_types: [gpkg]\nfetch_hosts: [\"127.0.0.1\"]\napprovals: { page: false }\n",
    )
    .await;

    // OGC API Features: the first supported access method, two pages
    // followed by the next link, nothing from another host.
    let r = ok(&call(
        &c,
        "registry_fetch",
        json!({"id": "sites-ogc", "bbox": [-98.0, 30.0, -97.0, 31.0]}),
    )
    .await);
    assert_eq!(r["result"]["status"], "fetched", "{r}");
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 3);
    assert_eq!(r["result"]["layer"]["summary"]["crs"]["code"], "EPSG:4326");
    let h = r["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let rows = ok(&call(&c, "read_output", json!({"handle": h})).await);
    assert!(rows["result"]["rows"].to_string().contains("site 3"));
    // The layer works with the other tools.
    let b = ok(&call(
        &c,
        "vector_op",
        json!({"op": "reproject", "inputs": {"layer": h}, "params": {"crs": "EPSG:3081"}}),
    )
    .await);
    assert_eq!(b["result"]["layer"]["summary"]["rows"], 3);

    // ArcGIS REST: paged by offset while the server says there is more;
    // an unreviewed entry and an unknown licence are flagged.
    let r = ok(&call(&c, "registry_fetch", json!({"id": "sites-arcgis"})).await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 3, "{r}");
    let i = ids(&r);
    assert!(
        i.contains(&"registry.unreviewed".into()) && i.contains(&"registry.license_unknown".into()),
        "{i:?}"
    );
    // A limit stops early and says so.
    let r = ok(&call(
        &c,
        "registry_fetch",
        json!({"id": "sites-arcgis", "limit": 2}),
    )
    .await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 2);
    assert!(ids(&r).contains(&"fetch.truncated".into()));

    // Redirects are not followed; private addresses are refused.
    let e = err(&call(&c, "registry_fetch", json!({"id": "moved"})).await);
    assert!(e.contains("redirect"), "{e}");
    let e = err(&call(&c, "registry_fetch", json!({"id": "internal"})).await);
    assert!(
        e.contains("not a public address") || e.contains("cannot resolve"),
        "{e}"
    );
    // An access method it cannot read, by index.
    let e = err(&call(
        &c,
        "registry_fetch",
        json!({"id": "sites-ogc", "access": 0}),
    )
    .await);
    assert!(e.contains("registry_fetch reads"), "{e}");
    c.cancel().await.unwrap();

    // A host the organization did not pre-approve needs the person.
    let c = start(
        &root,
        &py,
        "connection_types: [gpkg]\napprovals: { page: false }\n",
    )
    .await;
    let r = ok(&call(&c, "registry_fetch", json!({"id": "sites-ogc"})).await);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    assert!(r["result"]["approval"]["summary"]
        .as_str()
        .unwrap()
        .contains("licence CC0-1.0"));
    c.cancel().await.unwrap();
}
