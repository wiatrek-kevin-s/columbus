//! The main window's read-only views: what is set up, the audit log, the
//! person's notes and the organization's pack, project databases, skills,
//! each session's files and activity, and the requests waiting across
//! sessions (view only), plus the app's own preferences.
//!
//! Nothing here widens what an assistant may do. Reads are confined to the
//! folders Columbus keeps (the audit log, notes, context pack, workspaces,
//! the skills beside the server), by relative path, with no links followed
//! out of them. The one write to the person's files is "Mark reviewed" on
//! their own note, which only narrows what an assistant may change (a
//! reviewed note is the person's alone). Approvals from other AI apps are
//! never decided here: the inbox lists them and asks the session to open
//! its browser page, where the person decides.

use std::path::{Component, Path, PathBuf};

use columbus_shared::paths::{env_path, platform_dirs};
use columbus_shared::policy::OrgPolicy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::settings::{self, LiveSession};

/// Most bytes of one audit month the viewer reads.
const MAX_AUDIT_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes of one note or pack entry shown.
const MAX_TEXT: u64 = 512 * 1024;
/// Most files listed from one folder walk.
const MAX_FILES: usize = 2000;

// ---------- Where things are ----------

/// The folders the sessions on this computer use, from the newest live
/// session that says so, else the defaults the server would use.
#[derive(Serialize, Clone)]
pub struct Places {
    audit: String,
    workspaces: String,
    notes: String,
    context: String,
    /// Whether these came from a running session (else defaults).
    from_session: bool,
}

fn places_from(live: &[LiveSession]) -> Places {
    let (org, _user, local) = platform_dirs();
    let from = live
        .iter()
        .find(|s| s.settings["audit"].is_string())
        .map(|s| &s.settings);
    let pick = |k: &str, default: PathBuf| -> PathBuf {
        from.and_then(|v| v[k].as_str())
            .map(PathBuf::from)
            .unwrap_or(default)
    };
    let workspaces = pick(
        "workspaces",
        env_path("COLUMBUS_WORKSPACES").unwrap_or_else(|| local.join("workspaces")),
    );
    let notes = pick(
        "notes",
        env_path("COLUMBUS_NOTES")
            .unwrap_or_else(|| workspaces.parent().unwrap_or(&workspaces).join("notes")),
    );
    Places {
        audit: pick(
            "audit",
            env_path("COLUMBUS_AUDIT_DIR").unwrap_or_else(|| org.join("audit")),
        )
        .display()
        .to_string(),
        context: pick(
            "context",
            env_path("COLUMBUS_CONTEXT").unwrap_or_else(|| org.join("context")),
        )
        .display()
        .to_string(),
        workspaces: workspaces.display().to_string(),
        notes: notes.display().to_string(),
        from_session: from.is_some(),
    }
}

fn places() -> Places {
    places_from(&settings::live_all())
}

/// The folders sessions (and so assistants) write to: their workspaces and
/// the person's notes, by default and as running sessions say.
pub(crate) fn session_folders() -> Vec<PathBuf> {
    let mut out = vec![];
    for p in [places_from(&[]), places()] {
        out.push(PathBuf::from(p.workspaces));
        out.push(PathBuf::from(p.notes));
    }
    out
}

/// A relative path inside `root`, with no way out of it: plain names only
/// (no `..`, no drive or root), and once resolved, still inside `root`
/// (a link that leads elsewhere is refused).
fn inside(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel_path = Path::new(rel);
    if rel.is_empty()
        || rel.len() > 400
        || !rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err("not a file in this folder".into());
    }
    let base = root
        .canonicalize()
        .map_err(|_| format!("{} is not there", root.display()))?;
    let full = base
        .join(rel_path)
        .canonicalize()
        .map_err(|_| "that file is not there".to_string())?;
    if !full.starts_with(&base) || !full.is_file() {
        return Err("not a file in this folder".into());
    }
    Ok(full)
}

