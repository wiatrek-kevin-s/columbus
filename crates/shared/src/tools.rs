//! The person's own tools ("personal tools"): engine plugins the assistant
//! proposed with `propose_tool` and the person installed in the Columbus
//! app. Shared by the server (which files proposals and loads installed
//! tools) and the app (which lists, installs, declines and removes them),
//! so the two always agree on the folder, the files and the hash.
//!
//! Layout of the tools folder (`tools/` beside the session workspaces, or
//! `COLUMBUS_TOOLS`):
//!
//! ```text
//! tools/
//!   approved.yaml                       id -> sha256 of what the person installed
//!   personal.locate-measures/           an installed tool
//!     plugin.yaml   tool.py             the two files the hash covers
//!     install.json                      when, and from which proposal (display only)
//!   proposals/
//!     personal.locate-measures/         waiting for the person
//!       plugin.yaml   tool.py   proposal.json
//! ```
//!
//! A tool runs only when `approved.yaml` holds the hash of exactly its
//! `plugin.yaml` and `tool.py`; the assistant has no tool that writes there.
//! Only the app's Settings window installs, after the person confirms with
//! Touch ID or Windows Hello, and it installs exactly the bytes it showed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::canon::sha256_hex;

pub const MANIFEST: &str = "plugin.yaml";
pub const ENTRY: &str = "tool.py";
pub const PROPOSAL: &str = "proposal.json";
pub const INSTALL: &str = "install.json";
pub const APPROVED: &str = "approved.yaml";
pub const PROPOSALS: &str = "proposals";
/// Every personal tool's id starts with this; nothing else may use it.
pub const ID_PREFIX: &str = "personal.";
/// The one tool in each personal tool: `personal.<name>.run`.
pub const TOOL: &str = "run";
/// Largest `tool.py` and `plugin.yaml`, in bytes.
pub const MAX_CODE: usize = 100 * 1024;
pub const MAX_MANIFEST: usize = 64 * 1024;
/// Most proposals waiting at once, so one cannot be buried among many.
pub const MAX_PROPOSALS: usize = 20;
/// Most installed tools loaded.
pub const MAX_INSTALLED: usize = 100;
/// Longest line shown, in characters: the person must see every line whole.
pub const MAX_LINE: usize = 200;

/// The person's tools folder: `COLUMBUS_TOOLS`, else `tools/` beside the
/// session workspaces (`COLUMBUS_WORKSPACES`'s parent, or the person's
/// local Columbus folder).
pub fn tools_dir() -> PathBuf {
    if let Some(d) = crate::paths::env_path("COLUMBUS_TOOLS") {
        return d;
    }
    if let Some(ws) = crate::paths::env_path("COLUMBUS_WORKSPACES") {
        return ws
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(ws)
            .join("tools");
    }
    crate::paths::platform_dirs().2.join("tools")
}

