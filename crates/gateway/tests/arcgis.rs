//! `arcgis` connections (T-10, build brief Part 4, REQ-8, REQ-9, R-10 to
//! R-12, R-14) through the real `columbus-mcp` binary and Python worker,
//! against a fake FeatureServer on 127.0.0.1 (debug builds may use plain
//! HTTP there when COLUMBUS_FETCH_TEST_LOOPBACK is set; release builds
//! never do). The fake logs every request, so a test can show that a
//! refusal sent none.
//!
//! - `Roads/FeatureServer`: two layers both named `Roadways` (polylines
//!   with M, and points) and one table, `Inspections`. It sends the hidden
//!   field `SSN` whatever it is asked, so the gateway's trimming shows.
//! - `Big/FeatureServer/0`: 250,000 points, generated page by page.
//! - `Busy/FeatureServer`: every query answers HTTP 429.
//!
//! Needs Python with geopandas, pyogrio, shapely 2.1+ and pyproj; skipped
//! otherwise.

mod support;

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
    support::python_for(
        "import geopandas, pyogrio, pyproj, shapely; assert hasattr(shapely, 'has_m')",
    )
}

/// Every request the fake answered, as `GET <path>?<query>` (or `POST`).
type Log = Arc<Mutex<Vec<String>>>;

const BIG_ROWS: u64 = 250_000;
const BIG_PAGE: u64 = 64_000;

fn sr() -> Value {
    json!({"wkid": 102100, "latestWkid": 3857})
}

fn roads_fields() -> Value {
    json!([
        {"name": "OBJECTID", "type": "esriFieldTypeOID", "alias": "OBJECTID"},
        {"name": "DISTRICT", "type": "esriFieldTypeInteger", "alias": "District",
         "domain": {"type": "codedValue", "name": "Districts",
                    "codedValues": [{"name": "Austin", "code": 14}, {"name": "Waco", "code": 9}]}},
        {"name": "LANES", "type": "esriFieldTypeSmallInteger", "alias": "Lanes"},
        {"name": "NAME", "type": "esriFieldTypeString", "alias": "Name", "length": 50},
        {"name": "SSN", "type": "esriFieldTypeString", "alias": "Owner SSN", "length": 11}
    ])
}

fn layer_json(id: u64) -> Option<Value> {
    let paging = json!({"supportsPagination": true, "supportsOrderBy": true});
    Some(match id {
        0 => json!({
            "id": 0, "name": "Roadways", "type": "Feature Layer",
            "geometryType": "esriGeometryPolyline", "hasM": true, "hasZ": false,
            "objectIdField": "OBJECTID", "maxRecordCount": 1,
            "advancedQueryCapabilities": paging,
            "extent": {"xmin": -10_890_000.0, "ymin": 3_530_000.0, "xmax": -10_870_000.0, "ymax": 3_550_000.0,
                       "spatialReference": sr()},
            "fields": roads_fields(),
        }),
        1 => json!({
            "id": 1, "name": "Roadways", "type": "Feature Layer",
            "geometryType": "esriGeometryPoint", "hasM": false, "hasZ": false,
            "objectIdField": "OBJECTID", "maxRecordCount": 1000,
            "advancedQueryCapabilities": paging,
            "extent": {"xmin": -10_890_000.0, "ymin": 3_530_000.0, "xmax": -10_870_000.0, "ymax": 3_550_000.0,
                       "spatialReference": sr()},
            "fields": roads_fields(),
        }),
        2 => json!({
            "id": 2, "name": "Inspections", "type": "Table",
            "objectIdField": "OBJECTID", "maxRecordCount": 1000,
            "advancedQueryCapabilities": paging,
            "fields": [
                {"name": "OBJECTID", "type": "esriFieldTypeOID"},
                {"name": "DISTRICT", "type": "esriFieldTypeInteger"},
                {"name": "RESULT", "type": "esriFieldTypeString", "length": 10}
            ],
        }),
        _ => return None,
    })
}

/// Roadways (layer 0): five routes with measures; three in district 14.
fn route(oid: u64) -> Value {
    let district = if oid <= 3 { 14 } else { 9 };
    let x = -10_880_000.0 + oid as f64 * 100.0;
    json!({
        "attributes": {"OBJECTID": oid, "DISTRICT": district, "LANES": 1 + oid, "NAME": format!("Road {oid}"),
                       "SSN": format!("123-45-000{oid}")},
        "geometry": {"hasM": true, "paths": [[[x, 3_540_000.0, 100.5 * oid as f64], [x + 50.0, 3_540_050.0, 100.5 * oid as f64 + 7.25]]]}
    })
}

