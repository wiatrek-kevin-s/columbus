//! Settings: the person's connections, the organization's switches for
//! edits, scripts and personal tools, and the person's own tools, edited
//! in the app instead of in files (the main window's Connections, Tools
//! and Settings › Organization pages, and the classic settings window).
//!
//! The assistant has no way to change these: settings are never a tool,
//! and only the app's own windows may call these commands
//! (capabilities/main.json, capabilities/settings.json).
//! Anything that loosens access (a new connection, read-write, more
//! datasets, a removed row filter, edits, scripts or personal tools turned
//! on, a tool installed) is confirmed
//! by the person at the keyboard first: Touch ID or the account password on
//! macOS, Windows Hello on Windows. Tightening and removing are not.
//!
//! Files are written whole, then moved into place, and checked by loading
//! them exactly as the server does; a file the server would refuse is never
//! left behind.

use std::path::{Path, PathBuf};

use columbus_shared::paths;
use columbus_shared::policy::{ArcgisAuth, ArcgisOAuth, Connection, DatasetRules, Mode, OrgPolicy};
use columbus_shared::tools;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Where passwords live: the server reads them under this service, by
/// connection id (`columbus-mcp secret set <id>` stores them the same way).
const SECRET_SERVICE: &str = "columbus";

/// Connection types the window can set up.
const KINDS: &[&str] = &[
    "gpkg",
    "sqlite",
    "shapefile",
    "filegdb",
    "rasters",
    "postgis",
    "sqlserver",
    "oracle",
    "files",
    "arcgis",
];

#[derive(Serialize)]
pub struct ConnView {
    file: String,
    #[serde(flatten)]
    conn: Option<Value>,
    has_password: bool,
    problem: Option<String>,
}

#[derive(Serialize)]
pub struct PolicyView {
    path: String,
    exists: bool,
    writable: bool,
    allow_edits: bool,
    allow_scripts: bool,
    allow_personal_tools: bool,
    allow_workflows: bool,
    allow_shared_tools: bool,
    connection_types: Vec<String>,
    problem: Option<String>,
}

#[derive(Serialize)]
pub struct SettingsState {
    connections_dir: String,
    connections: Vec<ConnView>,
    policy: PolicyView,
    /// How loosening changes are confirmed here.
    confirm_with: String,
    /// Connections folders running sessions read that are not this one
    /// (the extension's own setting can point elsewhere).
    other_folders: Vec<String>,
    /// A running session reading these files is too old to pick up changes
    /// by itself: the AI app must be restarted for it.
    restart_needed: bool,
}

#[derive(Deserialize, Debug)]
pub struct ConnForm {
    /// Set when editing an existing connection (its id cannot change).
    #[serde(default)]
    existing: bool,
    id: String,
    kind: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    database: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    sslmode: Option<String>,
    #[serde(default)]
    search_path: Option<String>,
    #[serde(default)]
    dsn: Option<String>,
    mode: String,
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default)]
    row_filter: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// A new password; None keeps the stored one.
    #[serde(default)]
    password: Option<String>,
    /// `files` (F-2): read subfolders too.
    #[serde(default)]
    recursive: bool,
    /// `files` (F-2): `EPSG:<code>` for CSV and Excel geometry.
    #[serde(default)]
    crs: Option<String>,
    /// `files` (F-2): the coordinate columns, both or neither.
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    /// `arcgis` (F-3): the service or layer address.
    #[serde(default)]
    url: Option<String>,
    /// `arcgis` (F-3): `none` (or empty) or `oauth`.
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    portal: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    page_size: Option<u64>,
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn trimmed(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Workflows' email as the organization set it up (no secret), and
/// whether the mail server's password is saved.
#[tauri::command]
pub fn email_settings() -> Value {
    let policy = OrgPolicy::load(&paths::policy_file()).unwrap_or_default();
    match policy.email {
        None => json!({"on": false}),
        Some(e) => json!({
            "on": true, "host": e.smtp_host, "port": e.smtp_port, "security": e.security,
            "from": e.from, "username": e.username, "allowed_domains": e.allowed_domains,
            "attachments": e.attachments, "max_attachment_mb": e.max_attachment_mb,
        }),
    }
}

/// Save (or, empty, forget) the mail server's password, for workflows'
/// emails, in the credential store Columbus's server reads.
#[tauri::command]
pub async fn email_password_set(password: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let e = secret_entry("columbus.email-smtp")?;
        if password.is_empty() {
            match e.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(err) => Err(err.to_string()),
            }
        } else {
            e.set_password(&password).map_err(|e| e.to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

fn secret_entry(id: &str) -> Result<keyring_core::Entry, String> {
    crate::chat::ensure_store()?;
    keyring_core::Entry::new(SECRET_SERVICE, id).map_err(|e| e.to_string())
}

/// The connections whose password this app stored, by id: a note beside
/// the connection files, so the Connections page can say "Password stored"
/// without reading every password from the credential store (on a Mac each
/// read can ask for the person's password). It holds no secret. A password
/// stored before this note existed shows as not noted until it's saved again.
fn password_note() -> PathBuf {
    paths::connections_dir().join(".columbus-passwords.json")
}

fn noted() -> Vec<String> {
    std::fs::read_to_string(password_note())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn note_password(id: &str, stored: bool) {
    let mut ids = noted();
    ids.retain(|i| i != id);
    if stored {
        ids.push(id.to_string());
    }
    ids.sort();
    if let Ok(t) = serde_json::to_string(&ids) {
        let _ = write_atomic(&password_note(), &t);
    }
}

fn has_password(id: &str) -> bool {
    noted().iter().any(|i| i == id)
}

/// Whether a secret may be stored under this name. Fails closed: an answer
/// other than "none" (the person declined the Keychain's prompt, say)
/// counts as one. No credential store at all means none can be.
fn secret_may_exist(id: &str) -> bool {
    let Ok(e) = secret_entry(id) else {
        return false;
    };
    !matches!(e.get_password(), Err(keyring_core::Error::NoEntry))
}

/// Every connection file in the folder, with the file it came from.
/// The ids of the person's connections that read.
pub(crate) fn connection_ids() -> Vec<String> {
    scan(&paths::connections_dir())
        .into_iter()
        .filter_map(|(_, c)| c.ok().map(|c| c.id))
        .collect()
}

fn scan(dir: &Path) -> Vec<(PathBuf, Result<Connection, String>)> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "yaml" || ext == "yml" {
            out.push((p.clone(), Connection::load(&p).map_err(|e| e.to_string())));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub(crate) fn writable(path: &Path) -> bool {
    if path.exists() {
        std::fs::OpenOptions::new().append(true).open(path).is_ok()
    } else {
        path.parent().is_some_and(|d| {
            let probe = d.join(".columbus-write-test");
            let ok = std::fs::write(&probe, b"").is_ok();
            let _ = std::fs::remove_file(&probe);
            ok
        })
    }
}

#[tauri::command]
pub async fn settings_state() -> Result<SettingsState, String> {
    tauri::async_runtime::spawn_blocking(state)
        .await
        .map_err(|e| e.to_string())
}

fn state() -> SettingsState {
    let dir = paths::connections_dir();
    let connections = scan(&dir)
        .into_iter()
        .map(|(file, c)| match c {
            Ok(c) => ConnView {
                file: file.display().to_string(),
                has_password: has_password(&c.id),
                conn: serde_json::to_value(&c).ok(),
                problem: None,
            },
            Err(e) => ConnView {
                file: file.display().to_string(),
                conn: None,
                has_password: false,
                problem: Some(e),
            },
        })
        .collect();
    let path = paths::policy_file();
    let (policy, problem) = if path.exists() {
        match OrgPolicy::load(&path) {
            Ok(p) => (p, None),
            Err(e) => (OrgPolicy::default(), Some(e.to_string())),
        }
    } else {
        (OrgPolicy::default(), None)
    };
    SettingsState {
        connections_dir: dir.display().to_string(),
        connections,
        policy: PolicyView {
            path: path.display().to_string(),
            exists: path.exists(),
            writable: writable(&path),
            allow_edits: policy.allow_edits,
            allow_scripts: policy.allow_scripts,
            allow_personal_tools: policy.allow_personal_tools,
            allow_workflows: policy.allow_workflows,
            allow_shared_tools: policy.allow_shared_tools,
            connection_types: policy.connection_types.clone(),
            problem,
        },
        confirm_with: confirm_with().into(),
        other_folders: other_folders(&dir),
        restart_needed: sessions_here(&dir).iter().any(|s| s.key.is_none()),
    }
}

/// A running Columbus session, as its live file announces it.
pub(crate) struct LiveSession {
    pub(crate) session: String,
    pub(crate) port: u16,
    /// Its key for the signed channel; sessions from before 0.6 have none
    /// (and do not pick up changed settings either).
    pub(crate) key: Option<String>,
    pub(crate) last_used: Option<String>,
    pub(crate) started: Option<String>,
    pub(crate) user: Option<String>,
    /// The settings files and folders the session reads (no secrets).
    pub(crate) settings: Value,
}

/// Every running session that answers, the one in use first.
pub(crate) fn live_all() -> Vec<LiveSession> {
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(crate::live_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "app.json" || !name.ends_with(".json") {
            continue;
        }
        let Some(v) = std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let Some(port) = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()) else {
            continue;
        };
        if port < 1024 || !crate::answers(port) {
            continue;
        }
        let key = std::fs::read_to_string(p.with_extension("key"))
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()));
        let s = |k: &str| v[k].as_str().map(String::from);
        out.push(LiveSession {
            session: s("session").unwrap_or_default(),
            port,
            key,
            last_used: s("last_used"),
            started: s("started"),
            user: s("user"),
            settings: v["settings"].clone(),
        });
    }
    out.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| b.started.cmp(&a.started))
    });
    out
}

