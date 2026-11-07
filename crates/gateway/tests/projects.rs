//! Projects: a session started for a project (`COLUMBUS_PROJECT`) uses only
//! the connections the project lists, reports the project and its
//! instructions in whoami, and keeps its project databases apart from
//! other projects'. A project that cannot be read stops the session from
//! starting, rather than starting it with everything.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn python(modules: &str) -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", &format!("import {modules}")])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn setup(py: Option<&str>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in ["connections", "projects"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let gpkg = root.join("county.gpkg");
    if let Some(py) = py {
        let script = r#"
import sys
import geopandas as gpd
from shapely.geometry import box
gpd.GeoDataFrame({"parcel_id": ["P1", "P2"]}, geometry=[box(0, 0, 1, 1), box(1, 1, 2, 2)],
    crs="EPSG:2277").to_file(sys.argv[1], layer="parcels", driver="GPKG", engine="pyogrio")
gpd.GeoDataFrame({"road": ["R1"]}, geometry=[box(0, 0, 3, 3)],
    crs="EPSG:2277").to_file(sys.argv[1], layer="roads", driver="GPKG", engine="pyogrio")
"#;
        assert!(StdCommand::new(py)
            .args(["-c", script])
            .arg(&gpkg)
            .status()
            .unwrap()
            .success());
    } else {
        std::fs::write(&gpkg, b"").unwrap();
    }
    let p = serde_json::to_string(&gpkg.display().to_string()).unwrap();
    for id in ["county", "payroll"] {
        std::fs::write(
            root.join(format!("connections/{id}.yaml")),
            format!("id: {id}\ntype: gpkg\npath: {p}\ndatasets:\n  allow: [\"*\"]\n"),
        )
        .unwrap();
    }
    std::fs::write(root.join("policy.yaml"), "connection_types: [gpkg]\n").unwrap();
    for (id, conns) in [("review", "[county]"), ("pay-audit", "[payroll]")] {
        std::fs::create_dir_all(root.join("projects").join(id)).unwrap();
        std::fs::write(
            root.join("projects").join(id).join("project.yaml"),
            format!("id: {id}\nname: Parcel review\ninstructions: |\n  Report areas in acres.\nconnections: {conns}\nexport_folder: outbox\n"),
        )
        .unwrap();
    }
    Env { _dir: dir, root }
}

fn command(root: &Path, project: Option<&str>, py: Option<&str>) -> tokio::process::Command {
    let worker =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/python/columbus_worker.py");
    let root = root.to_path_buf();
    let project = project.map(String::from);
    let py = py.unwrap_or("python3").to_string();
    tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
        cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
            .env("COLUMBUS_CONNECTIONS", root.join("connections"))
            .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
            .env("COLUMBUS_PROJECTS", root.join("projects"))
            .env("COLUMBUS_DATA_GROUPS", root.join("data-groups"))
            .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
            .env("COLUMBUS_LIVE_DIR", root.join("live"))
            .env("COLUMBUS_WORKER", &worker)
            .env("COLUMBUS_PYTHON", &py)
            .env("COLUMBUS_LOG", "warn")
            .env("COLUMBUS_COMPANION", "off")
            .env("COLUMBUS_APPROVALS_BROWSER", "none");
        if let Some(p) = &project {
            cmd.env("COLUMBUS_PROJECT", p);
        }
    })
}

async fn start(
    root: &Path,
    project: Option<&str>,
    py: Option<&str>,
) -> RunningService<RoleClient, ()> {
    ().serve(TokioChildProcess::new(command(root, project, py)).unwrap())
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
    r.structured_content.clone().expect("structured output")
}

