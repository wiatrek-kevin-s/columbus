//! Suggested changes to the person's notes: what an assistant thinks a note
//! should say, kept for the person to accept or dismiss whenever they get to
//! it (Data › Notes › Proposals), without holding the assistant up.
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
            "{MAX_WAITING} suggestions are already waiting for the person; ask them to look at Data › Notes › Proposals first"
        ));
    }
    let text = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    crate::notes::write_bytes(
        root,
        &Path::new(FOLDER).join(format!("{}.json", s.id)),
        &text,
    )
}

/// Remove a waiting suggestion's file.
pub fn forget(root: &Path, id: &str) -> Result<(), String> {
    if !id_ok(id) {
        return Err("no such suggestion".into());
    }
    let f = folder(root).join(format!("{id}.json"));
    match std::fs::symlink_metadata(&f) {
        Ok(m) if m.is_file() => std::fs::remove_file(&f).map_err(|e| e.to_string()),
        _ => Err("no such suggestion (it may have been accepted or dismissed already)".into()),
    }
}

/// The hash of what a suggestion says (its changes, reason, and who made
/// it in which session), as its audit record holds it.
pub fn content_hash(s: &Suggestion) -> String {
    let v = json!({"id": s.id, "changes": s.changes, "why": s.why, "client": s.client, "session": s.session});
    columbus_shared::canon::sha256_hex(v.to_string().as_bytes())
}

/// One line of plain text, at most `max` characters (for what the file
/// says of itself, shown to the person).
fn plain(t: &str, max: usize) -> String {
    t.chars()
        .filter(|&c| !columbus_shared::canon::hidden_char(c))
        .take(max)
        .collect()
}

/// What the audit log holds about suggestions, read in one pass: for each
/// suggestion made, the hash of what it says and the data (labels) the
/// suggesting session had read; and the suggestions already accepted or
/// dismissed. Accepting trusts this, not the files in `proposals/` (which
/// anything able to write the notes folder could change or put back).
#[derive(Default)]
struct Seals {
    made: std::collections::HashMap<String, (String, Vec<String>)>,
    closed: BTreeSet<String>,
}

fn seals(state: &State, since: &str) -> Seals {
    use std::io::BufRead;
    let mut out = Seals::default();
    let Ok(from) = chrono::DateTime::parse_from_rfc3339(since) else {
        return out;
    };
    let mut month = from.with_timezone(&chrono::Utc).format("%Y-%m").to_string();
    let now = chrono::Utc::now().format("%Y-%m").to_string();
    for _ in 0..24 {
        let f = state.audit.dir().join(format!("{month}.jsonl"));
        if let Ok(file) = std::fs::File::open(&f) {
            for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
                if !line.contains("suggestion ") {
                    continue;
                }
                let Ok(r) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if r["decision"] != "allowed" {
                    continue;
                }
                let reason = r["reason"].as_str().unwrap_or("");
                let word = |prefix: &str| {
                    reason
                        .strip_prefix(prefix)
                        .and_then(|rest| rest.split([' ', ':']).next())
                        .map(String::from)
                };
                match r["tool"].as_str() {
                    Some("suggest_note_changes") => {
                        let Some(rest) = reason.strip_prefix("suggestion ") else {
                            continue;
                        };
                        let mut parts = rest.splitn(3, ' ');
                        let (Some(id), Some("sha256"), Some(tail)) =
                            (parts.next(), parts.next(), parts.next())
                        else {
                            continue;
                        };
                        let hash: String = tail.chars().take(64).collect();
                        let labels = r["labels"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|l| l.as_str().map(String::from))
                            .collect();
                        out.made.insert(id.to_string(), (hash, labels));
                    }
                    Some("accept_note_suggestion") => {
                        if let Some(id) = word("accepted suggestion ") {
                            out.closed.insert(id);
                        }
                    }
                    Some("dismiss_note_suggestion") => {
                        if let Some(id) = word("dismissed suggestion ") {
                            out.closed.insert(id);
                        }
                    }
                    _ => {}
                }
            }
        }
        if month >= now {
            break;
        }
        let Ok(d) = chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d") else {
            break;
        };
        month = (d + chrono::Months::new(1)).format("%Y-%m").to_string();
    }
    out
}

