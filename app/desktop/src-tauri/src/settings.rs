//! The Settings window: the person's connections and the organization's
//! switches for edits and scripts, edited here instead of in files.
//!
//! The assistant has no way to change these: settings are never a tool,
//! and only this window may call these commands (capabilities/settings.json).
//! Anything that loosens access (a new connection, read-write, more
//! datasets, a removed row filter, edits or scripts turned on) is confirmed
//! by the person at the keyboard first: Touch ID or the account password on
//! macOS, Windows Hello on Windows. Tightening and removing are not.
//!
//! Files are written whole, then moved into place, and checked by loading
//! them exactly as the server does; a file the server would refuse is never
//! left behind.

use std::path::{Path, PathBuf};

use columbus_shared::paths;
use columbus_shared::policy::{Connection, DatasetRules, Mode, OrgPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Where passwords live: the server reads them under this service, by
/// connection id (`columbus-mcp secret set <id>` stores them the same way).
const SECRET_SERVICE: &str = "columbus";

/// Connection types the window can set up.
const KINDS: &[&str] = &[
    "gpkg",
    "sqlite",
    "rasters",
    "postgis",
    "sqlserver",
    "oracle",
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

fn secret_entry(id: &str) -> Result<keyring_core::Entry, String> {
    crate::chat::ensure_store()?;
    keyring_core::Entry::new(SECRET_SERVICE, id).map_err(|e| e.to_string())
}

fn has_password(id: &str) -> bool {
    secret_entry(id)
        .ok()
        .is_some_and(|e| e.get_password().is_ok())
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

fn writable(path: &Path) -> bool {
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
            connection_types: policy.connection_types.clone(),
            problem,
        },
        confirm_with: confirm_with().into(),
        other_folders: other_folders(&dir),
        restart_needed: sessions_here(&dir).iter().any(|s| s.key.is_none()),
    }
}

/// A running session that reads this window's connections folder.
struct LiveSession {
    port: u16,
    /// Its key for connection tests; sessions from before 0.6 have none
    /// (and do not pick up changed settings either).
    key: Option<String>,
    last_used: Option<String>,
    started: Option<String>,
}

/// The running sessions reading `dir`, the one in use first.
fn sessions_here(dir: &Path) -> Vec<LiveSession> {
    let mine = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
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
        let Some(c) = v["settings"]["connections"].as_str() else {
            continue;
        };
        let theirs = Path::new(c)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(c));
        let Some(port) = v["port"].as_u64().and_then(|p| u16::try_from(p).ok()) else {
            continue;
        };
        if theirs != mine || port < 1024 || !crate::answers(port) {
            continue;
        }
        let key = std::fs::read_to_string(p.with_extension("key"))
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()));
        let s = |k: &str| v[k].as_str().map(String::from);
        out.push(LiveSession {
            port,
            key,
            last_used: s("last_used"),
            started: s("started"),
        });
    }
    out.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| b.started.cmp(&a.started))
    });
    out
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
    use columbus_shared::proof;
    let dir = paths::connections_dir();
    let found = scan(&dir);
    let old = found
        .iter()
        .filter_map(|(_, c)| c.as_ref().ok())
        .find(|c| form.existing && c.id.eq_ignore_ascii_case(&form.id));
    let conn = build(&form, old)?;
    let sessions = sessions_here(&dir);
    if sessions.is_empty() {
        return Err("No Columbus session is running to test with. Open Claude desktop (it starts Columbus), then try again. You can still save it now.".into());
    }
    let body = serde_json::to_vec(&json!({
        "connection": conn,
        "password": form.password.as_deref().filter(|p| !p.is_empty()),
    }))
    .map_err(|e| e.to_string())?;
    let mut last = "The Columbus running now is too old to test connections. Update the Columbus extension in Claude desktop, then try again.".to_string();
    for s in &sessions {
        let Some(key) = &s.key else { continue };
        // First the session proves it holds the key (a program that merely
        // took over a port it used gets nothing); the key is never sent.
        // From the operating system's secure random source.
        let mine = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let (code, hello) = match post_local(
            s.port,
            "/api/test-hello",
            &[(
                "X-Columbus-Test-Hello",
                &format!(
                    "{mine}.{}",
                    proof::prove(key, proof::HELLO, &[mine.as_bytes()])
                ),
            )],
            b"",
        ) {
            Ok(v) => v,
            Err(e) => {
                last = e;
                continue;
            }
        };
        let (Some(challenge), Some(theirs)) =
            (hello["challenge"].as_str(), hello["proof"].as_str())
        else {
            last = format!("The Columbus session did not answer the test ({code}).");
            continue;
        };
        if code != 200
            || !proof::same(
                theirs,
                &proof::prove(key, proof::SERVER, &[mine.as_bytes()]),
            )
        {
            last = "A program on this computer answered for Columbus but could not prove it is your Columbus session, so nothing was sent to it.".into();
            continue;
        }
        let sig = proof::prove(
            key,
            proof::APP,
            &[challenge.as_bytes(), proof::body_hash(&body).as_bytes()],
        );
        let header = format!("{challenge}.{sig}");
        let (code, reply) = post_local(
            s.port,
            "/api/test-connection",
            &[("X-Columbus-Test", &header)],
            &body,
        )?;
        return match code {
            200 => Ok(reply),
            429 => Err("A test is already running; wait for it to finish.".into()),
            413 => Err("The form is too long to test (shorten the description or filter).".into()),
            _ => Err(format!(
                "The Columbus session could not run the test ({code}: {}).",
                reply["error"].as_str().unwrap_or("no reason given")
            )),
        };
    }
    Err(last)
}

