//! A project has files (T-31; build brief 3, D-19, F-17, REQ-41).
//!
//! A chat started in a project (`COLUMBUS_PROJECT`) under policy
//! `allow_project_files` writes a script into the project's files, lists
//! it, reads it, runs it by its path (with a helper kept beside it),
//! changes it and removes it, each step audited. Paths the rules refuse
//! are refused in plain words and nothing outside the folder is read or
//! written. A chat with no project is told to start one in a project;
//! with the policy off the tool is not offered. What a file may quote
//! keeps its data labels, and the organization's flow rules apply.
//!
//! The parts that read governed data or run a script need Python with
//! geopandas, pyogrio, shapely and pyproj; skipped otherwise.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

fn engine() -> Option<String> {
    let py = support::python_for("import geopandas, pyogrio, shapely, pyproj");
    if py.is_none() {
        eprintln!("skipping: the GIS engine is not installed");
    }
    py
}

/// Whether this machine can set the sandbox up. Where CI says tests must
/// not skip, a machine that cannot fails the test here, on every system
/// (the one check in `support`).
fn can_confine() -> bool {
    support::can_confine(std::path::Path::new(env!("CARGO_BIN_EXE_columbus-confine")))
}

fn sha256(text: &str) -> String {
    columbus_shared::canon::sha256_hex(text.as_bytes())
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    py: String,
}

