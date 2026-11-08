//! Sharing a personal tool or a workflow with a coworker as one file.
//!
//! **Share** in the Columbus app saves a `.columbus` file: the tool's two
//! files (or the workflow's text and the personal tools it uses), who shared
//! it and when, and the hash of every file. No data travels: no rows, no
//! outputs, no connection details, no schedule.
//!
//! **Open a shared file** reads it back, and `columbus-mcp share import`
//! files what it holds as *proposals*, exactly as if the recipient's own
//! assistant had proposed them: the recipient reads every line in the app
//! and approves each with their own Touch ID or Windows Hello. Nothing in a
//! shared file is ever installed or run by opening it. The organization's
//! `allow_shared_tools` switch (off by default) decides whether shared
//! files can be opened at all.
//!
//! A workflow names the sharer's connections and export folder. On import
//! the recipient says which of theirs stands for each, and the workflow is
//! rewritten to use those before it is filed; it can't run until every name
//! is one of the recipient's own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::tools;
use crate::workflows as wf;

/// What the file says it is.
pub const FORMAT: &str = "columbus-share";
pub const VERSION: u32 = 1;
/// The file name ending the app saves and opens.
pub const EXTENSION: &str = "columbus";
/// Largest shared file read, in bytes: a workflow with its tools, as text.
pub const MAX_FILE: u64 = 4 * 1024 * 1024;
/// Most personal tools one shared workflow may bring.
pub const MAX_TOOLS: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareFile {
    pub format: String,
    pub version: u32,
    /// `tool` or `workflow`.
    pub kind: String,
    /// Who shared it (their sign-in name on their computer) and when.
    pub shared_by: String,
    pub shared_at: String,
    /// The Columbus version it was shared from.
    pub columbus: String,
    /// For a tool: that tool. For a workflow: the personal tools it uses.
    #[serde(default)]
    pub tools: Vec<SharedTool>,
    /// For a workflow: its `workflow.yaml`, without a schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// sha256 of the workflow's text, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_sha256: Option<String>,
    /// The sharer's signature over everything above, when signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ShareSignature>,
}

/// An Ed25519 signature over a shared file (see [`sign`]): the sharer's
/// public key and the signature, both hex.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareSignature {
    pub key: String,
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedTool {
    pub id: String,
    /// `plugin.yaml`, as installed.
    pub manifest: String,
    /// `tool.py`, as installed.
    pub code: String,
    /// The hash of the two (as `approved.yaml` holds it).
    pub hash: String,
}

fn shared_tool(root: &Path, id: &str) -> Result<SharedTool, String> {
    let t = tools::list_installed(root)
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("{id} is not one of your installed tools"))?;
    if t.state.as_deref() != Some("ready") {
        return Err(format!(
            "{id} isn't ready ({}), so it can't be shared",
            t.problem
                .clone()
                .or(t.state.clone())
                .unwrap_or_else(|| "not installed".into())
        ));
    }
    let dir = root.join(id);
    let manifest = read_text(&dir.join(tools::MANIFEST), tools::MAX_MANIFEST)?;
    let code = read_text(&dir.join(tools::ENTRY), tools::MAX_CODE)?;
    let hash = tools::content_hash(manifest.as_bytes(), code.as_bytes());
    // Exactly what the person installed.
    if tools::read_approvals(root).get(id) != Some(&hash) {
        return Err(format!(
            "{id}'s files are not the ones you installed, so it can't be shared"
        ));
    }
    Ok(SharedTool {
        id: id.to_string(),
        manifest,
        code,
        hash,
    })
}

fn read_text(p: &Path, max: usize) -> Result<String, String> {
    let b = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if b.len() > max {
        return Err(format!("{} is larger than allowed", p.display()));
    }
    String::from_utf8(b).map_err(|_| format!("{} is not UTF-8 text", p.display()))
}

