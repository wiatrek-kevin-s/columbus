//! The assistant reads and steers the person's map (T-30; D-18, REQ-40),
//! end to end against the real gateway, with a stand-in for the map page:
//! a client that holds a page session and says what "its map" shows, as
//! the companion page does (`POST /api/view`).
//!
//! * With no map open, `map_view` says so, a change changes nothing, and
//!   `map_snapshot` of the view is refused in plain words.
//! * The view the page reports is the view the assistant reads, again
//!   after the person "pans and picks a feature"; the selected feature's
//!   attributes are read by the gateway from the session's own layer.
//! * A change the assistant asks for reaches the page with its next report;
//!   when the page says it has applied it, the tool returns the new view,
//!   within a few seconds.
//! * `map_snapshot` with `extent: "view"` draws the layers that are on at
//!   exactly the view's extent.
//! * Web map layers stay out of reach (D-14, REQ-34): a report that names
//!   one, its address or its state is refused whole, and nothing of a web
//!   layer the person has saved appears in any answer.
//! * `deny_tools: [map_view]` turns all of it off; every call is audited,
//!   reports are not.
//! * The selected feature's attributes follow the rules for rows: left out
//!   when the policy denies `read_output` or returns no rows.
//!
//! Needs the GIS engine; skipped without it.

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
    support::python_for("import geopandas, pyogrio, shapely, pyproj, rasterio")
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

/// What a web layer the person saved would say, were any of it to leak.
const WEB_LAYER: [&str; 5] = [
    "County imagery 2025",
    "county-imagery",
    "imagery.example.gov",
    "ortho2025",
    "wmts",
];

fn setup(py: &str, policy_extra: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    // Three squares side by side near Austin, stored in a Texas CRS in
    // metres, so nothing here is in degrees until the map shows it.
    let script = r#"
import sys, os
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"NAME": ["A", "B", "C"], "ACRES": [10.5, 20.0, 30.25]},
    geometry=[box(-97.80 + i * 0.03, 30.25, -97.78 + i * 0.03, 30.27) for i in range(3)],
    crs="EPSG:4326").to_crs("EPSG:3081").to_file(os.path.join(sys.argv[1], "t.gpkg"),
    layer="tracts", driver="GPKG", engine="pyogrio")