impl Env {
    /// The project's own folder, and its files.
    fn project(&self, id: &str) -> PathBuf {
        self.root.join("projects").join(id)
    }
    fn files(&self, id: &str) -> PathBuf {
        self.project(id).join("files")
    }
    fn policy(&self, text: &str) {
        std::fs::write(
            self.root.join("policy.yaml"),
            format!("connection_types: [gpkg]\napprovals: {{ page: false }}\n{text}"),
        )
        .unwrap();
    }
    fn audit(&self) -> String {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.root.join("audit"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        files
            .iter()
            .map(|f| std::fs::read_to_string(f).unwrap())
            .collect()
    }
}

/// Two connections on one GeoPackage (with the engine: two parcels and a
/// road), and two projects: `review` (county only) and `both`.
fn setup(py: Option<&str>, policy: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    // A real path: macOS's temporary folder is behind a link.
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    for (id, conns) in [("review", "connections: [county]\n"), ("both", "")] {
        std::fs::create_dir_all(root.join("projects").join(id)).unwrap();
        std::fs::write(
            root.join("projects").join(id).join("project.yaml"),
            format!("id: {id}\nname: Parcel review\n{conns}"),
        )
        .unwrap();
    }
    let env = Env {
        _dir: dir,
        root,
        py: py.unwrap_or("python3").to_string(),
    };
    env.policy(policy);
    env
}

async fn start(env: &Env, project: Option<&str>) -> RunningService<RoleClient, ()> {
    let worker =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workers/python/columbus_worker.py");
    let root = env.root.clone();
    let project = project.map(String::from);
    let py = env.py.clone();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |cmd| {
            cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
                .env("COLUMBUS_CONNECTIONS", root.join("connections"))
                .env("COLUMBUS_ENVIRONMENTS", root.join("environments"))
                .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
                .env("COLUMBUS_PROJECTS", root.join("projects"))
                .env("COLUMBUS_DATA_GROUPS", root.join("data-groups"))
                .env("COLUMBUS_TOOLS", root.join("tools"))
                .env("COLUMBUS_ORG_TOOLS", root.join("org-tools"))
                .env("COLUMBUS_SERVERS", root.join("servers"))
                .env("COLUMBUS_PLUGINS", root.join("plugins"))
                .env("COLUMBUS_PERSONAL_PLUGINS", root.join("personal-plugins"))
                .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
                .env("COLUMBUS_LIVE_DIR", root.join("live"))
                .env("COLUMBUS_WORKER", &worker)
                .env("COLUMBUS_PYTHON", &py)
                .env("COLUMBUS_LOG", "warn")
                .env("COLUMBUS_COMPANION", "off")
                .env("COLUMBUS_APPROVALS_BROWSER", "none");
            match &project {
                Some(p) => cmd.env("COLUMBUS_PROJECT", p),
                None => cmd.env_remove("COLUMBUS_PROJECT"),
            };
        });
    ().serve(TokioChildProcess::new(cmd).unwrap())
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

fn err(r: &CallToolResult) -> String {
    assert_eq!(
        r.is_error,
        Some(true),
        "expected an error, got {:?}",
        r.structured_content
    );
    serde_json::to_string(&r.content).unwrap()
}

/// One project_files call.
async fn files(c: &RunningService<RoleClient, ()>, args: Value) -> CallToolResult {
    call(c, "project_files", args).await
}

async fn written(c: &RunningService<RoleClient, ()>, path: &str, text: &str) -> Value {
    ok(&files(c, json!({"action": "write", "path": path, "text": text})).await)
}

/// The main script: it imports a helper kept beside it and reads its input.
const SCRIPT: &str = r#"# marker-in-the-script-text
import json, sqlite3
import geom
paths = json.load(open("inputs.json"))
db = sqlite3.connect(paths["parcels"])
table = db.execute("SELECT table_name FROM gpkg_contents").fetchone()[0]
rows = db.execute(f'SELECT count(*) FROM "{table}"').fetchone()[0]
print(geom.describe(rows))
"#;
const HELPER: &str = "def describe(n):\n    return f\"helper says {n} parcels\"\n";

/// Every file under a folder, with its bytes, to see that nothing changed.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                todo.push(p);
            } else if m.is_file() {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn a_chat_in_a_project_writes_lists_reads_runs_changes_and_removes_a_script() {
    let Some(py) = engine() else { return };
    let env = setup(
        Some(&py),
        "allow_scripts: true\nscripts: { approval: none }\nallow_project_files: true\n",
    );
    let c = start(&env, Some("review")).await;
    let w = ok(&call(&c, "whoami", json!({})).await);
    assert_eq!(w["result"]["policy"]["project_files_allowed"], true);
    // The assistant finds the tool by describing the job.
    let found = ok(&call(&c, "search_tools", json!({"query": "project files script"})).await);
    assert!(
        found["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "project_files"),
        "{found}"
    );

    // Nothing there yet (the folder is made with the first file).
    let l = ok(&files(&c, json!({"action": "list"})).await);
    assert_eq!(l["result"]["files"], json!([]), "{l}");
    assert_eq!(l["result"]["project"]["id"], "review");
    assert!(!env.files("review").exists());

    // Write a script and a helper beside it.
    let made = written(&c, "scripts/widths.py", SCRIPT).await;
    assert_eq!(made["result"]["status"], "written", "{made}");
    assert_eq!(made["result"]["replaced"], false);
    assert_eq!(made["result"]["sha256"], sha256(SCRIPT));
    assert_eq!(made["result"]["bytes"], SCRIPT.len());
    let on_disk = env.files("review").join("scripts").join("widths.py");
    assert_eq!(std::fs::read_to_string(&on_disk).unwrap(), SCRIPT);
    written(&c, "scripts/geom.py", HELPER).await;

    // List it: the path, the size and when it changed.
    let l = ok(&files(&c, json!({"action": "list"})).await);
    let listed = l["result"]["files"].as_array().unwrap();
    assert_eq!(listed.len(), 2, "{l}");
    assert_eq!(listed[1]["path"], "scripts/widths.py");
    assert_eq!(listed[1]["bytes"], SCRIPT.len());
    let when = listed[1]["modified"].as_str().unwrap();
    assert!(
        chrono::DateTime::parse_from_rfc3339(when).is_ok(),
        "a time: {when}"
    );
    assert_eq!(l["result"]["folders"], json!(["scripts"]));
    assert_eq!(l["result"]["total_bytes"], SCRIPT.len() + HELPER.len());
    let inside = ok(&files(&c, json!({"action": "list", "path": "scripts"})).await);
    assert_eq!(inside["result"]["files"].as_array().unwrap().len(), 2);

    // Read it: the text, and the hash a later change must name.
    let r = ok(&files(&c, json!({"action": "read", "path": "scripts/widths.py"})).await);
    assert_eq!(r["result"]["text"], SCRIPT);
    assert_eq!(r["result"]["sha256"], sha256(SCRIPT));
    assert_eq!(r["result"]["lines"], SCRIPT.lines().count());
    assert_eq!(r["checks"][0]["id"], "project_files.text", "{r}");

    // Run it by its path: the same sandbox, the text that is there now,
    // the helper beside it.
    let q = ok(&call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "parcels", "fields": ["parcel_id"]}),
    )
    .await);
    let parcels = q["result"]["layer"]["handle"].as_str().unwrap().to_string();
    let run = json!({"script": "scripts/widths.py", "inputs": {"parcels": parcels}});
    let ran = call(&c, "run_python", run.clone()).await;
    if !can_confine() {
        assert!(err(&ran).contains("confined"));
    } else {
        let ran = ok(&ran);
        assert_eq!(ran["result"]["status"], "done", "{ran}");
        assert!(
            ran["result"]["stdout"]
                .as_str()
                .unwrap()
                .contains("helper says 2 parcels"),
            "{ran}"
        );
        // The code hash is of the text that ran.
        assert_eq!(ran["result"]["code_sha256"], sha256(SCRIPT));
        let ws = PathBuf::from(
            ok(&call(&c, "whoami", json!({})).await)["result"]["workspace"]
                .as_str()
                .unwrap(),
        );
        let prov = ran["provenance"].as_str().unwrap();
        let rec: Value = serde_json::from_str(
            &std::fs::read_to_string(ws.join("provenance").join(format!("{prov}.prov.json")))
                .unwrap(),
        )
        .unwrap();
        let script = &rec["steps"][0]["script"];
        assert_eq!(script["code"], SCRIPT);
        assert_eq!(script["path"], "scripts/widths.py");
        assert_eq!(script["project"], "review");
        assert_eq!(script["helpers"][0]["name"], "geom.py");
        assert_eq!(script["helpers"][0]["sha256"], sha256(HELPER));
        assert_eq!(rec["steps"][0]["confinement"]["level"], "A");
        // Code and a path together are refused; so is a path that is not there.
        let e = err(&call(
            &c,
            "run_python",
            json!({"script": "scripts/widths.py", "code": "print(1)"}),
        )
        .await);
        assert!(e.contains("code or script, not both"), "{e}");
        let e = err(&call(&c, "run_python", json!({"script": "scripts/gone.py"})).await);
        assert!(e.contains("there is no file scripts/gone.py"), "{e}");
        let e = err(&call(&c, "run_python", json!({"script": "../project.yaml"})).await);
        assert!(e.contains("climb out"), "{e}");
    }

    // Change it: only with the hash of what was read.
    let changed = SCRIPT.replace("print(geom.describe(rows))", "print(\"changed\", rows)");
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "scripts/widths.py", "text": changed}),
    )
    .await);
    assert!(e.contains("read it first"), "{e}");
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "scripts/widths.py", "text": changed, "sha256": sha256("something else")}),
    )
    .await);
    assert!(e.contains("changed since you read it"), "{e}");
    assert_eq!(std::fs::read_to_string(&on_disk).unwrap(), SCRIPT);
    let again = ok(&files(
        &c,
        json!({"action": "write", "path": "scripts/widths.py", "text": changed, "sha256": sha256(SCRIPT)}),
    )
    .await);
    assert_eq!(again["result"]["replaced"], true, "{again}");
    assert_eq!(std::fs::read_to_string(&on_disk).unwrap(), changed);
    if can_confine() {
        let ran = ok(&call(&c, "run_python", run).await);
        assert!(
            ran["result"]["stdout"]
                .as_str()
                .unwrap()
                .contains("changed 2"),
            "{ran}"
        );
        assert_eq!(ran["result"]["code_sha256"], sha256(&changed));
    }
    // The person changes it in their editor: the assistant's next write,
    // made from what it read before, does not overwrite that.
    let theirs = format!("{changed}# the person's own line\n");
    std::fs::write(&on_disk, &theirs).unwrap();
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "scripts/widths.py", "text": SCRIPT, "sha256": sha256(&changed)}),
    )
    .await);
    assert!(e.contains("changed since you read it"), "{e}");
    assert_eq!(std::fs::read_to_string(&on_disk).unwrap(), theirs);

    // Remove it: the same rule, and an emptied folder goes with its last file.
    let e = err(&files(&c, json!({"action": "delete", "path": "scripts/widths.py"})).await);
    assert!(e.contains("read it first"), "{e}");
    let e = err(&files(
        &c,
        json!({"action": "delete", "path": "scripts/widths.py", "sha256": sha256(&changed)}),
    )
    .await);
    assert!(e.contains("changed since you read it"), "{e}");
    assert!(on_disk.exists());
    let d = ok(&files(
        &c,
        json!({"action": "delete", "path": "scripts/widths.py", "sha256": sha256(&theirs)}),
    )
    .await);
    assert_eq!(d["result"]["status"], "deleted", "{d}");
    assert_eq!(d["result"]["removed_folders"], json!([]));
    assert!(!on_disk.exists() && env.files("review").join("scripts").is_dir());
    let d = ok(&files(
        &c,
        json!({"action": "delete", "path": "scripts/geom.py", "sha256": sha256(HELPER)}),
    )
    .await);
    assert_eq!(d["result"]["removed_folders"], json!(["scripts"]));
    assert!(!env.files("review").join("scripts").exists());
    assert!(env.files("review").is_dir());
    let e = err(&files(&c, json!({"action": "read", "path": "scripts/widths.py"})).await);
    assert!(e.contains("there is no file scripts/widths.py"), "{e}");
    let e = err(&files(&c, json!({"action": "copy", "path": "x.py"})).await);
    assert!(e.contains("list, read, write or delete"), "{e}");

    c.cancel().await.unwrap();
    // Every action is in the audit log, by path and hash, never by text.
    let audit = env.audit();
    let lines: Vec<Value> = audit
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["tool"] == "project_files")
        .collect();
    let said = |decision: &str, word: &str| {
        lines.iter().any(|l| {
            l["decision"] == decision
                && l["dataset"] == "project:review/scripts/widths.py"
                && l["reason"].as_str().is_some_and(|r| r.contains(word))
        })
    };
    assert!(said("allowed", "created"), "{lines:?}");
    assert!(said("allowed", "read "), "{lines:?}");
    assert!(said("allowed", "replaced"), "{lines:?}");
    assert!(said("allowed", "deleted"), "{lines:?}");
    assert!(said("denied", "changed since you read it"), "{lines:?}");
    assert!(lines.iter().any(|l| l["decision"] == "allowed"
        && l["dataset"] == "project:review/"
        && l["rows"] == 2));
    assert!(
        !audit.contains("marker-in-the-script-text") && !audit.contains("helper says"),
        "the audit log holds no file text"
    );
    if can_confine() {
        assert!(
            audit.lines().any(|l| l.contains("\"tool\":\"run_python\"")
                && l.contains("project:review/scripts/widths.py")
                && l.contains("\"decision\":\"allowed\"")),
            "{audit}"
        );
    }
    let audit_file = std::fs::read_dir(env.root.join("audit"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    columbus_shared::audit::verify(&audit_file).unwrap();
}

#[tokio::test]
async fn paths_and_files_the_rules_refuse_stay_refused() {
    let env = setup(None, "allow_project_files: true\n");
    let files_dir = env.files("review");
    let outside = env.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "the secret outside").unwrap();
    let c = start(&env, Some("review")).await;
    written(&c, "scripts/widths.py", "print(1)\n").await;
    // What the person put there themselves.
    std::fs::write(
        files_dir.join("columbus-project.yaml"),
        "name: Parcel review\n",
    )
    .unwrap();
    std::fs::write(files_dir.join("big.txt"), vec![b'a'; 1024 * 1024 + 1]).unwrap();
    std::fs::write(
        files_dir.join("layer.gpkg"),
        b"SQLite format 3\0\x10\0\x01\x01",
    )
    .unwrap();
    std::fs::write(
        files_dir.join("picture.png"),
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR",
    )
    .unwrap();
    std::fs::write(files_dir.join("codes.csv"), "code,name\n7,arterial\n").unwrap();
    std::fs::write(files_dir.join(".env"), "DB_PASSWORD=hunter2\n").unwrap();
    std::fs::write(files_dir.join("run.bat"), "echo theirs\r\n").unwrap();
    std::fs::create_dir_all(files_dir.join(".git")).unwrap();
    std::fs::write(files_dir.join(".git").join("config"), "[core]\n").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret.txt"), files_dir.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, files_dir.join("linked")).unwrap();
    }
    let before_project = snapshot(&env.project("review"));
    let before_outside = snapshot(&outside);
    let project_yaml = std::fs::read_to_string(env.project("review").join("project.yaml")).unwrap();

    // Each refused path, for reading, writing and deleting.
    let mut cases = vec![
        ("/etc/passwd", "absolute"),
        ("C:\\Users\\me\\x.py", "absolute"),
        ("~/x.py", "absolute"),
        ("../project.yaml", "climb out"),
        ("scripts/../../project.yaml", "climb out"),
        ("scripts\\widths.py", "use / between"),
        (".git/config", "kept for Git"),
        ("scripts/.git/hooks/pre-commit", "kept for Git"),
        (".github/workflows/ci.yml", "kept for Git"),
        (".columbus/state.json", "kept for Git"),
        ("nul.py", "Windows keeps"),
        ("scripts/COM1.txt", "Windows keeps"),
        ("aux", "Windows keeps"),
        ("Scripts/widths.py", "differ only by upper and lower case"),
        ("scripts/Widths.py", "differ only by upper and lower case"),
        ("SCRIPTS/new.py", "differ only by upper and lower case"),
        (".env", "password or key"),
        ("keys/server.pem", "password or key"),
        ("db/prod.sde", "connection file"),
    ];
    if cfg!(unix) {
        cases.push(("link.txt", "links are never followed"));
        cases.push(("linked/secret.txt", "links are never followed"));
        cases.push(("linked/new.txt", "links are never followed"));
    }
    for (path, why) in &cases {
        for args in [
            json!({"action": "read", "path": path}),
            json!({"action": "write", "path": path, "text": "x = 1\n"}),
            json!({"action": "write", "path": path, "text": "x = 1\n", "sha256": sha256("the secret outside")}),
            json!({"action": "delete", "path": path, "sha256": sha256("the secret outside")}),
        ] {
            let e = err(&files(&c, args.clone()).await);
            assert!(e.contains(why), "{path} {args}: {e}");
            assert!(
                !e.contains("the secret outside") && !e.contains("hunter2"),
                "{e}"
            );
        }
    }
    for path in ["scripts/..", "linked"] {
        let e = err(&files(&c, json!({"action": "list", "path": path})).await);
        assert!(!e.contains("secret.txt"), "{e}");
    }
    let e = err(&call(&c, "run_python", json!({"script": "../project.yaml"})).await);
    assert!(
        e.contains("allow_scripts") || e.contains("climb out"),
        "{e}"
    );

    // A file over the size limit, and one that is not text: not read, not
    // replaced, not removed.
    for (path, why) in [
        ("big.txt", "larger than 1024 KB"),
        ("picture.png", "not a text file"),
    ] {
        let e = err(&files(&c, json!({"action": "read", "path": path})).await);
        assert!(e.contains(why), "{path}: {e}");
        let e = err(&files(
            &c,
            json!({"action": "write", "path": path, "text": "x", "sha256": sha256("x")}),
        )
        .await);
        assert!(
            e.contains(why) && e.contains("does not replace it"),
            "{path}: {e}"
        );
        let e = err(&files(
            &c,
            json!({"action": "delete", "path": path, "sha256": sha256("x")}),
        )
        .await);
        assert!(
            e.contains(why) && e.contains("does not remove it"),
            "{path}: {e}"
        );
    }
    // Data (R-103): a project's files are for scripts and notes, so the
    // assistant writes no data format there, new or over one the person
    // put there. Results leave a chat only through an export (D-19). One
    // the person put there is read as before, when it is text.
    let e = err(&files(&c, json!({"action": "read", "path": "layer.gpkg"})).await);
    assert!(e.contains("not a text file"), "{e}");
    let r = ok(&files(&c, json!({"action": "read", "path": "codes.csv"})).await);
    assert_eq!(r["result"]["text"], "code,name\n7,arterial\n");
    let theirs = r["result"]["sha256"].as_str().unwrap().to_string();
    for args in [
        json!({"action": "write", "path": "codes.csv", "text": "code,name\n8,local\n", "sha256": theirs}),
        json!({"action": "write", "path": "layer.gpkg", "text": "x", "sha256": sha256("x")}),
        json!({"action": "write", "path": "data/rows.csv", "text": "id,owner\n1,M. Okafor\n"}),
        json!({"action": "write", "path": "data/rows.tsv", "text": "id\towner\n"}),
        json!({"action": "write", "path": "roads.geojson", "text": "{\"type\": \"FeatureCollection\", \"features\": []}"}),
        json!({"action": "write", "path": "places.kml", "text": "<kml/>"}),
        json!({"action": "write", "path": "book.xlsx", "text": "x"}),
        json!({"action": "write", "path": "cache.sqlite", "text": "x"}),
    ] {
        let e = err(&files(&c, args.clone()).await);
        assert!(
            e.contains("is data") && e.contains("only through an export (export_output)"),
            "{args}: {e}"
        );
    }
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "bundle.zip", "text": "x"}),
    )
    .await);
    assert!(e.contains("is an archive"), "{e}");
    assert!(!files_dir.join("data").exists());
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "notes/long.md", "text": "a".repeat(1024 * 1024 + 1)}),
    )
    .await);
    assert!(e.contains("at most 1024 KB"), "{}", &e[..200.min(e.len())]);
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "notes/odd.md", "text": "a\u{0}b"}),
    )
    .await);
    assert!(e.contains("text files only"), "{e}");
    assert!(
        !files_dir.join("notes").exists(),
        "a refused write makes no folder"
    );

    // The shared settings: read, never changed. Types the system runs when
    // opened: read (the person's own), never written.
    let r = ok(&files(
        &c,
        json!({"action": "read", "path": "columbus-project.yaml"}),
    )
    .await);
    assert_eq!(r["result"]["text"], "name: Parcel review\n");
    let theirs = r["result"]["sha256"].as_str().unwrap().to_string();
    for args in [
        json!({"action": "write", "path": "columbus-project.yaml", "text": "name: Mine\n", "sha256": theirs}),
        json!({"action": "write", "path": "Columbus-Project.yaml", "text": "name: Mine\n"}),
        json!({"action": "delete", "path": "columbus-project.yaml", "sha256": theirs}),
    ] {
        let e = err(&files(&c, args).await);
        assert!(e.contains("shared settings"), "{e}");
    }
    let r = ok(&files(&c, json!({"action": "read", "path": "run.bat"})).await);
    assert_eq!(r["result"]["text"], "echo theirs\r\n");
    for path in [
        "run.bat",
        "open me.cmd",
        "tools/setup.vbs",
        "report.pdf.lnk",
        "page.url",
        // R-104: more kinds, and every part of the path.
        "tool.pyw",
        "tool.pyz",
        "tool.pyzw",
        "office.rdp",
        "query.iqy",
        "sheet.slk",
        "Tool.app/Contents/MacOS/tool.py",
        "tools/Daily.workflow/notes.md",
        "a.app/x.py",
    ] {
        let e = err(&files(
            &c,
            json!({"action": "write", "path": path, "text": "x", "sha256": r["result"]["sha256"]}),
        )
        .await);
        assert!(
            e.contains("runs or follows when it is opened"),
            "{path}: {e}"
        );
    }

    // What a listing says of all this: plain files with their sizes, and
    // what is there but not offered.
    let l = ok(&files(&c, json!({"action": "list"})).await);
    let listed: Vec<&str> = l["result"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        listed,
        [
            ".env",
            "big.txt",
            "codes.csv",
            "columbus-project.yaml",
            "layer.gpkg",
            "picture.png",
            "run.bat",
            "scripts/widths.py"
        ]
    );
    assert!(
        !files_dir.join("Tool.app").exists() && !files_dir.join("tools").exists(),
        "a refused write makes no folder"
    );
    let note = |path: &str| {
        l["result"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == path)
            .unwrap()["not_readable"]
            .clone()
    };
    assert!(note(".env").as_str().unwrap().contains("password or key"));
    assert!(note("big.txt").as_str().unwrap().contains("larger than"));
    assert!(note("scripts/widths.py").is_null());
    let skipped: Vec<&str> = l["result"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    if cfg!(unix) {
        assert_eq!(skipped, [".git", "link.txt", "linked"], "{l}");
    } else {
        assert_eq!(skipped, [".git"], "{l}");
    }

    // Nothing outside the files folder was read or written, and nothing in
    // it changed.
    assert_eq!(snapshot(&env.project("review")), before_project);
    assert_eq!(snapshot(&outside), before_outside);
    assert_eq!(
        std::fs::read_to_string(env.project("review").join("project.yaml")).unwrap(),
        project_yaml
    );
    let top: Vec<String> = std::fs::read_dir(env.project("review"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(top.len(), 2, "only project.yaml and files: {top:?}");

    // The folder holds a thousand files at most once the assistant writes.
    let many = files_dir.join("many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..1000 {
        std::fs::write(many.join(format!("f{i:04}.txt")), "x").unwrap();
    }
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "one-more.txt", "text": "x"}),
    )
    .await);
    assert!(e.contains("adds none past 1000"), "{e}");
    assert!(!files_dir.join("one-more.txt").exists());

    c.cancel().await.unwrap();
    let audit = env.audit();
    assert!(!audit.contains("the secret outside") && !audit.contains("hunter2"));
    assert!(audit.contains("\"decision\":\"denied\""));
}