/// A shared file for one installed, ready tool.
pub fn export_tool(
    tools_root: &Path,
    id: &str,
    by: &str,
    now: &str,
    columbus: &str,
) -> Result<ShareFile, String> {
    Ok(ShareFile {
        format: FORMAT.into(),
        version: VERSION,
        kind: "tool".into(),
        shared_by: by.into(),
        shared_at: now.into(),
        columbus: columbus.into(),
        tools: vec![shared_tool(tools_root, id)?],
        workflow: None,
        workflow_sha256: None,
        signature: None,
    })
}

/// A shared file for one installed, approved workflow, with the personal
/// tools it uses. Its schedule is left out: the recipient sets their own.
pub fn export_workflow(
    wf_root: &Path,
    tools_root: &Path,
    id: &str,
    by: &str,
    now: &str,
    columbus: &str,
) -> Result<ShareFile, String> {
    let (w, _fingerprint) =
        wf::ready(wf_root, tools_root, id).map_err(|e| format!("{id} can't be shared: {e}"))?;
    let mut w = w;
    // Never the sharer's schedule, nor whom they email.
    w.schedule = None;
    w.outputs.email = None;
    let text = to_text(&w)?;
    let mut shared = Vec::new();
    for t in w.personal_tools() {
        shared.push(shared_tool(tools_root, &t)?);
    }
    Ok(ShareFile {
        format: FORMAT.into(),
        version: VERSION,
        kind: "workflow".into(),
        shared_by: by.into(),
        shared_at: now.into(),
        columbus: columbus.into(),
        tools: shared,
        workflow_sha256: Some(crate::canon::sha256_hex(text.as_bytes())),
        workflow: Some(text),
        signature: None,
    })
}

/// A workflow as `workflow.yaml` text, checked to read back the same.
fn to_text(w: &wf::Workflow) -> Result<String, String> {
    let text = serde_yaml::to_string(w).map_err(|e| e.to_string())?;
    let back = wf::check(&text)?;
    if &back != w {
        return Err("the workflow does not read back the same".into());
    }
    Ok(text)
}

/// The file's JSON, as saved.
pub fn to_json(s: &ShareFile) -> Result<String, String> {
    serde_json::to_string_pretty(s).map_err(|e| e.to_string())
}

/// Read a shared file and check what can be checked without the server:
/// its shape, sizes, ids, every hash, and the workflow's text. Whatever it
/// says of itself (who shared it, the text) is the sharer's word, shown as
/// such; nothing here trusts it further.
pub fn read(path: &Path) -> Result<ShareFile, String> {
    read_bytes(path).and_then(|b| parse_bytes(&b))
}

/// A shared file's bytes, read once (at most [`MAX_FILE`]): hash and parse
/// these same bytes, so what is checked is what is recorded.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    // A regular file before it is opened (opening a named pipe would wait
    // for a writer), and opened without waiting in case it was swapped.
    let first = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !first.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let f = opts
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = f
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let mut bytes = Vec::new();
    f.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!(
            "{} is larger than a shared file can be ({} MB)",
            path.display(),
            MAX_FILE / 1024 / 1024
        ));
    }
    Ok(bytes)
}

pub fn parse_bytes(bytes: &[u8]) -> Result<ShareFile, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "this is not a Columbus shared file".to_string())?;
    parse(text)
}