/// A small HTTP/1.1 POST to a session's page on this computer, waiting at
/// most a minute in all for its answer (a database can be slow to answer).
fn post_local(
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<(u16, Value), String> {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::{Duration, Instant};
    const MAX: usize = 64 * 1024;
    let deadline = Instant::now() + Duration::from_secs(60);
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
    let file_kind = matches!(form.kind.as_str(), "gpkg" | "sqlite" | "rasters");
    let path = trimmed(&form.path);
    if file_kind {
        let p = path.as_deref().ok_or("choose the file or folder")?;
        let pb = Path::new(p);
        if !pb.is_absolute() {
            return Err("the location must be a full path".into());
        }
        let want_dir = form.kind == "rasters";
        if want_dir && !pb.is_dir() {
            return Err(format!("{p} is not a folder"));
        }
        if !want_dir && !pb.is_file() {
            return Err(format!("{p} is not a file"));
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
    Ok(Connection {
        id: form.id.clone(),
        kind: form.kind.clone(),
        dsn: trimmed(&form.dsn).filter(|_| !file_kind),
        path: path.filter(|_| file_kind),
        host: trimmed(&form.host).filter(|_| !file_kind),
        port: form.port.filter(|_| !file_kind),
        database: trimmed(&form.database).filter(|_| !file_kind),
        user: trimmed(&form.user).filter(|_| !file_kind),
        sslmode: trimmed(&form.sslmode).filter(|_| form.kind == "postgis"),
        search_path: trimmed(&form.search_path).filter(|_| form.kind == "postgis"),
        mode,
        datasets,
        row_filter: trimmed(&form.row_filter),
        limits: old.map(|o| o.limits).unwrap_or_default(),
        expires: old.and_then(|o| o.expires),
        description: trimmed(&form.description),
    })
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
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Write a file whole (a fresh temporary file beside it, flushed to disk),
/// then move it into place, keeping the old file's permissions. A link is
/// not followed: the window will not change a file that lives elsewhere.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
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
    let password = form.password.as_deref().filter(|p| !p.is_empty());
    // A password left under this name (by an earlier connection, the
    // command line, or a connected server's key) would go to the new place.
    if old.is_none() && manifest_credentials().contains(&new.id.to_lowercase()) {
        return Err(format!(
            "{:?} is the name of a connected server's key; choose another name",
            new.id
        ));
    }
    if old.is_none() && password.is_none() && secret_may_exist(&new.id) {
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
        // A connected server's key of the same name stays.
        if forget_password && !manifest_credentials().contains(&id.to_lowercase()) {
            if let Ok(e) = secret_entry(&id) {
                let _ = e.delete_credential();
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
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
    if !matches!(name, "allow_edits" | "allow_scripts") {
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
        confirm(if name == "allow_edits" {
            "let your assistant write to read-write connections, after your approval of each change"
        } else {
            "let your assistant run Python scripts, after your approval of each one"
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

/// Open a file or folder picker; the chosen path, or None.
#[tauri::command]
pub async fn settings_pick(app: tauri::AppHandle, kind: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let d = app.dialog().file();
    let picked = match kind.as_str() {
        "rasters" => d
            .set_title("Choose the folder of rasters")
            .blocking_pick_folder(),
        "gpkg" => d
            .set_title("Choose a GeoPackage")
            .add_filter("GeoPackage", &["gpkg"])
            .blocking_pick_file(),
        "sqlite" => d
            .set_title("Choose a SQLite database")
            .add_filter("SQLite", &["sqlite", "db", "sqlite3"])
            .blocking_pick_file(),
        _ => return Err("nothing to pick for this type".into()),
    };
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

fn confirm_with() -> &'static str {
    if cfg!(target_os = "macos") {
        "Touch ID or your Mac password"
    } else if cfg!(windows) {
        "Windows Hello"
    } else {
        "not available on this system"
    }
}

/// The person at the keyboard confirms a change that loosens access.
fn confirm(what: &str) -> Result<(), String> {
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
