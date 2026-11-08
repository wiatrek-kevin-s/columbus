//! Documents the person imports as notes (`columbus-mcp notes import`):
//! previewed without writing, added as a draft entry with the original kept
//! beside it, found by the context tools as the person's own note, and
//! refused (nothing kept) when it wouldn't read back as a valid entry.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::{json, Value};

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            StdCommand::new(p)
                .args(["-c", "import sys; sys.exit(0)"])
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn worker() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../workers/python/columbus_worker.py")
        .canonicalize()
        .unwrap()
}

fn envs(cmd: &mut std::process::Command, root: &Path, py: &str) {
    cmd.env("COLUMBUS_POLICY", root.join("policy.yaml"))
        .env("COLUMBUS_CONNECTIONS", root.join("connections"))
        .env("COLUMBUS_WORKSPACES", root.join("workspaces"))
        .env("COLUMBUS_AUDIT_DIR", root.join("audit"))
        .env("COLUMBUS_LIVE_DIR", root.join("live"))
        .env("COLUMBUS_NOTES", root.join("notes"))
        .env("COLUMBUS_CONTEXT", root.join("no-pack"))
        .env("COLUMBUS_WORKER", worker())
        .env("COLUMBUS_PYTHON", py)
        .env("COLUMBUS_GIS_PRELOAD", "0")
        .env("COLUMBUS_LOG", "warn")
        .env("COLUMBUS_COMPANION", "off")
        .env("COLUMBUS_APPROVALS_BROWSER", "none");
}

fn import(root: &Path, py: &str, req: Value) -> Value {
    let mut cmd = StdCommand::new(env!("CARGO_BIN_EXE_columbus-mcp"));
    cmd.args(["notes", "import", &req.to_string()]);
    envs(&mut cmd, root, py);
    let out = cmd.output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    serde_json::from_str(text.lines().last().unwrap_or(""))
        .unwrap_or_else(|_| panic!("not JSON: {text} {}", String::from_utf8_lossy(&out.stderr)))
}

fn audit(root: &Path) -> String {
    std::fs::read_dir(root.join("audit"))
        .map(|d| {
            d.flatten()
                .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_document_becomes_a_draft_note() {
    let Some(py) = python() else {
        eprintln!("skipped: no Python");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("connections")).unwrap();
    std::fs::write(root.join("policy.yaml"), "connection_types: [gpkg]\n").unwrap();
    let doc = root.join("LRS QC procedure v4.md");
    std::fs::write(
        &doc,
        "# Monthly LRS check\n\n1. Take active segments only (STATUS = 'A').\n2. Flag gaps longer than 0.005 mile.\n",
    )
    .unwrap();

    // A preview writes nothing.
    let p = import(&root, &py, json!({"file": doc, "preview": true}));
    assert_eq!(p["ok"], true, "{p}");
    assert_eq!(p["title"], "Monthly LRS check");
    assert!(p["markdown"].as_str().unwrap().contains("0.005 mile"));
    assert!(!root.join("notes/imported").exists());
    let sha = p["sha256"].as_str().unwrap().to_string();

    // Refused, nothing kept: an SOP needs when to use it; a summary is one
    // or two sentences; the file must be the one previewed.
    let base = json!({"file": doc, "sha256": sha, "kind": "sop", "title": "Monthly LRS check",
        "summary": "Check the events against the centerlines.", "when_to_use": "The first week of each month."});
    let mut no_when = base.clone();
    no_when.as_object_mut().unwrap().remove("when_to_use");
    assert_eq!(import(&root, &py, no_when)["ok"], false);
    let mut long = base.clone();
    long["summary"] = json!("One. Two. Three.");
    let r = import(&root, &py, long);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r["error"].as_str().unwrap().contains("summary"), "{r}");
    let mut changed = base.clone();
    changed["sha256"] = json!("00");
    assert_eq!(import(&root, &py, changed)["ok"], false);
    let mut hidden = base.clone();
    hidden["title"] = json!("Monthly \u{202e}check");
    assert_eq!(import(&root, &py, hidden)["ok"], false);
    let imported = root.join("notes/imported");
    assert!(
        !imported.exists() || std::fs::read_dir(&imported).unwrap().next().is_none(),
        "nothing kept when refused"
    );

    // Added: the entry and the original beside it.
    let r = import(&root, &py, base.clone());
    assert_eq!(r["ok"], true, "{r}");
    let id = r["id"].as_str().unwrap().to_string();
    assert_eq!(id, "doc-monthly-lrs-check");
    let note =
        std::fs::read_to_string(root.join("notes").join(r["path"].as_str().unwrap())).unwrap();
    assert!(
        note.contains("status: draft") && note.contains("kind: sop"),
        "{note}"
    );
    assert!(
        note.contains("Imported from LRS QC procedure v4.md"),
        "{note}"
    );
    let kept = std::fs::read(root.join("notes").join(r["source"].as_str().unwrap())).unwrap();
    assert_eq!(kept, std::fs::read(&doc).unwrap());
    // A second import of the same title gets its own id.
    let r2 = import(&root, &py, base.clone());
    assert_eq!(r2["id"], "doc-monthly-lrs-check-2", "{r2}");
    let a = audit(&root);
    assert!(
        a.contains("import_document") && a.contains("denied") && a.contains("allowed"),
        "{a}"
    );

    // The assistant finds it as the person's own note.
    let r3 = root.clone();
    let py2 = py.clone();
    let cmd =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_columbus-mcp")).configure(move |c| {
            let mut s = std::process::Command::new("x");
            envs(&mut s, &r3, &py2);
            for (k, v) in s.get_envs() {
                if let Some(v) = v {
                    c.env(k, v);
                }
            }
        });
    let c = ().serve(TokioChildProcess::new(cmd).unwrap()).await.unwrap();
    let mut p = CallToolRequestParams::new("context_search");
    p = p.with_arguments(json!({"query": "monthly lrs"}).as_object().unwrap().clone());
    let s = c.call_tool(p).await.unwrap();
    let text = format!("{:?}", s.structured_content);
    assert!(text.contains("doc-monthly-lrs-check"), "{text}");
    c.cancel().await.unwrap();

    // Organization policy can turn it off.
    std::fs::write(
        root.join("policy.yaml"),
        "connection_types: [gpkg]\ndeny_tools: [import_document]\n",
    )
    .unwrap();
    let r = import(&root, &py, json!({"file": doc, "preview": true}));
    assert_eq!(r["ok"], false, "{r}");
}