pub fn parse(text: &str) -> Result<ShareFile, String> {
    let s: ShareFile = serde_json::from_str(text)
        .map_err(|e| format!("this is not a Columbus shared file ({e})"))?;
    if s.format != FORMAT {
        return Err("this is not a Columbus shared file".into());
    }
    if s.version != VERSION {
        return Err(format!(
            "this shared file is version {}, and this Columbus reads version {VERSION}: update Columbus",
            s.version
        ));
    }
    for (what, v, max) in [
        ("shared_by", &s.shared_by, 100),
        ("shared_at", &s.shared_at, 40),
        ("columbus", &s.columbus, 40),
    ] {
        if v.chars().count() > max || v.contains('\n') || tools::plain_chars(v).is_err() {
            return Err(format!("the shared file's {what} is not one plain line"));
        }
    }
    if s.tools.len() > MAX_TOOLS {
        return Err(format!("a shared file brings at most {MAX_TOOLS} tools"));
    }
    let mut seen = BTreeSet::new();
    for t in &s.tools {
        if !seen.insert(t.id.clone()) {
            return Err(format!("the shared file lists {} twice", t.id));
        }
        if t.manifest.len() > tools::MAX_MANIFEST || t.code.len() > tools::MAX_CODE {
            return Err(format!("{}: its files are larger than allowed", t.id));
        }
        tools::check_files(&t.id, t.manifest.as_bytes(), t.code.as_bytes())
            .map_err(|e| format!("{}: {e}", t.id))?;
        if tools::content_hash(t.manifest.as_bytes(), t.code.as_bytes()) != t.hash {
            return Err(format!(
                "{}: its files don't match the hash the shared file gives (changed after it was shared?)",
                t.id
            ));
        }
    }
    match s.kind.as_str() {
        "tool" => {
            if s.tools.len() != 1 || s.workflow.is_some() {
                return Err("a shared tool holds exactly one tool".into());
            }
        }
        "workflow" => {
            let text = s
                .workflow
                .as_deref()
                .ok_or("a shared workflow holds its workflow")?;
            if s.workflow_sha256.as_deref() != Some(&crate::canon::sha256_hex(text.as_bytes())) {
                return Err(
                    "the workflow doesn't match the hash the shared file gives (changed after it was shared?)"
                        .into(),
                );
            }
            let w = wf::check(text)?;
            if w.schedule.is_some() {
                return Err("a shared workflow has no schedule".into());
            }
            // It brings every personal tool it uses, and only those.
            let uses = w.personal_tools();
            let brings: BTreeSet<String> = s.tools.iter().map(|t| t.id.clone()).collect();
            if uses != brings {
                return Err(
                    "the shared workflow doesn't bring exactly the personal tools it uses".into(),
                );
            }
        }
        _ => return Err("a shared file holds a tool or a workflow".into()),
    }
    Ok(s)
}

/// What a shared workflow needs from the recipient before it can be filed:
/// a connection of theirs for each it names, and an export folder.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Needs {
    pub connections: Vec<String>,
    pub export: Option<String>,
}

pub fn needs(w: &wf::Workflow) -> Needs {
    let mut c: BTreeSet<String> = w.scope.connections.iter().cloned().collect();
    for s in &w.steps {
        c.extend(wf::step_connections(s));
    }
    Needs {
        connections: c.into_iter().collect(),
        export: w.outputs.folder.as_ref().map(|f| f.export.clone()),
    }
}