// Made up for these tests, and put together here so that no line of this
// file looks like a credential: none is real.
const PASSWORD: &str = "Wint3r2026";
fn made_up_token() -> String {
    format!("gh{}_{}", "p", "0123456789abcdefghijABCDEFGHIJ012345")
}

/// No secrets to the assistant (R-104): a file that looks like it holds a
/// password, a key or a token is not read to it, and is not written by it.
/// The answer names the file and the lines, never what is on them.
#[tokio::test]
async fn a_file_that_holds_a_secret_is_not_read_to_the_assistant_or_written_by_it() {
    let env = setup(None, "allow_project_files: true\n");
    let files_dir = env.files("review");
    std::fs::create_dir_all(files_dir.join("notes")).unwrap();
    let token = made_up_token();
    // What the person (or a colleague, through sync) put there.
    std::fs::write(
        files_dir.join("notes").join("connect.md"),
        format!("# How I connect\n\nuser: gis_reader\npassword: {PASSWORD}\n\ntoken {token}\n"),
    )
    .unwrap();
    std::fs::write(
        files_dir.join("notes").join("plain.md"),
        "# Plain notes\nThe password is kept in the credential store.\n",
    )
    .unwrap();
    let before = snapshot(&files_dir);
    let c = start(&env, Some("review")).await;

    let e = err(&files(&c, json!({"action": "read", "path": "notes/connect.md"})).await);
    assert!(
        e.contains("notes/connect.md is not read to you")
            && e.contains("line 4 (a password written into the file)")
            && e.contains("line 6 (a GitHub token)"),
        "{e}"
    );
    assert!(
        e.contains("take the secret out of the file") && e.contains("connection"),
        "what to do: {e}"
    );
    assert!(
        !e.contains(PASSWORD) && !e.contains(&token) && !e.contains("gis_reader"),
        "nothing of the file's text: {e}"
    );
    // A file that only talks about passwords reads as before.
    let r = ok(&files(&c, json!({"action": "read", "path": "notes/plain.md"})).await);
    assert!(r["result"]["text"]
        .as_str()
        .unwrap()
        .contains("credential store"));
    // It is still listed (by name and size), and cannot be replaced or
    // removed by the assistant: it has no hash of it to give.
    let l = ok(&files(&c, json!({"action": "list"})).await);
    assert!(l["result"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "notes/connect.md"));
    for args in [
        json!({"action": "write", "path": "notes/connect.md", "text": "x\n"}),
        json!({"action": "delete", "path": "notes/connect.md"}),
    ] {
        let e = err(&files(&c, args).await);
        assert!(e.contains("read it first"), "{e}");
    }

    // The assistant writes no secret either: the text is refused, and the
    // answer says which line, so it can leave it out.
    for (path, text, line, what) in [
        (
            "scripts/load.py",
            format!("import psycopg2\n\nconn = psycopg2.connect(host=\"db\", user=\"gis\", password=\"{PASSWORD}\")\n"),
            "line 3",
            "a password or key written into the file",
        ),
        (
            "conf/db.yaml",
            format!("host: db\nuser: gis\npassword: {PASSWORD}\n"),
            "line 3",
            "a password written into the file",
        ),
        (
            "notes/api.md",
            format!("call it with\n\n    curl -H 'Authorization: Bearer {token}' https://api.example.org\n"),
            "line 3",
            "a GitHub token",
        ),
    ] {
        let e = err(&files(
            &c,
            json!({"action": "write", "path": path, "text": text}),
        )
        .await);
        assert!(
            e.contains("was not written") && e.contains(line) && e.contains(what),
            "{path}: {e}"
        );
        assert!(!e.contains(PASSWORD) && !e.contains(&token), "{e}");
    }
    assert!(!files_dir.join("scripts").exists() && !files_dir.join("conf").exists());
    // The way to keep a password out of a script is fine.
    let fine = "import os\n\npassword = os.environ[\"PGPASSWORD\"]\n";
    assert_eq!(
        written(&c, "scripts/env.py", fine).await["result"]["status"],
        "written"
    );
    std::fs::remove_dir_all(files_dir.join("scripts")).unwrap();
    assert_eq!(snapshot(&files_dir), before, "nothing else changed");

    c.cancel().await.unwrap();
    let audit = env.audit();
    assert!(
        !audit.contains(PASSWORD) && !audit.contains(&token),
        "the audit log holds no secret"
    );
    assert!(
        audit.lines().any(|l| l.contains("\"decision\":\"denied\"")
            && l.contains("project:review/notes/connect.md")
            && l.contains("line 4")),
        "{audit}"
    );
}

