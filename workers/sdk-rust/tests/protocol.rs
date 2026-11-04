//! Speaks the worker protocol with the built `geometry_stats` example, as
//! the gateway does: one process, line-delimited JSON-RPC on stdin/stdout.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::{json, Value};

/// The example executable. A plain `cargo test` builds it; `cargo test
/// --test protocol` does not, so it is built here once when missing.
fn example_binary() -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            // target/<profile>/deps/protocol-<hash>[.exe] → target/<profile>/examples/
            let exe = std::env::current_exe().unwrap();
            let profile_dir = exe.parent().and_then(Path::parent).unwrap();
            let bin = profile_dir
                .join("examples")
                .join(format!("geometry_stats{}", std::env::consts::EXE_SUFFIX));
            if !bin.is_file() {
                let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
                let mut cmd = Command::new(cargo);
                cmd.args([
                    "build",
                    "-p",
                    "columbus-plugin",
                    "--example",
                    "geometry_stats",
                ]);
                if profile_dir.file_name().is_some_and(|n| n == "release") {
                    cmd.arg("--release");
                }
                let status = cmd.status().expect("run cargo to build the example");
                assert!(status.success(), "building the example failed");
            }
            assert!(bin.is_file(), "example not built at {}", bin.display());
            bin
        })
        .clone()
}

struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    /// Messages read while waiting for something else.
    seen: Vec<Value>,
}

impl Worker {
    fn start() -> Worker {
        let mut child = Command::new(example_binary())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start the example plugin");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.expect("read stdout");
                let msg: Value = serde_json::from_str(&line).unwrap_or_else(|e| {
                    panic!("worker wrote a line that is not JSON ({e}): {line:?}")
                });
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        Worker {
            stdin: child.stdin.take(),
            child,
            lines: rx,
            seen: Vec::new(),
        }
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, id: i64, method: &str, params: Value) {
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.send_raw(&msg.to_string());
    }

    /// Wait for the first message `pred` accepts, keeping the others.
    fn wait_for(&mut self, within: Duration, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.seen.iter().position(&pred) {
            return self.seen.remove(i);
        }
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(m) if pred(&m) => return m,
                Ok(m) => self.seen.push(m),
                Err(RecvTimeoutError::Timeout) => {
                    panic!("no {what} within {within:?}; saw {:?}", self.seen)
                }
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("worker closed stdout waiting for {what}")
                }
            }
        }
    }

    fn reply(&mut self, id: i64, within: Duration) -> Value {
        let m = self.wait_for(within, &format!("reply {id}"), |m| m["id"] == id);
        assert_eq!(m["jsonrpc"], "2.0");
        m
    }

    fn notifications(&self, method: &str) -> Vec<&Value> {
        self.seen.iter().filter(|m| m["method"] == method).collect()
    }

    /// Close stdin and wait for the process to exit.
    fn finish(mut self, within: Duration) -> ExitStatus {
        drop(self.stdin.take());
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("worker did not exit within {within:?} of stdin closing");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const SECOND: Duration = Duration::from_secs(1);

// --- GeoPackage blobs ----------------------------------------------------

fn gpkg(wkb: Vec<u8>) -> Vec<u8> {
    // Header: magic, version 0, flags (little endian, no envelope), srs_id.
    let mut b = vec![b'G', b'P', 0, 0b0000_0001];
    b.extend_from_slice(&4326i32.to_le_bytes());
    b.extend(wkb);
    b
}

fn wkb(ty: u32, body: &[u8]) -> Vec<u8> {
    let mut b = vec![1];
    b.extend_from_slice(&ty.to_le_bytes());
    b.extend_from_slice(body);
    b
}

fn coords(pts: &[[f64; 2]]) -> Vec<u8> {
    let mut b = (pts.len() as u32).to_le_bytes().to_vec();
    for p in pts {
        b.extend_from_slice(&p[0].to_le_bytes());
        b.extend_from_slice(&p[1].to_le_bytes());
    }
    b
}

fn polygon(ring: &[[f64; 2]]) -> Vec<u8> {
    let mut body = 1u32.to_le_bytes().to_vec();
    body.extend(coords(ring));
    gpkg(wkb(3, &body))
}

fn make_extract(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE data (fid INTEGER, name TEXT, geom BLOB);
         CREATE TABLE columbus_extract (key TEXT PRIMARY KEY, value TEXT);",
    )
    .unwrap();
    let meta = [
        ("connection", "test"),
        ("dataset", "shapes"),
        ("rows", "5"),
        ("geometry_column", "geom"),
        ("geometry_format", "gpkg"),
        ("srid", "4326"),
        ("labels", "[]"),
    ];
    for (k, v) in meta {
        db.execute("INSERT INTO columbus_extract VALUES (?1, ?2)", [k, v])
            .unwrap();
    }
    let point = gpkg(wkb(
        1,
        &[(-97.75f64).to_le_bytes(), 30.25f64.to_le_bytes()].concat(),
    ));
    let line = gpkg(wkb(2, &coords(&[[-98.0, 30.0], [-97.0, 31.0]])));
    let square = polygon(&[
        [-97.5, 30.1],
        [-97.4, 30.1],
        [-97.4, 30.2],
        [-97.5, 30.2],
        [-97.5, 30.1],
    ]);
    let unclosed = polygon(&[[-97.2, 29.5], [-97.1, 29.5], [-97.1, 29.6], [-97.2, 29.6]]);
    let rows: [(i64, &str, Option<Vec<u8>>); 5] = [
        (1, "point", Some(point)),
        (2, "line", Some(line)),
        (3, "square", Some(square)),
        (4, "unclosed, \"bad\"", Some(unclosed)),
        (5, "no geometry", None),
    ];
    for (fid, name, geom) in rows {
        db.execute(
            "INSERT INTO data VALUES (?1, ?2, ?3)",
            rusqlite::params![fid, name, geom],
        )
        .unwrap();
    }
}