/// The running sessions reading `dir`, the one in use first.
fn sessions_here(dir: &Path) -> Vec<LiveSession> {
    let mine = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    live_all()
        .into_iter()
        .filter(|s| {
            s.settings["connections"].as_str().is_some_and(|c| {
                Path::new(c)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(c))
                    == mine
            })
        })
        .collect()
}

/// Ask a running session to try the connection in the form, exactly as it
/// would use it. Nothing is saved and nothing needs confirming: a typed
/// password goes only to that session's engine, and a stored one is used
/// only where it already goes (the session checks).
#[tauri::command]
pub async fn settings_test_connection(form: ConnForm) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || test_connection(form))
        .await
        .map_err(|e| e.to_string())?
}

fn test_connection(form: ConnForm) -> Result<Value, String> {
    let dir = paths::connections_dir();
    let found = scan(&dir);
    let old = found
        .iter()
        .filter_map(|(_, c)| c.as_ref().ok())
        .find(|c| form.existing && c.id.eq_ignore_ascii_case(&form.id));
    let conn = build(&form, old)?;
    let body = serde_json::to_vec(&json!({
        "connection": conn,
        "password": form.password.as_deref().filter(|p| !p.is_empty()),
    }))
    .map_err(|e| e.to_string())?;
    let (code, reply) = signed_post(&dir, "/api/test-connection", &body, "test connections")?;
    match code {
        200 => Ok(reply),
        429 => Err("A test is already running; wait for it to finish.".into()),
        413 => Err("The form is too long to test (shorten the description or filter).".into()),
        _ => Err(format!(
            "The Columbus session could not run the test ({code}: {}).",
            reply["error"].as_str().unwrap_or("no reason given")
        )),
    }
}

