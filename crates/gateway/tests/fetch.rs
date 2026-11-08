//! registry_fetch (design section 6): download open data an entry
//! describes, through a local test server (debug builds may use plain HTTP
//! on 127.0.0.1 when COLUMBUS_FETCH_TEST_LOOPBACK is set; release builds
//! never do). Paging, redirects refused, private addresses refused, and
//! hosts not pre-approved need a person.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use reqwest::Url;
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

type Log = Arc<Mutex<Vec<String>>>;

/// One fake ArcGIS layer. `slots` is the server's internal row window:
/// `None` is a slot the query filters out, so a window can come back empty
/// while more rows follow (what AF-4 lost).
struct FakeLayer {
    slots: Vec<Option<u32>>,
    max_records: usize,
    pagination: bool,
    /// Answer the first N queries with HTTP 429.
    busy: usize,
}

fn arc_feature(oid: u32) -> Value {
    json!({"type": "Feature", "id": oid,
           "geometry": {"type": "Point", "coordinates": [-97.7 + oid as f64 * 0.0001, 30.27]},
           "properties": {"OBJECTID_1": oid, "name": format!("row {oid}")}})
}

fn fake_layers() -> HashMap<&'static str, FakeLayer> {
    let mut m = HashMap::new();
    // (a) 7 rows; the second window of 3 is all filtered out.
    let mut slots: Vec<Option<u32>> = (1..=3).map(Some).collect();
    slots.extend([None, None, None]);
    slots.extend((4..=7).map(Some));
    m.insert(
        "Empty",
        FakeLayer {
            slots,
            max_records: 3,
            pagination: true,
            busy: 0,
        },
    );
    // (b) 5 rows, no pagination: resultOffset is ignored, as old servers do.
    m.insert(
        "NoPage",
        FakeLayer {
            slots: (1..=5).map(Some).collect(),
            max_records: 2,
            pagination: false,
            busy: 0,
        },
    );
    // (c) more pages than the cap (500 pages of 1).
    m.insert(
        "Many",
        FakeLayer {
            slots: (1..=520).map(Some).collect(),
            max_records: 1,
            pagination: true,
            busy: 0,
        },
    );
    // HTTP 429 twice, then answers.
    m.insert(
        "Busy",
        FakeLayer {
            slots: (1..=2).map(Some).collect(),
            max_records: 10,
            pagination: true,
            busy: 2,
        },
    );
    // HTTP 429 for ever.
    m.insert(
        "Jammed",
        FakeLayer {
            slots: (1..=2).map(Some).collect(),
            max_records: 10,
            pagination: true,
            busy: usize::MAX,
        },
    );
    m
}