/// The same for a script run by its path, and for the helper files copied
/// beside it (R-104): a script with a secret in it is not run, and a helper
/// with one is not copied.
#[tokio::test]
async fn a_script_that_holds_a_secret_is_not_run_and_such_a_helper_is_not_copied() {
    let Some(py) = engine() else { return };
    let env = setup(
        Some(&py),
        "allow_scripts: true\nscripts: { approval: none }\nallow_project_files: true\n",
    );
    let files_dir = env.files("review");
    std::fs::create_dir_all(files_dir.join("scripts")).unwrap();
    std::fs::create_dir_all(files_dir.join("jobs")).unwrap();
    let token = made_up_token();
    std::fs::write(
        files_dir.join("scripts").join("load.py"),
        format!("import os\n# sign in\nPASSWORD = \"{PASSWORD}\"\nprint(\"ran with\", PASSWORD)\n"),
    )
    .unwrap();
    // A clean script, beside a helper that holds a token and one that does not.
    std::fs::write(
        files_dir.join("jobs").join("count.py"),
        "import geom\ntry:\n    import keys\n    print(\"keys came along:\", keys.TOKEN)\nexcept ImportError:\n    print(\"no keys module here\")\nprint(geom.describe(2))\n",
    )
    .unwrap();
    std::fs::write(files_dir.join("jobs").join("geom.py"), HELPER).unwrap();
    std::fs::write(
        files_dir.join("jobs").join("keys.py"),
        format!("# not for anyone\nTOKEN = \"{token}\"\n"),
    )
    .unwrap();
    let c = start(&env, Some("review")).await;

    let e = err(&call(&c, "run_python", json!({"script": "scripts/load.py"})).await);
    assert!(
        e.contains("scripts/load.py is not run")
            && e.contains("line 3 (a password or key written into the file")
            && e.contains("take the secret out of the file"),
        "{e}"
    );
    assert!(!e.contains(PASSWORD) && !e.contains("ran with"), "{e}");

    let ran = call(&c, "run_python", json!({"script": "jobs/count.py"})).await;
    if !can_confine() {
        assert!(err(&ran).contains("confined"));
    } else {
        let ran = ok(&ran);
        assert_eq!(ran["result"]["status"], "done", "{ran}");
        let out = ran["result"]["stdout"].as_str().unwrap();
        assert!(
            out.contains("no keys module here") && out.contains("helper says 2 parcels"),
            "{ran}"
        );
        let skipped = ran["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["id"] == "script.helper_skipped")
            .unwrap_or_else(|| panic!("the skipped helper is named: {ran}"));
        let said = skipped["message"].as_str().unwrap();
        assert!(
            said.contains("keys.py") && said.contains("line 2 (a GitHub token)"),
            "{said}"
        );
        assert!(!ran.to_string().contains(&token), "{ran}");
    }
    c.cancel().await.unwrap();
    let audit = env.audit();
    assert!(!audit.contains(PASSWORD) && !audit.contains(&token));
    assert!(
        audit.lines().any(|l| l.contains("\"tool\":\"run_python\"")
            && l.contains("\"decision\":\"denied\"")
            && l.contains("scripts/load.py is not run")
            && l.contains("line 3")),
        "{audit}"
    );
}

/// `columbus-project.yaml` at the top of the folder holds the project's
/// shared settings (its name and its instructions for the assistant), and
/// travels with the files when the project syncs (F-17). Instructions steer
/// the assistant, so the assistant may read the file and never writes or
/// removes it: in any letter case, with the file there or not.
#[tokio::test]
async fn the_assistant_never_writes_or_removes_the_shared_settings_file() {
    let env = setup(None, "allow_project_files: true\n");
    let files_dir = env.files("review");
    let c = start(&env, Some("review")).await;
    let names = [
        "columbus-project.yaml",
        "Columbus-Project.yaml",
        "COLUMBUS-PROJECT.YAML",
    ];
    let planted = "name: Mine\ninstructions: planted-instructions-marker\n";
    let mut refused = 0;

    // Not there (a project that is not linked): the assistant cannot make it.
    for name in names {
        let e = err(&files(
            &c,
            json!({"action": "write", "path": name, "text": planted}),
        )
        .await);
        assert!(
            e.contains("shared settings") && e.contains("may read it, not change it"),
            "{name}: {e}"
        );
        let e = err(&files(
            &c,
            json!({"action": "delete", "path": name, "sha256": sha256(planted)}),
        )
        .await);
        assert!(
            e.contains("shared settings") && e.contains("may read it, not remove it"),
            "{name}: {e}"
        );
        refused += 2;
    }
    assert!(!files_dir.exists(), "a refused write makes no folder");

    // There, as a sync leaves it: read, and neither replaced nor removed,
    // with or without the hash a read returned.
    let theirs = "name: Parcel review\ninstructions: Report areas in acres.\n";
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("columbus-project.yaml"), theirs).unwrap();
    let r = ok(&files(
        &c,
        json!({"action": "read", "path": "columbus-project.yaml"}),
    )
    .await);
    assert_eq!(r["result"]["text"], theirs);
    let seen = r["result"]["sha256"].as_str().unwrap().to_string();
    assert_eq!(seen, sha256(theirs));
    for name in names {
        for args in [
            json!({"action": "write", "path": name, "text": planted, "sha256": seen}),
            json!({"action": "write", "path": name, "text": planted}),
        ] {
            let e = err(&files(&c, args.clone()).await);
            assert!(
                e.contains("shared settings") && e.contains("may read it, not change it"),
                "{args}: {e}"
            );
            refused += 1;
        }
        for args in [
            json!({"action": "delete", "path": name, "sha256": seen}),
            json!({"action": "delete", "path": name}),
        ] {
            let e = err(&files(&c, args.clone()).await);
            assert!(
                e.contains("shared settings") && e.contains("may read it, not remove it"),
                "{args}: {e}"
            );
            refused += 1;
        }
    }
    // The file is as it was, and nothing was put beside it.
    assert_eq!(
        snapshot(&files_dir),
        [(
            files_dir.join("columbus-project.yaml"),
            theirs.as_bytes().to_vec()
        )]
    );

    c.cancel().await.unwrap();
    // Every refusal is in the audit log with its reason, never the text.
    let audit = env.audit();
    let denied = audit
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| {
            l["tool"] == "project_files"
                && l["decision"] == "denied"
                && l["dataset"]
                    .as_str()
                    .is_some_and(|d| d.eq_ignore_ascii_case("project:review/columbus-project.yaml"))
                && l["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("shared settings"))
        })
        .count();
    assert_eq!(denied, refused, "{audit}");
    assert!(!audit.contains("planted-instructions-marker"), "{audit}");
}