#[tokio::test]
async fn a_project_session_uses_only_its_connections_and_says_so() {
    let env = setup(None);
    let c = start(&env.root, Some("review"), None).await;
    let who = ok(&call(&c, "whoami", json!({})).await);
    let p = &who["result"]["project"];
    assert_eq!(p["id"], "review", "{who}");
    assert_eq!(p["name"], "Parcel review");
    assert_eq!(p["connections"], json!(["county"]));
    assert_eq!(p["export_folder"], "outbox");
    assert!(p["instructions"].as_str().unwrap().contains("acres"));
    let cards = who["result"]["connections"].as_array().unwrap().clone();
    let card = |id: &str| cards.iter().find(|c| c["id"] == id).unwrap().clone();
    assert_eq!(card("county")["status"], "ok", "{cards:?}");
    assert_ne!(card("payroll")["status"], "ok", "{cards:?}");
    // The other connection cannot be read in this project.
    let r = call(&c, "list_datasets", json!({"connection": "payroll"})).await;
    assert_eq!(r.is_error, Some(true), "{:?}", r.structured_content);
    c.cancel().await.unwrap();

    // Outside a project: both, and no project in whoami.
    let c = start(&env.root, None, None).await;
    let who = ok(&call(&c, "whoami", json!({})).await);
    assert!(who["result"].get("project").is_none(), "{who}");
    let cards = who["result"]["connections"].as_array().unwrap();
    assert!(cards.iter().all(|c| c["status"] == "ok"), "{cards:?}");
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_project_that_cannot_be_read_stops_the_session() {
    let env = setup(None);
    // No such project, a broken file, and a file naming another id.
    std::fs::create_dir_all(env.root.join("projects/broken")).unwrap();
    std::fs::write(
        env.root.join("projects/broken/project.yaml"),
        "id: broken\nname: X\nloosen: true\n",
    )
    .unwrap();
    std::fs::create_dir_all(env.root.join("projects/elsewhere")).unwrap();
    std::fs::write(
        env.root.join("projects/elsewhere/project.yaml"),
        "id: review\nname: X\n",
    )
    .unwrap();
    for id in ["missing", "broken", "elsewhere", "../review"] {
        let started =
            ().serve(TokioChildProcess::new(command(&env.root, Some(id), None)).unwrap())
                .await;
        assert!(
            started.is_err(),
            "{id}: a session started for a project that cannot be read"
        );
    }
}

#[tokio::test]
async fn project_databases_are_kept_apart_per_project() {
    let Some(py) = python("geopandas, pyogrio, shapely, duckdb") else {
        eprintln!("skipped: needs Python with geopandas, pyogrio, shapely and duckdb");
        return;
    };
    let env = setup(Some(&py));
    let c = start(&env.root, Some("review"), Some(&py)).await;
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "parcels"}),
    )
    .await);
    let layer = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    ok(&call(
        &c,
        "project_db",
        json!({"action": "create", "db": "parcels", "scope": "project"}),
    )
    .await);
    ok(&call(&c, "project_db", json!({"action": "load", "db": "parcels", "scope": "project", "layer": layer, "table": "parcels"})).await);
    assert!(env
        .root
        .join("workspaces/project-dbs/review/parcels")
        .is_dir());
    let list = ok(&call(&c, "project_db", json!({"action": "list"})).await);
    assert!(list.to_string().contains("\"parcels\""), "{list}");
    c.cancel().await.unwrap();

    // Another project does not see it, nor does a session outside projects.
    for project in [Some("pay-audit"), None] {
        let c = start(&env.root, project, Some(&py)).await;
        let list = ok(&call(&c, "project_db", json!({"action": "list"})).await);
        assert!(
            !list.to_string().contains("\"parcels\""),
            "{project:?}: {list}"
        );
        let r = call(&c, "project_db", json!({"action": "query", "db": "parcels", "scope": "project", "sql": "select count(*) from parcels"})).await;
        assert_eq!(r.is_error, Some(true), "{project:?}");
        c.cancel().await.unwrap();
    }

    // The app's Projects page lists them through `app query`, as each
    // project's chats see them, and can do nothing else with them.
    let page = |req: Value| {
        let out = command(&env.root, None, Some(&py))
            .args(["app", "query", &req.to_string()])
            .as_std_mut()
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        serde_json::from_str::<Value>(text.lines().last().unwrap_or("")).unwrap_or(Value::Null)
    };
    let list = |project: Option<&str>| {
        let mut req = json!({"tool": "project_db", "args": {"action": "list"}});
        if let Some(p) = project {
            req["project"] = json!(p);
        }
        page(req)
    };
    let v = list(Some("review"));
    assert_eq!(v["ok"], true, "{v}");
    let dbs = v["result"]["result"]["databases"].to_string();
    assert!(dbs.contains("\"parcels\""), "{v}");
    for p in [Some("pay-audit"), None] {
        let v = list(p);
        assert_eq!(v["ok"], true, "{v}");
        assert!(!v.to_string().contains("\"parcels\""), "{p:?}: {v}");
    }
    // A project that doesn't exist fails closed.
    let v = list(Some("nope"));
    assert_eq!(v["ok"], false, "{v}");
    // Only listing.
    for args in [
        json!({"action": "query", "db": "parcels", "scope": "project", "sql": "select 1"}),
        json!({"action": "delete", "db": "parcels", "scope": "project"}),
        json!({"action": "list", "db": "parcels"}),
    ] {
        let v = page(json!({"tool": "project_db", "args": args, "project": "review"}));
        assert_eq!(v["ok"], false, "{args}: {v}");
    }
    assert!(env
        .root
        .join("workspaces/project-dbs/review/parcels")
        .is_dir());
}

