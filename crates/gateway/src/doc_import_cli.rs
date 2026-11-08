//! `columbus-mcp notes import '<json>'`: a document the person chose (Word,
//! Excel, PDF, CSV, Markdown or text) becomes a draft entry in their notes,
//! for the Columbus app's Documents > Import.
//!
//! The JSON is `{"file": "<path>", "preview": true}` to see what it would
//! become (nothing is written), or `{"file", "sha256", "kind", "title",
//! "summary", "when_to_use", "owner"}` to add it: `sha256` is the hash the
//! preview gave, so a file changed since is refused.
//!
//! What it writes, in the person's notes folder:
//! - `imported/<id>.md`: the entry, `status: draft`, its text the document's
//!   text as Markdown (headings, lists and tables; nothing in the document
//!   is run or followed), headed by a line saying where it came from;
//! - `sources/imported/<id>/<name>`: a copy of the original, beside it.
//!
//! The document is read where it can do no harm: confined when this computer
//! can (no network, writing only its own folder, a memory cap), as the
//! person's scripts are, and within a time limit. Characters a person can't
//! see are removed, so the preview shows all there is.
//!
//! The entry is the person's own, served to assistants as information, never
//! instruction, like every note, and marked as text from outside Columbus
//! (`context.imported`); no assistant can change it (`update_notes` changes
//! only notes about a connection). The entry is read back as the context
//! tools read it, and removed if it doesn't read as it should. Every preview,
//! import and refusal is audited as `import_document`. Organization policy
//! can deny it.
//!
//! Prints `{"ok": true, ...}` or `{"ok": false, "error": "..."}`.

use std::path::{Path, PathBuf};

use columbus_shared::audit::AuditRecord;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::Pack;
use crate::server::State;

pub const CLIENT: &str = "columbus-app-documents";
pub const TOOL: &str = "import_document";
const KINDS: &[&str] = &["sop", "note", "glossary"];
const EXTS: &[&str] = &["docx", "xlsx", "pdf", "csv", "md", "markdown", "txt"];
/// Most memory reading one document may use, in MiB, when read confined.
const MEMORY_MB: u64 = 1536;
/// Longest reading one document may take.
const SECONDS: u64 = 120;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    file: String,
    #[serde(default)]
    preview: bool,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    when_to_use: Option<String>,
    #[serde(default)]
    owner: Option<String>,
}