/// Answer one fake ArcGIS request: `None` when the path is not one.
fn arcgis(
    layers: &mut HashMap<&'static str, FakeLayer>,
    path: &str,
    q: &HashMap<String, String>,
) -> Option<(&'static str, Value, &'static str)> {
    let rest = path.strip_prefix("/arcgis/rest/services/")?;
    let mut parts = rest.split('/');
    let name = parts.next()?;
    let (_, l) = layers.iter_mut().find(|(k, _)| **k == name)?;
    let is_query = path.ends_with("/0/query");
    if !is_query {
        let mut info = json!({"name": name, "objectIdField": "OBJECTID_1", "maxRecordCount": l.max_records,
                              "fields": [{"name": "OBJECTID_1", "type": "esriFieldTypeOID"}, {"name": "name", "type": "esriFieldTypeString"}]});
        if l.pagination {
            info["advancedQueryCapabilities"] =
                json!({"supportsPagination": true, "supportsOrderBy": true});
        } else {
            info["advancedQueryCapabilities"] =
                json!({"supportsPagination": false, "supportsOrderBy": false});
        }
        return Some(("200 OK", info, ""));
    }
    if l.busy > 0 {
        l.busy -= 1;
        let wait = if l.busy > 1000 {
            "Retry-After: 0\r\n"
        } else {
            "Retry-After: 1\r\n"
        };
        return Some(("429 Too Many Requests", json!({}), wait));
    }
    let rows: Vec<u32> = l.slots.iter().flatten().copied().collect();
    if q.get("returnIdsOnly").map(String::as_str) == Some("true") {
        // Unordered, as servers may send them.
        let mut ids = rows.clone();
        ids.reverse();
        return Some((
            "200 OK",
            json!({"objectIdFieldName": "OBJECTID_1", "objectIds": ids}),
            "",
        ));
    }
    if let Some(list) = q.get("objectIds") {
        let want: Vec<u32> = list.split(',').filter_map(|x| x.parse().ok()).collect();
        let feats: Vec<Value> = rows
            .iter()
            .filter(|r| want.contains(r))
            .map(|r| arc_feature(*r))
            .collect();
        return Some((
            "200 OK",
            json!({"type": "FeatureCollection", "features": feats}),
            "",
        ));
    }
    let n: usize = q
        .get("resultRecordCount")
        .and_then(|v| v.parse().ok())
        .unwrap_or(l.max_records)
        .min(l.max_records);
    let k: usize = if l.pagination {
        q.get("resultOffset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    } else {
        0
    };
    let end = (k + n).min(l.slots.len());
    let window = if k < l.slots.len() {
        &l.slots[k..end]
    } else {
        &[][..]
    };
    let feats: Vec<Value> = window.iter().flatten().map(|r| arc_feature(*r)).collect();
    let more = end < l.slots.len();
    let mut body = json!({"type": "FeatureCollection", "features": feats});
    if more {
        body["properties"] = json!({"exceededTransferLimit": true});
    }
    Some(("200 OK", body, ""))
}

/// A tiny HTTP server: an OGC API collection in two pages, an ArcGIS layer
/// in two pages, the fake ArcGIS layers above, and a redirect. Every
/// request is logged as `GET <target>` or `POST <path>?<form>`.
fn serve() -> (u16, Log) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    std::thread::spawn(move || {
        let mut layers = fake_layers();
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap_or(0);
            let mut length = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut form = vec![0u8; length];
            r.read_exact(&mut form).unwrap_or(());
            let form = String::from_utf8_lossy(&form).to_string();
            let method = line.split_whitespace().next().unwrap_or("GET").to_string();
            let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let full = if method == "POST" {
                format!("{target}?{form}")
            } else {
                target.clone()
            };
            log2.lock().unwrap().push(format!("{method} {full}"));
            let u = Url::parse(&format!("http://x{full}")).unwrap();
            let q: HashMap<String, String> = u
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let mut extra = "";
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
            } else if target.starts_with("/arcgis/rest/services/Sites/FeatureServer/0") {
                (
                    "200 OK",
                    json!({"objectIdField": "OBJECTID", "maxRecordCount": 1000,
                           "advancedQueryCapabilities": {"supportsPagination": true}}),
                )
            } else if let Some((st, b, x)) = arcgis(&mut layers, u.path(), &q) {
                extra = x;
                (st, b)
            } else if target.starts_with("/moved.geojson") {
                extra = "Location: http://169.254.169.254/latest\r\n";
                ("302 Found", json!({}))
            } else {
                ("404 Not Found", json!({}))
            };
            let text = body.to_string();
            let mut w = s;
            let _ = write!(w, "HTTP/1.1 {status}\r\nContent-Type: application/geo+json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
        }
    });
    (port, log)
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
  *[{"id": f"arc-{n.lower()}", "kind": "data", "name": n, "publisher": "Test", "license": "CC0-1.0", "status": "active",
     "review": {"state": "reviewed"}, "access": [{"type": "arcgis-rest", "url": base + f"/arcgis/rest/services/{n}/FeatureServer/0", "auth": "none"}]}
    for n in ("Empty", "NoPage", "Many", "Busy", "Jammed")],
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
    let (port, _log) = serve();
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