/// Draft the person's notes about a saved connection, from its database,
/// by a running session (the person's own action: no confirmation, since
/// it gives the assistant no more access; it only writes notes).
#[tauri::command]
pub async fn settings_draft_notes(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = paths::connections_dir();
        if !scan(&dir)
            .iter()
            .any(|(_, c)| c.as_ref().is_ok_and(|c| c.id == id))
        {
            return Err(format!("there is no connection {id:?}"));
        }
        let body = serde_json::to_vec(&json!({"connection": id})).map_err(|e| e.to_string())?;
        let (code, reply) = signed_post(&dir, "/api/draft-notes", &body, "draft notes")?;
        match code {
            200 => Ok(reply),
            429 => Err("A test or a draft is already running; wait for it to finish.".into()),
            404 => Err("The Columbus running now is too old to draft notes. Update the Columbus extension in Claude desktop, then try again.".into()),
            _ => Err(format!(
                "The Columbus session could not draft notes ({code}: {}).",
                reply["error"].as_str().unwrap_or("no reason given")
            )),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// POST `body` to `path` on a running session that reads `dir`: first the
/// session proves it holds the key in its `live/<session>.key` (so a
/// program that merely took over a port it used gets nothing), then the
/// request goes with the app's proof over the session's challenge and the
/// exact body. The key is never sent.
fn signed_post(dir: &Path, path: &str, body: &[u8], what: &str) -> Result<(u16, Value), String> {
    let sessions = sessions_here(dir);
    if sessions.is_empty() {
        return Err(format!("No Columbus session is running to {what} with. Open Claude desktop (it starts Columbus), then try again."));
    }
    let mut last = format!("The Columbus running now is too old to {what}. Update the Columbus extension in Claude desktop, then try again.");
    for s in &sessions {
        match hello(s) {
            // The first session that proves itself gets the request, and
            // its answer is final (a slow draft is never sent twice).
            Ok(proven) => return send_signed(s, &proven, path, body),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// POST `body` to `path` on one running session over the signed channel
/// (see [`signed_post`]).
pub(crate) fn signed_post_to(
    s: &LiveSession,
    path: &str,
    body: &[u8],
) -> Result<(u16, Value), String> {
    let proven = hello(s)?;
    send_signed(s, &proven, path, body)
}

/// A session that proved it holds its key: the key and its challenge.
struct Proven {
    key: String,
    challenge: String,
}

/// The first half of the signed channel: the session proves it holds the
/// key in its `live/<session>.key` and gives a challenge.
fn hello(s: &LiveSession) -> Result<Proven, String> {
    use columbus_shared::proof;
    let Some(key) = &s.key else {
        return Err("The Columbus running in this session is too old for this. Update the Columbus extension in your AI app, then try again.".into());
    };
    // From the operating system's secure random source.
    let mine = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let hello_header = format!(
        "{mine}.{}",
        proof::prove(key, proof::HELLO, &[mine.as_bytes()])
    );
    let (code, hello) = post_local(
        s.port,
        "/api/test-hello",
        &[("X-Columbus-Test-Hello", &hello_header)],
        b"",
    )?;
    let (Some(challenge), Some(theirs)) = (hello["challenge"].as_str(), hello["proof"].as_str())
    else {
        return Err(format!("The Columbus session did not answer ({code})."));
    };
    if code != 200
        || !proof::same(
            theirs,
            &proof::prove(key, proof::SERVER, &[mine.as_bytes()]),
        )
    {
        return Err("A program on this computer answered for Columbus but could not prove it is your Columbus session, so nothing was sent to it.".into());
    }
    Ok(Proven {
        key: key.clone(),
        challenge: challenge.to_string(),
    })
}

/// The request itself, with the app's proof over the challenge and the
/// exact body. The key is never sent.
fn send_signed(
    s: &LiveSession,
    p: &Proven,
    path: &str,
    body: &[u8],
) -> Result<(u16, Value), String> {
    let sig = columbus_shared::proof::app_proof(&p.key, "POST", path, &p.challenge, body);
    let header = format!("{}.{sig}", p.challenge);
    post_local(s.port, path, &[("X-Columbus-Test", &header)], body)
}

/// A small HTTP/1.1 POST to a session's page on this computer, waiting at
/// most a minute in all for its answer (a database can be slow to answer).
pub(crate) fn post_local(
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, Value), String> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::{Duration, Instant};
    const MAX: usize = 64 * 1024;
    // Drafting notes for a large database takes longer than a test.
    let deadline =
        Instant::now() + Duration::from_secs(if path == "/api/draft-notes" { 600 } else { 60 });
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(800))
        .map_err(|e| format!("could not reach the Columbus session: {e}"))?;
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())
        .and_then(|_| s.write_all(body))
        .map_err(|e| format!("could not reach the Columbus session: {e}"))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the Columbus session did not answer in time".into());
        }
        let _ = s.set_read_timeout(Some(left));
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > MAX {
                    return Err("the Columbus session's answer was too long".into());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err("the Columbus session did not answer in time".into()),
        }
    }
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("the Columbus session gave no answer")?;
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let rest = &buf[split + 4..];
    let code = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or("the Columbus session gave no answer")?;
    let chunked = head
        .lines()
        .any(|l| l.to_ascii_lowercase().replace(' ', "") == "transfer-encoding:chunked");
    let body = if chunked {
        dechunk(rest)
    } else {
        rest.to_vec()
    };
    Ok((code, serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

/// The body of a chunked HTTP answer, as bytes.
fn dechunk(mut s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(eol) = s.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&s[..eol])
            .ok()
            .map(|t| t.split(';').next().unwrap_or("").trim());
        let Some(n) = size.and_then(|t| usize::from_str_radix(t, 16).ok()) else {
            break;
        };
        let rest = &s[eol + 2..];
        if n == 0 || rest.len() < n {
            break;
        }
        out.extend_from_slice(&rest[..n]);
        s = rest[n..].strip_prefix(b"\r\n").unwrap_or(&rest[n..]);
    }
    out
}

/// The connections folders running sessions say they read, other than `dir`.
fn other_folders(dir: &Path) -> Vec<String> {
    let mine = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut out = vec![];
    let Ok(entries) = std::fs::read_dir(crate::live_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name().and_then(|n| n.to_str()) == Some("app.json") {
            continue;
        }
        let Some(v) = std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        if let Some(c) = v["settings"]["connections"].as_str() {
            let theirs = Path::new(c)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(c));
            if theirs != mine && !out.contains(&c.to_string()) {
                out.push(c.to_string());
            }
        }
    }
    out
}

/// A connection from the form, on top of the one it replaces (limits and
/// expiry are kept; the window does not edit them).
fn build(form: &ConnForm, old: Option<&Connection>) -> Result<Connection, String> {
    if !id_ok(&form.id) {
        return Err("the name may use letters, digits, - and _ (at most 40), starting with a letter or digit".into());
    }
    if !KINDS.contains(&form.kind.as_str()) {
        return Err(format!("unknown connection type {:?}", form.kind));
    }
    let mode: Mode = serde_json::from_value(json!(form.mode))
        .map_err(|_| "mode must be read or read-write".to_string())?;
    if matches!(form.kind.as_str(), "shapefile" | "filegdb") && mode != Mode::Read {
        return Err("shapefiles and file geodatabases are read only in Columbus".into());
    }
    if matches!(form.kind.as_str(), "files" | "arcgis") && mode != Mode::Read {
        return Err("folders of files and ArcGIS services are read only in Columbus".into());
    }
    let file_kind = matches!(
        form.kind.as_str(),
        "gpkg" | "sqlite" | "shapefile" | "filegdb" | "rasters" | "files"
    );
    let arcgis = form.kind == "arcgis";
    let path = trimmed(&form.path);
    if file_kind {
        let p = path.as_deref().ok_or("choose the file or folder")?;
        let pb = Path::new(p);
        if !pb.is_absolute() {
            return Err("the location must be a full path".into());
        }
        match form.kind.as_str() {
            "rasters" if !pb.is_dir() => return Err(format!("{p} is not a folder")),
            "filegdb"
                if !(pb.is_dir()
                    && pb
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("gdb"))) =>
            {
                return Err(format!(
                    "{p} is not a file geodatabase (a folder whose name ends in .gdb)"
                ))
            }
            "shapefile"
                if !(pb.is_dir()
                    || (pb.is_file()
                        && pb
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("shp")))) =>
            {
                return Err(format!("{p} is not a folder of shapefiles or a .shp file"))
            }
            "gpkg" | "sqlite" if !pb.is_file() => return Err(format!("{p} is not a file")),
            "files" if !(pb.is_dir() || pb.is_file()) => {
                return Err(format!("{p} is not a folder or a file"))
            }
            _ => {}
        }
    } else if arcgis {
        if trimmed(&form.url).is_none() {
            return Err("give the service's address (ending in FeatureServer or MapServer, or a layer number)".into());
        }
    } else if form.kind == "oracle" {
        if trimmed(&form.dsn).is_none() && trimmed(&form.host).is_none() {
            return Err("give the database's service name (DSN) or host".into());
        }
    } else if trimmed(&form.host).is_none() || trimmed(&form.database).is_none() {
        return Err("give the server's host and the database name".into());
    }
    let allow: Vec<String> = form
        .allow
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let deny: Vec<String> = form
        .deny
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let datasets = DatasetRules {
        allow: if allow.is_empty() {
            vec!["*".into()]
        } else {
            allow
        },
        deny,
    };
    datasets.permits("").map_err(|e| e.to_string())?;
    let files = form.kind == "files";
    let db = !file_kind && !arcgis;
    let auth = if !arcgis {
        None
    } else {
        match trimmed(&form.auth).as_deref() {
            None | Some("none") => None,
            Some("oauth") => Some(ArcgisAuth::OAuth(ArcgisOAuth {
                kind: "oauth".into(),
                portal: trimmed(&form.portal)
                    .ok_or("give the portal's address for signing in, e.g. https://www.arcgis.com")?,
                client_id: trimmed(&form.client_id)
                    .ok_or("give your organization's ArcGIS app client id (your ArcGIS administrator has it)")?,
            })),
            Some(other) => return Err(format!("sign-in must be none or oauth, not {other:?}")),
        }
    };
    let conn = Connection {
        id: form.id.clone(),
        kind: form.kind.clone(),
        dsn: trimmed(&form.dsn).filter(|_| db),
        path: path.filter(|_| file_kind),
        host: trimmed(&form.host).filter(|_| db),
        port: form.port.filter(|_| db),
        database: trimmed(&form.database).filter(|_| db),
        user: trimmed(&form.user).filter(|_| db),
        sslmode: trimmed(&form.sslmode).filter(|_| form.kind == "postgis"),
        search_path: trimmed(&form.search_path).filter(|_| form.kind == "postgis"),
        mode,
        datasets,
        row_filter: trimmed(&form.row_filter),
        limits: old.map(|o| o.limits).unwrap_or_default(),
        expires: old.and_then(|o| o.expires),
        description: trimmed(&form.description),
        // The files settings (F-2), kept when editing (T-12).
        recursive: (files && form.recursive).then_some(true),
        crs: trimmed(&form.crs).filter(|_| files),
        x: trimmed(&form.x).filter(|_| files),
        y: trimmed(&form.y).filter(|_| files),
        // The arcgis settings (F-3).
        url: trimmed(&form.url).filter(|_| arcgis),
        auth,
        page_size: form.page_size.filter(|_| arcgis),
    };
    // The server's own checks (F-2, F-3), in the form's words.
    conn.validate(Path::new("the form"))
        .map_err(|e| e.to_string().replace("the form: ", ""))?;
    Ok(conn)
}

