//! Suggested changes to the person's notes: what an assistant thinks a note
//! should say, kept for the person to accept or dismiss whenever they get to
//! it (Documents > Proposals), without holding the assistant up.
//!
//! - The assistant suggests with `suggest_note_changes`: the same changes
//!   `update_notes` makes (title, summary, field aliases and notes, code
//!   labels, text added to a note), checked the same way when suggested
//!   (plain text, the note's own fields and codes, the organization's flow
//!   rules for what the session has read), and kept in the notes folder
//!   under `proposals/`, which no context tool reads.
//! - Unlike `update_notes` it can be for a reviewed note: the assistant can
//!   no longer change those, but the person can, by accepting.
//! - `columbus-mcp notes suggestions '<json>'` (the app): `{}` lists them;
//!   `{"accept": id}` makes the change, checked again against the note as
//!   it is now and the rules in force now, all or nothing, and marks what
//!   was written as carrying the data the suggesting session had read;
//!   `{"dismiss": id}` drops it. Each is audited, as the app's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use columbus_shared::audit::AuditRecord;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::notes::{NoteChange, Planned};
use crate::server::{Gateway, State};

pub const CLIENT: &str = "columbus-app-documents";
/// Most suggestions waiting at once.
pub const MAX_WAITING: usize = 200;
/// Longest reason an assistant gives.
pub const MAX_WHY: usize = 600;
const FOLDER: &str = "proposals";

/// One suggestion, as kept on disk.
#[derive(Serialize, Deserialize, Clone)]
pub struct Suggestion {
    pub id: String,
    pub created: String,
    /// The assistant (MCP client) that suggested it, and its session.
    pub client: String,
    pub session: String,
    /// Why, in the assistant's words (where the information came from).
    pub why: String,
    pub changes: Vec<NoteChange>,
    /// Per note: what the change says, as shown to the person.
    pub lines: BTreeMap<String, Vec<String>>,
    /// Per note: the data the suggesting session had read (flow labels),
    /// which the note carries once the change is made.
    pub carries: BTreeMap<String, Vec<String>>,
    /// Per note: the file's SHA-256 when suggested.
    pub files: BTreeMap<String, String>,
}

fn folder(root: &Path) -> PathBuf {
    root.join(FOLDER)
}

fn id_ok(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|u| u.hyphenated().to_string() == id)
}

/// The suggestions waiting, oldest first (unreadable files are skipped).
pub fn waiting(root: &Path) -> Vec<Suggestion> {
    let Ok(rd) = std::fs::read_dir(folder(root)) else {
        return vec![];
    };
    let mut out: Vec<Suggestion> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = name.strip_suffix(".json")?;
            if !id_ok(id) || e.metadata().ok()?.len() > 2 * 1024 * 1024 {
                return None;
            }
            let s: Suggestion = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            (s.id == id).then_some(s)
        })
        .collect();
    out.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
    out
}

/// Keep a new suggestion; its id.
pub fn keep(root: &Path, s: &Suggestion) -> Result<(), String> {
    if waiting(root).len() >= MAX_WAITING {
        return Err(format!(
            "{MAX_WAITING} suggestions are already waiting for the person; ask them to look at Documents > Proposals first"
        ));
    }
    let text = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    crate::notes::write_bytes(
        root,
        &Path::new(FOLDER).join(format!("{}.json", s.id)),
        &text,
    )
}

fn drop_file(root: &Path, id: &str) -> Result<(), String> {
    if !id_ok(id) {
        return Err("no such suggestion".into());
    }
    let f = folder(root).join(format!("{id}.json"));
    match std::fs::symlink_metadata(&f) {
        Ok(m) if m.is_file() => std::fs::remove_file(&f).map_err(|e| e.to_string()),
        _ => Err("no such suggestion (it may have been accepted or dismissed already)".into()),
    }
}

/// Write every planned note, or none: a note already written goes back as
/// it was when a later one can't be written. The ids written.
pub fn write_all(root: &Path, planned: &[Planned]) -> Result<Vec<String>, String> {
    for p in planned {
        if std::fs::read(root.join(&p.rel)).ok().as_deref() != Some(p.before.as_slice()) {
            return Err(format!("{} changed just now; nothing was written", p.id));
        }
    }
    let mut written: Vec<&Planned> = Vec::new();
    for p in planned {
        if let Err(e) = crate::notes::write_note(root, &p.rel, &p.text) {
            let mut kept = Vec::new();
            for w in &written {
                if crate::notes::write_note(root, &w.rel, &String::from_utf8_lossy(&w.before))
                    .is_err()
                {
                    kept.push(w.id.clone());
                }
            }
            return Err(if kept.is_empty() {
                format!("{}: {e}; nothing was changed", p.id)
            } else {
                format!(
                    "{}: {e}; these could not be put back and keep the change: {}",
                    p.id,
                    kept.join(", ")
                )
            });
        }
        written.push(p);
    }
    Ok(written.iter().map(|p| p.id.clone()).collect())
}

