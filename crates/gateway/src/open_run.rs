//! A workflow run's results, opened in a chat (AF-19, REQ-27, R-20).
//!
//! The runner records each layer a run made in `run.json` (`outputs`): its
//! file in the run's workspace, its SHA-256, its provenance record (and that
//! record's SHA-256), its rows and its data labels. **Open in a chat** in the
//! app, or `columbus-mcp workflow open <id> <run>`, starts a session with
//! `COLUMBUS_OPEN_RUN=<id>/<run>`: before it serves anything, it checks every
//! output is byte-for-byte what the run wrote (or refuses, naming the file)
//! and copies them into its own workspace, labelled and fingerprinted as the
//! run left them, with the run's provenance. The session then works under
//! the workflow's scope, and its copies carry the run's labels, so flow
//! rules apply to any export as they did in the run.
//!
//! The run is never changed: its folder, its workspace and its exports are
//! only read (and its exports can't be overwritten from the chat); filters,
//! edits and exports in the chat make new outputs.

use std::path::PathBuf;

use chrono::Local;
use columbus_shared::workflows as wf;
use serde_json::{json, Value};

use crate::providers::{InheritedFail, OutputMeta, OutputSource};
use crate::server::State;

/// The most of a run's report given to the assistant, in characters.
const MAX_REPORT: usize = 20_000;

/// Runner side: the layers the run made (its steps' results), each with
/// the fingerprint the session recorded when it wrote it. A file that
/// changed since it was written is left out (the run's own report says
/// what it made).
pub(crate) fn record_outputs(state: &State, record: &mut Value) {
    let Some(results) = record["results"].as_object().cloned() else {
        record["outputs"] = json!([]);
        return;
    };
    let root = state.sandbox.root();
    let mut out = Vec::new();
    for (step, handle) in &results {
        let Some(h) = handle.as_str() else { continue };
        let Ok(path) = crate::providers::resolve_handle(&state.session, &state.sandbox, h) else {
            continue;
        };
        let Some(meta) = state.outputs.get(&path) else {
            continue;
        };
        if !state.outputs.unchanged(&path) {
            continue;
        }
        let Some(sha) = meta.sha256.clone() else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let file: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let prov_sha = meta.provenance.as_deref().and_then(|id| {
            columbus_shared::canon::sha256_file(
                &root.join("provenance").join(format!("{id}.prov.json")),
            )
            .ok()
            .map(|(h, _)| h)
        });
        out.push(json!({
            "step": step,
            "handle": h,
            "file": file.join("/"),
            "sha256": sha,
            "rows": meta.rows,
            "made_by": meta.made_by,
            "source": serde_json::to_value(meta.source).unwrap_or(Value::Null),
            "truncated": meta.truncated,
            "inherited": meta.inherited,
            "provenance": if prov_sha.is_some() { json!(meta.provenance) } else { Value::Null },
            "provenance_sha256": prov_sha,
            "labels": state.flow.file_labels(&path).into_iter().collect::<Vec<_>>(),
        }));
    }
    record["outputs"] = json!(out);
}

/// What an opened run protects in this session: its exports, by folder and
/// name, which the chat may never replace.
#[derive(Debug, Clone, Default)]
pub struct Opened {
    /// For whoami and the app: the run, its layers here, and its report.
    pub info: Value,
    pub exports: Vec<(String, String)>,
}

impl Opened {
    /// Refusal for an export that would replace one of the run's.
    pub fn protects(&self, folder: &str, name: &str) -> Option<String> {
        self.exports
            .iter()
            .find(|(f, n)| f == folder && n.eq_ignore_ascii_case(name))
            .map(|(f, n)| {
                format!(
                    "{n} in {f} is an export of workflow run {} of {}; an opened run's files are never changed: choose another name",
                    self.info["run"].as_str().unwrap_or(""),
                    self.info["workflow"].as_str().unwrap_or("")
                )
            })
    }
}

fn source_of(v: &Value) -> OutputSource {
    match v.as_str() {
        Some("engine") => OutputSource::Engine,
        Some("plugin") => OutputSource::Plugin,
        Some("script") => OutputSource::Script,
        _ => OutputSource::Provider,
    }
}