/// Now, as the install record writes it.
pub fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// `personal.<name>`: a personal tool's id.
pub fn id_ok(id: &str) -> bool {
    id.strip_prefix(ID_PREFIX).is_some_and(|n| {
        (3..=40).contains(&n.len())
            && n.starts_with(|c: char| c.is_ascii_lowercase())
            && !n.ends_with('-')
            && !n.contains("--")
            && n.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

/// The id for a tool name the assistant gave (`locate_measures` ->
/// `personal.locate-measures`).
pub fn id_for(name: &str) -> String {
    format!("{ID_PREFIX}{}", name.replace('_', "-"))
}

/// The hash a person approves: over `plugin.yaml`, a zero byte, and the
/// entry file, as for every engine plugin.
pub fn content_hash(manifest: &[u8], entry: &[u8]) -> String {
    let mut all = Vec::with_capacity(manifest.len() + entry.len() + 1);
    all.extend_from_slice(manifest);
    all.push(0);
    all.extend_from_slice(entry);
    format!("sha256:{}", sha256_hex(&all))
}

/// Text is shown exactly as it runs: no characters that display differently
/// from how Python (or YAML) reads them. Refused: carriage returns (Python
/// breaks a line at a lone CR that a browser shows as one line), other
/// control characters but tab and newline, bidirectional and invisible
/// formatting characters (Trojan Source), and lines too long to see whole.
pub fn plain_text(text: &str) -> Result<(), String> {
    for (i, line) in text.split('\n').enumerate() {
        if line.chars().count() > MAX_LINE {
            return Err(format!(
                "line {} is longer than {MAX_LINE} characters; split it, so the person can read every line",
                i + 1
            ));
        }
    }
    plain_chars(text)
}

/// As [`plain_text`], without the limit on line length (prose, and the
/// manifest, which the app shows wrapped).
pub fn plain_chars(text: &str) -> Result<(), String> {
    for (i, line) in text.split('\n').enumerate() {
        if let Some(c) = line
            .chars()
            .find(|&c| c != '\t' && crate::canon::hidden_char(c))
        {
            return Err(format!(
                "line {} contains the character U+{:04X}, which displays differently from how it is read; it must be plain text",
                i + 1,
                c as u32
            ));
        }
    }
    Ok(())
}

/// A regular file's bytes, never through a link, at most `cap`.
fn read_plain(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let md = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !md.file_type().is_file() {
        return Err(format!("{} is not a plain file", path.display()));
    }
    if md.len() as usize > cap {
        return Err(format!(
            "{} is larger than {} KB",
            path.display(),
            cap / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() > cap {
        return Err(format!(
            "{} is larger than {} KB",
            path.display(),
            cap / 1024
        ));
    }
    Ok(bytes)
}

/// The two files of a tool, checked for what the app can check without the
/// server: plain text within the limits, a manifest naming this id, Python,
/// `tool.py` as its entry, no confinement but A, and the one `run` tool.
/// The server checks everything again (the full manifest rules) when it
/// loads the tool.
pub fn check_files(id: &str, manifest: &[u8], entry: &[u8]) -> Result<serde_yaml::Value, String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a personal tool's id"));
    }
    let m = std::str::from_utf8(manifest).map_err(|_| "plugin.yaml is not UTF-8 text")?;
    let code = std::str::from_utf8(entry).map_err(|_| "tool.py is not UTF-8 text")?;
    plain_chars(m).map_err(|e| format!("plugin.yaml: {e}"))?;
    plain_text(code).map_err(|e| format!("tool.py: {e}"))?;
    let v: serde_yaml::Value =
        serde_yaml::from_str(m).map_err(|e| format!("plugin.yaml does not read: {e}"))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    if let Some(name) = s("name") {
        // Shown in the install prompt, quoted, and in approval summaries.
        if name.chars().count() > 80 || name.contains(['"', '\n']) || plain_chars(name).is_err() {
            return Err("the tool's name (its title) must be one plain line of at most 80 characters, without double quotes".into());
        }
    }
    if s("id") != Some(id) {
        return Err(format!("plugin.yaml names another id than {id}"));
    }
    if s("runtime") != Some("python") || s("entry") != Some(ENTRY) {
        return Err("a personal tool is Python, with tool.py as its entry".into());
    }
    if let Some(c) = v.get("confinement") {
        let level = c.get("level").and_then(|l| l.as_str()).unwrap_or("A");
        if !level.eq_ignore_ascii_case("A") {
            return Err("a personal tool runs at confinement level A only".into());
        }
    }
    let tools = v
        .get("tools")
        .and_then(|t| t.as_mapping())
        .ok_or("plugin.yaml lists no tools")?;
    if tools.len() != 1 || tools.get(TOOL).is_none() {
        return Err("a personal tool has exactly one tool, named run".into());
    }
    if let Some(r) = tools
        .get(TOOL)
        .and_then(|t| t.get("risk"))
        .and_then(|r| r.as_str())
    {
        if r != "read" {
            return Err(
                "a personal tool only reads its inputs and writes its outputs (risk: read)".into(),
            );
        }
    }
    Ok(v)
}

/// What the Settings window shows of a tool.
#[derive(Debug, Clone, Serialize)]
pub struct ToolView {
    pub id: String,
    /// `personal.<name>.run`, as assistants call it.
    pub tool: String,
    pub title: String,
    pub description: String,
    pub not_for: String,
    pub version: String,
    pub hash: String,
    /// Installed tools: `ready`, `changed` (its files differ from what was
    /// installed: off), `not_installed` (no approval recorded: off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    /// Proposals: what the server recorded (when, the test run, warnings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Value>,
    /// Proposals: the full files, for the person to read before installing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Proposals: the installed tool of the same id it would replace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

fn view(id: &str, dir: &Path) -> ToolView {
    let mut v = ToolView {
        id: id.to_string(),
        tool: format!("{id}.{TOOL}"),
        title: id.to_string(),
        description: String::new(),
        not_for: String::new(),
        version: String::new(),
        hash: String::new(),
        state: None,
        installed: None,
        proposal: None,
        manifest_text: None,
        code: None,
        replaces: None,
        problem: None,
    };
    let files = read_plain(&dir.join(MANIFEST), MAX_MANIFEST)
        .and_then(|m| read_plain(&dir.join(ENTRY), MAX_CODE).map(|e| (m, e)));
    let (m, e) = match files {
        Ok(x) => x,
        Err(err) => {
            v.problem = Some(err);
            return v;
        }
    };
    v.hash = content_hash(&m, &e);
    match check_files(id, &m, &e) {
        Ok(y) => {
            let t = y.get("tools").and_then(|t| t.get(TOOL));
            let s = |x: Option<&serde_yaml::Value>| {
                x.and_then(|x| x.as_str()).unwrap_or_default().to_string()
            };
            v.title = y
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(id)
                .to_string();
            v.version = s(y.get("version"));
            v.description = s(t.and_then(|t| t.get("description")));
            if v.description.is_empty() {
                v.description = s(t.and_then(|t| t.get("summary")));
            }
            v.not_for = s(t.and_then(|t| t.get("not_for")));
        }
        Err(err) => v.problem = Some(err),
    }
    v.manifest_text = Some(String::from_utf8_lossy(&m).into_owned());
    v.code = Some(String::from_utf8_lossy(&e).into_owned());
    v
}

/// `approved.yaml`: id -> hash. A file that does not read approves nothing.
pub fn read_approvals(root: &Path) -> BTreeMap<String, String> {
    read_plain(&root.join(APPROVED), 1024 * 1024)
        .ok()
        .and_then(|b| serde_yaml::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_approvals(root: &Path, map: &BTreeMap<String, String>) -> Result<(), String> {
    let text = format!(
        "# Personal tools the person installed in the Columbus app, by the hash of exactly\n# their plugin.yaml and tool.py. A tool whose files differ is off.\n{}",
        serde_yaml::to_string(map).map_err(|e| e.to_string())?
    );
    write_atomic(&root.join(APPROVED), text.as_bytes())
}

/// Write a file whole beside its final name, then move it into place; a
/// link at the final name is refused, never written through.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let show = |e: std::io::Error| format!("{}: {e}", path.display());
    if std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(format!("{} is not a plain file", path.display()));
    }
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(show)?;
        f.write_all(bytes).map_err(show)?;
        f.sync_all().map_err(show)?;
        std::fs::rename(&tmp, path).map_err(show)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A folder that is really a folder (not a link to one).
fn is_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_dir())
}

fn folder_ids(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| is_dir(&e.path()))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| id_ok(n))
        .collect();
    ids.sort();
    ids
}

/// The installed tools, with whether each is on.
pub fn list_installed(root: &Path) -> Vec<ToolView> {
    let approvals = read_approvals(root);
    folder_ids(root)
        .into_iter()
        .take(MAX_INSTALLED)
        .map(|id| {
            let dir = root.join(&id);
            let mut v = view(&id, &dir);
            v.state = Some(
                match approvals.get(&id) {
                    None => "not_installed",
                    Some(h) if *h == v.hash && v.problem.is_none() => "ready",
                    Some(_) => "changed",
                }
                .into(),
            );
            v.installed = read_plain(&dir.join(INSTALL), 64 * 1024)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|j| j["installed"].as_str().map(String::from));
            // The code is shown for proposals; installed tools show it on request.
            v.manifest_text = None;
            v.code = None;
            v
        })
        .collect()
}

/// The proposals waiting for the person, oldest first, with their full files.
pub fn list_proposals(root: &Path) -> Vec<ToolView> {
    let approvals = read_approvals(root);
    let mut out: Vec<ToolView> = folder_ids(&root.join(PROPOSALS))
        .into_iter()
        .map(|id| read_proposal_view(root, &id, &approvals))
        .collect();
    out.sort_by(|a, b| {
        let at = |v: &ToolView| {
            v.proposal
                .as_ref()
                .and_then(|p| p["proposed"].as_str().map(String::from))
                .unwrap_or_default()
        };
        at(a).cmp(&at(b))
    });
    out
}

fn read_proposal_view(root: &Path, id: &str, approvals: &BTreeMap<String, String>) -> ToolView {
    let dir = root.join(PROPOSALS).join(id);
    let mut v = view(id, &dir);
    v.proposal = read_plain(&dir.join(PROPOSAL), 1024 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if is_dir(&root.join(id)) {
        v.replaces = Some(approvals.get(id).cloned().unwrap_or_default());
    }
    v
}

/// One waiting proposal, read now.
pub fn read_proposal(root: &Path, id: &str) -> Result<ToolView, String> {
    if !id_ok(id) || !is_dir(&root.join(PROPOSALS).join(id)) {
        return Err(format!("there is no tool {id:?} waiting"));
    }
    let v = read_proposal_view(root, id, &read_approvals(root));
    match &v.problem {
        Some(p) => Err(format!("{id} cannot be installed: {p}")),
        None => Ok(v),
    }
}

/// File a proposal (the server, after the test run): it replaces a waiting
/// proposal of the same id. Refused when too many others are waiting.
pub fn write_proposal(
    root: &Path,
    id: &str,
    manifest: &[u8],
    entry: &[u8],
    info: &Value,
) -> Result<PathBuf, String> {
    check_files(id, manifest, entry)?;
    let proposals = root.join(PROPOSALS);
    let others = folder_ids(&proposals)
        .into_iter()
        .filter(|p| p != id)
        .count();
    if others >= MAX_PROPOSALS {
        return Err(format!(
            "{others} tools are already waiting for the person to review in Settings; ask them to install or decline those first"
        ));
    }
    std::fs::create_dir_all(&proposals).map_err(|e| format!("{}: {e}", proposals.display()))?;
    let staging = proposals.join(format!(".{id}.{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let written = (|| {
        write_atomic(&staging.join(MANIFEST), manifest)?;
        write_atomic(&staging.join(ENTRY), entry)?;
        let text = serde_json::to_vec_pretty(info).map_err(|e| e.to_string())?;
        write_atomic(&staging.join(PROPOSAL), &text)?;
        replace_dir(&staging, &proposals.join(id))
    })();
    if written.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    written.map(|_| proposals.join(id))
}

/// Move `from` to `to`, replacing a folder already there.
fn replace_dir(from: &Path, to: &Path) -> Result<(), String> {
    let old = to.with_file_name(format!(
        ".old-{}-{}",
        to.file_name().and_then(|n| n.to_str()).unwrap_or("x"),
        uuid::Uuid::new_v4().simple()
    ));
    let had = std::fs::symlink_metadata(to).is_ok();
    if had {
        std::fs::rename(to, &old).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    if let Err(e) = std::fs::rename(from, to) {
        if had {
            let _ = std::fs::rename(&old, to);
        }
        return Err(format!("{}: {e}", to.display()));
    }
    if had {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

/// Install a waiting proposal: exactly the files whose hash is `want` (what
/// the person was shown), checked again here. The tool is on from the next
/// call of every running session. Called only by the app, after the person
/// confirmed.
pub fn install(root: &Path, id: &str, want: &str, now: &str) -> Result<ToolView, String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a personal tool's id"));
    }
    let from = root.join(PROPOSALS).join(id);
    if !is_dir(&from) {
        return Err(format!("there is no tool {id:?} waiting"));
    }
    let manifest = read_plain(&from.join(MANIFEST), MAX_MANIFEST)?;
    let entry = read_plain(&from.join(ENTRY), MAX_CODE)?;
    check_files(id, &manifest, &entry)?;
    let hash = content_hash(&manifest, &entry);
    if hash != want {
        return Err(
            "the tool changed after you looked at it; nothing was installed. Look again".into(),
        );
    }
    let proposal: Value = read_plain(&from.join(PROPOSAL), 1024 * 1024)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let staging = root.join(format!(".install-{id}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let placed = (|| {
        write_atomic(&staging.join(MANIFEST), &manifest)?;
        write_atomic(&staging.join(ENTRY), &entry)?;
        let info = json!({
            "installed": now,
            "hash": hash,
            "proposed": proposal["proposed"],
            "session": proposal["session"],
            "client": proposal["client"],
        });
        write_atomic(
            &staging.join(INSTALL),
            &serde_json::to_vec_pretty(&info).map_err(|e| e.to_string())?,
        )?;
        replace_dir(&staging, &root.join(id))
    })();
    if let Err(e) = placed {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let mut approvals = read_approvals(root);
    approvals.insert(id.to_string(), hash);
    write_approvals(root, &approvals)?;
    let _ = std::fs::remove_dir_all(&from);
    list_installed(root)
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("{id} was installed but does not list"))
}

/// Decline a waiting proposal: it is deleted.
pub fn decline(root: &Path, id: &str) -> Result<(), String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a personal tool's id"));
    }
    let dir = root.join(PROPOSALS).join(id);
    if !is_dir(&dir) {
        return Err(format!("there is no tool {id:?} waiting"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Remove an installed tool: off at once (its approval goes first), then
/// its folder is deleted.
pub fn remove(root: &Path, id: &str) -> Result<(), String> {
    if !id_ok(id) {
        return Err(format!("{id:?} is not a personal tool's id"));
    }
    let mut approvals = read_approvals(root);
    let had = approvals.remove(id).is_some();
    let dir = root.join(id);
    if !had && !is_dir(&dir) {
        return Err(format!("there is no tool {id:?} installed"));
    }
    if had {
        write_approvals(root, &approvals)?;
    }
    if is_dir(&dir) {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const YAML: &str = "id: personal.locate-measures\nname: Locate measures\nversion: 1.0.0\nruntime: python\nentry: tool.py\ntools:\n  run:\n    summary: Locate points along routes.\n    description: Locate points along routes as route id and measure.\n    not_for: Lines or polygons.\n    risk: read\n";
    const CODE: &str = "def run(ctx):\n    return {}\n";

    #[test]
    fn ids_and_names() {
        assert_eq!(id_for("locate_measures"), "personal.locate-measures");
        assert!(id_ok("personal.locate-measures"));
        for bad in [
            "personal.",
            "personal.ab",
            "personal.Locate",
            "personal.a--b",
            "personal.a-",
            "personal.../x",
            "contoso.x",
            "personal.a.b",
        ] {
            assert!(!id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn the_hash_is_the_plugin_hash() {
        let mut all = YAML.as_bytes().to_vec();
        all.push(0);
        all.extend_from_slice(CODE.as_bytes());
        assert_eq!(
            content_hash(YAML.as_bytes(), CODE.as_bytes()),
            format!("sha256:{}", sha256_hex(&all))
        );
    }

    #[test]
    fn files_are_checked() {
        let id = "personal.locate-measures";
        assert!(check_files(id, YAML.as_bytes(), CODE.as_bytes()).is_ok());
        assert!(check_files("personal.other-name", YAML.as_bytes(), CODE.as_bytes()).is_err());
        for (from, to) in [
            ("entry: tool.py", "entry: worker.py"),
            ("runtime: python", "runtime: rust"),
            ("risk: read", "risk: write"),
            ("  run:", "  go:"),
        ] {
            let y = YAML.replace(from, to);
            assert!(
                check_files(id, y.as_bytes(), CODE.as_bytes()).is_err(),
                "{to}"
            );
        }
        for bad in [
            "Locate \\\"measures\\\"",
            "Locate\\nmeasures",
            "Locate\\u2028measures",
        ] {
            let y = YAML.replace("name: Locate measures", &format!("name: \"{bad}\""));
            assert!(
                check_files(id, y.as_bytes(), CODE.as_bytes()).is_err(),
                "{bad}"
            );
        }
        let y = format!("{YAML}confinement: {{ level: B }}\n");
        assert!(check_files(id, y.as_bytes(), CODE.as_bytes()).is_err());
        assert!(check_files(id, YAML.as_bytes(), b"x = 1 # \xe2\x80\xae").is_err());
        assert!(check_files(id, YAML.as_bytes(), b"a\rb").is_err());
    }

    #[test]
    fn text_that_shows_differently_is_refused() {
        assert!(plain_chars("a\tb\nc d").is_ok());
        for c in [
            '\u{FE0F}',
            '\u{FE00}',
            '\u{E0100}',
            '\u{E01EF}',
            '\u{180B}',
            '\u{180D}',
            '\u{2028}',
            '\u{2029}',
            '\u{FFF9}',
            '\u{FFFB}',
            '\u{202E}',
            '\u{200B}',
            '\u{E0041}',
            '\u{00AD}',
            '\r',
            '\u{1b}',
        ] {
            let t = format!("x = 1{c}2");
            assert!(plain_chars(&t).is_err(), "U+{:04X}", c as u32);
            assert!(plain_text(&t).is_err(), "U+{:04X}", c as u32);
            assert!(crate::canon::hidden_char(c), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn propose_install_change_remove() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let id = "personal.locate-measures";
        let info = json!({"proposed": "2026-11-05T10:00:00Z", "session": "s1"});
        write_proposal(root, id, YAML.as_bytes(), CODE.as_bytes(), &info).unwrap();
        let p = list_proposals(root);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].tool, "personal.locate-measures.run");
        assert_eq!(p[0].title, "Locate measures");
        assert_eq!(p[0].code.as_deref(), Some(CODE));
        assert!(p[0].replaces.is_none());
        let hash = p[0].hash.clone();

        // The person installs what they saw, not something changed since.
        assert!(install(root, id, "sha256:00", "now").is_err());
        let t = install(root, id, &hash, "2026-11-05T10:01:00Z").unwrap();
        assert_eq!(t.state.as_deref(), Some("ready"));
        assert_eq!(t.installed.as_deref(), Some("2026-11-05T10:01:00Z"));
        assert!(list_proposals(root).is_empty());
        assert_eq!(read_approvals(root)[id], hash);

        // An update is a new proposal, which says what it replaces.
        let code2 = "def run(ctx):\n    return {'v': 2}\n";
        write_proposal(root, id, YAML.as_bytes(), code2.as_bytes(), &info).unwrap();
        assert_eq!(
            list_proposals(root)[0].replaces.as_deref(),
            Some(hash.as_str())
        );
        decline(root, id).unwrap();
        assert!(list_proposals(root).is_empty());

        // A changed file switches it off.
        std::fs::write(root.join(id).join(ENTRY), code2).unwrap();
        assert_eq!(list_installed(root)[0].state.as_deref(), Some("changed"));
        remove(root, id).unwrap();
        assert!(list_installed(root).is_empty());
        assert!(!read_approvals(root).contains_key(id));
        assert!(remove(root, id).is_err());
    }

    #[test]
    fn proposals_are_capped() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..MAX_PROPOSALS {
            let id = format!("personal.tool-{i:02}");
            let y = YAML.replace("personal.locate-measures", &id);
            write_proposal(d.path(), &id, y.as_bytes(), CODE.as_bytes(), &json!({})).unwrap();
        }
        let y = YAML.replace("personal.locate-measures", "personal.one-more");
        assert!(write_proposal(
            d.path(),
            "personal.one-more",
            y.as_bytes(),
            CODE.as_bytes(),
            &json!({})
        )
        .is_err());
        // Replacing one already waiting is fine.
        let y = YAML.replace("personal.locate-measures", "personal.tool-00");
        write_proposal(
            d.path(),
            "personal.tool-00",
            y.as_bytes(),
            CODE.as_bytes(),
            &json!({}),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let id = "personal.locate-measures";
        write_proposal(root, id, YAML.as_bytes(), CODE.as_bytes(), &json!({})).unwrap();
        let hash = list_proposals(root)[0].hash.clone();
        let entry = root.join(PROPOSALS).join(id).join(ENTRY);
        std::fs::remove_file(&entry).unwrap();
        let elsewhere = root.join("elsewhere.py");
        std::fs::write(&elsewhere, CODE).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &entry).unwrap();
        assert!(install(root, id, &hash, "now").is_err());
    }
}