fn arc_logs(log: &Log, layer: &str) -> Vec<String> {
    let path = format!("/arcgis/rest/services/{layer}/FeatureServer/0/query");
    log.lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains(&path))
        .cloned()
        .collect()
}

fn decoded(line: &str) -> HashMap<String, String> {
    let target = line.split_once(' ').unwrap().1;
    Url::parse(&format!("http://x{target}"))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// REQ-7 / AF-4: the shared ArcGIS pager loses no rows.
#[tokio::test]
async fn arcgis_paging_loses_no_rows() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    let (port, log) = serve();
    make_index(&py, &root.join("registry"), port);
    let c = start(
        &root,
        &py,
        "connection_types: [gpkg]\nfetch_hosts: [\"127.0.0.1\"]\napprovals: { page: false }\n",
    )
    .await;

    // (a) An empty page with exceededTransferLimit mid-stream: paging goes
    // on, the total is the service's 7, sorted by the object id.
    let r = ok(&call(&c, "registry_fetch", json!({"id": "arc-empty"})).await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 7, "{r}");
    assert!(!ids(&r).contains(&"fetch.truncated".into()), "{r}");
    let reqs = arc_logs(&log, "Empty");
    assert_eq!(reqs.len(), 4, "{reqs:?}");
    for q in &reqs {
        let p = decoded(q);
        assert_eq!(p["orderByFields"], "OBJECTID_1 ASC", "{q}");
        assert_eq!(p["resultRecordCount"], "3", "{q}");
    }
    let rows = ok(&call(
        &c,
        "read_output",
        json!({"handle": r["result"]["layer"]["handle"]}),
    )
    .await);
    let text = rows["result"]["rows"].to_string();
    assert!(text.contains("row 1") && text.contains("row 7"), "{text}");

    // (b) No pagination: ids first, then object-id batches, ascending.
    let r = ok(&call(&c, "registry_fetch", json!({"id": "arc-nopage"})).await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 5, "{r}");
    let reqs = arc_logs(&log, "NoPage");
    assert!(reqs.iter().all(|q| !q.contains("resultOffset")), "{reqs:?}");
    assert!(
        reqs[0].starts_with("POST") && decoded(&reqs[0])["returnIdsOnly"] == "true",
        "{reqs:?}"
    );
    let batches: Vec<String> = reqs[1..]
        .iter()
        .map(|q| {
            assert!(q.starts_with("POST"), "{q}");
            decoded(q)["objectIds"].clone()
        })
        .collect();
    assert_eq!(batches, ["1,2", "3,4", "5"], "{reqs:?}");

    // (c) More pages than the cap: stops at 500 and says so.
    let r = ok(&call(&c, "registry_fetch", json!({"id": "arc-many"})).await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 500, "{r}");
    assert!(ids(&r).contains(&"fetch.truncated".into()), "{r}");
    assert_eq!(arc_logs(&log, "Many").len(), 500);

    // The read limit cuts the page that crosses it, and says so.
    let r = ok(&call(&c, "registry_fetch", json!({"id": "arc-empty", "limit": 5})).await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 5, "{r}");
    assert!(ids(&r).contains(&"fetch.truncated".into()), "{r}");

    // HTTP 429 is retried after Retry-After, at most 3 times.
    let r = ok(&call(&c, "registry_fetch", json!({"id": "arc-busy"})).await);
    assert_eq!(r["result"]["layer"]["summary"]["rows"], 2, "{r}");
    assert_eq!(arc_logs(&log, "Busy").len(), 3);
    let e = err(&call(&c, "registry_fetch", json!({"id": "arc-jammed"})).await);
    assert!(
        e.contains("HTTP 429") && e.contains("after 3 retries"),
        "{e}"
    );
    assert_eq!(arc_logs(&log, "Jammed").len(), 4);
    c.cancel().await.unwrap();
}