/// `COLUMBUS_OPEN_RUN` (`<id>/<run>`), or `workflow open <id> <run>`: copy
/// the run's outputs into this session, under the workflow's scope. Any
/// failure stops the session (it never starts wider, or with other data,
/// than the run).
pub(crate) fn open(state: &mut State, id: &str, run: &str) -> Result<(), String> {
    let root = wf::workflows_dir();
    let o = wf::open_check(&root, Some(&state.paths.workspaces), id, run)?;
    // The workflow's scope, narrowed further by a project's, if any.
    let mut scope: Vec<String> = if state.scope.connections.is_empty() {
        o.scope.clone()
    } else {
        o.scope
            .iter()
            .filter(|c| state.scope.connections.contains(c))
            .cloned()
            .collect()
    };
    if scope.is_empty() {
        scope = vec!["\u{0}none".into()];
    }
    state.scope.connections = scope;

    let results = state
        .sandbox
        .resolve("results")
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&results).map_err(|e| format!("{}: {e}", results.display()))?;
    let provs = state
        .sandbox
        .resolve("provenance")
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&provs).map_err(|e| format!("{}: {e}", provs.display()))?;
    let now = Local::now().to_rfc3339();
    let mut layers = Vec::new();
    let mut all_labels = std::collections::BTreeSet::new();
    for out in &o.outputs {
        let ext = out
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("gpkg")
            .to_ascii_lowercase();
        let dest: PathBuf = results.join(format!("{}-{}.{ext}", o.workflow, out.step));
        let got = crate::providers::copy_hashed(&out.path, &dest)
            .map_err(|e| format!("copying {}: {e}", out.path.display()))?;
        if got != out.sha256 {
            let _ = std::fs::remove_file(&dest);
            return Err(format!(
                "the run's output {} (step {}) changed on disk since the run, so the run can't be opened; run the workflow again",
                out.path.display(),
                out.step
            ));
        }
        let handle = crate::providers::handle_for(&state.session, state.sandbox.root(), &dest)
            .unwrap_or_default();
        let labels: std::collections::BTreeSet<String> = out.entry["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect();
        state.flow.record_file(&dest, &labels);
        for l in &labels {
            state.flow.add(l.clone());
        }
        all_labels.extend(labels.iter().cloned());
        let inherited: Vec<InheritedFail> =
            serde_json::from_value(out.entry["inherited"].clone()).unwrap_or_default();
        let prov = crate::server::new_prov_id();
        let run_prov = out.provenance.as_ref().map(|(_, _, r)| r.clone());
        let record = json!({
            "id": prov,
            "created": now,
            "finished": Local::now().to_rfc3339(),
            "user": state.user,
            "client": "columbus",
            "inputs": [{
                "dataset": format!("workflow {} run {}, step {}", o.workflow, o.run, out.step),
                "workflow": o.workflow, "run": o.run, "step": out.step,
                "file": out.path, "sha256": out.sha256,
                "provenance": out.provenance.as_ref().map(|(id, _, _)| id.clone()),
            }],
            "labels": labels,
            "steps": [{"tool": "open_run", "workflow": o.workflow, "run": o.run, "step": out.step,
                       "note": "a copy of the run's output, byte for byte; the run is not changed"}],
            "checks": run_prov.as_ref().map(|r| r["checks"].clone()).unwrap_or(json!([])),
            "outputs": [handle],
            "sha256": out.sha256,
            "run_provenance": run_prov,
            "server": format!("{} {}", crate::server::SERVER_NAME, env!("CARGO_PKG_VERSION")),
        });
        state
            .provs
            .lock()
            .expect("provs")
            .insert(prov.clone(), record.clone());
        let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
        std::fs::write(provs.join(format!("{prov}.prov.json")), text).map_err(|e| e.to_string())?;
        let rows = out.entry["rows"].as_u64();
        state.outputs.record_with_hash(
            &dest,
            OutputMeta {
                made_by: format!(
                    "workflow {} run {}, step {} ({})",
                    o.workflow,
                    o.run,
                    out.step,
                    out.entry["made_by"].as_str().unwrap_or("")
                ),
                provenance: Some(prov.clone()),
                rows,
                source: source_of(&out.entry["source"]),
                truncated: out.entry["truncated"].as_bool().unwrap_or(false),
                inherited,
                sha256: None,
            },
            got,
        );
        layers.push(json!({
            "step": out.step, "handle": handle, "rows": rows, "sha256": out.sha256,
            "labels": labels, "provenance": prov,
            "made_by": out.entry["made_by"],
        }));
    }
    let report: String = o.report.chars().take(MAX_REPORT).collect();
    let info = json!({
        "workflow": o.workflow,
        "run": o.run,
        "name": o.record["name"],
        "status": o.record["status"],
        "started": o.record["started"],
        "finished": o.record["finished"],
        "connections": o.scope,
        "layers": layers,
        "exports": o.exports.iter().map(|(f, n)| json!({"folder": f, "name": n})).collect::<Vec<_>>(),
        "report": report,
        "note": "These layers are copies of the run's outputs; the run itself (its files, report and exports) is never changed. Filters, edits and exports here make new outputs.",
    });
    let _ = state.audit.append(&columbus_shared::audit::AuditRecord {
        user: state.user.clone(),
        client: "columbus".into(),
        session: Some(state.session.clone()),
        tool: "open_run".into(),
        decision: "allowed".into(),
        reason: Some(format!(
            "opened run {} of workflow {}: {} layer(s) copied, fingerprints checked",
            o.run,
            o.workflow,
            o.outputs.len()
        )),
        labels: all_labels.into_iter().collect(),
        ..Default::default()
    });
    state.opened = Some(Opened {
        info,
        exports: o.exports,
    });
    Ok(())
}

/// `<id>/<run>`.
pub(crate) fn parse(v: &str) -> Result<(&str, &str), String> {
    v.split_once('/')
        .filter(|(i, r)| wf::id_ok(i) && wf::run_id_ok(r))
        .ok_or_else(|| format!("{v:?} is not <workflow id>/<run>"))
}