/// The workflow with the sharer's connection and export ids replaced by the
/// recipient's: `connections` maps every name `needs` lists, `export` is
/// the recipient's folder (required when the workflow exports). Returns the
/// new `workflow.yaml` text, checked.
pub fn remap(
    w: &wf::Workflow,
    connections: &BTreeMap<String, String>,
    export: Option<&str>,
) -> Result<String, String> {
    let need = needs(w);
    for c in &need.connections {
        if !connections.contains_key(c) {
            return Err(format!("say which of your connections stands for {c}"));
        }
    }
    if let Some(extra) = connections.keys().find(|k| !need.connections.contains(k)) {
        return Err(format!("the workflow doesn't name the connection {extra}"));
    }
    let mut w = w.clone();
    w.schedule = None;
    w.outputs.email = None;
    let map = |c: &str| connections.get(c).cloned().unwrap_or_else(|| c.to_string());
    let mut scope: Vec<String> = Vec::new();
    for c in &w.scope.connections {
        let m = map(c);
        if !scope.contains(&m) {
            scope.push(m);
        }
    }
    w.scope.connections = scope;
    for s in &mut w.steps {
        wf::rename_connections(s, &map);
    }
    match (&mut w.outputs.folder, export) {
        (Some(f), Some(e)) => f.export = e.to_string(),
        (Some(_), None) => return Err("say which of your export folders it exports to".into()),
        (None, Some(_)) => return Err("the workflow doesn't export to a folder".into()),
        (None, None) => {}
    }
    to_text(&w)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WF: &str = "id: crash-check\nname: Crash check\npurpose: Check the crash points\nscope:\n  connections: [tiger, roads]\nsteps:\n  - id: crashes\n    tool: query_features\n    args: {connection: tiger, dataset: crashes}\n  - id: routes\n    tool: query_features\n    args: {connection: roads, dataset: routes}\noutputs:\n  folder: {export: outbox, steps: [crashes]}\n";

    #[test]
    fn a_workflow_is_remapped_to_the_recipients_names() {
        let w = wf::check(WF).unwrap();
        assert_eq!(
            needs(&w),
            Needs {
                connections: vec!["roads".into(), "tiger".into()],
                export: Some("outbox".into())
            }
        );
        let map = BTreeMap::from([
            ("tiger".to_string(), "census".to_string()),
            ("roads".to_string(), "census".to_string()),
        ]);
        let text = remap(&w, &map, Some("shared")).unwrap();
        let r = wf::check(&text).unwrap();
        assert_eq!(r.scope.connections, vec!["census".to_string()]);
        assert!(r.steps.iter().all(|s| s.args["connection"] == "census"));
        assert_eq!(r.outputs.folder.unwrap().export, "shared");
        // Inside a step's inputs too.
        let nested = "id: nested\nname: Nested\nscope:\n  connections: [tiger, roads]\nsteps:\n  - id: a\n    tool: query_features\n    args: {connection: tiger, dataset: tracts}\n  - id: b\n    tool: vector_op\n    args: {op: clip, inputs: {layer: \"${a}\", mask: \"roads:zones\"}}\n  - id: c\n    tool: verify\n    args: {layer: {connection: roads, dataset: zones}}\n";
        let wn = wf::check(nested).unwrap();
        assert_eq!(
            needs(&wn).connections,
            vec!["roads".to_string(), "tiger".to_string()]
        );
        let m = BTreeMap::from([
            ("tiger".to_string(), "census".to_string()),
            ("roads".to_string(), "streets".to_string()),
        ]);
        let r = wf::check(&remap(&wn, &m, None).unwrap()).unwrap();
        assert_eq!(r.steps[1].args["inputs"]["mask"], "streets:zones");
        assert_eq!(r.steps[1].args["inputs"]["layer"], "${a}");
        assert_eq!(r.steps[2].args["layer"]["connection"], "streets");
        // Every name mapped, and only those.
        assert!(remap(&w, &BTreeMap::new(), Some("x")).is_err());
        let mut more = map.clone();
        more.insert("other".into(), "x".into());
        assert!(remap(&w, &more, Some("x")).is_err());
        assert!(remap(&w, &map, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_named_pipe_is_refused_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.columbus");
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_bytes(&p).unwrap_err().contains("not a file"));
        assert!(read_bytes(dir.path()).is_err());
    }

    #[test]
    fn a_shared_file_is_checked_before_anything_else() {
        let w = wf::check(WF).unwrap();
        let text = to_text(&w).unwrap();
        let good = ShareFile {
            format: FORMAT.into(),
            version: VERSION,
            kind: "workflow".into(),
            shared_by: "kevin".into(),
            shared_at: "2026-11-07T17:00:00-06:00".into(),
            columbus: "0.13.0".into(),
            tools: vec![],
            workflow_sha256: Some(crate::canon::sha256_hex(text.as_bytes())),
            workflow: Some(text.clone()),
            signature: None,
        };
        assert!(parse(&to_json(&good).unwrap()).is_ok());
        // Changed after sharing.
        let mut bad = good.clone();
        bad.workflow = Some(text.replace("Crash check", "Crash check 2"));
        assert!(parse(&to_json(&bad).unwrap()).is_err());
        // A schedule never travels.
        let mut sched = w.clone();
        sched.schedule = Some(wf::Schedule {
            days: wf::Days::Named("daily".into()),
            at: "06:00".into(),
        });
        let t2 = serde_yaml::to_string(&sched).unwrap();
        let mut bad = good.clone();
        bad.workflow_sha256 = Some(crate::canon::sha256_hex(t2.as_bytes()));
        bad.workflow = Some(t2);
        assert!(parse(&to_json(&bad).unwrap()).is_err());
        // Not ours, or a newer format.
        assert!(parse("{}").is_err());
        let mut v = good.clone();
        v.version = 2;
        assert!(parse(&to_json(&v).unwrap()).is_err());
        // A hidden character in who shared it.
        let mut h = good;
        h.shared_by = "kev\u{202e}in".into();
        assert!(parse(&to_json(&h).unwrap()).is_err());
    }
}

// ---- signatures --------------------------------------------------------
//
// A shared file can be signed with the sharer's own key (made on their
// computer the first time they share, kept in the system keychain). The
// recipient sees who signed it only as a key's fingerprint, until they
// choose to trust that key under a name (after the sharer reads them the
// fingerprint, by phone or in person), or their organization lists it.
// A signature says the file is exactly as that key's owner saved it; it
// never makes the contents trusted: everything in it is still reviewed and
// approved by the recipient.

const SIG_CONTEXT: &[u8] = b"columbus-share-signature-v1\n";

/// What is signed: a fixed prefix and the hash of the file's JSON without
/// its signature.
fn signed_message(s: &ShareFile) -> Result<Vec<u8>, String> {
    let mut u = s.clone();
    u.signature = None;
    let mut m = SIG_CONTEXT.to_vec();
    m.extend(crate::canon::sha256_hex(to_json(&u)?.as_bytes()).as_bytes());
    Ok(m)
}

/// A new signing key (PKCS#8 bytes, to keep in the keychain).
pub fn new_key() -> Result<Vec<u8>, String> {
    let rng = ring::rand::SystemRandom::new();
    ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
        .map(|k| k.as_ref().to_vec())
        .map_err(|_| "a signing key could not be made".to_string())
}

/// A signing key's public half, hex.
pub fn public_key(pkcs8: &[u8]) -> Result<String, String> {
    use ring::signature::KeyPair;
    let k = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| "the signing key is not readable".to_string())?;
    Ok(hex::encode(k.public_key().as_ref()))
}