/// Whether going from `old` to `new` gives access to anything `old` did
/// not: then the person confirms it. Only a description change, a lower
/// mode, fewer allowed patterns or more denied ones count as no loosening.
pub(crate) fn loosens(old: Option<&Connection>, new: &Connection) -> bool {
    let Some(old) = old else { return true };
    let same = |a: &Connection, b: &Connection| {
        let strip = |c: &Connection| {
            let mut v = serde_json::to_value(c).unwrap_or(Value::Null);
            if let Some(o) = v.as_object_mut() {
                for k in ["description", "mode", "datasets", "row_filter"] {
                    o.remove(k);
                }
            }
            v
        };
        strip(a) == strip(b)
    };
    if !same(old, new) {
        return true;
    }
    if new.mode == Mode::ReadWrite && old.mode != Mode::ReadWrite {
        return true;
    }
    if !new
        .datasets
        .allow
        .iter()
        .all(|p| old.datasets.allow.contains(p))
    {
        return true;
    }
    if !old
        .datasets
        .deny
        .iter()
        .all(|p| new.datasets.deny.contains(p))
    {
        return true;
    }
    // Any change of row filter: even a new one can reveal what it tests,
    // by the rows it keeps.
    old.row_filter != new.row_filter
}

/// One change at a time, across the window's commands.
pub(crate) static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write a file whole (a fresh temporary file beside it, flushed to disk),
/// then move it into place, keeping the old file's permissions. A link is
/// not followed: the window will not change a file that lives elsewhere.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let show = |e: std::io::Error| format!("{}: {e}", path.display());
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!(
            "{} is a link to another file; change that file instead",
            path.display()
        ));
    }
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut b = [0u8; 8];
    getrandom_bytes(&mut b);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("settings");
    let tmp = dir.join(format!(
        ".{name}.{}.tmp",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(show)?;
        f.write_all(text.as_bytes()).map_err(show)?;
        f.sync_all().map_err(show)?;
        if let Ok(m) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&tmp, m.permissions());
        }
        std::fs::rename(&tmp, path).map_err(show)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Random bytes for a temporary file's name (uniqueness, not secrecy).
fn getrandom_bytes(b: &mut [u8]) {
    use std::hash::{BuildHasher, Hasher};
    for chunk in b.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        let v = h.finish().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
}

