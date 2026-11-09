//! T-23 (build brief part 2: REQ-30, success measure 7): the owner's weekly
//! right-of-way (ROW) job, end to end on fixtures.
//!
//! 1. A read-only connection `lrs` holds the route network with measures
//!    (`ROW_ROUTES`, LINESTRING M, three routes A, B and C, measures 0 to
//!    1,000 ft) and an LRS event table of ROW widths (`ROW_EVENTS`) with
//!    exactly two gaps (A 400 to 600, C 700 to 1,000). An `arcgis`
//!    connection `row-polygons` reads the owned-ROW polygons from a fake
//!    ArcGIS service (`examples/fake_oauth_server.rs`, `ROW/FeatureServer`).
//! 2. Two stand-in personal tools (`tests/fixtures/row_job/`), written from a
//!    chat with `propose_tool`, test-run after the person's approval and
//!    installed as the app does: `find_gaps` and `fix_widths` (transects
//!    every 50 ft, runs of widths within 2 ft grouped into one segment).
//! 3. A weekly workflow, `row-weekly`, runs them and dissolves the widths by
//!    route name, started with `columbus-mcp workflow run row-weekly
//!    --scheduled`, as Task Scheduler and launchd start it: no approval.
//! 4. `columbus-mcp workflow open row-weekly <run>` opens the run in a chat,
//!    which removes route C with `vector_op` `filter` and exports the
//!    approved widths as a file geodatabase to a folder that is not granted:
//!    the one approval of the job, given on the approvals page.
//! 5. A fake imagery WMTS, saved as a web layer with `columbus-mcp
//!    map-source inspect/save` (no `map.hosts`: saving is the person's
//!    approval, the confirm path), is turned on in the opened chat: its
//!    tiles load through the chat's own map page.
//!
//! Two runs of the same job: on Oracle (`ROADS.ROW_ROUTES`, SDO_GTYPE 3302,
//! in `tests/fixtures/oracle.sql`), when `COLUMBUS_TEST_ORA_HOST` is set (the
//! Oracle CI job), and on a GeoPackage holding the same data, wherever the
//! GIS engine is installed.
//!
//! Needs the GIS engine (Python with geopandas, pyogrio, shapely 2.1+ and
//! pyproj), a machine that can confine at level A, and the example built
//! (`cargo build --example fake_oauth_server`).

mod support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use columbus_shared::workflows as wf;
use reqwest::StatusCode;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

/// Made-up host names test builds let stand for the fake (127.0.0.1).
const TEST_HOSTS: &str = "imagery.fake.test";
const ORA_PASSWORD: &str = "columbus_ro_test";
const FIND_GAPS: &str = "personal.find-gaps.run";
const FIX_WIDTHS: &str = "personal.fix-widths.run";
const WEB_LAYER: &str = "county-imagery";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn engine() -> &'static str {
    "import geopandas, pyogrio, pyproj, shapely; assert hasattr(shapely, 'has_m')"
}

fn can_confine() -> bool {
    Command::new(env!("CARGO_BIN_EXE_columbus-confine"))
        .args(["--check", "A"])
        .output()
        .is_ok_and(|o| o.status.success())
}

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