/// Sign a shared file with a key (PKCS#8).
pub fn sign(s: &mut ShareFile, pkcs8: &[u8]) -> Result<(), String> {
    let k = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| "the signing key is not readable".to_string())?;
    let msg = signed_message(s)?;
    s.signature = Some(ShareSignature {
        key: public_key(pkcs8)?,
        sig: hex::encode(k.sign(&msg).as_ref()),
    });
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Signed {
    Unsigned,
    /// A good signature by this key (hex).
    Valid(String),
    /// A signature that doesn't match the file or isn't readable.
    Invalid,
}

pub fn verify(s: &ShareFile) -> Signed {
    let Some(sig) = &s.signature else {
        return Signed::Unsigned;
    };
    let (Ok(key), Ok(bytes), Ok(msg)) = (
        hex::decode(&sig.key),
        hex::decode(&sig.sig),
        signed_message(s),
    ) else {
        return Signed::Invalid;
    };
    // One spelling of a key only (lowercase hex), so it is found as trusted.
    if key.len() != 32 || bytes.len() != 64 || !key_ok(&sig.key) {
        return Signed::Invalid;
    }
    match ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, &key)
        .verify(&msg, &bytes)
    {
        Ok(()) => Signed::Valid(sig.key.clone()),
        Err(_) => Signed::Invalid,
    }
}

/// A key's fingerprint, as people read it to each other: the first 32 hex
/// digits (128 bits) of its SHA-256, in groups of four.
pub fn fingerprint(key_hex: &str) -> String {
    let h = crate::canon::sha256_hex(key_hex.to_ascii_lowercase().as_bytes()).to_ascii_uppercase();
    h.as_bytes()[..32]
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("-")
}

/// A key the person (or their organization) trusts, under a name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustedSender {
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub added: String,
}