#[tokio::test]
async fn a_workflow_proposed_in_a_project_reaches_no_further() {
    let env = setup(None);
    std::fs::write(
        env.root.join("policy.yaml"),
        "connection_types: [gpkg]\nallow_workflows: true\n",
    )
    .unwrap();
    let c = start(&env.root, Some("review"), None).await;
    let wf = |conn: &str| {
        format!(
        "id: parcel-check\nname: Parcel check\npurpose: Check parcels.\nscope:\n  connections: [{conn}]\nsteps:\n  - id: rows\n    tool: query_features\n    args: {{ connection: {conn}, dataset: parcels }}\n"
    )
    };
    let r = call(&c, "propose_workflow", json!({"workflow": wf("payroll")})).await;
    assert_eq!(r.is_error, Some(true), "{:?}", r.structured_content);
    assert!(
        format!("{:?}", r.content).contains("does not use"),
        "{:?}",
        r.content
    );
    let r = call(&c, "propose_workflow", json!({"workflow": wf("county")})).await;
    assert_ne!(r.is_error, Some(true), "{:?}", r.content);
    c.cancel().await.unwrap();
}

fn group_project(root: &Path, groups: &str) {
    std::fs::create_dir_all(root.join("projects/grouped")).unwrap();
    std::fs::write(
        root.join("projects/grouped/project.yaml"),
        format!("id: grouped\nname: Grouped\ndata_groups: [{groups}]\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn a_projects_data_groups_narrow_to_their_datasets_and_rows() {
    let Some(py) = python("geopandas, pyogrio, shapely") else {
        eprintln!("skipped: needs Python with geopandas, pyogrio and shapely");
        return;
    };
    let env = setup(Some(&py));
    std::fs::create_dir_all(env.root.join("data-groups")).unwrap();
    std::fs::write(
        env.root.join("data-groups/one-parcel.yaml"),
        "id: one-parcel\nname: One parcel\ndatasets:\n  - { connection: county, dataset: parcels, filter: \"parcel_id = 'P1'\" }\n",
    )
    .unwrap();
    group_project(&env.root, "one-parcel");
    let c = start(&env.root, Some("grouped"), Some(&py)).await;
    let who = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(
        who["result"]["project"]["data_groups"][0]["id"], "one-parcel",
        "{who}"
    );
    assert_eq!(
        who["result"]["project"]["data_groups"][0]["datasets"][0]["dataset"],
        "parcels"
    );
    // Only the group's dataset, and only its rows.
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "parcels"}),
    )
    .await);
    assert_eq!(q["result"]["layer"]["summary"]["rows"], 1, "{q}");
    let r = call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "roads"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true), "a dataset outside the group");
    // A connection with no group in the project gives nothing.
    let r = call(
        &c,
        "query_features",
        json!({"connection": "payroll", "dataset": "parcels"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true), "a connection outside the groups");
    // The person's own group, described.
    let d = ok(&call(&c, "describe_data_group", json!({"id": "one-parcel"})).await);
    assert_eq!(d["result"]["datasets"][0]["usable_here"], true, "{d}");
    assert_eq!(d["result"]["in_this_project"], true, "{d}");
    // Another of the person's groups, not attached here: described, but
    // marked as outside the project, and its datasets unusable.
    std::fs::write(
        env.root.join("data-groups/all-roads.yaml"),
        "id: all-roads\nname: All roads\ndatasets:\n  - { connection: county, dataset: roads }\n",
    )
    .unwrap();
    let d = ok(&call(&c, "describe_data_group", json!({"id": "all-roads"})).await);
    assert_eq!(d["result"]["in_this_project"], false, "{d}");
    assert_eq!(d["result"]["datasets"][0]["usable_here"], false, "{d}");
    assert!(
        d["checks"].to_string().contains("group_not_in_project"),
        "{d}"
    );
    // list_datasets names only the group's dataset, without the stored
    // extent of rows the filter keeps out.
    let l = ok(&call(&c, "list_datasets", json!({"connection": "county"})).await);
    let ds = l["result"]["datasets"].as_array().unwrap();
    assert_eq!(ds.len(), 1, "{l}");
    assert_eq!(ds[0]["name"], "parcels");
    assert!(ds[0]["extent"].is_null(), "{l}");
    // describe_connection shows the narrowing.
    let dc = ok(&call(&c, "describe_connection", json!({"connection": "county"})).await);
    assert_eq!(
        dc["result"]["data_group_datasets"][0]["dataset"], "parcels",
        "{dc}"
    );
    assert_eq!(
        dc["result"]["data_group_datasets"][0]["filter"], "parcel_id = 'P1'",
        "{dc}"
    );
    // No workflow from here yet: it couldn't keep to the groups.
    let r = call(&c, "propose_workflow", json!({"workflow": "id: x-flow\nname: X\npurpose: X.\nscope:\n  connections: [county]\nsteps:\n  - id: rows\n    tool: query_features\n    args: { connection: county, dataset: roads }\n"})).await;
    assert_eq!(r.is_error, Some(true));
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_data_group_that_cannot_be_used_stops_the_session() {
    let env = setup(None);
    std::fs::create_dir_all(env.root.join("data-groups")).unwrap();
    // Missing; a connection the person doesn't have; a filter Columbus won't run.
    std::fs::write(
        env.root.join("data-groups/elsewhere.yaml"),
        "id: elsewhere\nname: E\ndatasets:\n  - { connection: nope, dataset: x }\n",
    )
    .unwrap();
    std::fs::write(env.root.join("data-groups/sneaky.yaml"), "id: sneaky\nname: S\ndatasets:\n  - { connection: county, dataset: parcels, filter: \"1=1) OR (1=1\" }\n").unwrap();
    for g in ["missing-group", "elsewhere", "sneaky"] {
        group_project(&env.root, g);
        let started =
            ().serve(TokioChildProcess::new(command(&env.root, Some("grouped"), None)).unwrap())
                .await;
        assert!(
            started.is_err(),
            "{g}: a session started with a data group it can't keep to"
        );
    }
}

#[tokio::test]
async fn a_project_database_is_withheld_once_its_data_groups_change() {
    let Some(py) = python("geopandas, pyogrio, shapely, duckdb") else {
        eprintln!("skipped: needs Python with geopandas, pyogrio, shapely and duckdb");
        return;
    };
    let env = setup(Some(&py));
    std::fs::create_dir_all(env.root.join("data-groups")).unwrap();
    let group = |filter: &str| {
        std::fs::write(
            env.root.join("data-groups/parcels-only.yaml"),
            format!("id: parcels-only\nname: Parcels\ndatasets:\n  - {{ connection: county, dataset: parcels, filter: \"{filter}\" }}\n"),
        )
        .unwrap();
    };
    group("parcel_id <> 'zz'");
    group_project(&env.root, "parcels-only");
    let c = start(&env.root, Some("grouped"), Some(&py)).await;
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "parcels"}),
    )
    .await);
    let layer = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    ok(&call(
        &c,
        "project_db",
        json!({"action": "create", "db": "work", "scope": "project"}),
    )
    .await);
    ok(&call(&c, "project_db", json!({"action": "load", "db": "work", "scope": "project", "layer": layer, "table": "parcels"})).await);
    ok(&call(&c, "project_db", json!({"action": "query", "db": "work", "scope": "project", "sql": "select count(*) as n from parcels"})).await);
    c.cancel().await.unwrap();

    // The group narrows: what was built under the wider one is withheld.
    group("parcel_id = 'P1'");
    let c = start(&env.root, Some("grouped"), Some(&py)).await;
    let r = call(&c, "project_db", json!({"action": "query", "db": "work", "scope": "project", "sql": "select count(*) as n from parcels"})).await;
    assert_eq!(r.is_error, Some(true), "{:?}", r.structured_content);
    assert!(
        format!("{:?}", r.content).contains("data groups"),
        "{:?}",
        r.content
    );
    c.cancel().await.unwrap();

    // Back to the first group: readable again (every load ran under it).
    group("parcel_id <> 'zz'");
    let c = start(&env.root, Some("grouped"), Some(&py)).await;
    ok(&call(&c, "project_db", json!({"action": "query", "db": "work", "scope": "project", "sql": "select count(*) as n from parcels"})).await);
    c.cancel().await.unwrap();
    // Without groups, wider data goes in; the group back again can't read it.
    std::fs::write(
        env.root.join("projects/grouped/project.yaml"),
        "id: grouped\nname: Grouped\n",
    )
    .unwrap();
    let c = start(&env.root, Some("grouped"), Some(&py)).await;
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "roads"}),
    )
    .await);
    let roads = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    ok(&call(&c, "project_db", json!({"action": "load", "db": "work", "scope": "project", "layer": roads, "table": "roads"})).await);
    c.cancel().await.unwrap();
    group_project(&env.root, "parcels-only");
    let c = start(&env.root, Some("grouped"), Some(&py)).await;
    let r = call(&c, "project_db", json!({"action": "query", "db": "work", "scope": "project", "sql": "select count(*) as n from roads"})).await;
    assert_eq!(
        r.is_error,
        Some(true),
        "wider data loaded without the group: {:?}",
        r.structured_content
    );
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn a_filter_naming_a_column_its_table_lacks_never_binds_outside() {
    let Some(py) = python("geopandas, pyogrio, shapely") else {
        eprintln!("skipped: needs Python with geopandas, pyogrio and shapely");
        return;
    };
    let env = setup(Some(&py));
    std::fs::create_dir_all(env.root.join("data-groups")).unwrap();
    // parcels has no `status`: the filter must fail, not match the outer query's.
    std::fs::write(
        env.root.join("data-groups/typo.yaml"),
        "id: typo\nname: Typo\ndatasets:\n  - { connection: county, dataset: parcels, filter: \"status = 'X'\" }\n",
    )
    .unwrap();
    group_project(&env.root, "typo");
    let c = start(&env.root, Some("grouped"), Some(&py)).await;
    let r = call(&c, "sql_select", json!({"connection": "county", "sql": "SELECT (SELECT group_concat(parcel_id) FROM parcels) AS ids FROM (SELECT 'X' AS status) d"})).await;
    let out = format!("{:?} {:?}", r.content, r.structured_content);
    assert!(
        r.is_error == Some(true) || !out.contains("P1"),
        "the filter was bypassed: {out}"
    );
    c.cancel().await.unwrap();
}