/// What a change gives access to, in words, for the confirmation prompt.
fn widening(old: Option<&Connection>, new: &Connection, password: bool) -> String {
    let place = new
        .path
        .clone()
        .or_else(|| {
            new.host.clone().map(|h| {
                format!(
                    "{h}{}",
                    new.database
                        .as_deref()
                        .map(|d| format!("/{d}"))
                        .unwrap_or_default()
                )
            })
        })
        .or_else(|| new.dsn.clone())
        .or_else(|| new.url.clone())
        .unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    match old {
        None => parts.push(format!(
            "add the connection {} ({}, {}), {}",
            new.id,
            place,
            if new.mode == Mode::ReadWrite {
                "read and write"
            } else {
                "read only"
            },
            if new.datasets.allow == ["*"] {
                "all datasets".to_string()
            } else {
                format!("datasets {}", new.datasets.allow.join(", "))
            }
        )),
        Some(o) => {
            let mut a = serde_json::to_value(o).unwrap_or(Value::Null);
            let mut b = serde_json::to_value(new).unwrap_or(Value::Null);
            for v in [&mut a, &mut b] {
                if let Some(m) = v.as_object_mut() {
                    for k in ["description", "mode", "datasets", "row_filter"] {
                        m.remove(k);
                    }
                }
            }
            if a != b {
                parts.push(format!("point {} at {place}", new.id));
            }
            if new.mode == Mode::ReadWrite && o.mode != Mode::ReadWrite {
                parts.push(format!("make {} read and write", new.id));
            }
            let more: Vec<&String> = new
                .datasets
                .allow
                .iter()
                .filter(|p| !o.datasets.allow.contains(p))
                .collect();
            if !more.is_empty() {
                parts.push(format!(
                    "allow {}",
                    more.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let fewer: Vec<&String> = o
                .datasets
                .deny
                .iter()
                .filter(|p| !new.datasets.deny.contains(p))
                .collect();
            if !fewer.is_empty() {
                parts.push(format!(
                    "stop refusing {}",
                    fewer
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if o.row_filter != new.row_filter {
                parts.push(match &new.row_filter {
                    Some(f) => format!("set the row filter to {f}"),
                    None => "remove the row filter".into(),
                });
            }
        }
    }
    if password {
        parts.push(format!("store a new password for {}", new.id));
    }
    parts.join("; ")
}

/// Credential names that connected servers' manifests use (`auth:
/// {credential: …}`), lower case.
fn manifest_credentials() -> Vec<String> {
    let mut out = vec![];
    for d in paths::servers_dirs() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Some(v) = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
            else {
                continue;
            };
            if let Some(c) = v
                .get("auth")
                .and_then(|a| a.get("credential"))
                .and_then(|c| c.as_str())
            {
                out.push(c.to_lowercase());
            }
        }
    }
    out
}

/// A row filter must not name a field the organization hides: counting the
/// rows it keeps would reveal the hidden values.
fn filter_uses_hidden(filter: &str, hidden: &[String]) -> Option<String> {
    if hidden.is_empty() {
        return None;
    }
    let rules = DatasetRules {
        allow: hidden.to_vec(),
        deny: vec![],
    };
    let mut word = String::new();
    let mut words = vec![];
    for c in filter.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' || c == '$' || c == '#' {
            word.push(c);
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
    }
    words
        .into_iter()
        .find(|w| rules.permits(w).unwrap_or(false))
}

#[tauri::command]
pub async fn settings_save_connection(form: ConnForm) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || save_connection(form))
        .await
        .map_err(|e| e.to_string())?
}

fn save_connection(form: ConnForm) -> Result<String, String> {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = paths::connections_dir();
    let found = scan(&dir);
    let existing = found.iter().find(|(_, c)| {
        c.as_ref()
            .is_ok_and(|c| c.id.eq_ignore_ascii_case(&form.id))
    });
    if form.existing != existing.is_some() {
        return Err(if form.existing {
            format!("there is no connection {:?} any more", form.id)
        } else {
            format!("a connection named {:?} already exists", form.id)
        });
    }
    let old = existing.and_then(|(_, c)| c.as_ref().ok());
    let new = build(&form, old)?;
    // Folders of files and ArcGIS services use no password (an ArcGIS
    // sign-in is kept under arcgis:<id>, by Sign in).
    let uses_password = !matches!(new.kind.as_str(), "files" | "arcgis");
    let password = form
        .password
        .as_deref()
        .filter(|p| !p.is_empty() && uses_password);
    // A password left under this name (by an earlier connection, the
    // command line, or a connected server's key) would go to the new place.
    if old.is_none() && manifest_credentials().contains(&new.id.to_lowercase()) {
        return Err(format!(
            "{:?} is the name of a connected server's key; choose another name",
            new.id
        ));
    }
    if old.is_none() && uses_password && password.is_none() && secret_may_exist(&new.id) {
        return Err(format!(
            "a password named {:?} is already stored on this computer (from an earlier connection or a connected server). Enter this connection's password, or choose another name",
            new.id
        ));
    }
    let hidden = paths::policy_file();
    let hide_fields = OrgPolicy::load(&hidden)
        .map(|p| p.hide_fields)
        .unwrap_or_default();
    if let Some(f) = new.row_filter.as_deref() {
        if let Some(w) = filter_uses_hidden(f, &hide_fields) {
            return Err(format!(
                "the row filter uses {w}, a field your organization hides"
            ));
        }
    }
    let file = existing
        .map(|(p, _)| p.clone())
        .unwrap_or_else(|| dir.join(format!("{}.yaml", new.id)));
    let previous = std::fs::read_to_string(&file).ok();
    if loosens(old, &new) || password.is_some() {
        confirm(&widening(old, &new, password.is_some()))?;
        // Nothing else changed it while the person was asked.
        if std::fs::read_to_string(&file).ok() != previous {
            return Err("the connection changed while you were asked; nothing was saved. Look again and save".into());
        }
    }
    let text = format!(
        "# Written by the Columbus app. The password, if any, is in the system's credential store.\n{}",
        serde_yaml::to_string(&new).map_err(|e| e.to_string())?
    );
    write_atomic(&file, &text)?;
    // The server loads the whole folder; one bad file would stop it.
    if let Err(e) = Connection::load_dir(&dir) {
        match previous {
            Some(p) => {
                let _ = write_atomic(&file, &p);
            }
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
        return Err(format!("not saved: the server would refuse it: {e}"));
    }
    if let Some(p) = password {
        secret_entry(&new.id)?.set_password(p).map_err(|e| {
            format!("the connection is saved, but its password could not be stored: {e}")
        })?;
        note_password(&new.id, true);
    }
    Ok(file.display().to_string())
}

#[tauri::command]
pub async fn settings_delete_connection(id: String, forget_password: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = paths::connections_dir();
        let (file, _) = scan(&dir)
            .into_iter()
            .find(|(_, c)| c.as_ref().is_ok_and(|c| c.id == id))
            .ok_or_else(|| format!("there is no connection {id:?}"))?;
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        // An ArcGIS connection's sign-in goes with it (F-3b), whatever
        // forget_password says.
        if let Ok(e) = secret_entry(&format!("arcgis:{id}")) {
            let _ = e.delete_credential();
        }
        // And the record of when that sign-in ends (never a token).
        let _ = columbus_shared::arcgis_state::forget(&dir, &id);
        // A connected server's key of the same name stays.
        if forget_password && !manifest_credentials().contains(&id.to_lowercase()) {
            if let Ok(e) = secret_entry(&id) {
                let _ = e.delete_credential();
            }
            note_password(&id, false);
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// One ArcGIS sign-in from the app at a time.
static SIGNING_IN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long the app waits for a sign-in: Columbus's own wait for the
/// browser (5 minutes), the code exchange (30 s) and a margin.
const SIGNIN_LIMIT: std::time::Duration = std::time::Duration::from_secs(420);

/// Sign in to an `arcgis` connection with `auth: oauth` (REQ-11, T-11):
/// after the person confirms at the keyboard, Columbus's own server runs
/// `columbus-mcp arcgis signin <id>` (PKCE, a one-shot 127.0.0.1 redirect)
/// and prints the portal's sign-in address, which the app opens in the
/// person's browser. The tokens go from that program to the credential
/// store; the app never sees them. Returns the outcome in plain words.
#[tauri::command]
pub async fn settings_arcgis_signin(app: tauri::AppHandle, id: String) -> Result<String, String> {
    let (server, engine) = crate::registry::program(&app)?;
    tauri::async_runtime::spawn_blocking(move || arcgis_signin(&server, engine.as_deref(), &id))
        .await
        .map_err(|e| e.to_string())?
}

/// The portal the saved `arcgis` connection `id` signs in at.
fn signin_portal(id: &str) -> Result<String, String> {
    let conn = scan(&paths::connections_dir())
        .into_iter()
        .find_map(|(_, c)| c.ok().filter(|c| c.id == id))
        .ok_or_else(|| format!("there is no connection {id:?}; save it first"))?;
    if !columbus_shared::policy::is_arcgis(&conn.kind) {
        return Err(format!(
            "{id} is not an ArcGIS connection, so it has no ArcGIS sign-in"
        ));
    }
    conn.auth
        .as_ref()
        .and_then(ArcgisAuth::oauth)
        .map(|o| o.portal.trim().trim_end_matches('/').to_string())
        .ok_or_else(|| {
            format!("{id} is a public ArcGIS service (sign-in: none); it needs no sign-in")
        })
}

/// Whether `url` is the sign-in page of `portal`: under it, https (or, in
/// test builds reaching a local fake portal, http to this computer).
fn signin_url_ok(url: &str, portal: &str) -> bool {
    let https = url.starts_with("https://");
    let local = columbus_shared::policy::arcgis_test_loopback()
        && (url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:"));
    (https || local)
        && url.starts_with(&format!("{portal}/"))
        && url.contains("/oauth2/authorize?")
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn arcgis_signin(server: &Path, engine: Option<&Path>, id: &str) -> Result<String, String> {
    use std::io::{BufRead, Read};
    use std::sync::atomic::Ordering;
    let portal = signin_portal(id)?;
    if SIGNING_IN.swap(true, Ordering::SeqCst) {
        return Err("A sign-in is already waiting in your browser: finish it there, or wait a few minutes for it to end.".into());
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            SIGNING_IN.store(false, Ordering::SeqCst);
        }
    }
    let _done = Done;
    confirm(&format!(
        "sign in to ArcGIS at {portal} for {id}, so your assistant can read the layers shared with you there"
    ))?;
    let mut cmd = std::process::Command::new(server);
    cmd.args(["arcgis", "signin", id])
        .env("COLUMBUS_SIGNIN_BROWSER", "print")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(e) = engine {
        cmd.env("COLUMBUS_PYTHON", e);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", server.display()))?;
    let out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut err).take(64 << 10).read_to_string(&mut s);
        let _ = err_tx.send(s);
    });
    let start = std::time::Instant::now();
    let mut lines = vec![];
    let mut opened = false;
    let status = loop {
        while let Ok(line) = rx.try_recv() {
            if let Some(url) = line.strip_prefix("open: ") {
                let url = url.trim();
                if opened || !signin_url_ok(url, &portal) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Columbus gave an unexpected sign-in address, so nothing was opened. Sign in from a terminal instead: columbus-mcp arcgis signin ".to_string() + id);
                }
                opened = true;
                if let Err(e) = open_browser(url) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Your browser could not be opened ({e}). Sign in from a terminal instead: columbus-mcp arcgis signin {id}"));
                }
            } else {
                lines.push(line);
            }
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() < SIGNIN_LIMIT => {
                std::thread::sleep(std::time::Duration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("The sign-in did not finish in time. Press Sign in to try again, or sign in from a terminal with: columbus-mcp arcgis signin {id} --paste-code"));
            }
        }
    };
    // What is left of its output.
    while let Ok(line) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
        if !line.starts_with("open: ") {
            lines.push(line);
        }
    }
    let stderr = err_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap_or_default();
    if status.success() {
        return Ok(lines
            .iter()
            .rev()
            .find(|l| l.starts_with("Signed in"))
            .cloned()
            .unwrap_or_else(|| format!("Signed in for {id}.")));
    }
    let why = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.strip_prefix("Error: ").unwrap_or(l).to_string())
        .unwrap_or_else(|| "Columbus did not say why".into());
    if why.contains("no code was entered") {
        return Err(format!("The browser did not come back to Columbus. Press Sign in to try again, or sign in from a terminal with: columbus-mcp arcgis signin {id} --paste-code"));
    }
    Err(format!("Not signed in: {why}"))
}