/// What accepting would write now: each note's change planned against the
/// note as it is now and the rules in force now, carrying the data the
/// suggesting session had read. The planned changes and the organization
/// flow rules that ask for approval (accepting is that approval).
fn plan_now(
    gw: &Gateway,
    state: &State,
    root: &Path,
    s: &Suggestion,
    seals: &Seals,
) -> Result<(Vec<Planned>, BTreeSet<usize>), String> {
    let mut ids = BTreeSet::new();
    if s.changes.is_empty() || s.changes.iter().any(|c| !ids.insert(c.id.clone())) {
        return Err(
            "this suggestion is not as Columbus kept it (a note appears twice or none does)".into(),
        );
    }
    if seals.closed.contains(&s.id) {
        return Err("this suggestion was accepted or dismissed already; dismiss this copy".into());
    }
    let (hash, labels) = seals.made.get(&s.id).cloned().ok_or(
        "Columbus has no record of making this suggestion, so it can't be accepted; dismiss it",
    )?;
    if hash != content_hash(s) {
        return Err("this suggestion changed after the assistant made it, so it can't be accepted; dismiss it".into());
    }
    let carried: BTreeSet<String> = labels
        .into_iter()
        .filter(|l| l.starts_with("connection:") || l.starts_with("server:"))
        .collect();
    let pack = gw.notes_pack().ok_or("there are no notes")?;
    let policy = state.policy();
    let editor = format!(
        "the person, accepting {}'s suggestion",
        plain(&s.client, 80)
    );
    let mut planned = Vec::new();
    let mut asks = BTreeSet::new();
    for c in &s.changes {
        let (p, a) = gw.plan_change(&pack, root, c, &carried, &policy.flows, &editor, true)?;
        asks.extend(a);
        planned.push(p);
    }
    Ok((planned, asks))
}