"#;
    assert!(StdCommand::new(py)
        .arg("-c")
        .arg(script)
        .arg(&root)
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::create_dir_all(root.join("map-sources")).unwrap();
    std::fs::write(
        root.join("policy.yaml"),
        format!("connection_types: [gpkg]\napprovals: {{ page: true, wait_seconds: 0 }}\n{policy_extra}"),
    )
    .unwrap();
    let gpkg = serde_json::to_string(&root.join("t.gpkg").display().to_string()).unwrap();
    std::fs::write(
        root.join("connections").join("tiger.yaml"),
        format!("id: tiger\ntype: gpkg\npath: {gpkg}\ndatasets:\n  allow: [\"*\"]\n"),
    )
    .unwrap();
    // A web map layer the person saved (F-13). The map page would offer
    // it; the assistant is told its title by whoami, and nothing more.
    std::fs::write(
        root.join("map-sources").join("county-imagery.yaml"),
        "id: county-imagery\ntitle: County imagery 2025\nkind: wmts\n\
         url: \"https://imagery.example.gov/wmts/1.0.0/WMTSCapabilities.xml\"\nlayer: ortho2025\n",
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
                .env("COLUMBUS_MAP_SOURCES", root.join("map-sources"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_TOOLS", root.join("tools"))
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

async fn call_raw(
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

/// A call that must succeed: its structured answer.
async fn call(c: &RunningService<RoleClient, ()>, tool: &'static str, args: Value) -> Value {
    let r = call_raw(c, tool, args).await;
    assert_ne!(r.is_error, Some(true), "unexpected error: {:?}", r.content);
    r.structured_content.clone().expect("structured output")
}

/// A call that must be refused: what it says.
async fn refused(c: &RunningService<RoleClient, ()>, tool: &'static str, args: Value) -> String {
    let r = call_raw(c, tool, args.clone()).await;
    assert_eq!(r.is_error, Some(true), "{tool} {args} was not refused");
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
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

/// The stand-in for the map page: a page session on the gateway's local
/// server, which reports "its map" as the companion page does.
struct Page {
    http: reqwest::Client,
    base: String,
    tok: String,
}

impl Page {
    /// Open a window on the session, as the Columbus app does, and trade
    /// its link for a page session.
    async fn open(env: &Env, session: &str) -> Page {
        let live: Value = serde_json::from_str(&wait_for(
            &env.root.join("live").join(format!("{session}.json")),
        ))
        .unwrap();
        let base = format!("http://127.0.0.1:{}", live["port"].as_u64().unwrap());
        let http = reqwest::Client::new();
        let launch_file = env.root.join("launch.txt");
        let _ = std::fs::remove_file(&launch_file);
        let r = http
            .post(format!("{base}/api/open"))
            .header("x-columbus-open", "1")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let link = wait_for(&launch_file);
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
        Page { http, base, tok }
    }

    /// Say what the map shows; the status and the server's answer.
    async fn say(&self, report: &Value) -> (StatusCode, Value) {
        let r = self
            .http
            .post(format!("{}/api/view", self.base))
            .bearer_auth(&self.tok)
            .header("origin", &self.base)
            .json(report)
            .send()
            .await
            .unwrap();
        let code = r.status();
        (code, r.json::<Value>().await.unwrap_or(Value::Null))
    }

    /// A report the server takes; the change it hands over, if any.
    async fn report(&self, report: &Value) -> Value {
        let (code, v) = self.say(report).await;
        assert_eq!(code, StatusCode::OK, "{report}: {v}");
        v["command"].clone()
    }

    /// Keep reporting, as the page does every second, until the server
    /// hands over a change.
    async fn until_command(&self, report: &Value) -> Value {
        let t = Instant::now();
        loop {
            let c = self.report(report).await;
            if !c.is_null() {
                return c;
            }
            assert!(
                t.elapsed() < Duration::from_secs(7),
                "no change reached the page"
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
}

/// A report of a map on show.
#[allow(clippy::too_many_arguments)]
fn showing(
    extent: [f64; 4],
    zoom: f64,
    layers: &[&str],
    active: Option<&str>,
    selected: Option<(&str, u64)>,
    table: bool,
    streets: bool,
    applied: u64,
) -> Value {
    json!({
        "showing": true,
        "idle_ms": 200,
        "applied": applied,
        "not_done": [],
        "view": {
            "extent": extent,
            "zoom": zoom,
            "layers": layers,
            "active": active,
            "selected": selected.map(|(l, f)| json!({"layer": l, "feature": f})),
            "table": table,
            "streets": streets,
        },
    })
}

fn mercator(lon: f64, lat: f64) -> (f64, f64) {
    let r = 6_378_137.0_f64;
    (
        r * lon.to_radians(),
        r * (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0)
            .tan()
            .ln(),
    )
}

fn audit_lines(env: &Env, tool: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(env.root.join("audit")).unwrap().flatten() {
        if e.path().extension().is_some_and(|x| x == "jsonl") {
            for line in std::fs::read_to_string(e.path()).unwrap().lines() {
                let v: Value = serde_json::from_str(line).unwrap();
                if v["tool"] == tool {
                    out.push(v);
                }
            }
        }
    }
    out
}

fn assert_no_web_layer(what: &str, text: &str) {
    for bad in WEB_LAYER {
        assert!(!text.contains(bad), "{what} mentions {bad}: {text}");
    }
}

#[tokio::test]
async fn the_assistant_reads_and_steers_the_persons_map() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(&py, "");
    let client = start(&env, &py).await;
    let w = call(&client, "whoami", json!({})).await;
    let session = w["result"]["session"].as_str().unwrap().to_string();
    // The person has saved a web map layer: whoami names it, by title only.
    assert_eq!(w["result"]["web_layers"], json!(["County imagery 2025"]));

    // The tool is there to be found, with what it is for.
    let found = call(
        &client,
        "search_tools",
        json!({"query": "what is the person looking at on their map"}),
    )
    .await;
    let names: Vec<&str> = found["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    assert!(names.contains(&"map_view"), "{names:?}");
    let card = found["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "map_view")
        .unwrap();
    assert_eq!(card["approval"], "no", "it needs no approval: {card}");
    let d = call(&client, "describe_tool", json!({"name": "map_view"})).await;
    let props = &d["result"]["input_schema"]["properties"];
    for arg in ["show", "hide", "active", "zoom_to"] {
        assert!(props[arg].is_object(), "{arg}: {d}");
    }
    assert_no_web_layer("describe_tool", &d.to_string());

    // Two layers of this session's.
    let q = call(
        &client,
        "query_features",
        json!({"connection": "tiger", "dataset": "tracts"}),
    )
    .await;
    let all = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let q = call(
        &client,
        "query_features",
        json!({"connection": "tiger", "dataset": "tracts", "where": "NAME = 'C'"}),
    )
    .await;
    let one = q["result"]["layer"]["handle"].as_str().unwrap().to_string();

    // ---- No map is open: it says so, and changes nothing.
    let v = call(&client, "map_view", json!({})).await;
    assert_eq!(v["result"]["status"], "not_open", "{v}");
    assert!(
        v["result"]["message"]
            .as_str()
            .unwrap()
            .contains("No map is open"),
        "{v}"
    );
    assert!(v["result"].get("view").is_none(), "{v}");
    let t = Instant::now();
    let v = call(&client, "map_view", json!({"show": [all], "zoom_to": all})).await;
    assert_eq!(v["result"]["status"], "not_open", "{v}");
    assert_eq!(v["result"]["changed"], false, "{v}");
    assert!(
        v["result"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Nothing was changed."),
        "{v}"
    );
    assert!(
        t.elapsed() < Duration::from_secs(12),
        "it does not wait long for a map that is not there: {:?}",
        t.elapsed()
    );
    let e = refused(&client, "map_snapshot", json!({"extent": "view"})).await;
    assert!(e.contains("map is not open"), "{e}");

    // What it will not take, map or no map: a name that is not one of the
    // session's layers (a web layer's id, an address, another session's
    // file), a table to draw, a layer both on and off, an extent that is
    // not one.
    for (args, why) in [
        (
            json!({"show": ["county-imagery"]}),
            "not a workspace handle",
        ),
        (
            json!({"show": ["https://imagery.example.gov/wmts/1.0.0/WMTSCapabilities.xml"]}),
            "not a workspace handle",
        ),
        (
            json!({"active": "ws://another-session/results/x.gpkg"}),
            "another session",
        ),
        (
            json!({"zoom_to": format!("ws://{session}/results/nothing.gpkg")}),
            "does not exist",
        ),
        (json!({"show": [all], "hide": [all]}), "both show and hide"),
        (json!({"hide": [one], "zoom_to": one}), "hide turns off"),
        (json!({"zoom_to": [-97.0, 30.0, -98.0, 31.0]}), "extent"),
        (json!({"zoom_to": [-98.0, 30.0, -97.0]}), "extent"),
        (json!({"zoom_to": {"west": -98}}), "zoom_to is"),
    ] {
        let e = refused(&client, "map_view", args.clone()).await;
        assert!(e.contains(why), "{args}: {e}");
    }
    // An argument it does not have is an error, not a read.
    let mut p = CallToolRequestParams::new("map_view");
    p = p.with_arguments(json!({"zoom": 4}).as_object().unwrap().clone());
    assert!(
        client
            .call_tool(p)
            .await
            .map_or(true, |r| r.is_error == Some(true)),
        "an unknown argument is refused"
    );

    // ---- The map opens: the page says what it shows, and that is what
    // the assistant reads.
    let page = Page::open(&env, &session).await;
    let first = showing(
        [-97.85, 30.2, -97.65, 30.32],
        11.5,
        &[&all],
        Some(&all),
        None,
        false,
        true,
        0,
    );
    assert_eq!(page.report(&first).await, Value::Null);
    let v = call(&client, "map_view", json!({})).await;
    let r = &v["result"];
    assert_eq!(r["status"], "open", "{v}");
    assert_eq!(r["view"]["crs"], "EPSG:4326");
    assert_eq!(r["view"]["extent"], json!([-97.85, 30.2, -97.65, 30.32]));
    assert_eq!(r["view"]["zoom"], 11.5);
    assert_eq!(r["view"]["layers_on"], json!([all]));
    assert_eq!(r["view"]["active"], all.as_str());
    assert_eq!(r["view"]["selected"], Value::Null);
    assert_eq!(r["view"]["table_open"], false);
    assert_eq!(r["view"]["street_background"], true);
    assert!(r["seen_seconds_ago"].as_f64().unwrap() < 5.0, "{v}");

    // The person pans, turns the second layer on, opens the table and
    // picks the second feature of the first layer.
    let moved = showing(
        [-97.81, 30.24, -97.7, 30.29],
        13.25,
        &[&one, &all],
        Some(&all),
        Some((&all, 1)),
        true,
        false,
        0,
    );
    page.report(&moved).await;
    let v = call(&client, "map_view", json!({})).await;
    let r = &v["result"];
    assert_eq!(r["view"]["extent"], json!([-97.81, 30.24, -97.7, 30.29]));
    assert_eq!(r["view"]["zoom"], 13.25);
    assert_eq!(r["view"]["layers_on"], json!([one, all]));
    assert_eq!(r["view"]["table_open"], true);
    assert_eq!(r["view"]["street_background"], false);
    // The selected feature: its layer and row from the page, its
    // attributes read here from the session's own layer (row 1 is "B").
    let sel = &r["view"]["selected"];
    assert_eq!(sel["layer"], all.as_str(), "{v}");
    assert_eq!(sel["feature"], 1);
    assert_eq!(sel["attributes"]["NAME"], "B", "{v}");
    assert_eq!(sel["attributes"]["ACRES"], 20.0, "{v}");
    assert_eq!(sel["geometry"]["type"], "Polygon", "{v}");
    assert!(
        sel["attributes"].get("geometry").is_none() && sel["attributes"].get("geom").is_none(),
        "{v}"
    );

    // ---- A change: the assistant asks, the page receives it with its
    // next report, applies it and says so; the tool returns the new view.
    let t = Instant::now();
    let asked = json!({"show": [one], "hide": [all], "active": one, "zoom_to": one});
    let (v, command) = tokio::join!(call(&client, "map_view", asked), async {
        let command = page.until_command(&moved).await;
        // Told again until it is done, by the same number.
        assert_eq!(page.report(&moved).await["seq"], command["seq"]);
        // Applied: the first layer off, the map on the third square.
        let after = showing(
            [-97.745, 30.245, -97.715, 30.275],
            15.0,
            &[&one],
            Some(&one),
            Some((&all, 1)),
            true,
            false,
            command["seq"].as_u64().unwrap(),
        );
        assert_eq!(page.report(&after).await, Value::Null);
        command
    });
    assert_eq!(
        command,
        json!({"seq": command["seq"], "show": [one], "hide": [all], "active": one, "zoom_to": one}),
        "the page receives exactly what was asked"
    );
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "the answer comes when the page has followed: {:?}",
        t.elapsed()
    );
    let r = &v["result"];
    assert_eq!(r["status"], "changed", "{v}");
    assert_eq!(r["changed"], true);
    assert_eq!(r["asked"]["zoom_to"], one.as_str());
    assert_eq!(
        r["view"]["extent"],
        json!([-97.745, 30.245, -97.715, 30.275])
    );
    assert_eq!(r["view"]["layers_on"], json!([one]));
    assert_eq!(r["view"]["active"], one.as_str());
    assert!(r.get("not_done").is_none(), "{v}");
    let now = showing(
        [-97.745, 30.245, -97.715, 30.275],
        15.0,
        &[&one],
        Some(&one),
        Some((&all, 1)),
        true,
        false,
        command["seq"].as_u64().unwrap(),
    );
    assert_eq!(
        page.report(&now).await,
        Value::Null,
        "nothing waits any more"
    );

    // A zoom to an extent, which this page says it could not do.
    let extent = json!([-97.9, 30.1, -97.6, 30.4]);
    let (v, command) = tokio::join!(
        call(&client, "map_view", json!({"zoom_to": extent})),
        async {
            let command = page.until_command(&now).await;
            let mut after = now.clone();
            after["applied"] = command["seq"].clone();
            after["not_done"] = json!(["zoom_to"]);
            page.report(&after).await;
            command
        }
    );
    assert_eq!(command["zoom_to"], extent);
    assert_eq!(command["show"], json!([]));
    assert_eq!(v["result"]["status"], "partly_changed", "{v}");
    assert_eq!(v["result"]["not_done"][0]["part"], "zoom_to", "{v}");
    assert!(
        v["result"]["not_done"][0]["why"]
            .as_str()
            .unwrap()
            .contains("nothing to zoom to"),
        "{v}"
    );
    let mut now = now;
    now["applied"] = command["seq"].clone();

    // ---- Web map layers stay out of reach (D-14, REQ-34). A report that
    // carries anything of one is refused whole, and what the server kept
    // is unchanged.
    for junk in [
        json!({"web_layers": [{"id": "county-imagery", "title": "County imagery 2025", "on": true, "opacity": 0.4,
            "url": "https://imagery.example.gov/wmts/1.0.0/WMTSCapabilities.xml", "layer": "ortho2025"}]}),
        json!({"view": {"web": ["county-imagery"]}}),
        json!({"view": {"basemap": "web:county-imagery"}}),
        json!({"view": {"layers": ["county-imagery"]}}),
        json!({"view": {"layers": ["https://imagery.example.gov/wmts/1.0.0/WMTSCapabilities.xml"]}}),
        json!({"view": {"active": "County imagery 2025"}}),
        json!({"view": {"selected": {"layer": "county-imagery", "feature": 3}}}),
        json!({"view": {"selected": {"layer": one, "feature": 0, "attributes": {"SOURCE": "ortho2025"}}}}),
        json!({"not_done": ["wmts"]}),
    ] {
        let mut bad = now.clone();
        for (k, val) in junk.as_object().unwrap() {
            match (bad.get_mut(k), val) {
                (Some(Value::Object(have)), Value::Object(more)) => {
                    for (k2, v2) in more {
                        have.insert(k2.clone(), v2.clone());
                    }
                }
                _ => bad[k] = val.clone(),
            }
        }
        let (code, said) = page.say(&bad).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{bad}: {said}");
        assert_no_web_layer("the refusal", &said.to_string());
    }
    let v = call(&client, "map_view", json!({})).await;
    assert_eq!(v["result"]["status"], "open", "{v}");
    assert_eq!(v["result"]["view"]["layers_on"], json!([one]), "{v}");
    assert_no_web_layer("map_view", &v.to_string());
    // Only whoami mentions it, by title (as before this tool).
    let w = call(&client, "whoami", json!({})).await;
    assert_eq!(w["result"]["web_layers"], json!(["County imagery 2025"]));

    // ---- A picture of the same view: the layers that are on, at exactly
    // the view's extent.
    page.report(&moved).await;
    let snap = call_raw(&client, "map_snapshot", json!({"extent": "view"})).await;
    assert_ne!(snap.is_error, Some(true), "{:?}", snap.content);
    let img = snap
        .content
        .iter()
        .find_map(|c| c.as_image())
        .expect("a picture for the assistant");
    assert_eq!(img.mime_type, "image/png");
    let s = snap.structured_content.clone().unwrap();
    let r = &s["result"];
    assert_eq!(
        r["view"]["extent"],
        json!([-97.81, 30.24, -97.7, 30.29]),
        "{s}"
    );
    assert_eq!(r["view"]["crs"], "EPSG:4326");
    assert_eq!(r["view"]["zoom"], 13.25);
    assert_eq!(r["crs"], "EPSG:3857", "drawn as the map draws it: {s}");
    let (minx, miny) = mercator(-97.81, 30.24);
    let (maxx, maxy) = mercator(-97.7, 30.29);
    for (got, want) in r["extent"]
        .as_array()
        .unwrap()
        .iter()
        .zip([minx, miny, maxx, maxy])
    {
        assert!(
            (got.as_f64().unwrap() - want).abs() < 0.01,
            "the snapshot's extent is the view's: {s}"
        );
    }
    // The picture has the view's shape.
    let shape = (maxx - minx) / (maxy - miny);
    let drawn = r["width"].as_f64().unwrap() / r["height"].as_f64().unwrap();
    assert!((shape - drawn).abs() < 0.01, "{s}");
    // The two layers that are on, the lower one first; all three squares
    // of the first are in the view, and the one of the second.
    let layers = r["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 2, "{s}");
    assert_eq!(layers[0]["handle"], all.as_str());
    assert_eq!(layers[1]["handle"], one.as_str());
    assert_eq!(layers[0]["features"], 3);
    assert_eq!(layers[0]["in_view"], 3, "{s}");
    assert_eq!(layers[1]["in_view"], 1, "{s}");
    assert_eq!(r["view"]["layers_on_map"], 2);
    assert_eq!(r["view"]["layers_left_out"], 0);
    assert_no_web_layer("map_snapshot", &s.to_string());
    assert!(s.to_string().contains("No web basemap"), "{s}");
    // Named layers at the person's extent; a view zoomed in on the third
    // square has one of the first layer's three in it.
    let zoomed = showing(
        [-97.745, 30.245, -97.715, 30.275],
        15.0,
        &[&one],
        Some(&one),
        None,
        false,
        false,
        now["applied"].as_u64().unwrap(),
    );
    page.report(&zoomed).await;
    let s = call(
        &client,
        "map_snapshot",
        json!({"extent": "view", "layers": [all]}),
    )
    .await;
    assert_eq!(s["result"]["layers"].as_array().unwrap().len(), 1, "{s}");
    assert_eq!(s["result"]["layers"][0]["handle"], all.as_str());
    assert_eq!(s["result"]["layers"][0]["in_view"], 1, "{s}");
    assert_eq!(
        s["result"]["view"]["extent"],
        json!([-97.745, 30.245, -97.715, 30.275])
    );
    // Not with a bbox as well, and no other word than "view".
    let e = refused(
        &client,
        "map_snapshot",
        json!({"extent": "view", "bbox": [0, 0, 1, 1]}),
    )
    .await;
    assert!(e.contains("not both"), "{e}");
    let e = refused(
        &client,
        "map_snapshot",
        json!({"extent": "everything", "layers": [all]}),
    )
    .await;
    assert!(e.contains("can only be \"view\""), "{e}");
    // The usual snapshot is as it was.
    let s = call(&client, "map_snapshot", json!({"layers": [all]})).await;
    assert_eq!(s["result"]["crs"], "EPSG:3081", "{s}");
    assert!(s["result"].get("view").is_none(), "{s}");
    // A workspace file that is not a layer or table in the session's
    // results (the picture just made) is nothing the map can show.
    let picture = s["result"]["handle"].as_str().unwrap().to_string();
    let e = refused(&client, "map_view", json!({"show": [picture]})).await;
    assert!(e.contains("not a layer or table the map lists"), "{e}");

    // ---- The page is no longer on show (its panel closed, its window in
    // the background): no map is open, at once.
    let (code, _) = page
        .say(&json!({"showing": false, "idle_ms": 0, "applied": 0}))
        .await;
    assert_eq!(code, StatusCode::OK);
    let v = call(&client, "map_view", json!({})).await;
    assert_eq!(v["result"]["status"], "not_open", "{v}");
    let e = refused(&client, "map_snapshot", json!({"extent": "view"})).await;
    assert!(e.contains("map is not open"), "{e}");
    // On show again; then the page stops reporting (its window was
    // closed): after a few seconds no map is open.
    page.report(&zoomed).await;
    assert_eq!(
        call(&client, "map_view", json!({})).await["result"]["status"],
        "open"
    );
    tokio::time::sleep(Duration::from_millis(5500)).await;
    let v = call(&client, "map_view", json!({})).await;
    assert_eq!(v["result"]["status"], "not_open", "{v}");

    // ---- Only the page may report: its session token, its own origin,
    // this host; and not more than a report's worth.
    let url = format!("{}/api/view", page.base);
    let r = page.http.post(&url).json(&zoomed).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = page
        .http
        .post(&url)
        .bearer_auth("not-the-token")
        .header("origin", &page.base)
        .json(&zoomed)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = page
        .http
        .post(&url)
        .bearer_auth(&page.tok)
        .json(&zoomed)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "no Origin");
    let r = page
        .http
        .post(&url)
        .bearer_auth(&page.tok)
        .header("origin", "https://evil.example")
        .json(&zoomed)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "another site's Origin");
    let r = page
        .http
        .post(&url)
        .bearer_auth(&page.tok)
        .header("origin", &page.base)
        .header("host", "localhost")
        .json(&zoomed)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "wrong Host");
    let mut huge = zoomed.clone();
    huge["view"]["layers"] = json!(vec![
        format!("ws://{session}/results/{}", "x".repeat(150));
        700
    ]);
    let (code, _) = page.say(&huge).await;
    assert_eq!(code, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        call(&client, "map_view", json!({})).await["result"]["status"],
        "not_open",
        "none of those was taken"
    );

    // ---- Every call is in the audit log; the page's reports are not.
    let lines = audit_lines(&env, "map_view");
    let allowed = lines.iter().filter(|l| l["decision"] == "allowed").count();
    let denied = lines.iter().filter(|l| l["decision"] == "denied").count();
    assert_eq!(denied, 10, "each refusal is audited");
    assert!(allowed >= 10, "each call is audited: {allowed}");
    assert!(
        lines
            .iter()
            .any(|l| l["reason"] == "changed the person's map view"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l["reason"] == "no map is open; nothing was changed"),
        "{lines:?}"
    );
    // The read that returned the selected feature's attributes is recorded
    // as a read of one row of that layer, as read_output records one.
    assert!(
        lines.iter().any(|l| l["rows"] == 1
            && l["prov"].is_string()
            && l["labels"] == json!(["connection:tiger"])),
        "{lines:?}"
    );
    let all_lines = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect::<String>();
    assert!(
        !all_lines.contains("\"tool\":\"view") && !all_lines.contains("api/view"),
        "reports are not audited one by one"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn an_organization_can_turn_the_map_tool_off() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    let env = setup(&py, "deny_tools: [map_view]\n");
    let client = start(&env, &py).await;
    let w = call(&client, "whoami", json!({})).await;
    let session = w["result"]["session"].as_str().unwrap().to_string();
    let q = call(
        &client,
        "query_features",
        json!({"connection": "tiger", "dataset": "tracts"}),
    )
    .await;
    let all = q["result"]["layer"]["handle"].as_str().unwrap().to_string();

    // The page still reports (it does not know the policy); nothing is
    // kept, and nothing is handed to it.
    let page = Page::open(&env, &session).await;
    let view = showing(
        [-97.85, 30.2, -97.65, 30.32],
        11.5,
        &[&all],
        Some(&all),
        None,
        false,
        false,
        0,
    );
    assert_eq!(page.report(&view).await, Value::Null);

    let e = refused(&client, "map_view", json!({})).await;
    assert!(e.contains("map_view"), "{e}");
    let e = refused(&client, "map_view", json!({"zoom_to": all})).await;
    assert!(e.contains("map_view"), "{e}");
    assert_eq!(page.report(&view).await, Value::Null);
    // The view cannot be had through a snapshot either.
    let e = refused(&client, "map_snapshot", json!({"extent": "view"})).await;
    assert!(e.contains("turns map_view off"), "{e}");
    // The usual snapshot still works: it says nothing of the person's map.
    let s = call(&client, "map_snapshot", json!({"layers": [all]})).await;
    assert!(s["result"].get("view").is_none(), "{s}");
    // And the tool is not offered.
    let found = call(&client, "search_tools", json!({"query": "map view zoom"})).await;
    assert!(
        !found["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "map_view"),
        "{found}"
    );
    let e = refused(&client, "describe_tool", json!({"name": "map_view"})).await;
    assert!(e.contains("no tool named"), "{e}");
    assert!(audit_lines(&env, "map_view")
        .iter()
        .all(|l| l["decision"] == "denied"));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn the_selected_features_values_follow_the_rules_for_rows() {
    let Some(py) = python() else {
        eprintln!("skipping: the GIS engine is not installed");
        return;
    };
    // Where the assistant may not read rows of a result, or is returned
    // none, the selection is named (its layer and row) without its values.
    for policy in ["deny_tools: [read_output]\n", "return_rows: 0\n"] {
        let env = setup(&py, policy);
        let client = start(&env, &py).await;
        let w = call(&client, "whoami", json!({})).await;
        let session = w["result"]["session"].as_str().unwrap().to_string();
        let q = call(
            &client,
            "query_features",
            json!({"connection": "tiger", "dataset": "tracts"}),
        )
        .await;
        let all = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
        let page = Page::open(&env, &session).await;
        page.report(&showing(
            [-97.85, 30.2, -97.65, 30.32],
            11.5,
            &[&all],
            Some(&all),
            Some((&all, 1)),
            true,
            false,
            0,
        ))
        .await;
        let v = call(&client, "map_view", json!({})).await;
        assert_eq!(v["result"]["status"], "open", "{policy}: {v}");
        assert_eq!(
            v["result"]["view"]["selected"],
            json!({"layer": all, "feature": 1}),
            "{policy}: {v}"
        );
        assert!(
            !v.to_string().contains("ACRES") && !v.to_string().contains("20.0"),
            "{policy}: {v}"
        );
        assert!(
            audit_lines(&env, "map_view")
                .iter()
                .all(|l| l.get("rows").is_none()),
            "{policy}: no row was read"
        );
        client.cancel().await.unwrap();
    }
}