#[tokio::test]
async fn with_no_project_or_with_the_policy_off_there_are_no_project_files() {
    // A chat that is not in a project: told to start one in a project.
    let env = setup(
        None,
        "allow_scripts: true\nscripts: { approval: none }\nallow_project_files: true\n",
    );
    let c = start(&env, None).await;
    for args in [
        json!({"action": "list"}),
        json!({"action": "read", "path": "scripts/widths.py"}),
        json!({"action": "write", "path": "scripts/widths.py", "text": "print(1)\n"}),
        json!({"action": "delete", "path": "scripts/widths.py", "sha256": sha256("x")}),
    ] {
        let e = err(&files(&c, args).await);
        assert!(e.contains("start the chat in a project"), "{e}");
    }
    let e = err(&call(&c, "run_python", json!({"script": "scripts/widths.py"})).await);
    assert!(e.contains("start the chat in a project"), "{e}");
    c.cancel().await.unwrap();
    assert!(!env.files("review").exists() && !env.files("both").exists());

    // The policy off: not offered, in a project or not.
    env.policy("allow_scripts: true\nscripts: { approval: none }\n");
    for project in [Some("review"), None] {
        let c = start(&env, project).await;
        let w = ok(&call(&c, "whoami", json!({})).await);
        assert_eq!(w["result"]["policy"]["project_files_allowed"], false);
        for query in ["project files", "write a script file", ""] {
            let found = ok(&call(&c, "search_tools", json!({"query": query})).await);
            assert!(
                !found["result"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| t["name"] == "project_files"),
                "{query:?}: {found}"
            );
        }
        let e = err(&call(&c, "describe_tool", json!({"name": "project_files"})).await);
        assert!(e.contains("allow_project_files is off"), "{e}");
        for args in [
            json!({"action": "list"}),
            json!({"action": "read", "path": "a.py"}),
            json!({"action": "write", "path": "a.py", "text": "print(1)\n"}),
            json!({"action": "delete", "path": "a.py", "sha256": sha256("x")}),
        ] {
            let e = err(&files(&c, args).await);
            assert!(e.contains("allow_project_files is off"), "{e}");
        }
        let e = err(&call(&c, "run_python", json!({"script": "a.py"})).await);
        assert!(e.contains("allow_project_files is off"), "{e}");
        c.cancel().await.unwrap();
    }
    assert!(!env.files("review").exists());
}