fn roads_query(q: &HashMap<String, String>) -> Value {
    // The fake understands the two filters the tests use.
    let w = q.get("where").cloned().unwrap_or_default();
    let district = w.starts_with("(DISTRICT = 14)");
    let lanes = w.contains("(LANES > 2)");
    let rows: Vec<u64> = (1..=5)
        .filter(|o| !district || *o <= 3)
        .filter(|o| !lanes || 1 + *o > 2)
        .collect();
    if q.get("returnCountOnly").map(String::as_str) == Some("true") {
        return json!({"count": rows.len()});
    }
    let k: usize = q
        .get("resultOffset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let n: usize = q
        .get("resultRecordCount")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .min(1);
    let end = (k + n).min(rows.len());
    let page: Vec<Value> = rows[k.min(rows.len())..end]
        .iter()
        .map(|o| route(*o))
        .collect();
    let mut body = json!({
        "objectIdFieldName": "OBJECTID", "geometryType": "esriGeometryPolyline", "hasM": true,
        "spatialReference": sr(), "fields": roads_fields(), "features": page,
    });
    if end < rows.len() {
        body["exceededTransferLimit"] = json!(true);
    }
    body
}

/// One page of the big layer, written straight to text (cheap to make).
fn big_query(q: &HashMap<String, String>) -> String {
    if q.get("returnCountOnly").map(String::as_str) == Some("true") {
        return json!({"count": BIG_ROWS}).to_string();
    }
    let k: u64 = q
        .get("resultOffset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let n: u64 = q
        .get("resultRecordCount")
        .and_then(|v| v.parse().ok())
        .unwrap_or(BIG_PAGE)
        .min(BIG_PAGE);
    let end = (k + n).min(BIG_ROWS);
    let mut s = String::with_capacity(((end.saturating_sub(k)) * 80 + 400) as usize);
    s.push_str(r#"{"objectIdFieldName":"OBJECTID","geometryType":"esriGeometryPoint","spatialReference":{"wkid":4326,"latestWkid":4326},"fields":[{"name":"OBJECTID","type":"esriFieldTypeOID"},{"name":"VOLUME","type":"esriFieldTypeInteger"}],"features":["#);
    for oid in (k + 1)..=end {
        if oid > k + 1 {
            s.push(',');
        }
        s.push_str(&format!(
            r#"{{"attributes":{{"OBJECTID":{oid},"VOLUME":{}}},"geometry":{{"x":{:.6},"y":{:.6}}}}}"#,
            oid % 997,
            -98.0 + (oid % 1000) as f64 * 0.001,
            30.0 + (oid / 1000) as f64 * 0.001
        ));
    }
    s.push(']');
    if end < BIG_ROWS {
        s.push_str(r#","exceededTransferLimit":true"#);
    }
    s.push('}');
    s
}

fn big_layer() -> Value {
    json!({
        "id": 0, "name": "Counters", "type": "Feature Layer", "geometryType": "esriGeometryPoint",
        "objectIdField": "OBJECTID", "maxRecordCount": BIG_PAGE,
        "advancedQueryCapabilities": {"supportsPagination": true, "supportsOrderBy": true},
        "extent": {"xmin": -98.0, "ymin": 30.0, "xmax": -97.0, "ymax": 30.25, "spatialReference": {"wkid": 4326, "latestWkid": 4326}},
        "fields": [{"name": "OBJECTID", "type": "esriFieldTypeOID"}, {"name": "VOLUME", "type": "esriFieldTypeInteger"}],
    })
}

/// (status, extra headers, body) for one request.
fn answer(path: &str, q: &HashMap<String, String>) -> (&'static str, &'static str, String) {
    let ok = |v: Value| ("200 OK", "", v.to_string());
    let missing = (
        "404 Not Found",
        "",
        json!({"error": {"code": 400, "message": "Invalid URL"}}).to_string(),
    );
    if let Some(rest) = path.strip_prefix("/arcgis/rest/services/Roads/FeatureServer") {
        let rest = rest.trim_end_matches('/');
        if rest == "/layers" {
            return ok(json!({
                "layers": [layer_json(0), layer_json(1)],
                "tables": [layer_json(2)],
            }));
        }
        let mut parts = rest.trim_start_matches('/').split('/');
        let Some(id) = parts.next().and_then(|i| i.parse::<u64>().ok()) else {
            return missing;
        };
        let Some(layer) = layer_json(id) else {
            return missing;
        };
        return match parts.next() {
            None => ok(layer),
            Some("query") if id == 0 => ok(roads_query(q)),
            Some("query") if q.get("returnCountOnly").map(String::as_str) == Some("true") => {
                ok(json!({"count": 1}))
            }
            Some("query") => ok(json!({
                "objectIdFieldName": "OBJECTID",
                "fields": layer["fields"],
                "features": [{"attributes": {"OBJECTID": 1, "DISTRICT": 14, "RESULT": "pass"}}],
            })),
            _ => missing,
        };
    }
    if let Some(rest) = path.strip_prefix("/arcgis/rest/services/Big/FeatureServer/0") {
        return match rest {
            "" | "/" => ok(big_layer()),
            "/query" => ("200 OK", "", big_query(q)),
            _ => missing,
        };
    }
    if let Some(rest) = path.strip_prefix("/arcgis/rest/services/Busy/FeatureServer") {
        return match rest {
            "/layers" => ok(json!({"layers": [layer_json(1)], "tables": []})),
            "/1" => ok(layer_json(1).unwrap()),
            "/1/query" => (
                "429 Too Many Requests",
                "Retry-After: 1\r\n",
                json!({"error": {"code": 429, "message": "Too many requests"}}).to_string(),
            ),
            _ => missing,
        };
    }
    missing
}

/// The fake FeatureServer: one request at a time, each logged.
fn serve() -> (u16, Log) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    std::thread::spawn(move || {
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
            let (status, extra, text) = answer(u.path(), &q);
            let mut w = s;
            let _ = write!(w, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", text.len());
            let _ = w.write_all(text.as_bytes());
        }
    });
    (port, log)
}

/// The query parameters of a logged request.
fn params(entry: &str) -> HashMap<String, String> {
    let target = entry.split_once(' ').map(|(_, t)| t).unwrap_or("");
    Url::parse(&format!("http://x{target}"))
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn setup(port: u16) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [arcgis]\nhosts: [\"127.0.0.1\"]\nhide_fields: [SSN]\n\
         limits: { rows: 200000, seconds: 120 }\napprovals: { page: false }\n",
    )
    .unwrap();
    let base = format!("http://127.0.0.1:{port}/arcgis/rest/services");
    let conns = [
        (
            "roads",
            format!("id: roads\ntype: arcgis\nurl: {base}/Roads/FeatureServer\nrow_filter: \"DISTRICT = 14\"\ndescription: Roads (fake)\ndatasets:\n  allow: [\"*\"]\n"),
        ),
        (
            "elsewhere",
            format!("id: elsewhere\ntype: arcgis\nurl: https://localhost:{port}/arcgis/rest/services/Roads/FeatureServer\ndatasets:\n  allow: [\"*\"]\n"),
        ),
        (
            "big",
            format!("id: big\ntype: arcgis\nurl: {base}/Big/FeatureServer/0\ndatasets:\n  allow: [\"*\"]\n"),
        ),
        (
            "busy",
            format!("id: busy\ntype: arcgis\nurl: {base}/Busy/FeatureServer\ndatasets:\n  allow: [\"*\"]\n"),
        ),
    ];
    for (id, text) in conns {
        std::fs::write(root.join(format!("connections/{id}.yaml")), text).unwrap();
    }
    (dir, root)
}

async fn start(root: &Path, py: &str) -> RunningService<RoleClient, ()> {
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

/// The workspace layer a result handle names (`ws://<session>/<rel>`).
fn layer_path(root: &Path, handle: &str) -> PathBuf {
    let rel = handle
        .strip_prefix("ws://")
        .and_then(|h| h.split_once('/'))
        .map(|(_, r)| r.to_string())
        .unwrap();
    let mut found = Vec::new();
    let mut stack = vec![root.join("workspaces")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().replace('\\', "/").ends_with(&rel) {
                found.push(p);
            }
        }
    }
    assert_eq!(found.len(), 1, "{handle}: {found:?}");
    found.remove(0)
}

#[tokio::test]
async fn arcgis_lists_describes_and_queries_a_public_service() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let (port, log) = serve();
    let (_dir, root) = setup(port);
    let c = start(&root, &py).await;

    // REQ-8: three datasets, F-1b names, geometry type and latestWkid; the
    // hidden field is not listed.
    let list = ok(&call(&c, "list_datasets", json!({"connection": "roads"})).await);
    let ds = list["result"]["datasets"].as_array().unwrap();
    let names: Vec<&str> = ds.iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Roadways_0", "Roadways_1", "Inspections"], "{list}");
    assert_eq!(ds[0]["geometry_type"], "LineString M");
    assert_eq!(ds[0]["crs"]["latest_wkid"], 3857);
    assert_eq!(ds[1]["geometry_type"], "Point");
    assert_eq!(ds[2]["type"], "table");
    assert!(ds[2]["geometry_column"].is_null());
    assert!(!list.to_string().contains("SSN"), "{list}");

    // describe_dataset: fields with domains, CRS from latestWkid, and the
    // row count under the row filter (returnCountOnly).
    let before = log.lock().unwrap().len();
    let d = ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "roads", "dataset": "Roadways_0"}),
    )
    .await);
    assert_eq!(d["result"]["rows"], 3, "{d}");
    assert_eq!(d["result"]["crs"]["code"], "EPSG:3857");
    let district = d["result"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "DISTRICT")
        .unwrap();
    assert_eq!(district["domain"]["coded_values"][0]["name"], "Austin");
    assert!(!d.to_string().contains("SSN"));
    assert!(ids(&d).contains(&"query.row_filter".to_string()));
    let counts: Vec<HashMap<String, String>> = log.lock().unwrap()[before..]
        .iter()
        .map(|e| params(e))
        .filter(|p| p.get("returnCountOnly").map(String::as_str) == Some("true"))
        .collect();
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0]["where"], "(DISTRICT = 14)");

    // REQ-9: the row filter and the caller's where, each rebuilt, in
    // parentheses and ANDed; Z/M asked for; the hidden field not asked for
    // and, though the service sends it, not kept.
    let before = log.lock().unwrap().len();
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "roads", "dataset": "Roadways_0", "where": "lanes > 2"}),
    )
    .await);
    assert_eq!(q["result"]["rows_read"], 2, "{q}");
    let queries: Vec<HashMap<String, String>> = log.lock().unwrap()[before..]
        .iter()
        .filter(|e| e.contains("/Roads/FeatureServer/0/query"))
        .map(|e| params(e))
        .collect();
    assert_eq!(queries.len(), 2, "two pages of 1: {queries:?}");
    for p in &queries {
        assert_eq!(p["where"], "(DISTRICT = 14) AND (LANES > 2)");
        assert_eq!(p["f"], "json");
        assert_eq!(p["returnM"], "true");
        assert_eq!(p["outFields"], "OBJECTID,DISTRICT,LANES,NAME");
        assert_eq!(p["orderByFields"], "OBJECTID ASC");
    }
    // Its values never come back; its name only in the usual notice.
    assert!(!q.to_string().contains("123-45"), "{q}");
    assert!(!q["result"]["columns"].to_string().contains("SSN"));
    assert!(ids(&q).contains(&"policy.hidden_fields".to_string()));
    assert!(q["result"]["executed_sql"]
        .as_str()
        .unwrap()
        .contains("(DISTRICT = 14) AND (LANES > 2)"));

    // M values are in the workspace layer; the hidden field is not.
    let path = layer_path(&root, q["result"]["layer"]["handle"].as_str().unwrap());
    let script = r#"