/// The fake ArcGIS service and web map services, and its port.
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

    async fn log(&self) -> Vec<Value> {
        reqwest::get(format!("http://127.0.0.1:{}/test/log", self.1))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

/// Where the routes and events are.
enum Lrs {
    Oracle {
        host: String,
        port: String,
        service: String,
    },
    Gpkg,
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    py: String,
    routes: &'static str,
    events: &'static str,
}

fn q(p: &Path) -> String {
    serde_json::to_string(&p.display().to_string()).unwrap()
}

/// The routes and events of the Oracle fixture, as a GeoPackage.
fn make_gpkg(py: &str, path: &Path) {
    let script = r#"
import sys
import geopandas as gpd
import pandas as pd
import pyogrio
import shapely
sys.path.insert(0, sys.argv[2])
import columbus_gis as g
x0, x1 = -97.75, -97.746833
routes = gpd.GeoDataFrame(
    {"ROUTE_ID": ["RA", "RB", "RC"], "ROUTE_NAME": ["A", "B", "C"]},
    geometry=[shapely.from_wkt(f"LINESTRING M ({x0} {y} 0, {x1} {y} 1000)") for y in (30.25, 30.252, 30.254)],
    crs="EPSG:4326")
g.write_ogr(routes, sys.argv[1], "GPKG", layer="ROW_ROUTES")
events = pd.DataFrame({"ID": [1, 2, 3, 4], "ROUTE_ID": ["RA", "RA", "RB", "RC"],
                       "FROM_M": [0.0, 600.0, 0.0, 0.0], "TO_M": [400.0, 1000.0, 1000.0, 700.0],
                       "ROW_WIDTH_FT": [80.0, 86.0, 90.0, 100.0]})
pyogrio.write_dataframe(events, sys.argv[1], layer="ROW_EVENTS", driver="GPKG")
"#;
    let out = Command::new(py)
        .arg("-c")
        .arg(script)
        .arg(path)
        .arg(repo().join("workers/python"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Env {
    fn new(py: &str, port: u16, lrs: &Lrs) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        for d in ["connections", "map-sources", "review", "ui/assets"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("ui/index.html"), "<!doctype html><p>map</p>").unwrap();
        let mut hosts = vec!["127.0.0.1".to_string()];
        let (conn, routes, events) = match lrs {
            Lrs::Oracle {
                host,
                port,
                service,
            } => {
                hosts.push(host.clone());
                (
                    format!(
                        "type: oracle\nhost: {host}\nport: {port}\ndatabase: {service}\nuser: columbus_ro\n\
                         datasets:\n  allow: [\"ROADS.ROW_*\"]\n"
                    ),
                    "ROADS.ROW_ROUTES",
                    "ROADS.ROW_EVENTS",
                )
            }
            Lrs::Gpkg => {
                let gpkg = root.join("lrs.gpkg");
                make_gpkg(py, &gpkg);
                (
                    format!(
                        "type: gpkg\npath: {}\ndatasets:\n  allow: [\"ROW_*\"]\n",
                        q(&gpkg)
                    ),
                    "ROW_ROUTES",
                    "ROW_EVENTS",
                )
            }
        };
        // No `map:` section: each web layer's own host is the person's to
        // approve when they save it (F-14's default).
        std::fs::write(
            root.join("policy.yaml"),
            format!(
                "connection_types: [oracle, gpkg, arcgis]\nhosts: {}\n\
                 limits: {{ rows: 1000, seconds: 120 }}\napprovals: {{ page: true, wait_seconds: 0 }}\n\
                 allow_workflows: true\nallow_personal_tools: true\nallow_scripts: true\n\
                 exports:\n  - {{ id: review, path: {} }}\n",
                serde_json::to_string(&hosts).unwrap(),
                q(&root.join("review"))
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("connections/lrs.yaml"),
            format!("id: lrs\nmode: read\n{conn}"),
        )
        .unwrap();
        std::fs::write(
            root.join("connections/row-polygons.yaml"),
            format!(
                "id: row-polygons\ntype: arcgis\n\
                 url: http://127.0.0.1:{port}/arcgis/rest/services/ROW/FeatureServer\n\
                 datasets:\n  allow: [\"*\"]\n"
            ),
        )
        .unwrap();
        Env {
            _dir: dir,
            root,
            py: py.to_string(),
            routes,
            events,
        }
    }

    fn workflows(&self) -> PathBuf {
        self.root.join("workflows")
    }

    fn tools(&self) -> PathBuf {
        self.root.join("tools")
    }

    fn launch_file(&self) -> PathBuf {
        self.root.join("launch.txt")
    }

    fn vars(&self) -> Vec<(String, String)> {
        let r = |p: &str| self.root.join(p).display().to_string();
        vec![
            ("COLUMBUS_POLICY".into(), r("policy.yaml")),
            ("COLUMBUS_CONNECTIONS".into(), r("connections")),
            ("COLUMBUS_MAP_SOURCES".into(), r("map-sources")),
            ("COLUMBUS_SERVERS".into(), r("servers")),
            ("COLUMBUS_PERSONAL_SERVERS".into(), r("personal-servers")),
            ("COLUMBUS_PLUGINS".into(), r("plugins")),
            ("COLUMBUS_PERSONAL_PLUGINS".into(), r("personal-plugins")),
            ("COLUMBUS_WORKSPACES".into(), r("workspaces")),
            ("COLUMBUS_TOOLS".into(), r("tools")),
            ("COLUMBUS_ORG_TOOLS".into(), r("org-tools")),
            ("COLUMBUS_WORKFLOWS".into(), r("workflows")),
            ("COLUMBUS_AUDIT_DIR".into(), r("audit")),
            ("COLUMBUS_LIVE_DIR".into(), r("live")),
            ("COLUMBUS_UI_DIR".into(), r("ui")),
            ("COLUMBUS_DEV_SECRETS".into(), "1".into()),
            ("COLUMBUS_DEV_SECRETS_DIR".into(), r("secrets")),
            ("COLUMBUS_SECRET_LRS".into(), ORA_PASSWORD.into()),
            ("COLUMBUS_FETCH_TEST_LOOPBACK".into(), "1".into()),
            ("COLUMBUS_WEB_TEST_HOSTS".into(), TEST_HOSTS.into()),
            ("COLUMBUS_APPROVALS_LAUNCH_FILE".into(), r("launch.txt")),
            (
                "COLUMBUS_WORKER".into(),
                repo()
                    .join("workers/python/columbus_worker.py")
                    .display()
                    .to_string(),
            ),
            ("COLUMBUS_PYTHON".into(), self.py.clone()),
            ("COLUMBUS_LOG".into(), "warn".into()),
        ]
    }

    /// A chat: `columbus-mcp` with these arguments, as an AI app starts it.
    async fn start(&self, args: &[&str]) -> RunningService<RoleClient, ()> {
        let vars = self.vars();
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(
            move |cmd| {
                cmd.args(&args);
                for (k, v) in &vars {
                    cmd.env(k, v);
                }
            },
        );
        ().serve(TokioChildProcess::new(cmd).unwrap())
            .await
            .unwrap()
    }

    /// `columbus-mcp <args>` as the Columbus app runs it: (ok, JSON, text).
    fn cli(&self, args: &[&str]) -> (bool, Value, String) {
        let o = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"))
            .args(args)
            .envs(self.vars())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        let all = format!("{out}{}", String::from_utf8_lossy(&o.stderr));
        (
            o.status.success(),
            serde_json::from_str(&out).unwrap_or(Value::Null),
            all,
        )
    }

    /// Run a workflow as the scheduler does: (exit code, run.json, report).
    fn run(&self, id: &str) -> (i32, Value, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_columbus-mcp"))
            .args(["workflow", "run", id, "--scheduled"])
            .envs(self.vars())
            .output()
            .unwrap();
        let code = out.status.code().unwrap_or(-1);
        let runs = wf::list_runs(&self.workflows(), id, 1);
        let latest = runs.first().unwrap_or_else(|| {
            panic!(
                "no run recorded (exit {code}): {}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let (rec, report) =
            wf::read_run(&self.workflows(), id, latest["run"].as_str().unwrap()).unwrap();
        (code, rec, report)
    }

    /// Python on the engine: the script's stdout, as JSON.
    fn python(&self, script: &str, args: &[&Path]) -> Value {
        let out = Command::new(&self.py)
            .arg("-c")
            .arg(script)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

async fn call(c: &RunningService<RoleClient, ()>, tool: &str, args: Value) -> CallToolResult {
    let mut p = CallToolRequestParams::new(tool.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    c.call_tool(p).await.unwrap()
}

fn ok(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().expect("structured output")
}

fn handle(r: &Value) -> String {
    r["result"]["layer"]["handle"]
        .as_str()
        .or_else(|| r["result"]["saved"][0]["handle"].as_str())
        .unwrap_or_else(|| panic!("no layer: {r}"))
        .to_string()
}

/// The person's browser on a session's approvals and map page.
struct Browser {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Browser {
    /// The page the session opened (its launch link), signed in.
    async fn open(launch_file: &Path) -> Browser {
        let deadline = Instant::now() + Duration::from_secs(20);
        let launch = loop {
            if let Ok(u) = std::fs::read_to_string(launch_file) {
                if !u.is_empty() {
                    break u;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the approvals page was never opened"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let (base, t) = launch.trim().split_once("/#t=").unwrap();
        let r = http
            .post(format!("{base}/api/session"))
            .header("origin", base)
            .header("sec-fetch-site", "same-origin")
            .json(&json!({"launch": t}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let token = r.json::<Value>().await.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        Browser {
            base: base.to_string(),
            token,
            http,
        }
    }

    async fn pending(&self, id: &str) -> Value {
        let all: Value = self
            .http
            .get(format!("{}/api/approvals", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        all["pending"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is not waiting on the page: {all}"))
    }

    async fn allow(&self, id: &str) {
        let r = self
            .http
            .post(format!("{}/api/approvals/{id}", self.base))
            .bearer_auth(&self.token)
            .header("origin", &self.base)
            .header("sec-fetch-site", "same-origin")
            .json(&json!({"decision": "allow_once"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    /// A GET from the page, with its session token: (status, type, body).
    async fn get(&self, path: &str) -> (StatusCode, String, Vec<u8>) {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let code = r.status();
        let ct = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        (code, ct, r.bytes().await.unwrap().to_vec())
    }
}

fn read_code(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/row_job")
            .join(name),
    )
    .unwrap()
}

fn route_fields() -> Value {
    json!([
        {"name": "ROUTE_ID", "type": "text", "description": "the route's id"},
        {"name": "ROUTE_NAME", "type": "text", "description": "the route's name"}
    ])
}

/// propose_tool's arguments for find_gaps, as an assistant would write them.
fn find_gaps(routes: &str, events: &str) -> Value {
    json!({
        "name": "find_gaps",
        "title": "Find gaps in ROW width events",
        "description": "List the measure ranges along each route that have no ROW width event, as lines along the route with ROUTE_ID, ROUTE_NAME, FROM_M and TO_M. Use it on the weekly ROW check, before fix_widths fills the gaps from the owned-ROW polygons.",
        "not_for": "Routes without M values, or events measured in a unit other than the routes' measures.",
        "version": "1.0.0",
        "inputs": {
            "routes": {"kind": "layer", "description": "The route network: one line per route, with M values (measures).",
                       "geometry": "line", "crs": "any", "fields": route_fields()},
            "events": {"kind": "table", "description": "ROW width events: ROUTE_ID, FROM_M, TO_M and ROW_WIDTH_FT.",
                       "fields": [{"name": "ROUTE_ID", "type": "text"}, {"name": "FROM_M", "type": "number"},
                                  {"name": "TO_M", "type": "number"}]}
        },
        "outputs": {
            "gaps": {"kind": "layer", "description": "One line per gap, along its route, with its measure range.",
                     "geometry": "line", "crs": "same as routes",
                     "fields": [{"name": "ROUTE_ID", "type": "text"}, {"name": "ROUTE_NAME", "type": "text"},
                                {"name": "FROM_M", "type": "number"}, {"name": "TO_M", "type": "number"}]}
        },
        "examples": [{"description": "The gaps in the week's ROW width events",
                      "arguments": {"routes": "ws://<session>/results/routes.gpkg", "events": "ws://<session>/results/events.csv"}}],
        "code": read_code("find_gaps.py"),
        "test": {"inputs": {"routes": routes, "events": events}},
    })
}

/// propose_tool's arguments for fix_widths.
fn fix_widths(routes: &str, gaps: &str, row: &str) -> Value {
    json!({
        "name": "fix_widths",
        "title": "Fix ROW widths from the owned-ROW polygons",
        "description": "Fill each gap with ROW widths measured from the owned-ROW polygons: transects square to the route every interval feet, the overlap of each with the polygons in feet, and runs of widths within the tolerance grouped into one segment of their mean width.",
        "not_for": "Routes without M values, or ROW polygons that do not cover the routes (those stations get no width).",
        "version": "1.0.0",
        "inputs": {
            "routes": {"kind": "layer", "description": "The route network: one line per route, with M values (measures).",
                       "geometry": "line", "crs": "any", "fields": route_fields()},
            "gaps": {"kind": "layer", "description": "The gaps find_gaps lists: ROUTE_ID, ROUTE_NAME, FROM_M, TO_M.",
                     "geometry": "line", "crs": "any"},
            "row": {"kind": "layer", "description": "The owned right-of-way polygons, such as the ArcGIS Online layer.",
                    "geometry": "polygon", "crs": "any (reprojected)"}
        },
        "params": {
            "interval": {"type": "number", "description": "Distance between transects along the route", "unit": "foot", "default": 50, "min": 1, "max": 5280},
            "tolerance": {"type": "number", "description": "Widths within this of a run's mean join the run", "unit": "foot", "default": 2, "min": 0, "max": 100},
            "transect_length": {"type": "number", "description": "Length of each transect, across the route", "unit": "foot", "default": 300, "min": 10, "max": 5280}
        },
        "outputs": {
            "segments": {"kind": "layer", "description": "Segments of one ROW width, along each gap, with the width in feet.",
                         "geometry": "line", "crs": "same as routes",
                         "fields": [{"name": "ROUTE_ID", "type": "text"}, {"name": "ROUTE_NAME", "type": "text"},
                                    {"name": "FROM_M", "type": "number"}, {"name": "TO_M", "type": "number"},
                                    {"name": "ROW_WIDTH_FT", "type": "number"}, {"name": "TRANSECTS", "type": "integer"}]}
        },
        "checks": {"row.no_polygon": "Transects that crossed no ROW polygon: those stations have no width"},
        "examples": [{"description": "Fill the week's gaps from the ROW polygons, a transect every 50 ft",
                      "arguments": {"routes": "ws://<session>/results/routes.gpkg", "gaps": "ws://<session>/results/gaps.gpkg",
                                    "row": "ws://<session>/results/row.gpkg", "interval": 50}}],
        "code": read_code("fix_widths.py"),
        "test": {"inputs": {"routes": routes, "gaps": gaps, "row": row}},
    })
}

/// Propose a tool, approve its test run on the page, and install it as
/// Settings › Tools does. Returns the installed hash.
async fn propose_and_install(
    env: &Env,
    c: &RunningService<RoleClient, ()>,
    browser: &mut Option<Browser>,
    args: Value,
) -> String {
    let r = ok(&call(c, "propose_tool", args.clone()).await);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    let ap = r["result"]["approval"]["id"].as_str().unwrap().to_string();
    if browser.is_none() {
        *browser = Some(Browser::open(&env.launch_file()).await);
    }
    let b = browser.as_ref().unwrap();
    assert_eq!(b.pending(&ap).await["risk"], "script");
    b.allow(&ap).await;
    let mut approved = args;
    approved["approval_id"] = json!(ap);
    let r = ok(&call(c, "propose_tool", approved).await);
    assert_eq!(r["result"]["status"], "waiting_for_person", "{r}");
    let id = r["result"]["tool"]
        .as_str()
        .unwrap()
        .trim_end_matches(".run")
        .to_string();
    let hash = r["result"]["hash"].as_str().unwrap().to_string();
    columbus_shared::tools::install(&env.tools(), &id, &hash, "2026-11-09T12:00:00Z").unwrap();
    hash
}

fn workflow(env: &Env) -> String {
    format!(
        "id: row-weekly\nname: Weekly ROW widths\n\
         purpose: Find the gaps in the ROW width events every Friday, fill them from the owned-ROW polygons, and dissolve the widths by route for review.\n\
         schedule: {{ days: [fri], at: \"05:41\" }}\n\
         scope: {{ connections: [lrs, row-polygons], run_minutes: 20 }}\n\
         steps:\n\
         \x20 - id: routes\n    tool: query_features\n    args: {{ connection: lrs, dataset: {} }}\n\
         \x20 - id: events\n    tool: query_features\n    args: {{ connection: lrs, dataset: {} }}\n\
         \x20 - id: row\n    tool: query_features\n    args: {{ connection: row-polygons, dataset: ROW }}\n\
         \x20 - id: gaps\n    tool: {FIND_GAPS}\n    args: {{ routes: \"${{routes}}\", events: \"${{events}}\" }}\n\
         \x20 - id: widths\n    tool: {FIX_WIDTHS}\n    args: {{ routes: \"${{routes}}\", gaps: \"${{gaps}}\", row: \"${{row}}\" }}\n\
         \x20 - id: dissolved\n    tool: vector_op\n    args: {{ op: dissolve, inputs: {{ layer: \"${{widths}}\" }}, params: {{ by: ROUTE_NAME }} }}\n\
         \x20 - id: checked\n    tool: verify\n    args: {{ layer: \"${{widths}}\" }}\n\
         \x20 - id: checked_dissolved\n    tool: verify\n    args: {{ layer: \"${{dissolved}}\" }}\n",
        env.routes, env.events
    )
}

/// The rows of a layer file (a GeoPackage, or a file geodatabase's feature
/// class), sorted by route then measure: [route name, from, to, width].
const ROWS: &str = r#"
import json, sys
import pyogrio
kw = {"layer": sys.argv[2]} if len(sys.argv) > 2 else {}
df = pyogrio.read_dataframe(sys.argv[1], **kw)
cols = list(df.columns)
rows = sorted(([r["ROUTE_NAME"], float(r["FROM_M"]), float(r["TO_M"]), float(r["ROW_WIDTH_FT"])]
               for _, r in df.iterrows()), key=lambda r: (r[0], r[1]))
print(json.dumps({"rows": rows, "columns": cols, "geometry": sorted(set(df.geometry.geom_type))}))
"#;

/// The segments fix_widths makes for the fixtures: A's gap (400 to 600)
/// splits where the polygon widens from 80 to 86 ft (between the transects
/// at 450 and 500); C's (700 to 1,000) where it widens from 100 to 120 ft
/// (between 800 and 850).
fn expected_segments() -> Vec<(&'static str, f64, f64, f64)> {
    vec![
        ("A", 400.0, 475.0, 80.0),
        ("A", 475.0, 600.0, 86.0),
        ("C", 700.0, 825.0, 100.0),
        ("C", 825.0, 1000.0, 120.0),
    ]
}

fn assert_segments(rows: &Value, want: &[(&str, f64, f64, f64)]) {
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), want.len(), "{rows:?}");
    for (r, w) in rows.iter().zip(want) {
        assert_eq!(r[0], w.0, "{rows:?}");
        assert!((r[1].as_f64().unwrap() - w.1).abs() < 0.01, "{rows:?}");
        assert!((r[2].as_f64().unwrap() - w.2).abs() < 0.01, "{rows:?}");
        // Measured on a UTM projection of lon/lat polygons: within 0.5 ft.
        assert!(
            (r[3].as_f64().unwrap() - w.3).abs() <= 0.5,
            "width {} for {w:?}: {rows:?}",
            r[3]
        );
    }
}

async fn row_job(py: &str, lrs: Lrs) {
    let fake = Fake::start();
    let env = Env::new(py, fake.1, &lrs);

    // --- 1. The owner's tools, written from a chat.
    let c = env.start(&[]).await;
    let q = |conn: &str, ds: &str| json!({"connection": conn, "dataset": ds});
    let routes = ok(&call(&c, "query_features", q("lrs", env.routes)).await);
    assert_eq!(routes["result"]["rows_read"], 3, "{routes}");
    let events = ok(&call(&c, "query_features", q("lrs", env.events)).await);
    assert_eq!(events["result"]["rows_read"], 4, "{events}");
    let row = ok(&call(&c, "query_features", q("row-polygons", "ROW")).await);
    assert_eq!(row["result"]["rows_read"], 3, "{row}");
    let (routes, events, row) = (handle(&routes), handle(&events), handle(&row));
    let mut browser = None;
    propose_and_install(&env, &c, &mut browser, find_gaps(&routes, &events)).await;
    // Installed: the session uses it from its next call, with no approval.
    let g = ok(&call(
        &c,
        "run_tool",
        json!({"name": FIND_GAPS, "args": {"routes": routes, "events": events}}),
    )
    .await);
    assert_eq!(g["result"]["summary"]["gaps"], 2, "{g}");
    let gaps = handle(&g);
    propose_and_install(&env, &c, &mut browser, fix_widths(&routes, &gaps, &row)).await;

    // --- 2. The weekly workflow, proposed in the chat and approved in the app.
    let text = workflow(&env);
    let r = call(&c, "propose_workflow", json!({"workflow": text})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    c.cancel().await.unwrap();
    drop(browser);
    let p = wf::read_proposal(&env.workflows(), &env.tools(), "row-weekly").unwrap();
    wf::install(
        &env.workflows(),
        &env.tools(),
        "row-weekly",
        &p.fingerprint,
        "now",
    )
    .unwrap();

    // --- 3. Friday: the scheduler starts it. No person, no approval.
    let _ = std::fs::remove_file(env.launch_file());
    let (code, rec, report) = env.run("row-weekly");
    assert_eq!(code, 0, "{rec:#}");
    assert_eq!(rec["scheduled"], true);
    assert!(
        rec["status"].as_str().unwrap().starts_with("succeeded"),
        "{rec:#}"
    );
    assert!(
        !env.launch_file().exists(),
        "nothing in the run asked for a person"
    );
    let steps = rec["steps"].as_array().unwrap();
    let ids: Vec<&str> = steps.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "routes",
            "events",
            "row",
            "gaps",
            "widths",
            "dissolved",
            "checked",
            "checked_dissolved"
        ]
    );
    for s in steps {
        assert_eq!(s["status"], "done", "{s:#}");
        // Checks pass: no step has a failing check.
        let fails: Vec<&Value> = s["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["severity"] == "fail")
            .collect();
        assert!(fails.is_empty(), "{}: {fails:?}", s["id"]);
    }
    let workspace = PathBuf::from(rec["workspace"].as_str().unwrap());
    let summary = |id: &str| steps.iter().find(|s| s["id"] == id).unwrap().clone();
    assert!(summary("routes")["summary"]
        .as_str()
        .unwrap()
        .contains("3 rows"));
    // The run's layers, by step, and each one's file.
    let outputs = rec["outputs"].as_array().unwrap();
    let file_of = |step: &str| -> PathBuf {
        let o = outputs
            .iter()
            .find(|o| o["step"] == step)
            .unwrap_or_else(|| panic!("no output for {step}: {rec:#}"));
        workspace.join(o["file"].as_str().unwrap())
    };
    // M values came through from the route network.
    let m = env.python(
        "import json, sys, pyogrio, shapely\nmeta, t = pyogrio.read_arrow(sys.argv[1])\ng = shapely.from_wkb(t.column(meta.get('geometry_name') or 'wkb_geometry').to_pylist())\nprint(json.dumps(sorted(float(shapely.get_coordinates(x, include_m=True)[-1][-1]) for x in g)))",
        &[&file_of("routes")],
    );
    assert_eq!(m, json!([1000.0, 1000.0, 1000.0]), "{m}");
    // 2 gaps found.
    let gaps_rows = env.python(
        "import json, sys, pyogrio\ndf = pyogrio.read_dataframe(sys.argv[1])\nprint(json.dumps(sorted([r['ROUTE_NAME'], float(r['FROM_M']), float(r['TO_M'])] for _, r in df.iterrows())))",
        &[&file_of("gaps")],
    );
    assert_eq!(
        gaps_rows,
        json!([["A", 400.0, 600.0], ["C", 700.0, 1000.0]]),
        "{gaps_rows}"
    );
    // Fixed-width segments produced.
    let widths = env.python(ROWS, &[&file_of("widths")]);
    assert_eq!(widths["geometry"], json!(["LineString"]), "{widths}");
    assert_segments(&widths["rows"], &expected_segments());
    // The dissolved layer: one feature per route name with a gap.
    let dissolved = env.python(
        "import json, sys, pyogrio\ndf = pyogrio.read_dataframe(sys.argv[1])\nprint(json.dumps(sorted(df['ROUTE_NAME'].tolist())))",
        &[&file_of("dissolved")],
    );
    assert_eq!(dissolved, json!(["A", "C"]), "{dissolved}");
    let dsteps = summary("dissolved");
    assert!(
        dsteps["summary"].as_str().unwrap().contains("2 rows"),
        "{dsteps}"
    );
    assert!(
        report.contains("| gaps | personal.find-gaps.run |"),
        "{report}"
    );
    assert!(
        report.contains("| widths | personal.fix-widths.run |"),
        "{report}"
    );

    // --- 4. The imagery the analyst reviews against: a web layer, saved by
    // the person in the app (no map.hosts: saving is their approval).
    let caps = format!(
        "http://imagery.fake.test:{}/web/wmts/own/WMTSCapabilities.xml",
        fake.1
    );
    let (okk, v, all) = env.cli(&["map-source", "inspect", &caps]);
    assert!(okk, "{all}");
    assert_eq!(v["kind"], "wmts", "{v}");
    assert_eq!(
        v["confirm"], true,
        "the app asks at the keyboard before saving: {v}"
    );
    assert_eq!(v["layers"][0]["layer"], "ortho", "{v}");
    let (okk, v, all) = env.cli(&[
        "map-source",
        "save",
        &json!({"id": WEB_LAYER, "title": "County imagery (fake)", "kind": "wmts", "url": caps, "layer": "ortho"})
            .to_string(),
    ]);
    assert!(okk, "{all}");
    assert_eq!(v["id"], WEB_LAYER, "{v}");

    // --- 5. Open the run in a chat (`columbus-mcp workflow open`).
    let run = rec["run"].as_str().unwrap().to_string();
    let c = env
        .start(&["workflow", "open", "row-weekly", run.as_str()])
        .await;
    let who = ok(&call(&c, "whoami", json!({})).await);
    let opened = &who["result"]["opened_run"];
    assert_eq!(opened["workflow"], "row-weekly", "{who}");
    let layers = opened["layers"].as_array().unwrap();
    let layer = |step: &str| -> Value {
        layers
            .iter()
            .find(|l| l["step"] == step)
            .unwrap_or_else(|| panic!("{step} is not opened: {opened}"))
            .clone()
    };
    let opened_widths = layer("widths");
    assert_eq!(opened_widths["rows"], 4, "{opened_widths}");
    assert_eq!(layer("dissolved")["rows"], 2);
    assert_eq!(layer("gaps")["rows"], 2);
    // The web layer is listed for the chat by name only (REQ-34).
    assert!(
        who["result"].to_string().contains("County imagery (fake)"),
        "{who}"
    );
    assert!(!who["result"].to_string().contains("imagery.fake.test"));

    // "Remove route C from the output."
    let f = ok(&call(
        &c,
        "vector_op",
        json!({"op": "filter", "inputs": {"layer": opened_widths["handle"]}, "params": {"where": "ROUTE_NAME <> 'C'"}}),
    )
    .await);
    assert_eq!(f["result"]["layer"]["summary"]["rows"], 2, "{f}");
    let approved = handle(&f);

    // Export the approved layer to a folder that isn't granted: the job's
    // one approval, on the chat's page.
    let x =
        json!({"handle": approved, "folder": "review", "name": "approved_row", "format": "gdb"});
    let r = ok(&call(&c, "export_output", x.clone()).await);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    assert!(r["result"]["approval"]["summary"]
        .as_str()
        .unwrap()
        .contains("not a granted export folder"));
    let gdb = env.root.join("review").join("approved_row.gdb");
    assert!(!gdb.exists());
    let ap = r["result"]["approval"]["id"].as_str().unwrap().to_string();
    let page = Browser::open(&env.launch_file()).await;
    page.allow(&ap).await;
    let mut again = x;
    again["approval_id"] = json!(ap);
    let r = ok(&call(&c, "export_output", again).await);
    assert_eq!(r["result"]["status"], "exported", "{r}");
    assert!(gdb.is_dir(), "{r}");
    let out = env.python(ROWS, &[&gdb, Path::new("approved_row")]);
    assert_segments(&out["rows"], &expected_segments()[..2]);
    // A file geodatabase keeps lines as multi-part polylines.
    assert!(
        out["geometry"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g.as_str().unwrap().ends_with("LineString")),
        "{out}"
    );

    // The web layer turned on in this chat: the map page lists it and its
    // tiles load through the gateway with this chat's page token.
    let (code, _, body) = page.get("/api/web/sources").await;
    assert_eq!(code, StatusCode::OK);
    let sources: Value = serde_json::from_slice(&body).unwrap();
    assert!(sources.to_string().contains(WEB_LAYER), "{sources}");
    let before = fake.log().await.len();
    let (code, ct, body) = page
        .get(&format!("/api/web/tile/{WEB_LAYER}/15/7486/13494"))
        .await;
    assert_eq!(code, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(ct.starts_with("image/png"), "{ct}");
    assert!(body.starts_with(&[137, 80, 78, 71]), "a PNG tile");
    let log = fake.log().await;
    assert!(
        log[before..].iter().any(|e| e["host"]
            .as_str()
            .is_some_and(|h| h.starts_with("imagery.fake.test"))
            && e["target"]
                .as_str()
                .is_some_and(|t| t.starts_with("/web/wmts/tile/"))),
        "the tile came from the imagery service: {log:?}"
    );
    // Any other caller is refused.
    let r = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!(
            "{}/api/web/tile/{WEB_LAYER}/15/7486/13494",
            page.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    c.cancel().await.unwrap();
}

/// The job on Oracle (the Oracle CI job): `ROADS.ROW_ROUTES` (SDO_GTYPE
/// 3302) and `ROADS.ROW_EVENTS`, read through a read-only login.
#[tokio::test]
async fn row_job_end_to_end_on_oracle() {
    let Ok(host) = std::env::var("COLUMBUS_TEST_ORA_HOST") else {
        eprintln!("skipping: COLUMBUS_TEST_ORA_HOST not set");
        return;
    };
    let port = std::env::var("COLUMBUS_TEST_ORA_PORT").unwrap_or_else(|_| "1521".into());
    let service = std::env::var("COLUMBUS_TEST_ORA_SERVICE").unwrap_or_else(|_| "FREEPDB1".into());
    let py = support::python_for(&format!("import oracledb; {}", engine())).expect(
        "python with oracledb and the GIS engine is required when COLUMBUS_TEST_ORA_HOST is set",
    );
    assert!(
        can_confine(),
        "personal tools need confinement at level A on the Oracle job's runner"
    );
    row_job(
        &py,
        Lrs::Oracle {
            host,
            port,
            service,
        },
    )
    .await;
}

/// The same job with the routes and events in a GeoPackage, wherever the
/// GIS engine is installed.
#[tokio::test]
async fn row_job_end_to_end_on_a_geopackage() {
    let Some(py) = support::python_for(engine()) else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    if !can_confine() {
        eprintln!("skipping: this machine cannot confine personal tools at level A");
        return;
    }
    row_job(&py, Lrs::Gpkg).await;
}