pub async fn import(request: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let out = match State::load(crate::config::Paths::from_env()) {
        Ok(state) => {
            let workspace = state.sandbox.root().to_path_buf();
            let out = run(&state, request).await;
            let _ = std::fs::remove_dir(&workspace);
            out.unwrap_or_else(|e| json!({"ok": false, "error": e}))
        }
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    println!("{out}");
    Ok(())
}

fn record(state: &State, decision: &str, reason: String, sha: Option<String>) {
    let _ = state.audit.append(&AuditRecord {
        user: state.user.clone(),
        client: CLIENT.into(),
        session: Some(state.session.clone()),
        tool: TOOL.into(),
        connection: None,
        dataset: None,
        rows: None,
        decision: decision.into(),
        reason: Some(reason),
        approval: None,
        prov: None,
        request: sha,
        provider: None,
        provider_version: None,
        tools_hash: None,
        labels: vec![],
        flow_decision: None,
    });
}

/// One line of plain text, at most `max` characters, or why not.
fn line(what: &str, v: Option<&str>, max: usize, required: bool) -> Result<Option<String>, String> {
    let v = v.map(str::trim).filter(|v| !v.is_empty());
    match v {
        None if required => Err(format!("give it a {what}")),
        None => Ok(None),
        Some(v) => {
            if v.chars().count() > max {
                return Err(format!("its {what} is longer than {max} characters"));
            }
            if v.contains('\n') || columbus_shared::tools::plain_chars(v).is_err() {
                return Err(format!("its {what} must be one line of plain text"));
            }
            Ok(Some(v.to_string()))
        }
    }
}

async fn run(state: &State, request: &str) -> Result<Value, String> {
    if request.len() > 16 * 1024 {
        return Err("the request is too long".into());
    }
    let req: Request = serde_json::from_str(request).map_err(|e| format!("not a request: {e}"))?;
    let policy = &state.settings().policy;
    if !policy.tool_allowed(TOOL) {
        let e = format!("organization policy does not allow {TOOL}");
        record(state, "denied", e.clone(), None);
        return Err(e);
    }
    let shown = clean_name(
        Path::new(&req.file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("document"),
    );
    // Every outcome from here on is audited: previews, imports, refusals.
    let (sha, out) = attempt(state, &req).await;
    match &out {
        Ok(_) if req.preview => record(
            state,
            "allowed",
            format!("previewed {shown} (nothing written)"),
            sha,
        ),
        Ok(v) => record(
            state,
            "allowed",
            format!(
                "imported {shown} as the draft note {}",
                v["id"].as_str().unwrap_or("")
            ),
            sha,
        ),
        Err(e) => record(state, "denied", format!("{shown}: {e}"), sha),
    }
    out
}

async fn attempt(state: &State, req: &Request) -> (Option<String>, Result<Value, String>) {
    let path = PathBuf::from(&req.file);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => return (None, Err(format!("{}: {e}", path.display()))),
    };
    if !meta.is_file() {
        return (None, Err(format!("{} is not a file", path.display())));
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_string();
    let ext = Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !EXTS.contains(&ext.as_str()) {
        return (
            None,
            Err(
                "Columbus imports Word (.docx), Excel (.xlsx), PDF, CSV, Markdown and text files"
                    .into(),
            ),
        );
    }
    // Converted from a copy taken now: what is converted, kept and hashed
    // is one and the same.
    let bytes = match read_capped(&path) {
        Ok(b) => b,
        Err(e) => return (None, Err(e)),
    };
    let sha = columbus_shared::canon::sha256_hex(&bytes);
    let some = Some(sha.clone());
    if !req.preview {
        // Only the document the person previewed is added.
        match req.sha256.as_deref() {
            None | Some("") => {
                return (some, Err("preview the document before adding it".into()));
            }
            Some(s) if s != sha => {
                return (
                    some,
                    Err("the document changed after you previewed it; preview it again".into()),
                );
            }
            _ => {}
        }
    }
    let doc = match convert(state, &bytes, &ext, &clean_name(&name)).await {
        Ok(d) => d,
        Err(e) => return (some, Err(e)),
    };
    // What the person previews is all there is: nothing invisible is kept
    // for an assistant to read that they could not see.
    let (markdown, hidden) = visible(doc["markdown"].as_str().unwrap_or(""));
    let (found_title, _) = visible(doc["title"].as_str().unwrap_or(""));
    let mut warnings: Vec<Value> = doc["warnings"].as_array().cloned().unwrap_or_default();
    if hidden > 0 {
        warnings.push(json!(format!(
            "{hidden} invisible character{} (control, zero-width or hidden-tag characters) {} removed",
            if hidden == 1 { "" } else { "s" },
            if hidden == 1 { "was" } else { "were" }
        )));
    }
    if req.preview {
        return (
            some,
            Ok(json!({
                "ok": true,
                "file": clean_name(&name),
                "sha256": sha,
                "format": doc["format"],
                "title": found_title,
                "markdown": markdown,
                "warnings": warnings,
            })),
        );
    }
    let out = add(state, req, &name, &ext, &bytes, &sha, &markdown).await;
    (some, out)
}

/// Text without characters a person can't see (tabs and line breaks kept),
/// and how many were removed.
fn visible(s: &str) -> (String, usize) {
    let mut n = 0;
    let out = s
        .chars()
        .filter(|&c| {
            let hide = c != '\n' && c != '\t' && columbus_shared::canon::hidden_char(c);
            n += usize::from(hide);
            !hide
        })
        .collect();
    (out, n)
}

/// The document as Markdown. Read confined when this computer can (no
/// network, writing only its own folder, at most [`MEMORY_MB`] of memory),
/// as the person's own scripts are; otherwise in Columbus's engine. Either
/// way within [`SECONDS`].
async fn convert(state: &State, bytes: &[u8], ext: &str, name: &str) -> Result<Value, String> {
    let dir = state.sandbox.root().join("import");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("tmp")).map_err(|e| format!("{}: {e}", dir.display()))?;
    let input = dir.join(format!("in.{ext}"));
    let out = match std::fs::write(&input, bytes) {
        Err(e) => Err(format!("{}: {e}", input.display())),
        Ok(()) => match columbus_confine::ready(columbus_confine::Level::A) {
            Ok(launcher) => match confined(state, &launcher, &dir, &input, name).await {
                Err(Unconfined) => core(state, &input, name).await,
                Ok(r) => r,
            },
            Err(_) => core(state, &input, name).await,
        },
    };
    let _ = std::fs::remove_dir_all(&dir);
    out
}

async fn core(state: &State, input: &Path, name: &str) -> Result<Value, String> {
    state
        .worker
        .core(
            "convert_document",
            json!({"path": input, "name": name}),
            std::time::Duration::from_secs(SECONDS),
        )
        .await
        .map_err(|e| e.to_string())
}

/// The launcher could not confine it (nothing of the document was read).
struct Unconfined;

async fn confined(
    state: &State,
    launcher: &Path,
    dir: &Path,
    input: &Path,
    name: &str,
) -> Result<Result<Value, String>, Unconfined> {
    let Some(workers) = state.paths.worker_script.parent().map(Path::to_path_buf) else {
        return Err(Unconfined);
    };
    let script = workers.join("columbus_docs.py");
    if !script.is_file() {
        return Err(Unconfined);
    }
    let out = dir.join("out.json");
    let mut read_only = crate::plugins::python_dirs(&state.paths.python);
    read_only.push(workers.clone());
    let spec = columbus_confine::Spec {
        level: columbus_confine::Level::A,
        name: "document-import".into(),
        read_write: vec![dir.to_path_buf()],
        read_only,
        memory_mb: Some(MEMORY_MB),
        processes: Some(4),
    };
    let args: Vec<String> = vec![
        "-I".into(),
        "-B".into(),
        "-X".into(),
        "utf8".into(),
        script.display().to_string(),
        workers.display().to_string(),
        input.display().to_string(),
        out.display().to_string(),
        name.to_string(),
    ];
    let python = state.paths.python.to_string_lossy().into_owned();
    let ran = match crate::script_tools::run_under(
        launcher,
        &spec,
        dir,
        &python,
        &args,
        crate::script_tools::script_env(dir),
        std::time::Duration::from_secs(SECONDS),
    )
    .await
    {
        Ok(r) => r,
        Err(_) => return Err(Unconfined),
    };
    let stderr = ran.stderr.text();
    if ran.exit_code == Some(columbus_confine::EXIT_CONFINE_FAILED)
        && stderr.starts_with("columbus-confine:")
    {
        return Err(Unconfined);
    }
    if ran.timed_out {
        return Ok(Err(format!(
            "it took longer than {SECONDS} seconds to read, so it was stopped; save a shorter or simpler copy to import"
        )));
    }
    if ran.exit_code != Some(0) {
        if stderr.contains("columbus-confine: stopped the program: it used more than") {
            return Ok(Err(format!(
                "reading it needed more than the {MEMORY_MB} MB of memory Columbus allows, so it was stopped; save a shorter or simpler copy to import"
            )));
        }
        let last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        let (last, _) = visible(last);
        return Ok(Err(format!(
            "it could not be read ({})",
            last.chars().take(200).collect::<String>()
        )));
    }
    let answer = std::fs::File::open(&out).and_then(|f| {
        use std::io::Read;
        let mut b = Vec::new();
        f.take(16 * 1024 * 1024).read_to_end(&mut b)?;
        Ok(b)
    });
    let v: Value = match answer
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    {
        Some(v) => v,
        None => return Ok(Err("it could not be read (no answer came back)".into())),
    };
    Ok(if v["ok"] == true {
        Ok(v["result"].clone())
    } else {
        Err(v["error"]
            .as_str()
            .unwrap_or("it could not be read")
            .to_string())
    })
}

fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    const MAX: u64 = 50 * 1024 * 1024;
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut b = Vec::new();
    f.take(MAX + 1)
        .read_to_end(&mut b)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if b.len() as u64 > MAX {
        return Err("the document is larger than Columbus imports (50 MB)".into());
    }
    Ok(b)
}