/// Open `url` in the person's browser.
fn open_browser(url: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let mut c = Command::new(Path::new(&root).join("System32").join("rundll32.exe"));
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/open");
        c.arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Turn the organization's edit or script switch on or off. On is confirmed
/// first; the rest of the file, comments included, is kept as it is.
#[tauri::command]
pub async fn settings_set_switch(name: String, on: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || set_switch(&name, on))
        .await
        .map_err(|e| e.to_string())?
}

fn set_switch(name: &str, on: bool) -> Result<(), String> {
    if !matches!(
        name,
        "allow_edits"
            | "allow_scripts"
            | "allow_personal_tools"
            | "allow_workflows"
            | "allow_shared_tools"
    ) {
        return Err(format!("unknown setting {name:?}"));
    }
    let path = paths::policy_file();
    if !writable(&path) {
        return Err(format!(
            "{} is managed by your organization (this account cannot change it)",
            path.display()
        ));
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // Only a file that exists: where there is none, Columbus's strict
    // defaults apply, and an organization may rely on that (on Windows any
    // user could otherwise create one in ProgramData).
    let Ok(before) = std::fs::read_to_string(&path) else {
        return Err(format!(
            "there is no organization settings file at {} yet, so Columbus's defaults apply (no edits, no scripts). Create it as the install guide shows, then change it here",
            path.display()
        ));
    };
    let text = set_key(&before, name, on);
    if on {
        confirm(match name {
            "allow_edits" => {
                "let your assistant write to read-write connections, after your approval of each change"
            }
            "allow_scripts" => "let your assistant run Python scripts, after your approval of each one",
            "allow_workflows" => "let the workflows you approve run by themselves, on their schedule",
            "allow_shared_tools" => "let you open tools and workflows coworkers share with you, to review and approve each yourself",
            _ => "let your assistant use the tools you install from what it proposes, without asking each time",
        })?;
        if std::fs::read_to_string(&path).ok().as_deref() != Some(before.as_str()) {
            return Err("the settings file changed while you were asked; nothing was saved".into());
        }
    }
    write_atomic(&path, &text)?;
    if let Err(e) = OrgPolicy::load(&path) {
        let _ = write_atomic(&path, &before);
        return Err(format!("not saved: the server would refuse the file: {e}"));
    }
    Ok(())
}

/// The file with `key:` set to `on`: its line rewritten (an end-of-line
/// comment kept), every duplicate dropped, or a line added.
pub(crate) fn set_key(text: &str, key: &str, on: bool) -> String {
    let value = if on { "true" } else { "false" };
    let mut out = Vec::new();
    let mut done = false;
    for line in text.lines() {
        let is_key = line
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with(':'));
        if !is_key {
            out.push(line.to_string());
            continue;
        }
        if done {
            continue;
        }
        done = true;
        // An end-of-line comment, with the spaces before it.
        let comment = line
            .find(" #")
            .map(|h| {
                let start = line[..=h].trim_end().len();
                &line[start..]
            })
            .unwrap_or("");
        out.push(format!("{key}: {value}{comment}"));
    }
    if !done {
        out.push(format!("{key}: {value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// The person's own tools: installed ones, and those an assistant proposed
/// (with `propose_tool`) that wait for the person to read and install.
#[derive(Serialize)]
pub struct ToolsState {
    dir: String,
    /// The organization allows personal tools (`allow_personal_tools`).
    allowed: bool,
    installed: Vec<tools::ToolView>,
    waiting: Vec<tools::ToolView>,
}

#[tauri::command]
pub async fn settings_tools() -> Result<ToolsState, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let root = tools::tools_dir();
        ToolsState {
            dir: root.display().to_string(),
            allowed: policy_now().allow_personal_tools,
            installed: tools::list_installed(&root),
            waiting: tools::list_proposals(&root),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// The organization policy as the server reads it now (its defaults where
/// there is no file; a file that does not read allows nothing extra).
pub(crate) fn policy_now() -> OrgPolicy {
    let path = paths::policy_file();
    if path.exists() {
        OrgPolicy::load(&path).unwrap_or_default()
    } else {
        OrgPolicy::default()
    }
}

/// Install a tool waiting for the person: it adds to what the assistant
/// can run without asking, so the person confirms first, and exactly the
/// files they were shown (`hash`) are installed. Running sessions use it
/// from their next call.
#[tauri::command]
pub async fn settings_tool_install(id: String, hash: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || install_tool(&id, &hash))
        .await
        .map_err(|e| e.to_string())?
}

fn install_tool(id: &str, hash: &str) -> Result<(), String> {
    if !policy_now().allow_personal_tools {
        return Err("your organization's settings turn personal tools off, so none can be installed here (Allow personal tools, under Organization settings)".into());
    }
    let root = tools::tools_dir();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let p = tools::read_proposal(&root, id)?;
    if p.hash != hash {
        return Err(
            "the tool changed after you looked at it; nothing was installed. Look again".into(),
        );
    }
    confirm(&format!(
        "add the tool \"{}\" ({}), which your assistant may then run on your data without asking",
        p.title, p.tool
    ))?;
    // Checked again on the files themselves: what was shown is what goes in.
    tools::install(&root, id, hash, &tools::now()).map(|_| ())
}

/// Decline a waiting tool: it is deleted. Nothing to confirm.
#[tauri::command]
pub async fn settings_tool_decline(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        tools::decline(&tools::tools_dir(), &id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Remove an installed tool: off from the next call. Nothing to confirm.
#[tauri::command]
pub async fn settings_tool_remove(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        tools::remove(&tools::tools_dir(), &id)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// An installed tool's files, for the person to read again.
#[tauri::command]
pub async fn settings_tool_files(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !tools::id_ok(&id) {
            return Err(format!("{id:?} is not a personal tool"));
        }
        let dir = tools::tools_dir().join(&id);
        let read = |f: &str| {
            std::fs::read_to_string(dir.join(f))
                .map_err(|e| format!("{}: {e}", dir.join(f).display()))
        };
        Ok(json!({"manifest_text": read(tools::MANIFEST)?, "code": read(tools::ENTRY)?}))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Open a file or folder picker; the chosen path, or None.
#[tauri::command]
pub async fn settings_pick(app: tauri::AppHandle, kind: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let d = app.dialog().file();
    let picked = match kind.as_str() {
        "rasters" => d
            .set_title("Choose the folder of rasters")
            .blocking_pick_folder(),
        "shapefile" => d
            .set_title("Choose the folder of shapefiles")
            .blocking_pick_folder(),
        "filegdb" => d
            .set_title("Choose the file geodatabase (the folder ending in .gdb)")
            .blocking_pick_folder(),
        "gpkg" => d
            .set_title("Choose a GeoPackage")
            .add_filter("GeoPackage", &["gpkg"])
            .blocking_pick_file(),
        "sqlite" => d
            .set_title("Choose a SQLite database")
            .add_filter("SQLite", &["sqlite", "db", "sqlite3"])
            .blocking_pick_file(),
        "files" => d
            .set_title(
                "Choose the folder of files (CSV, Excel, GeoJSON, KML, GeoParquet, FlatGeobuf)",
            )
            .blocking_pick_folder(),
        "python" => d
            .set_title("Choose the Python interpreter (python.exe, or bin/python)")
            .blocking_pick_file(),
        _ => return Err("nothing to pick for this type".into()),
    };
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

pub(crate) fn confirm_with() -> &'static str {
    if cfg!(target_os = "macos") {
        "Touch ID or your Mac password"
    } else if cfg!(windows) {
        "Windows Hello"
    } else {
        "not available on this system"
    }
}

/// The person at the keyboard confirms a change that loosens access.
pub(crate) fn confirm(what: &str) -> Result<(), String> {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("COLUMBUS_APP_TEST_CONFIRM") {
        return if v == "allow" {
            Ok(())
        } else {
            Err("not confirmed".into())
        };
    }
    native_confirm(what)
}

#[cfg(any(target_os = "macos", windows))]
fn native_confirm(what: &str) -> Result<(), String> {
    use robius_authentication::{
        AndroidText, BiometricStrength, Context, PolicyBuilder, Text, WindowsText,
    };
    let policy = PolicyBuilder::new()
        .biometrics(Some(BiometricStrength::Strong))
        .password(true)
        .build()
        .ok_or("this system cannot confirm who is at the keyboard")?;
    let reason = format!("{what} (Columbus settings)");
    let text = Text {
        android: AndroidText {
            title: "Columbus",
            subtitle: None,
            description: None,
        },
        apple: &reason,
        windows: WindowsText::new_truncated("Columbus settings", &reason),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(Some(tx));
    Context::new(())
        .authenticate(text, &policy, move |r| {
            if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
                let _ = tx.send(r);
            }
        })
        .map_err(|e| format!("could not ask you to confirm: {e:?}"))?;
    match rx.recv_timeout(std::time::Duration::from_secs(180)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("not confirmed, so nothing was changed".into()),
        Err(_) => Err("no confirmation came, so nothing was changed".into()),
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn native_confirm(_what: &str) -> Result<(), String> {
    Err("changes that widen access need Touch ID or Windows Hello, which this system does not have: edit the settings files instead".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(yaml: &str) -> Connection {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn what_loosens_access() {
        let base = conn("id: a\ntype: gpkg\npath: /x.gpkg\ndatasets: {allow: [\"roads\", \"tracts\"], deny: [\"audit\"]}\nrow_filter: \"d = 14\"\n");
        let with = |f: &dyn Fn(&mut Connection)| {
            let mut c = base.clone();
            f(&mut c);
            c
        };
        assert!(loosens(None, &base), "a new connection");
        assert!(!loosens(Some(&base), &base));
        assert!(!loosens(
            Some(&base),
            &with(&|c| c.description = Some("x".into()))
        ));
        assert!(!loosens(
            Some(&base),
            &with(&|c| c.datasets.allow = vec!["roads".into()])
        ));
        assert!(!loosens(
            Some(&base),
            &with(&|c| c.datasets.deny.push("x".into()))
        ));
        assert!(loosens(Some(&base), &with(&|c| c.mode = Mode::ReadWrite)));
        assert!(loosens(
            Some(&base),
            &with(&|c| c.datasets.allow.push("*".into()))
        ));
        assert!(loosens(Some(&base), &with(&|c| c.datasets.deny.clear())));
        assert!(loosens(Some(&base), &with(&|c| c.row_filter = None)));
        assert!(loosens(
            Some(&base),
            &with(&|c| c.row_filter = Some("1 = 1".into()))
        ));
        assert!(loosens(
            Some(&base),
            &with(&|c| c.path = Some("/other.gpkg".into()))
        ));
        let rw = with(&|c| c.mode = Mode::ReadWrite);
        assert!(!loosens(Some(&rw), &base), "read-write to read");
    }

    #[test]
    fn switches_keep_the_rest_of_the_file() {
        let t = "connection_types: [gpkg]\nallow_scripts: false   # run_python\nallow_edits: false     # edits\n";
        let s = set_key(t, "allow_edits", true);
        assert_eq!(s, "connection_types: [gpkg]\nallow_scripts: false   # run_python\nallow_edits: true     # edits\n");
        // A duplicate (as an appended line once made) is dropped.
        let d = set_key(
            "allow_scripts: false\nallow_scripts: true\n",
            "allow_scripts",
            true,
        );
        assert_eq!(d, "allow_scripts: true\n");
        assert_eq!(
            set_key("a: 1\n", "allow_edits", false),
            "a: 1\nallow_edits: false\n"
        );
        // A key that only starts the same is not touched.
        assert_eq!(
            set_key("allow_edits_x: 1\n", "allow_edits", true),
            "allow_edits_x: 1\nallow_edits: true\n"
        );
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn filters_naming_hidden_fields_are_refused() {
        let hidden = vec!["*SSN*".to_string(), "owner_*".to_string()];
        assert_eq!(
            filter_uses_hidden("OWNER_SSN LIKE '4%'", &hidden).as_deref(),
            Some("OWNER_SSN")
        );
        assert_eq!(
            filter_uses_hidden("owner_name = 'x'", &hidden).as_deref(),
            Some("owner_name")
        );
        assert_eq!(filter_uses_hidden("district = 14", &hidden), None);
    }

    fn form(json: Value) -> ConnForm {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn files_and_arcgis_forms_build_their_settings() {
        let dir = std::env::temp_dir();
        let f = build(
            &form(json!({"id": "drops", "kind": "files", "path": dir.display().to_string(), "mode": "read",
                         "recursive": true, "crs": " EPSG:3081 ", "x": "EASTING", "y": "NORTHING",
                         "url": "https://ignored.example/FeatureServer", "host": "ignored"})),
            None,
        )
        .unwrap();
        assert_eq!(f.recursive, Some(true));
        assert_eq!(f.crs.as_deref(), Some("EPSG:3081"));
        assert_eq!(
            (f.x.as_deref(), f.y.as_deref()),
            (Some("EASTING"), Some("NORTHING"))
        );
        assert!(f.url.is_none() && f.host.is_none(), "{f:?}");
        // Checked as the server checks: x and y together, an EPSG code.
        let e = build(&form(json!({"id": "d", "kind": "files", "path": dir.display().to_string(), "mode": "read", "x": "E"})), None).unwrap_err();
        assert!(e.contains("give both or neither"), "{e}");
        let e = build(&form(json!({"id": "d", "kind": "files", "path": dir.display().to_string(), "mode": "read", "crs": "4326"})), None).unwrap_err();
        assert!(e.contains("EPSG code"), "{e}");
        let e = build(&form(json!({"id": "d", "kind": "files", "path": dir.display().to_string(), "mode": "read-write"})), None).unwrap_err();
        assert!(e.contains("read only"), "{e}");

        let public = build(
            &form(json!({"id": "assets", "kind": "arcgis", "mode": "read", "auth": "none",
                         "url": "https://services.arcgis.com/abc/arcgis/rest/services/Roads/FeatureServer",
                         "path": "/ignored", "portal": "https://www.arcgis.com", "client_id": "Ignored1", "page_size": 500})),
            None,
        )
        .unwrap();
        assert!(public.auth.is_none() && public.path.is_none() && public.recursive.is_none());
        assert_eq!(public.page_size, Some(500));
        let signed = build(
            &form(json!({"id": "secure", "kind": "arcgis", "mode": "read", "auth": "oauth",
                         "url": "https://services.arcgis.com/abc/arcgis/rest/services/Roads/FeatureServer/3",
                         "portal": "https://www.arcgis.com", "client_id": "AbC123"})),
            None,
        )
        .unwrap();
        let o = signed.auth.as_ref().and_then(ArcgisAuth::oauth).unwrap();
        assert_eq!(
            (o.kind.as_str(), o.portal.as_str(), o.client_id.as_str()),
            ("oauth", "https://www.arcgis.com", "AbC123")
        );
        // A changed portal is a loosening: the person confirms it.
        let mut moved = signed.clone();
        moved.auth = Some(ArcgisAuth::OAuth(ArcgisOAuth {
            portal: "https://gis.example.org/portal".into(),
            ..o.clone()
        }));
        assert!(loosens(Some(&signed), &moved));
        let e = build(&form(json!({"id": "s", "kind": "arcgis", "mode": "read", "auth": "oauth", "url": "https://services.arcgis.com/abc/arcgis/rest/services/Roads/FeatureServer"})), None).unwrap_err();
        assert!(e.contains("portal"), "{e}");
        let e = build(&form(json!({"id": "s", "kind": "arcgis", "mode": "read", "url": "http://services.example.com/arcgis/rest/services/Roads/FeatureServer"})), None).unwrap_err();
        assert!(e.contains("https"), "{e}");
    }

    #[test]
    fn only_the_portals_sign_in_page_is_opened() {
        let p = "https://www.arcgis.com";
        assert!(signin_url_ok(
            "https://www.arcgis.com/sharing/rest/oauth2/authorize?client_id=a&state=b",
            p
        ));
        assert!(!signin_url_ok(
            "https://evil.example/sharing/rest/oauth2/authorize?x=1",
            p
        ));
        assert!(!signin_url_ok(
            "https://www.arcgis.com.evil.example/sharing/rest/oauth2/authorize?x=1",
            p
        ));
        assert!(!signin_url_ok("file:///etc/passwd", p));
        assert!(!signin_url_ok("https://www.arcgis.com/home/index.html", p));
        assert!(!signin_url_ok(
            "https://www.arcgis.com/sharing/rest/oauth2/authorize?a=b c",
            p
        ));
    }

    #[test]
    fn writes_refuse_links() {
        let d = std::env::temp_dir().join(format!("columbus-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let real = d.join("real.yaml");
        std::fs::write(&real, "a: 1\n").unwrap();
        write_atomic(&real, "a: 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "a: 2\n");
        #[cfg(unix)]
        {
            let link = d.join("link.yaml");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(write_atomic(&link, "a: 3\n").is_err());
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "a: 2\n");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
