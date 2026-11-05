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
    }
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
/// mode, fewer allowed patterns, more denied ones, or a row filter added
/// where there was none count as no loosening.
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
    match (&old.row_filter, &new.row_filter) {
        (a, b) if a == b => false,
        (None, Some(_)) => false,
        _ => true,
    }
}

/// Write a file whole, then move it into place.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = path.with_extension("tmp-columbus");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

#[tauri::command]
pub async fn settings_save_connection(form: ConnForm) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || save_connection(form))
        .await
        .map_err(|e| e.to_string())?
}

fn save_connection(form: ConnForm) -> Result<String, String> {
    let dir = paths::connections_dir();
    let found = scan(&dir);
    let existing = found
        .iter()
        .find(|(_, c)| c.as_ref().is_ok_and(|c| c.id == form.id));
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
    if loosens(old, &new) || password.is_some() {
        let what = match old {
            None => format!("add the connection {}", new.id),
            Some(_) => format!("widen what the connection {} gives access to", new.id),
        };
        confirm(&what)?;
    }
    let file = existing
        .map(|(p, _)| p.clone())
        .unwrap_or_else(|| dir.join(format!("{}.yaml", new.id)));
    let previous = std::fs::read_to_string(&file).ok();
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
        std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        if forget_password {
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
    let before = std::fs::read_to_string(&path).ok();
    let text = match &before {
        Some(t) => set_key(t, name, on),
        // No file yet: Columbus's defaults, written out, with this switch.
        None => {
            let mut v = serde_yaml::to_value(OrgPolicy::default()).map_err(|e| e.to_string())?;
            v[name] = serde_yaml::Value::Bool(on);
            format!(
                "# Written by the Columbus app from Columbus's defaults.\n{}",
                serde_yaml::to_string(&v).map_err(|e| e.to_string())?
            )
        }
    };
    if on {
        confirm(if name == "allow_edits" {
            "let your assistant write to read-write connections, after your approval of each change"
        } else {
            "let your assistant run Python scripts, after your approval of each one"
        })?;
    }
    write_atomic(&path, &text)?;
    if let Err(e) = OrgPolicy::load(&path) {
        match before {
            Some(b) => {
                let _ = write_atomic(&path, &b);
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
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