import sys, warnings, geopandas as gpd, shapely
warnings.simplefilter("ignore")
df = gpd.read_file(sys.argv[1], engine="pyogrio", use_arrow=True)
assert "SSN" not in df.columns, list(df.columns)
assert bool(shapely.has_m(df.geometry.values).all()), "M lost"
first = shapely.get_coordinates(df.geometry.iloc[0], include_m=True)[0]
print(len(df), first[-1], df.crs.to_epsg())
"#;
    let o = Command::new(&py)
        .args(["-c", script])
        .arg(&path)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // Rows 2 and 3 (LANES 3, 4) in OBJECTID order; route 2 starts at M 201.
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "2 201.0 3857");

    // bbox becomes an envelope with inSR; order_by becomes orderByFields.
    let before = log.lock().unwrap().len();
    ok(&call(
        &c,
        "query_features",
        json!({"connection": "roads", "dataset": "Roadways_0", "order_by": "name",
               "bbox": [-10_890_000.0, 3_530_000.0, -10_870_000.0, 3_550_000.0], "fields": ["NAME"]}),
    )
    .await);
    let p = log.lock().unwrap()[before..]
        .iter()
        .filter(|e| e.contains("/0/query"))
        .map(|e| params(e))
        .next()
        .unwrap();
    assert_eq!(p["orderByFields"], "NAME ASC, OBJECTID ASC");
    assert_eq!(p["geometryType"], "esriGeometryEnvelope");
    assert_eq!(p["inSR"], "3857");
    assert_eq!(p["geometry"], "-10890000,3530000,-10870000,3550000");
    assert_eq!(p["outFields"], "NAME");
    assert_eq!(p["where"], "(DISTRICT = 14)");

    // sample_rows reads a few rows under the filter; a table works too.
    let s = ok(&call(
        &c,
        "sample_rows",
        json!({"connection": "roads", "dataset": "Roadways_0", "n": 1}),
    )
    .await);
    assert_eq!(s["result"]["rows_returned"], 1);
    ok(&call(
        &c,
        "query_features",
        json!({"connection": "roads", "dataset": "Inspections"}),
    )
    .await);

    // R-12: no SQL on an ArcGIS service.
    let e = err(&call(
        &c,
        "sql_select",
        json!({"connection": "roads", "sql": "SELECT * FROM Roadways_0"}),
    )
    .await);
    assert!(
        e.contains("query.unsupported") && e.contains("use query_features"),
        "{e}"
    );

    // Provenance names the request (no token in it: there is none) and the
    // audit log records the reads.
    let audit: String = std::fs::read_dir(root.join("audit"))
        .unwrap()
        .flatten()
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect();
    assert!(audit.contains("\"query_features\"") && audit.contains("\"roads\""));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn arcgis_refuses_before_any_request_retries_429_and_stops_at_the_read_limit() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let (port, log) = serve();
    let (_dir, root) = setup(port);
    let c = start(&root, &py).await;
    // The layer list and description are read once, so the refusals below
    // would have room to send a request if they did.
    ok(&call(
        &c,
        "describe_dataset",
        json!({"connection": "roads", "dataset": "Roadways_0"}),
    )
    .await);

    let q = |where_: &str| json!({"connection": "roads", "dataset": "Roadways_0", "where": where_});
    let cases: Vec<(&str, Value, &str)> = vec![
        // A host not on policy hosts.
        (
            "list_datasets",
            json!({"connection": "elsewhere"}),
            "not on the organization's allowed host list",
        ),
        (
            "query_features",
            json!({"connection": "elsewhere", "dataset": "Roadways_0"}),
            "not on the organization's allowed host list",
        ),
        // A where outside the parser's subset.
        (
            "query_features",
            q("LANES IN (SELECT LANES FROM Roadways_1)"),
            "not allowed in a where clause",
        ),
        // A hidden field in where (and order_by).
        (
            "query_features",
            q("SSN LIKE '1%'"),
            "SSN is hidden by organization policy",
        ),
        (
            "query_features",
            json!({"connection": "roads", "dataset": "Roadways_0", "order_by": "SSN"}),
            "SSN is hidden by organization policy",
        ),
        // The escape attempt does not parse.
        ("query_features", q("1=1) OR (1=1"), "where clause"),
    ];
    for (tool, args, why) in cases {
        let before = log.lock().unwrap().clone();
        let e = err(&call(&c, tool, args.clone()).await);
        assert!(e.contains(why), "{tool} {args}: {e}");
        let after = log.lock().unwrap().clone();
        assert_eq!(
            after.len(),
            before.len(),
            "{tool} {args} sent {:?}",
            &after[before.len()..]
        );
    }

    // HTTP 429: retried at most 3 times with Retry-After, then R-14.
    let before = log.lock().unwrap().len();
    let e = err(&call(
        &c,
        "query_features",
        json!({"connection": "busy", "dataset": "Roadways"}),
    )
    .await);
    assert!(e.contains("ArcGIS service at 127.0.0.1 is limiting requests (HTTP 429) and still refused after 3 retries"), "{e}");
    let tries = log.lock().unwrap()[before..]
        .iter()
        .filter(|e| e.contains("/Busy/FeatureServer/1/query"))
        .count();
    assert_eq!(tries, 4, "one request and 3 retries");

    // R-11: 250,000 rows under a 200,000 read limit: 200,000 rows, marked.
    let started = std::time::Instant::now();
    let before = log.lock().unwrap().len();
    let big = ok(&call(
        &c,
        "query_features",
        json!({"connection": "big", "dataset": "Counters"}),
    )
    .await);
    assert_eq!(big["result"]["rows_read"], 200_000, "{}", big["checks"]);
    assert!(
        ids(&big).contains(&"query.read_limit".to_string()),
        "{}",
        big["checks"]
    );
    assert_eq!(big["result"]["layer"]["summary"]["rows"], 200_000);
    let asked: Vec<u64> = log.lock().unwrap()[before..]
        .iter()
        .filter(|e| e.contains("/Big/FeatureServer/0/query") && !e.contains("returnCountOnly"))
        .map(|e| params(e)["resultRecordCount"].parse().unwrap())
        .collect();
    assert_eq!(asked, [64_000, 64_000, 64_000, 8_000], "pages asked for");
    eprintln!("200,000-row read took {:?}", started.elapsed());
    c.cancel().await.unwrap();
}