/// The hash of exactly what the person is shown for a suggestion: each
/// note's file as it is now, what changes, and the data it will carry.
fn shown_hash(id: &str, planned: &[Planned]) -> String {
    let v: Vec<Value> = planned
        .iter()
        .map(|p| json!({"id": p.id, "file": p.file_sha, "lines": p.lines, "carries": p.carries}))
        .collect();
    columbus_shared::canon::sha256_hex(json!({"suggestion": id, "notes": v}).to_string().as_bytes())
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
    /// For `accept`: the `shown` hash the list gave, so what is written is
    /// what the person saw.
    #[serde(default)]
    shown: Option<String>,
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

fn record(
    state: &State,
    tool: &str,
    decision: &str,
    reason: String,
    request: Option<String>,
    labels: Vec<String>,
) {
    let _ = state.audit.append(&AuditRecord {
        user: state.user.clone(),
        client: CLIENT.into(),
        session: Some(state.session.clone()),
        tool: tool.into(),
        decision: decision.into(),
        reason: Some(reason),
        request,
        labels,
        ..Default::default()
    });
}

async fn run(state: std::sync::Arc<State>, request: &str) -> Result<Value, String> {
    if request.len() > 4096 {
        return Err("the request is too long".into());
    }
    let req: Request = serde_json::from_str(request).map_err(|e| format!("not a request: {e}"))?;
    if req.accept.is_some() && req.dismiss.is_some() {
        return Err("accept or dismiss, not both".into());
    }
    let root = crate::notes::notes_root(&state.paths)?;
    let gw = Gateway::with_state(state.clone());
    gw.reload_settled().await;
    if let Some(why) = state.problem() {
        return Err(why);
    }
    if let Some(id) = req.dismiss {
        let r = forget(&root, &id);
        match &r {
            Ok(()) => record(
                &state,
                "dismiss_note_suggestion",
                "allowed",
                format!("dismissed suggestion {id}"),
                None,
                vec![],
            ),
            Err(e) => record(
                &state,
                "dismiss_note_suggestion",
                "denied",
                format!("{id}: {e}"),
                None,
                vec![],
            ),
        }
        r?;
        return Ok(json!({"ok": true, "dismissed": id}));
    }
    if let Some(id) = req.accept {
        return match accept(&gw, &state, &root, &id, req.shown.as_deref().unwrap_or("")) {
            Ok(v) => Ok(v),
            Err(e) => {
                record(
                    &state,
                    "accept_note_suggestion",
                    "denied",
                    format!("{id}: {e}"),
                    req.shown.clone(),
                    vec![],
                );
                Err(e)
            }
        };
    }
    // The list: each suggestion as accepting it would write it now.
    let all = waiting(&root);
    let known = seals(
        &state,
        &all.first().map(|s| s.created.clone()).unwrap_or_default(),
    );
    let list: Vec<Value> = all
        .into_iter()
        .map(|s| {
            let head = json!({"id": s.id, "created": plain(&s.created, 40), "client": plain(&s.client, 80),
                "why": plain(&s.why.replace('\n', " "), MAX_WHY)});
            let mut v = head;
            match plan_now(&gw, &state, &root, &s, &known) {
                Ok((planned, asks)) => {
                    let pack = gw.notes_pack();
                    v["shown"] = json!(shown_hash(&s.id, &planned));
                    v["asks"] = json!(asks.iter().map(|r| r + 1).collect::<Vec<_>>());
                    v["notes"] = json!(planned
                        .iter()
                        .map(|p| {
                            let e = pack.as_ref().and_then(|k| k.get(&p.id));
                            json!({
                                "id": p.id,
                                "title": plain(&e.and_then(|e| e.str_field("title")).unwrap_or_else(|| p.id.clone()), 200),
                                "path": p.rel.display().to_string(),
                                "status": e.and_then(|e| e.str_field("status")),
                                "changed_since": s.files.get(&p.id) != Some(&p.file_sha),
                                "lines": p.lines,
                                "carries": p.carries,
                            })
                        })
                        .collect::<Vec<_>>());
                }
                Err(e) => {
                    v["problem"] = json!(e);
                    v["notes"] = json!(s
                        .changes
                        .iter()
                        .map(|c| json!({"id": plain(&c.id, 120), "title": plain(&c.id, 120), "lines": []}))
                        .collect::<Vec<_>>());
                }
            }
            v
        })
        .collect();
    Ok(json!({"ok": true, "suggestions": list}))
}

/// Make a suggestion's changes, as the person: planned again now, and
/// written only if that is exactly what they were shown; all or nothing.
fn accept(
    gw: &Gateway,
    state: &State,
    root: &Path,
    id: &str,
    shown: &str,
) -> Result<Value, String> {
    {
        let policy = state.policy();
        if !policy.tool_allowed("suggest_note_changes") || !policy.tool_allowed("update_notes") {
            return Err("organization policy does not allow changes to notes (update_notes or suggest_note_changes)".into());
        }
    }
    let s = waiting(root)
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("no such suggestion (it may have been accepted or dismissed already)")?;
    let known = seals(state, &s.created);
    let (planned, asks) = plan_now(gw, state, root, &s, &known)?;
    let hash = shown_hash(id, &planned);
    if !columbus_shared::proof::same(&hash, shown) {
        return Err(
            "the note or the rules changed since you looked; look at it again before accepting"
                .into(),
        );
    }
    // Taken off the list first: a suggestion is accepted at most once.
    forget(root, id)?;
    let before: Vec<String> = planned
        .iter()
        .map(|p| format!("{} {}", p.id, p.file_sha))
        .collect();
    let ids = match write_all(root, &planned) {
        Ok(ids) => ids,
        Err(e) => {
            let _ = keep(root, &s);
            return Err(e);
        }
    };
    let labels: Vec<String> = planned
        .iter()
        .flat_map(|p| p.carries.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    record(
        state,
        "accept_note_suggestion",
        "allowed",
        format!(
            "accepted suggestion {id} (sha256 {}) from {} in session {}: {} note(s) changed: {}; before: {}{}",
            content_hash(&s),
            plain(&s.client, 80),
            plain(&s.session, 80),
            ids.len(),
            ids.join(", "),
            before.join(", "),
            if asks.is_empty() {
                String::new()
            } else {
                format!("; organization flow rule(s) {} asked for approval: this is it", asks.iter().map(|r| (r + 1).to_string()).collect::<Vec<_>>().join(", "))
            }
        ),
        Some(hash),
        labels,
    );
    Ok(json!({"ok": true, "accepted": id, "notes": ids}))
}