fn key_ok(key: &str) -> bool {
    key.len() == 64
        && key
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Where the person's trusted senders are kept (beside their tools).
pub fn trust_file() -> std::path::PathBuf {
    let t = tools::tools_dir();
    t.parent()
        .map(Path::to_path_buf)
        .unwrap_or(t)
        .join("trusted-senders.yaml")
}

/// The person's trusted senders (unreadable entries left out).
pub fn trusted(file: &Path) -> Vec<TrustedSender> {
    read_trusted(file).unwrap_or_default()
}

/// The trusted senders file as it is, or why it can't be read (a file that
/// isn't there is an empty list).
fn read_trusted(file: &Path) -> Result<Vec<TrustedSender>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let all: Vec<TrustedSender> = serde_yaml::from_str(&text)
        .map_err(|e| format!("{} can't be read ({e}); fix or remove it", file.display()))?;
    Ok(all
        .into_iter()
        .filter(|t| key_ok(&t.key) && name_ok(&t.name))
        .collect())
}

/// Keys the organization vouches for, as the policy lists them, normalised
/// (lowercase); entries that aren't a key and a plain name are left out.
pub fn org_signers(listed: &[TrustedSender]) -> Vec<TrustedSender> {
    listed
        .iter()
        .map(|t| TrustedSender {
            name: t.name.trim().to_string(),
            key: t.key.trim().to_ascii_lowercase(),
            added: t.added.clone(),
        })
        .filter(|t| key_ok(&t.key) && name_ok(&t.name))
        .collect()
}

fn name_ok(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= 80
        && !name.contains('\n')
        && tools::plain_chars(name).is_ok()
}

/// Trust a key under a name (replacing an earlier name for it). A name
/// already given to another key, the person's or the organization's, is
/// refused: one name, one key.
pub fn trust(
    file: &Path,
    name: &str,
    key: &str,
    now: &str,
    org: &[TrustedSender],
) -> Result<(), String> {
    let name = name.trim();
    if !name_ok(name) {
        return Err("give the sender's name: one plain line of at most 80 characters".into());
    }
    if !key_ok(key) {
        return Err("that is not a sharing key".into());
    }
    let mut all = read_trusted(file)?;
    let same = |t: &TrustedSender| t.name.eq_ignore_ascii_case(name) && t.key != key;
    if let Some(t) = all.iter().find(|t| same(t)) {
        return Err(format!(
            "you already trust {} with another key ({}). If they have a new key, remove the old one first, after checking with them",
            t.name,
            fingerprint(&t.key)
        ));
    }
    if org_signers(org).iter().any(same) {
        return Err(format!(
            "your organization already vouches for {name} with another key; this is not that key"
        ));
    }
    all.retain(|t| t.key != key);
    all.push(TrustedSender {
        name: name.to_string(),
        key: key.to_string(),
        added: now.to_string(),
    });
    let text = serde_yaml::to_string(&all).map_err(|e| e.to_string())?;
    tools::write_atomic(file, text.as_bytes())
}

/// Stop trusting a key.
pub fn untrust(file: &Path, key: &str) -> Result<(), String> {
    let mut all = read_trusted(file)?;
    let n = all.len();
    all.retain(|t| t.key != key);
    if all.len() == n {
        return Err("that key is not one you trust".into());
    }
    let text = serde_yaml::to_string(&all).map_err(|e| e.to_string())?;
    tools::write_atomic(file, text.as_bytes())
}

/// Who signed a shared file, as the recipient should be told: `status` is
/// `unsigned`, `invalid`, `trusted` (with `name`, and `by`: `you` or
/// `organization`) or `unknown` (a good signature by a key not trusted).
pub fn signer(s: &ShareFile, mine: &[TrustedSender], org: &[TrustedSender]) -> serde_json::Value {
    use serde_json::json;
    match verify(s) {
        Signed::Unsigned => json!({"status": "unsigned"}),
        Signed::Invalid => json!({"status": "invalid"}),
        Signed::Valid(key) => {
            let fp = fingerprint(&key);
            let org = org_signers(org);
            if let Some(t) = org.iter().find(|t| t.key == key) {
                json!({"status": "trusted", "by": "organization", "name": t.name, "key": key, "fingerprint": fp})
            } else if let Some(t) = mine.iter().find(|t| t.key == key) {
                json!({"status": "trusted", "by": "you", "name": t.name, "key": key, "fingerprint": fp})
            } else {
                json!({"status": "unknown", "key": key, "fingerprint": fp})
            }
        }
    }
}