#[tokio::test]
async fn a_files_text_keeps_its_data_labels_and_the_flow_rules_apply() {
    let Some(py) = engine() else { return };
    let env = setup(
        Some(&py),
        "allow_project_files: true\nflows:\n  - { from: [\"connection:payroll\"], to: [\"project:*\"], action: deny }\n",
    );
    let labels_file = env.project("both").join("files-labels.json");
    let c = start(&env, Some("both")).await;
    // Before anything governed is read, a file carries nothing.
    written(&c, "plan.md", "# Plan\n").await;
    assert!(!labels_file.exists());
    // After reading the county's parcels, what is written may quote them.
    ok(&call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "parcels", "fields": ["parcel_id"]}),
    )
    .await);
    let note = written(&c, "notes/parcels.md", "P1 and P2 are the parcels.\n").await;
    assert_eq!(note["checks"][0]["id"], "flow.labels", "{note}");
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(&labels_file).unwrap()).unwrap();
    assert_eq!(record, json!({"notes/parcels.md": ["connection:county"]}));
    // The record is beside the folder, never in it (it must not sync).
    assert!(!env.files("both").join("files-labels.json").exists());
    // Once payroll is read, the organization's rule forbids the flow.
    ok(&call(
        &c,
        "query_features",
        json!({"connection": "payroll", "dataset": "parcels", "fields": ["parcel_id"]}),
    )
    .await);
    let e = err(&files(
        &c,
        json!({"action": "write", "path": "notes/pay.md", "text": "salaries\n"}),
    )
    .await);
    assert!(
        e.contains("organization flow rule 1 forbids") && e.contains("connection:payroll"),
        "{e}"
    );
    assert!(!env.files("both").join("notes").join("pay.md").exists());
    c.cancel().await.unwrap();

    // A new chat in the project has read nothing, until it reads the note.
    let c = start(&env, Some("both")).await;
    let labels = |w: &Value| w["result"]["labels"].clone();
    assert_eq!(labels(&ok(&call(&c, "whoami", json!({})).await)), json!([]));
    ok(&files(&c, json!({"action": "read", "path": "plan.md"})).await);
    assert_eq!(labels(&ok(&call(&c, "whoami", json!({})).await)), json!([]));
    ok(&files(&c, json!({"action": "read", "path": "notes/parcels.md"})).await);
    assert_eq!(
        labels(&ok(&call(&c, "whoami", json!({})).await)),
        json!(["connection:county"])
    );
    // Removing the file removes its entry.
    let sha = sha256("P1 and P2 are the parcels.\n");
    ok(&files(
        &c,
        json!({"action": "delete", "path": "notes/parcels.md", "sha256": sha}),
    )
    .await);
    assert!(!labels_file.exists());
    c.cancel().await.unwrap();

    // A rule that asks: the person is asked, and nothing is written first.
    env.policy(
        "allow_project_files: true\nflows:\n  - { from: [\"connection:county\"], to: [\"project:both\"], action: approve }\n",
    );
    let c = start(&env, Some("both")).await;
    ok(&call(
        &c,
        "query_features",
        json!({"connection": "county", "dataset": "parcels", "fields": ["parcel_id"]}),
    )
    .await);
    let r = ok(&files(
        &c,
        json!({"action": "write", "path": "notes/asked.md", "text": "P1\n"}),
    )
    .await);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    assert!(
        r["result"]["approval"]["summary"]
            .as_str()
            .unwrap()
            .contains("an organization flow rule asks for it"),
        "{r}"
    );
    assert!(!env.files("both").join("notes").join("asked.md").exists());
    assert!(
        !env.files("both").join("notes").exists(),
        "a write that waits makes no folder"
    );
    c.cancel().await.unwrap();
}

#[tokio::test]
async fn where_scripts_are_approved_the_card_names_the_file_and_its_helpers() {
    let Some(py) = engine() else { return };
    if !can_confine() {
        return;
    }
    let env = setup(
        Some(&py),
        "allow_scripts: true\nallow_project_files: true\n",
    );
    let c = start(&env, Some("review")).await;
    written(&c, "scripts/widths.py", SCRIPT).await;
    written(&c, "scripts/geom.py", HELPER).await;
    // Files beside it that cannot be helpers are named, not taken.
    std::fs::write(
        env.files("review").join("scripts").join("my notes.py"),
        "x = 1\n",
    )
    .unwrap();
    let r = ok(&call(&c, "run_python", json!({"script": "scripts/widths.py"})).await);
    assert_eq!(r["result"]["status"], "approval_required", "{r}");
    let summary = r["result"]["approval"]["summary"].as_str().unwrap();
    assert!(
        summary.contains("from the project's file scripts/widths.py, with 1 helper file from its folder (geom.py)"),
        "{summary}"
    );
    assert_eq!(r["result"]["code_sha256"], sha256(SCRIPT));
    c.cancel().await.unwrap();
}