/// The start of a file, for its front matter.
fn read_head(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(16 * 1024)
        .read_to_end(&mut buf)
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// A whole file's bytes, refused if longer than [`MAX_TEXT`]: what is shown
/// (or changed) is never a cut-off part of it.
fn read_whole(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.take(MAX_TEXT + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > MAX_TEXT {
        return Err(format!(
            "this file is larger than {} KB, too large to show here: open it in an editor",
            MAX_TEXT / 1024
        ));
    }
    Ok(buf)
}

fn read_text(path: &Path) -> Result<String, String> {
    String::from_utf8(read_whole(path)?).map_err(|_| "not a text file".to_string())
}

/// Files under `root` (no links followed), relative paths with `/`.
fn walk(
    root: &Path,
    depth: usize,
    keep: &dyn Fn(&Path) -> bool,
) -> Vec<(String, std::fs::Metadata)> {
    let mut out = vec![];
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, d)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(m) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || m.file_type().is_symlink() {
                continue;
            }
            if m.is_dir() {
                if d + 1 < depth {
                    stack.push((p, d + 1));
                }
            } else if m.is_file() && keep(&p) {
                if let Ok(rel) = p.strip_prefix(root) {
                    let rel = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join("/");
                    out.push((rel, m));
                }
                if out.len() >= MAX_FILES {
                    return out;
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn modified(m: &std::fs::Metadata) -> Option<u64> {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// A session id as the server makes them: letters, digits, `-` and `_`.
fn session_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Front matter fields of a Markdown file (`key: value` lines between the
/// first two `---` lines), as plain strings.
fn front_matter(text: &str) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return out;
    }
    for l in lines {
        if l.trim() == "---" {
            break;
        }
        if l.starts_with(' ') || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            // An end-of-line comment is not part of the value.
            let v = match v.find(" #") {
                Some(i) if !v.trim_start().starts_with(['"', '\'']) => &v[..i],
                _ => v,
            };
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !k.trim().is_empty() && !v.is_empty() {
                out.insert(k.trim().to_string(), json!(v));
            }
        }
    }
    out
}

// ---------- Overview ----------

#[derive(Serialize)]
pub struct ShellInfo {
    version: &'static str,
    policy: Value,
    places: Places,
    confirm_with: String,
    /// Workflows are not built yet: no runner is set up.
    runner: &'static str,
    sessions: usize,
}

/// What the rail and Home show: the policy in force and where it comes
/// from, where things are, and how widening changes are confirmed.
#[tauri::command]
pub async fn shell_info() -> Result<ShellInfo, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let live = settings::live_all();
        let path = columbus_shared::paths::policy_file();
        let exists = path.exists();
        let (policy, problem) = if exists {
            match OrgPolicy::load(&path) {
                Ok(p) => (Some(p), None),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };
        let writable = settings::writable(&path);
        let source = if problem.is_some() {
            "unreadable"
        } else if !exists {
            "defaults"
        } else if writable {
            "file"
        } else {
            "organization"
        };
        let text = if exists { read_text(&path).ok() } else { None };
        let effective = policy.clone().unwrap_or_default();
        ShellInfo {
            version: env!("CARGO_PKG_VERSION"),
            policy: json!({
                "path": path.display().to_string(),
                "exists": exists,
                "writable": writable,
                "source": source,
                "problem": problem,
                "text": text,
                "in_force": serde_json::to_value(&effective).unwrap_or(Value::Null),
            }),
            places: places_from(&live),
            confirm_with: settings::confirm_with().to_string(),
            runner: "not set up",
            sessions: live.len(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// What the first-run checklist needs: whether the chat has a key and the
/// server, and whether the GIS engine's files are there. Nothing is run.
#[tauri::command]
pub async fn engine_check(app: AppHandle) -> Result<Value, String> {
    let s = crate::chat::load(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let server = crate::chat::server(&s);
        let engine = crate::chat::engine_python(server.as_deref());
        let worker = server
            .as_ref()
            .and_then(|s| s.parent())
            .map(|d| d.join("workers").join("python").join("columbus_worker.py"))
            .filter(|p| p.is_file());
        json!({
            "server": server.as_ref().map(|p| p.display().to_string()),
            "server_found": server.is_some(),
            "engine": engine.as_ref().map(|p| p.display().to_string()),
            "engine_found": engine.as_ref().is_some_and(|p| p.is_file()),
            "worker_found": worker.is_some(),
        })
    })
    .await
    .map_err(|e| e.to_string())
}

// ---------- Preferences ----------

#[derive(Serialize, Deserialize, Clone)]
pub struct Prefs {
    #[serde(default = "system")]
    theme: String,
    #[serde(default)]
    basemap: bool,
    /// First-run steps the person skipped.
    #[serde(default)]
    skipped: Vec<String>,
    #[serde(default)]
    setup_done: bool,
}

fn system() -> String {
    "system".into()
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme: system(),
            basemap: false,
            skipped: vec![],
            setup_done: false,
        }
    }
}

fn prefs_path(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("preferences.json"))
}

#[tauri::command]
pub async fn prefs_get(app: AppHandle) -> Result<Prefs, String> {
    Ok(prefs_path(&app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default())
}

/// The app's own look and first-run state. None of it reaches a session.
#[tauri::command]
pub async fn prefs_set(app: AppHandle, prefs: Prefs) -> Result<(), String> {
    if !matches!(prefs.theme.as_str(), "system" | "light" | "dark") {
        return Err("theme is system, light or dark".into());
    }
    if prefs.skipped.len() > 20
        || prefs
            .skipped
            .iter()
            .any(|s| s.len() > 40 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return Err("unknown setup steps".into());
    }
    let p = prefs_path(&app).ok_or("no settings folder")?;
    let text = serde_json::to_string_pretty(&prefs).map_err(|e| e.to_string())?;
    settings::write_atomic(&p, &text)
}

// ---------- Sessions: files, activity, the output panel ----------

/// The output panel's page for a running session: a view link the session
/// hands to this app over the signed channel. The main window shows it in
/// a sandboxed frame that may navigate only to that session's page.
#[tauri::command]
pub async fn session_page(app: AppHandle, session: String) -> Result<String, String> {
    if !session_ok(&session) {
        return Err("not a session".into());
    }
    let url = tauri::async_runtime::spawn_blocking(move || {
        let s = settings::live_all()
            .into_iter()
            .find(|s| s.session == session)
            .ok_or("that session is no longer running")?;
        let (code, v) = settings::signed_post_to(&s, "/api/app-view", b"{}")?;
        match code {
            200 => {}
            404 => return Err("The Columbus running in this session is older than this app: open the map in its own window, or update Columbus".into()),
            _ => {
                return Err(format!(
                    "the session did not open its page ({code}: {})",
                    v["error"].as_str().unwrap_or("no reason given")
                ))
            }
        }
        let url = v["url"].as_str().ok_or("the session gave no page")?;
        // Only that session's own page, on this computer.
        let want = format!("http://127.0.0.1:{}/embed#t=", s.port);
        let token = url.strip_prefix(&want).ok_or("the session gave another page")?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the session gave another page".into());
        }
        Ok::<_, String>((url.to_string(), s.port))
    })
    .await
    .map_err(|e| e.to_string())??;
    crate::allow_frame(&app, url.1);
    Ok(url.0)
}

/// The files in a session's workspace: what it made, with sizes.
#[tauri::command]
pub async fn session_files(session: String) -> Result<Value, String> {
    if !session_ok(&session) {
        return Err("not a session".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let p = places();
        let root = PathBuf::from(&p.workspaces).join(&session);
        let files: Vec<Value> = walk(&root, 4, &|_| true)
            .into_iter()
            .filter(|(rel, _)| !rel.ends_with(".prov.json"))
            .map(|(rel, m)| json!({"path": rel, "bytes": m.len(), "modified": modified(&m)}))
            .collect();
        json!({"folder": root.display().to_string(), "exists": root.is_dir(), "files": files})
    })
    .await
    .map_err(|e| e.to_string())
}

/// A provenance record from a session's workspace, as JSON.
#[tauri::command]
pub async fn session_provenance(session: String, id: String) -> Result<Value, String> {
    if !session_ok(&session)
        || id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("not a provenance record".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let root = PathBuf::from(places().workspaces).join(&session);
        let f = inside(&root, &format!("provenance/{id}.prov.json"))?;
        serde_json::from_str::<Value>(&read_text(&f)?).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The sessions running now, with the AI app each serves (from its audit
/// lines) and where its outputs are.
#[tauri::command]
pub async fn sessions_live() -> Result<Vec<Value>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let live = settings::live_all();
        let p = places_from(&live);
        let recent = audit_recent(Path::new(&p.audit), 20_000);
        live.iter()
            .map(|s| {
                let client = recent
                    .iter()
                    .rev()
                    .find(|l| {
                        l["session"].as_str() == Some(s.session.as_str())
                            && !l["client"].as_str().is_some_and(|c| {
                                c.starts_with("companion-window")
                                    || c.starts_with("columbus-app")
                                    || c == "approvals-page"
                            })
                    })
                    .and_then(|l| l["client"].as_str())
                    .map(String::from);
                json!({
                    "session": s.session,
                    "port": s.port,
                    "user": s.user,
                    "started": s.started,
                    "last_used": s.last_used,
                    "client": client,
                    "signed": s.key.is_some(),
                })
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// One session's tool calls, from the audit log, newest last.
#[tauri::command]
pub async fn session_activity(session: String) -> Result<Vec<Value>, String> {
    if !session_ok(&session) {
        return Err("not a session".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let p = places();
        let mut lines: Vec<Value> = audit_recent(Path::new(&p.audit), 20_000)
            .into_iter()
            .filter(|l| l["session"].as_str() == Some(session.as_str()))
            .collect();
        let n = lines.len();
        lines.drain(..n.saturating_sub(300));
        lines
    })
    .await
    .map_err(|e| e.to_string())
}

// ---------- Approvals inbox (view only) ----------

/// Requests waiting in every running session, as each session lists them
/// for the app (`/api/app-approvals`, signed). View only.
#[tauri::command]
pub async fn approvals_inbox() -> Result<Vec<Value>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        settings::live_all()
            .iter()
            .map(|s| {
                let base = json!({"session": s.session, "user": s.user, "started": s.started, "last_used": s.last_used});
                let mut v = base;
                match settings::signed_post_to(s, "/api/app-approvals", b"{}") {
                    Ok((200, r)) => {
                        v["pending"] = r["pending"].clone();
                    }
                    Ok((404, _)) => {
                        v["pending"] = json!([]);
                        v["error"] = json!("This session runs an older Columbus that cannot list its requests here; they show in your AI app or on the browser page.");
                    }
                    Ok((code, r)) => {
                        v["pending"] = json!([]);
                        v["error"] = json!(format!("{code}: {}", r["error"].as_str().unwrap_or("no reason given")));
                    }
                    Err(e) => {
                        v["pending"] = json!([]);
                        v["error"] = json!(e);
                    }
                }
                v
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Ask a session to open its browser approvals page, where the person
/// decides. Nothing is decided here.
#[tauri::command]
pub async fn approvals_open_browser(session: String) -> Result<bool, String> {
    if !session_ok(&session) {
        return Err("not a session".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let s = settings::live_all()
            .into_iter()
            .find(|s| s.session == session)
            .ok_or("that session is no longer running")?;
        let (code, v) = settings::signed_post_to(&s, "/api/app-approvals", br#"{"review":true}"#)?;
        match code {
            200 => Ok(v["opened"].as_bool().unwrap_or(false)),
            _ => Err(format!(
                "the session did not open its page ({code}: {})",
                v["error"].as_str().unwrap_or("no reason given")
            )),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Audit log ----------

fn month_ok(m: &str) -> bool {
    m.len() == 7
        && m.as_bytes()[4] == b'-'
        && m.bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || b.is_ascii_digit())
}

/// A month's lines, numbered as in the file. A month larger than
/// [`MAX_AUDIT_BYTES`] is read from its end, so the newest lines are always
/// there; how much was skipped is said (`Skipped`).
fn audit_lines(path: &Path) -> (Vec<Value>, Skipped) {
    use std::io::{BufRead, Read, Seek, SeekFrom};
    let none = (vec![], Skipped::default());
    let Ok(mut f) = std::fs::File::open(path) else {
        return none;
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(MAX_AUDIT_BYTES);
    // Count the lines before the part that is read, for their numbers.
    let mut before = 0usize;
    let mut cut_line = false;
    if from > 0 {
        let mut head = std::io::BufReader::new((&mut f).take(from));
        let mut last = b'\n';
        loop {
            let buf = match head.fill_buf() {
                Ok(b) if !b.is_empty() => b,
                _ => break,
            };
            before += buf.iter().filter(|&&b| b == b'\n').count();
            last = *buf.last().unwrap_or(&b'\n');
            let n = buf.len();
            head.consume(n);
        }
        cut_line = last != b'\n';
        if f.seek(SeekFrom::Start(from)).is_err() {
            return none;
        }
    }
    let mut lines = std::io::BufReader::new(f).lines().map_while(Result::ok);
    let mut first = before + 1;
    if cut_line {
        // The rest of a line that started before the part read.
        let _ = lines.next();
        first += 1;
    }
    let out: Vec<Value> = lines
        .enumerate()
        .filter_map(|(i, l)| {
            let mut v: Value = serde_json::from_str(&l).ok()?;
            v["line"] = json!(first + i);
            Some(v)
        })
        .collect();
    let skipped = Skipped {
        bytes: from,
        lines: first - 1,
    };
    (out, skipped)
}

/// What a large month's reading left out, at its start.
#[derive(Default, Clone, Copy)]
struct Skipped {
    bytes: u64,
    lines: usize,
}

fn months(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let m = name.strip_suffix(".jsonl")?;
            month_ok(m).then(|| (m.to_string(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// The lines in the last `bytes` of a file (the first, cut one dropped).
fn audit_tail(path: &Path, bytes: u64) -> Vec<Value> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return vec![];
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(bytes);
    if f.seek(SeekFrom::Start(from)).is_err() {
        return vec![];
    }
    let mut buf = Vec::new();
    if f.take(bytes).read_to_end(&mut buf).is_err() {
        return vec![];
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
        .into_iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The newest `n` lines of the latest two months (reading at most a few
/// megabytes of each, from the end), oldest first.
fn audit_recent(dir: &Path, n: usize) -> Vec<Value> {
    const TAIL: u64 = 4 * 1024 * 1024;
    let ms = months(dir);
    let mut lines = vec![];
    for (m, _) in ms.iter().take(2).rev() {
        lines.extend(audit_tail(&dir.join(format!("{m}.jsonl")), TAIL));
    }
    let k = lines.len();
    lines.drain(..k.saturating_sub(n));
    lines
}

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    #[serde(default)]
    month: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    client: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// One page of the audit log, newest first, filtered.
#[tauri::command]
pub async fn audit_page(query: AuditQuery) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = places();
        let dir = PathBuf::from(&p.audit);
        let ms = months(&dir);
        let month = match query.month.as_deref().filter(|m| !m.is_empty()) {
            Some(m) if month_ok(m) => Some(m.to_string()),
            Some(_) => return Err("not a month".to_string()),
            None => ms.first().map(|(m, _)| m.clone()),
        };
        let limit = query.limit.unwrap_or(100).clamp(1, 500);
        let (all, skipped) = month
            .as_ref()
            .map(|m| audit_lines(&dir.join(format!("{m}.jsonl"))))
            .unwrap_or_default();
        let has = |v: &Value, k: &str, want: &Option<String>| {
            want.as_deref()
                .filter(|w| !w.is_empty())
                .is_none_or(|w| v[k].as_str() == Some(w))
        };
        let text = query
            .text
            .as_deref()
            .map(str::to_lowercase)
            .filter(|t| !t.is_empty());
        let matching: Vec<&Value> = all
            .iter()
            .rev()
            .filter(|v| {
                has(v, "client", &query.client)
                    && has(v, "tool", &query.tool)
                    && has(v, "decision", &query.decision)
                    && has(v, "session", &query.session)
                    && text
                        .as_ref()
                        .is_none_or(|t| v.to_string().to_lowercase().contains(t))
            })
            .collect();
        let distinct = |k: &str| {
            let mut s: Vec<String> = all
                .iter()
                .filter_map(|v| v[k].as_str().map(String::from))
                .collect();
            s.sort();
            s.dedup();
            s
        };
        Ok(json!({
            "dir": p.audit,
            "months": ms.iter().map(|(m, b)| json!({"month": m, "bytes": b})).collect::<Vec<_>>(),
            "month": month,
            "total": all.len(),
            "matching": matching.len(),
            "offset": query.offset,
            "lines": matching.iter().skip(query.offset).take(limit).collect::<Vec<_>>(),
            // A month too large to read whole: its oldest lines are left out.
            "skipped_lines": skipped.lines,
            "skipped_bytes": skipped.bytes,
            "clients": distinct("client"),
            "tools": distinct("tool"),
            "decisions": distinct("decision"),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Check a month's hash chain, as `columbus-mcp audit verify` does.
#[tauri::command]
pub async fn audit_verify(month: String) -> Result<Value, String> {
    if !month_ok(&month) {
        return Err("not a month".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(places().audit).join(format!("{month}.jsonl"));
        match columbus_shared::audit::verify(&path) {
            Ok(n) => json!({"ok": true, "lines": n}),
            Err(columbus_shared::audit::VerifyError::Broken(line, why)) => {
                json!({"ok": false, "line": line, "message": why})
            }
            Err(columbus_shared::audit::VerifyError::Io(e)) => json!({"ok": false, "message": e}),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Every export the audit log records, newest first.
#[tauri::command]
pub async fn exports_list() -> Result<Vec<Value>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let dir = PathBuf::from(places().audit);
        let mut out = vec![];
        for (m, _) in months(&dir).iter().take(6) {
            for v in audit_lines(&dir.join(format!("{m}.jsonl")))
                .0
                .into_iter()
                .rev()
            {
                if v["tool"] == "export_output" && v["decision"] == "allowed" {
                    out.push(v);
                }
                if out.len() >= 300 {
                    return out;
                }
            }
        }
        out
    })
    .await
    .map_err(|e| e.to_string())
}

// ---------- Notes and the organization's pack ----------

/// The person's notes: every Markdown note, with its front matter.
#[tauri::command]
pub async fn notes_list() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let p = places();
        let root = PathBuf::from(&p.notes);
        let notes: Vec<Value> = walk(&root, 6, &|p| p.extension().is_some_and(|e| e == "md"))
            .into_iter()
            .map(|(rel, m)| {
                let fm = read_head(&root.join(&rel))
                    .map(|t| front_matter(&t))
                    .unwrap_or_default();
                json!({
                    "path": rel,
                    "bytes": m.len(),
                    "modified": modified(&m),
                    "id": fm.get("id"),
                    "title": fm.get("title"),
                    "status": fm.get("status"),
                    "connection": fm.get("connection"),
                    "kind": fm.get("kind"),
                    "owner": fm.get("owner"),
                    "last_confirmed": fm.get("last_confirmed"),
                })
            })
            .collect();
        json!({"dir": p.notes, "exists": root.is_dir(), "notes": notes})
    })
    .await
    .map_err(|e| e.to_string())
}

/// A note's text, and the SHA-256 of its exact bytes: "Mark reviewed"
/// names that hash, so it marks only the note the person read.
#[tauri::command]
pub async fn note_read(path: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let f = inside(Path::new(&places().notes), &path)?;
        let bytes = read_whole(&f)?;
        let sha256 = columbus_shared::canon::sha256_hex(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| "not a text file".to_string())?;
        Ok(json!({"text": text, "sha256": sha256}))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The note with its front matter's `status: draft` set to `reviewed`:
/// only that word changes (a comment after it and the line ending stay,
/// and so does every other byte). None if it is not a draft.
pub(crate) fn reviewed(text: &str) -> Option<String> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim() == "---" {
            return None;
        }
        if let Some(rest) = line.strip_prefix("status:") {
            let lead = rest.len() - rest.trim_start().len();
            let value = rest.trim_start();
            let word_end = value
                .find(|c: char| c.is_whitespace() || c == '#')
                .unwrap_or(value.len());
            if &value[..word_end] != "draft" {
                return None;
            }
            let at = offset + "status:".len() + lead;
            let mut out = String::with_capacity(text.len() + 3);
            out.push_str(&text[..at]);
            out.push_str("reviewed");
            out.push_str(&text[at + "draft".len()..]);
            return Some(out);
        }
        offset += line.len();
    }
    None
}

/// The note's front matter as YAML, as the server reads it.
fn yaml_front(text: &str) -> Option<serde_yaml::Mapping> {
    let t = text.replace("\r\n", "\n");
    let rest = t.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    serde_yaml::from_str(&rest[..end]).ok()
}

fn same_but_status(before: &str, after: &str) -> bool {
    let (Some(mut b), Some(mut a)) = (yaml_front(before), yaml_front(after)) else {
        return false;
    };
    let key = serde_yaml::Value::from("status");
    let (sb, sa) = (b.remove(&key), a.remove(&key));
    sb.as_ref().and_then(|v| v.as_str()) == Some("draft")
        && sa.as_ref().and_then(|v| v.as_str()) == Some("reviewed")
        && a == b
}

/// Mark one of the person's own notes reviewed. Their own file, their own
/// decision: an assistant can no longer change a reviewed note, so this
/// only narrows what it may do, and needs no confirmation. `sha256` is the
/// hash `note_read` gave of what the person read: if the file changed since
/// (an assistant allowed to change draft notes may have), nothing is marked.
#[tauri::command]
pub async fn note_mark_reviewed(path: String, sha256: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let f = inside(Path::new(&places().notes), &path)?;
        mark_reviewed(&f, &sha256)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn mark_reviewed(f: &Path, sha256: &str) -> Result<(), String> {
    if f.extension().is_none_or(|e| e != "md") {
        return Err("not a note".into());
    }
    let changed =
        || "the note changed after you read it; nothing was marked. Read it again".to_string();
    let bytes = read_whole(f)?;
    if !columbus_shared::proof::same(&columbus_shared::canon::sha256_hex(&bytes), sha256) {
        return Err(changed());
    }
    let before = String::from_utf8(bytes).map_err(|_| "not a text file".to_string())?;
    let after = reviewed(&before).ok_or("this note is not a draft")?;
    // Read both as the server reads them (YAML): the only difference must
    // be the status, draft before and reviewed after.
    if !same_but_status(&before, &after) {
        return Err("this note's front matter is unusual, so it was not marked; mark it reviewed by editing the file".into());
    }
    // Once more just before writing: still the note the person read.
    if read_whole(f).ok().as_deref() != Some(before.as_bytes()) {
        return Err(changed());
    }
    settings::write_atomic(f, &after)
}

/// The organization's context pack: its entries, read only here.
#[tauri::command]
pub async fn context_pack() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let p = places();
        let root = PathBuf::from(&p.context);
        let pack = read_text(&root.join("pack.yaml")).ok();
        let entries: Vec<Value> = walk(&root, 6, &|p| {
            p.extension()
                .is_some_and(|e| e == "md" || e == "yaml" || e == "yml")
        })
        .into_iter()
        .filter(|(rel, _)| rel != "pack.yaml")
        .map(|(rel, m)| {
            let fm = read_head(&root.join(&rel))
                .map(|t| front_matter(&t))
                .unwrap_or_default();
            json!({
                "path": rel,
                "bytes": m.len(),
                "title": fm.get("title"),
                "kind": fm.get("kind"),
                "status": fm.get("status"),
                "owner": fm.get("owner"),
            })
        })
        .collect();
        json!({
            "dir": p.context,
            "exists": root.is_dir(),
            "writable": root.is_dir() && settings::writable(&root.join("pack.yaml")),
            "pack": pack,
            "entries": entries,
        })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn context_read(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let f = inside(Path::new(&places().context), &path)?;
        read_text(&f)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Project databases, skills ----------

/// Files under `<workspaces>/projects`, where project databases will live.
#[tauri::command]
pub async fn project_databases() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let p = places();
        let root = PathBuf::from(&p.workspaces).join("projects");
        let files: Vec<Value> = walk(&root, 3, &|_| true)
            .into_iter()
            .map(|(rel, m)| json!({"path": rel, "bytes": m.len(), "modified": modified(&m)}))
            .collect();
        json!({"dir": root.display().to_string(), "exists": root.is_dir(), "files": files})
    })
    .await
    .map_err(|e| e.to_string())
}

/// The skills shipped with Columbus: from the `skills/` folder beside the
/// server, else the list this app was built with.
#[tauri::command]
pub async fn skills_list(app: AppHandle) -> Result<Value, String> {
    let s = crate::chat::load(&app);
    tauri::async_runtime::spawn_blocking(move || {
        let dir = crate::chat::server(&s)
            .and_then(|p| p.parent().map(|d| d.join("skills")))
            .filter(|d| d.is_dir());
        let mut skills = vec![];
        if let Some(d) = &dir {
            for (rel, _) in walk(d, 2, &|p| p.file_name().is_some_and(|n| n == "SKILL.md")) {
                if let Ok(t) = read_text(&d.join(&rel)) {
                    let fm = front_matter(&t);
                    skills.push(json!({
                        "name": fm.get("name").cloned().unwrap_or_else(|| json!(rel.trim_end_matches("/SKILL.md"))),
                        "description": fm.get("description"),
                    }));
                }
            }
        }
        let from = if skills.is_empty() {
            let built: Value = serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/skills.json")))
                .unwrap_or(json!([]));
            skills = built.as_array().cloned().unwrap_or_default();
            "built in"
        } else {
            "installed"
        };
        json!({"from": from, "dir": dir.map(|d| d.display().to_string()), "skills": skills})
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_their_folder() {
        let d = std::env::temp_dir().join(format!("columbus-shell-test-{}", std::process::id()));
        std::fs::create_dir_all(d.join("datasets/roads")).unwrap();
        std::fs::write(d.join("datasets/roads/routes.md"), "x").unwrap();
        std::fs::write(d.with_extension("secret"), "s").unwrap();
        assert!(inside(&d, "datasets/roads/routes.md").is_ok());
        assert!(inside(&d, "../x").is_err());
        assert!(inside(&d, "datasets/../../x").is_err());
        assert!(inside(&d, "/etc/passwd").is_err());
        assert!(inside(&d, "datasets").is_err(), "a folder is not a file");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.with_extension("secret"), d.join("link.md")).unwrap();
            assert!(inside(&d, "link.md").is_err(), "a link out is refused");
        }
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_file(d.with_extension("secret"));
    }

    #[test]
    fn marking_reviewed_changes_only_the_status() {
        let t =
            "---\nid: roads.routes\nstatus: draft\nowner: me\n---\n\nstatus: draft in the body\n";
        assert_eq!(
            reviewed(t).unwrap(),
            "---\nid: roads.routes\nstatus: reviewed\nowner: me\n---\n\nstatus: draft in the body\n"
        );
        // A comment and Windows line endings stay as they were.
        assert_eq!(
            reviewed("---\r\nstatus:  draft   # set reviewed\r\n---\r\nx\r\n").unwrap(),
            "---\r\nstatus:  reviewed   # set reviewed\r\n---\r\nx\r\n"
        );
        assert!(reviewed("---\nstatus: reviewed\n---\n").is_none());
        assert!(reviewed("---\nstatus: drafty\n---\n").is_none());
        assert!(reviewed("---\ntitle: x\n---\nstatus: draft\n").is_none());
        assert!(reviewed("no front matter\nstatus: draft\n").is_none());
    }

    #[test]
    fn marking_reviewed_needs_the_note_the_person_read() {
        let d = std::env::temp_dir().join(format!("columbus-review-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("n.md");
        let text = "---\nstatus: draft\n---\n\nbody\n";
        std::fs::write(&f, text).unwrap();
        let seen = columbus_shared::canon::sha256_hex(text.as_bytes());
        // Changed after it was read: refused, untouched.
        std::fs::write(&f, "---\nstatus: draft\n---\n\nother body\n").unwrap();
        assert!(mark_reviewed(&f, &seen).unwrap_err().contains("changed"));
        assert!(std::fs::read_to_string(&f)
            .unwrap()
            .contains("status: draft"));
        // As read: marked.
        std::fs::write(&f, text).unwrap();
        mark_reviewed(&f, &seen).unwrap();
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "---\nstatus: reviewed\n---\n\nbody\n"
        );
        // Larger than can be shown whole: refused, never cut short.
        let big = format!(
            "---\nstatus: draft\n---\n{}",
            "x".repeat(MAX_TEXT as usize + 10)
        );
        std::fs::write(&f, &big).unwrap();
        let h = columbus_shared::canon::sha256_hex(big.as_bytes());
        assert!(mark_reviewed(&f, &h).unwrap_err().contains("too large"));
        assert_eq!(std::fs::read_to_string(&f).unwrap().len(), big.len());
        assert!(read_text(&f).is_err());
        // A status: line inside a multi-line value is not the note's status:
        // refused, so the app never says "reviewed" while the server reads
        // a draft.
        let tricky = "---\ntitle: \"a\nstatus: draft\n b\"\nstatus: draft\n---\n\nbody\n";
        std::fs::write(&f, tricky).unwrap();
        let h = columbus_shared::canon::sha256_hex(tricky.as_bytes());
        assert!(mark_reviewed(&f, &h).unwrap_err().contains("unusual"));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), tricky);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn front_matter_is_read_as_plain_text() {
        let fm = front_matter("---\ntitle: \"Routes # 1\"\nstatus: draft   # set reviewed\n  nested: x\n---\nbody: y\n");
        assert_eq!(fm["title"], "Routes # 1");
        assert_eq!(fm["status"], "draft");
        assert!(!fm.contains_key("body"));
        assert!(!fm.contains_key("nested"));
    }

    #[test]
    fn the_audit_tail_drops_a_cut_line() {
        let d = std::env::temp_dir().join(format!("columbus-tail-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("2026-11.jsonl");
        let lines: Vec<String> = (0..50)
            .map(|i| format!("{{\"tool\":\"t{i}\",\"session\":\"s\"}}"))
            .collect();
        std::fs::write(&f, lines.join("\n") + "\n").unwrap();
        let all = audit_tail(&f, 1 << 20);
        assert_eq!(all.len(), 50);
        let tail = audit_tail(&f, 100);
        assert!(!tail.is_empty() && tail.len() < 50);
        assert_eq!(tail.last().unwrap()["tool"], "t49");
        assert_eq!(audit_recent(&d, 3).len(), 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_large_month_shows_its_newest_lines_numbered_as_in_the_file() {
        let d = std::env::temp_dir().join(format!("columbus-audit-big-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("2026-11.jsonl");
        let line = |i: usize| format!("{{\"tool\":\"t{i}\",\"pad\":\"{}\"}}\n", "x".repeat(1000));
        let n = (MAX_AUDIT_BYTES as usize / 1000) + 2000;
        let mut text = String::with_capacity(n * 1030);
        for i in 1..=n {
            text.push_str(&line(i));
        }
        std::fs::write(&f, &text).unwrap();
        let (lines, skipped) = audit_lines(&f);
        assert!(skipped.bytes > 0 && skipped.lines > 0);
        let last = lines.last().unwrap();
        assert_eq!(last["tool"], format!("t{n}"));
        assert_eq!(last["line"], n);
        // Numbers match the file, from the first line shown.
        let first = &lines[0];
        let k = first["line"].as_u64().unwrap() as usize;
        assert_eq!(first["tool"], format!("t{k}"));
        assert_eq!(skipped.lines, k - 1);
        assert_eq!(lines.len(), n - skipped.lines);
        // A small month: everything, nothing skipped.
        std::fs::write(&f, line(1) + &line(2)).unwrap();
        let (lines, skipped) = audit_lines(&f);
        assert_eq!((lines.len(), skipped.lines, skipped.bytes), (2, 0, 0));
        assert_eq!(lines[1]["line"], 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn months_and_sessions_are_plain() {
        assert!(month_ok("2026-11"));
        assert!(!month_ok("2026-1"));
        assert!(!month_ok("../2026"));
        assert!(session_ok("s_1a2b-3c"));
        assert!(!session_ok("../x"));
        assert!(!session_ok(""));
    }
}