#[cfg(test)]
mod signing_tests {
    use super::*;

    fn file() -> ShareFile {
        ShareFile {
            format: FORMAT.into(),
            version: VERSION,
            kind: "tool".into(),
            shared_by: "kevin".into(),
            shared_at: "2026-11-08T09:00:00Z".into(),
            columbus: "0.14.0".into(),
            tools: vec![],
            workflow: None,
            workflow_sha256: None,
            signature: None,
        }
    }

    #[test]
    fn a_signature_covers_the_whole_file() {
        let key = new_key().unwrap();
        let mut f = file();
        assert_eq!(verify(&f), Signed::Unsigned);
        sign(&mut f, &key).unwrap();
        let pk = public_key(&key).unwrap();
        assert_eq!(verify(&f), Signed::Valid(pk.clone()));
        // Read back from its JSON, it still verifies.
        let back: ShareFile = serde_json::from_str(&to_json(&f).unwrap()).unwrap();
        assert_eq!(verify(&back), Signed::Valid(pk.clone()));
        // The same key in capitals is not a key Columbus reads.
        let mut upper = f.clone();
        upper.signature.as_mut().unwrap().key = pk.to_ascii_uppercase();
        assert_eq!(verify(&upper), Signed::Invalid);
        // Any change breaks it; so does another key's name on it.
        let mut changed = f.clone();
        changed.shared_by = "someone else".into();
        assert_eq!(verify(&changed), Signed::Invalid);
        let mut swapped = f.clone();
        swapped.signature.as_mut().unwrap().key = public_key(&new_key().unwrap()).unwrap();
        assert_eq!(verify(&swapped), Signed::Invalid);
        assert_eq!(fingerprint(&pk).len(), 39);
    }

    #[test]
    fn trusted_senders_are_named_keys() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("t.yaml");
        let pk = public_key(&new_key().unwrap()).unwrap();
        assert!(trust(&f, "Ana\u{202e}", &pk, "now", &[]).is_err());
        assert!(trust(&f, "Ana", "zz", "now", &[]).is_err());
        trust(&f, "Ana", &pk, "now", &[]).unwrap();
        trust(&f, "Ana Ruiz", &pk, "now", &[]).unwrap();
        // One name, one key: another key under a trusted name is refused.
        let other = public_key(&new_key().unwrap()).unwrap();
        assert!(trust(&f, "ana ruiz", &other, "now", &[]).is_err());
        let org = [TrustedSender {
            name: "Bea".into(),
            key: other.to_ascii_uppercase(),
            added: String::new(),
        }];
        assert!(trust(
            &f,
            "Bea",
            &public_key(&new_key().unwrap()).unwrap(),
            "now",
            &org
        )
        .is_err());
        let t = trusted(&f);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "Ana Ruiz");
        let mut s = file();
        let k = new_key().unwrap();
        sign(&mut s, &k).unwrap();
        assert_eq!(signer(&s, &t, &[])["status"], "unknown");
        trust(&f, "Bo", &public_key(&k).unwrap(), "now", &[]).unwrap();
        assert_eq!(signer(&s, &trusted(&f), &[])["name"], "Bo");
        untrust(&f, &public_key(&k).unwrap()).unwrap();
        assert_eq!(trusted(&f).len(), 1);
        // A file that can't be read is never written over.
        std::fs::write(&f, "{not: [a list").unwrap();
        assert!(trust(
            &f,
            "Cy",
            &public_key(&new_key().unwrap()).unwrap(),
            "now",
            &[]
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{not: [a list");
    }
}