async fn add(
    state: &State,
    req: &Request,
    name: &str,
    ext: &str,
    bytes: &[u8],
    sha: &str,
    markdown: &str,
) -> Result<Value, String> {
    if req.sha256.as_deref() != Some(sha) {
        return Err("the document changed after you previewed it; preview it again".into());
    }
    let kind = req.kind.as_deref().unwrap_or("note");
    if !KINDS.contains(&kind) {
        return Err(format!("it can be added as {}", KINDS.join(", ")));
    }
    let title = line("title", req.title.as_deref(), 120, true)?.unwrap_or_default();
    let summary = line("summary", req.summary.as_deref(), 400, true)?.unwrap_or_default();
    let when = line(
        "when to use it",
        req.when_to_use.as_deref(),
        300,
        kind == "sop",
    )?;
    let owner =
        line("owner", req.owner.as_deref(), 100, false)?.unwrap_or_else(|| state.user.clone());
    if markdown.trim().is_empty() {
        return Err("no text was found in it to import".into());
    }

    let root = crate::notes::notes_root(&state.paths)?;
    crate::notes::ensure_pack(&root)?;
    // An id no note and no entry of the organization's pack has.
    let taken = |id: &str| {
        Pack::load(&root)
            .map(|p| p.entries.iter().any(|e| e.id == id))
            .unwrap_or(false)
            || state
                .context
                .as_ref()
                .is_some_and(|p| p.entries.iter().any(|e| e.id == id))
            || root.join("imported").join(format!("{id}.md")).exists()
            || root.join("sources/imported").join(id).exists()
    };
    let base = {
        let s = crate::notes::slug(&title);
        format!(
            "doc-{}",
            if s.is_empty() {
                "document".to_string()
            } else {
                s
            }
        )
    };
    let mut id = base.clone();
    let mut n = 2;
    while taken(&id) {
        if n > 99 {
            return Err("too many notes already have this title; give it another".into());
        }
        id = format!("{base}-{n}");
        n += 1;
    }
    let stem = crate::notes::slug(
        Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("document"),
    );
    let source_name = format!("{}.{ext}", if stem.is_empty() { "document" } else { &stem });
    let source_rel = format!("sources/imported/{id}/{source_name}");
    let today = chrono::Local::now().date_naive();
    let mut card = serde_yaml::Mapping::new();
    let mut put = |k: &str, v: serde_yaml::Value| {
        card.insert(serde_yaml::Value::from(k), v);
    };
    put("id", id.clone().into());
    put("kind", kind.into());
    put("title", title.clone().into());
    put("summary", summary.into());
    if let Some(w) = &when {
        put("when_to_use", w.clone().into());
    }
    put("owner", owner.into());
    put(
        "last_confirmed",
        today.format("%Y-%m-%d").to_string().into(),
    );
    put("status", "draft".into());
    let mut source = serde_yaml::Mapping::new();
    source.insert("file".into(), source_rel.clone().into());
    source.insert("sha256".into(), sha.into());
    source.insert("original_name".into(), clean_name(name).into());
    put("source", serde_yaml::Value::Mapping(source));
    put("imported", chrono::Local::now().to_rfc3339().into());
    let front = serde_yaml::to_string(&card).map_err(|e| e.to_string())?;
    let text = format!(
        "---\n{front}---\n\n> Imported from {} on {}. A draft: check it against the original before you rely on it, then mark it reviewed.\n\n{}\n",
        clean_name(name),
        today.format("%-d %B %Y"),
        markdown.trim()
    );
    let note_rel = PathBuf::from("imported").join(format!("{id}.md"));
    crate::notes::write_bytes(&root, Path::new(&source_rel), bytes)?;
    if let Err(e) = crate::notes::write_note(&root, &note_rel, &text) {
        let _ = std::fs::remove_file(root.join(&source_rel));
        let _ = std::fs::remove_dir(root.join("sources/imported").join(&id));
        return Err(e);
    }
    // Read back as the context tools read it: exactly this entry, with no
    // problem, or nothing is kept.
    let undo = || {
        let _ = std::fs::remove_file(root.join(&note_rel));
        let _ = std::fs::remove_file(root.join(&source_rel));
        let _ = std::fs::remove_dir(root.join("sources/imported").join(&id));
    };
    let pack = match Pack::load(&root) {
        Ok(p) => p,
        Err(e) => {
            undo();
            return Err(e);
        }
    };
    let rel = note_rel.display().to_string();
    let problems: Vec<String> = pack
        .lint(today)
        .into_iter()
        .filter(|p| p.starts_with(&rel))
        .collect();
    let mine: Vec<_> = pack.entries.iter().filter(|e| e.id == id).collect();
    if !problems.is_empty() || mine.len() != 1 || mine[0].kind != kind {
        undo();
        return Err(if problems.is_empty() {
            "the note did not read back as written, so it was not kept".into()
        } else {
            format!(
                "it was not kept: {}",
                problems
                    .iter()
                    .map(|p| p
                        .trim_start_matches(&rel)
                        .trim_start_matches(": ")
                        .to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        });
    }
    Ok(json!({"ok": true, "id": id, "path": rel, "source": source_rel}))
}

/// The original's name as shown in the note: plain characters only.
fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| columbus_shared::tools::plain_chars(&c.to_string()).is_ok() && *c != '\n')
        .take(120)
        .collect()
}