// ---- the app's command -------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    accept: Option<String>,
    #[serde(default)]
    dismiss: Option<String>,
}

pub async fn cli(request: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let out = match State::load(crate::config::Paths::from_env()) {
        Ok(state) => {
            let workspace = state.sandbox.root().to_path_buf();
            let out = run(std::sync::Arc::new(state), request).await;
            let _ = std::fs::remove_dir(&workspace);
            out.unwrap_or_else(|e| json!({"ok": false, "error": e}))
        }
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    println!("{out}");
    Ok(())
}

fn record(state: &State, tool: &str, decision: &str, reason: String) {
    let _ = state.audit.append(&AuditRecord {
        user: state.user.clone(),
        client: CLIENT.into(),
        session: Some(state.session.clone()),
        tool: tool.into(),
        decision: decision.into(),
        reason: Some(reason),
        ..Default::default()
    });
}

async fn run(state: std::sync::Arc<State>, request: &str) -> Result<Value, String> {
    if request.len() > 4096 {
        return Err("the request is too long".into());
    }
    let req: Request = serde_json::from_str(request).map_err(|e| format!("not a request: {e}"))?;
    let root = crate::notes::notes_root(&state.paths)?;
    let gw = Gateway::with_state(state.clone());
    gw.reload_settled().await;
    if let Some(id) = req.dismiss {
        let r = drop_file(&root, &id);
        match &r {
            Ok(()) => record(
                &state,
                "dismiss_note_suggestion",
                "allowed",
                format!("dismissed suggestion {id}"),
            ),
            Err(e) => record(
                &state,
                "dismiss_note_suggestion",
                "denied",
                format!("{id}: {e}"),
            ),
        }
        r?;
        return Ok(json!({"ok": true, "dismissed": id}));
    }
    if let Some(id) = req.accept {
        let r = accept(&gw, &state, &root, &id);
        match &r {
            Ok(ids) => record(
                &state,
                "accept_note_suggestion",
                "allowed",
                format!(
                    "accepted suggestion {id}: {} note(s) changed: {}",
                    ids.len(),
                    ids.join(", ")
                ),
            ),
            Err(e) => record(
                &state,
                "accept_note_suggestion",
                "denied",
                format!("{id}: {e}"),
            ),
        }
        let ids = r?;
        return Ok(json!({"ok": true, "accepted": id, "notes": ids}));
    }
    // The list, with each note as it is now.
    let pack = gw.notes_pack();
    let list: Vec<Value> = waiting(&root)
        .into_iter()
        .map(|s| {
            let notes: Vec<Value> = s
                .changes
                .iter()
                .map(|c| {
                    let e = pack.as_ref().and_then(|p| p.get(&c.id));
                    let sha = e.and_then(|e| std::fs::read(&e.file).ok()).map(|b| columbus_shared::canon::sha256_hex(&b));
                    json!({
                        "id": c.id,
                        "title": e.and_then(|e| e.str_field("title")).unwrap_or_else(|| c.id.clone()),
                        "path": e.and_then(|e| e.file.strip_prefix(&root).ok()).map(|p| p.display().to_string()),
                        "status": e.and_then(|e| e.str_field("status")),
                        "gone": e.is_none(),
                        "changed_since": sha.is_some() && sha.as_ref() != s.files.get(&c.id),
                        "lines": s.lines.get(&c.id).cloned().unwrap_or_default(),
                        "carries": s.carries.get(&c.id).cloned().unwrap_or_default(),
                    })
                })
                .collect();
            json!({"id": s.id, "created": s.created, "client": s.client, "why": s.why, "notes": notes})
        })
        .collect();
    Ok(json!({"ok": true, "suggestions": list}))
}

/// Make a suggestion's changes, as the person: checked again now, all or
/// nothing; then the suggestion is gone.
fn accept(gw: &Gateway, state: &State, root: &Path, id: &str) -> Result<Vec<String>, String> {
    let s = waiting(root)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("no such suggestion (it may have been accepted or dismissed already)")?;
    let pack = gw.notes_pack().ok_or("there are no notes")?;
    let policy = state.policy();
    let editor = format!("the person, accepting {}'s suggestion", s.client);
    let mut planned = Vec::new();
    for c in &s.changes {
        let carried: BTreeSet<String> = s
            .carries
            .get(&c.id)
            .into_iter()
            .flatten()
            .filter(|l| l.starts_with("connection:") || l.starts_with("server:"))
            .cloned()
            .collect();
        let (p, _asks) = gw.plan_change(&pack, root, c, &carried, &policy.flows, &editor, true)?;
        planned.push(p);
    }
    drop(policy);
    let ids = write_all(root, &planned)?;
    drop_file(root, id)?;
    Ok(ids)
}