// --- tests ---------------------------------------------------------------

#[test]
fn describe_matches_the_manifest() {
    let mut w = Worker::start();
    w.request(1, "describe", json!({}));
    let r = w.reply(1, 5 * SECOND);
    let d = &r["result"];
    assert_eq!(d["protocol"], 2);
    assert_eq!(d["plugin"]["id"], "columbus.example-rust");
    assert_eq!(d["plugin"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        d["tools"],
        json!([
            {"name": "bbox", "params": ["layer"], "outputs": ["stats"]},
            {"name": "wait", "params": ["seconds"], "outputs": []}
        ])
    );

    // The gateway disables a plugin whose describe differs from plugin.yaml.
    let yaml_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/geometry-stats.plugin.yaml");
    let m: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(yaml_path).unwrap()).unwrap();
    assert_eq!(m["id"].as_str(), d["plugin"]["id"].as_str());
    assert_eq!(
        m["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION")),
        "update the example manifest's version"
    );
    assert_eq!(m["runtime"].as_str(), Some("rust"));
    assert_eq!(m["entry"].as_str(), Some("geometry_stats"));
    let keys = |v: &serde_yaml::Value| -> BTreeSet<String> {
        v.as_mapping()
            .map(|m| {
                m.keys()
                    .filter_map(|k| k.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let names = |v: &Value| -> BTreeSet<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    };
    let tools = d["tools"].as_array().unwrap();
    assert_eq!(
        keys(&m["tools"]),
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    );
    for t in tools {
        let spec = &m["tools"][t["name"].as_str().unwrap()];
        assert_eq!(keys(&spec["params"]), names(&t["params"]));
        assert_eq!(keys(&spec["outputs"]), names(&t["outputs"]));
    }

    assert!(w.finish(5 * SECOND).success());
}

#[test]
fn bbox_writes_stats_and_warns_about_invalid_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let extract = dir.path().join("inputs").join("r1-layer.sqlite");
    std::fs::create_dir_all(extract.parent().unwrap()).unwrap();
    make_extract(&extract);
    let stats = dir.path().join("results").join("r1-stats.csv");

    let mut w = Worker::start();
    w.request(
        7,
        "run",
        json!({
            "run": "r1",
            "tool": "bbox",
            "params": {},
            "inputs": {"layer": {"kind": "dataset", "path": extract, "rows": 5}},
            "outputs": {"stats": {"type": "table", "path": stats}},
            "workspace": dir.path(),
            "timeout_s": 60
        }),
    );
    let r = w.reply(7, 10 * SECOND);
    assert!(r.get("error").is_none(), "{r}");
    let result = &r["result"];
    assert_eq!(
        result["summary"],
        json!({"rows": 5, "bbox": [-98.0, 29.5, -97.0, 31.0]})
    );
    assert_eq!(result["outputs"], json!({"stats": {"rows": 13}}));
    let warnings = result["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["id"], "geometry.invalid");
    assert_eq!(warnings[0]["severity"], "warn");
    assert_eq!(warnings[0]["detail"], json!({"count": 1}));
    assert!(warnings[0]["message"]
        .as_str()
        .unwrap()
        .starts_with("1 of 4"));
    assert!(!w.notifications("progress").is_empty());
    for p in w.notifications("progress") {
        assert_eq!(p["params"]["run"], "r1");
        assert!(p.get("id").is_none());
    }

    let csv = std::fs::read_to_string(&stats).unwrap();
    let expected = [
        "metric,value",
        "rows,5",
        "geometries,4",
        "null,1",
        "empty,0",
        "invalid,1",
        "undecodable,0",
        "min_x,-98",
        "min_y,29.5",
        "max_x,-97",
        "max_y,31",
        "type.LineString,1",
        "type.Point,1",
        "type.Polygon,2",
    ];
    assert_eq!(csv, expected.join("\r\n") + "\r\n");

    // The extract was opened read-only: unchanged, no journal left behind.
    let db = Connection::open(&extract).unwrap();
    let n: i64 = db
        .query_row("SELECT count(*) FROM data", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 5);

    assert!(w.finish(5 * SECOND).success());
}

#[test]
fn cancel_stops_a_run_and_busy_refuses_a_second() {
    let mut w = Worker::start();
    w.request(1, "run", json!({"run": "r_wait", "tool": "wait", "params": {"seconds": 30}, "inputs": {}, "outputs": {}, "timeout_s": 120}));
    let first = w.wait_for(5 * SECOND, "progress", |m| m["method"] == "progress");
    assert_eq!(first["params"]["run"], "r_wait");
    assert!(first["params"]["percent"].is_number());
    assert!(first["params"]["message"].is_string());

    // One run at a time.
    w.request(
        2,
        "run",
        json!({"run": "r_other", "tool": "wait", "params": {"seconds": 1}}),
    );
    let busy = w.reply(2, 5 * SECOND);
    assert_eq!(busy["error"]["code"], -32002);
    assert!(busy["error"]["message"].as_str().unwrap().contains("busy"));

    // The reader keeps serving while the run works.
    w.request(3, "describe", json!({}));
    assert_eq!(w.reply(3, 5 * SECOND)["result"]["protocol"], 2);

    let cancelled_at = Instant::now();
    w.request(4, "cancel", json!({"run": "r_wait"}));
    assert_eq!(w.reply(4, 5 * SECOND)["result"], json!({}));
    let r = w.reply(1, 10 * SECOND);
    assert_eq!(r["error"]["code"], -32004, "{r}");
    assert!(cancelled_at.elapsed() < 10 * SECOND);

    // The worker is free again.
    w.request(
        5,
        "run",
        json!({"run": "r_short", "tool": "wait", "params": {"seconds": 0.1}}),
    );
    let r = w.reply(5, 10 * SECOND);
    assert!(
        r["result"]["summary"]["waited_s"].as_f64().unwrap() >= 0.1,
        "{r}"
    );

    assert!(w.finish(5 * SECOND).success());
}

#[test]
fn timeout_s_is_enforced() {
    let mut w = Worker::start();
    w.request(
        1,
        "run",
        json!({"run": "r_t", "tool": "wait", "params": {"seconds": 30}, "timeout_s": 0.3}),
    );
    let r = w.reply(1, 10 * SECOND);
    assert_eq!(r["error"]["code"], -32003, "{r}");
    assert!(w.finish(5 * SECOND).success());
}

#[test]
fn protocol_errors() {
    let mut w = Worker::start();
    w.request(1, "no_such_method", json!({}));
    assert_eq!(w.reply(1, 5 * SECOND)["error"]["code"], -32601);

    w.send_raw("{this is not json");
    let parse = w.wait_for(5 * SECOND, "parse error", |m| m["error"]["code"] == -32700);
    assert_eq!(parse["id"], Value::Null);

    w.request(2, "run", json!({"run": "r", "tool": "no_such_tool"}));
    assert_eq!(w.reply(2, 5 * SECOND)["error"]["code"], -32602);

    w.request(
        3,
        "run",
        json!({"run": "r", "tool": "bbox", "inputs": {}, "outputs": {}}),
    );
    assert_eq!(w.reply(3, 5 * SECOND)["error"]["code"], -32602);

    w.request(
        4,
        "run",
        json!({"run": "r", "tool": "wait", "params": {"seconds": "soon"}}),
    );
    assert_eq!(w.reply(4, 5 * SECOND)["error"]["code"], -32602);

    let missing = std::env::temp_dir().join("columbus-plugin-test-missing.sqlite");
    w.request(
        5,
        "run",
        json!({"run": "r", "tool": "bbox",
        "inputs": {"layer": {"kind": "dataset", "path": missing}},
        "outputs": {"stats": {"type": "table", "path": "unused.csv"}}}),
    );
    assert_eq!(w.reply(5, 5 * SECOND)["error"]["code"], -32002);

    // Still alive and answering.
    w.request(6, "describe", json!({}));
    assert_eq!(w.reply(6, 5 * SECOND)["result"]["protocol"], 2);
    assert!(w.finish(5 * SECOND).success());
}

#[test]
fn eof_lets_an_active_run_reply_then_exits() {
    let mut w = Worker::start();
    w.request(
        1,
        "run",
        json!({"run": "r_eof", "tool": "wait", "params": {"seconds": 0.5}}),
    );
    w.wait_for(5 * SECOND, "progress", |m| m["method"] == "progress");
    drop(w.stdin.take());
    let r = w.reply(1, 10 * SECOND);
    assert!(r["result"].is_object(), "{r}");
    assert!(w.finish(5 * SECOND).success());
}
