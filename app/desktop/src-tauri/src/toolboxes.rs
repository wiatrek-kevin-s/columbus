//! Tools › Toolboxes (build brief 3, D-17, REQ-38, T-29): the toolboxes the
//! person installed, what Columbus's catalogue offers, and the tools inside
//! them.
//!
//! A toolbox adds to what the assistant can run without asking, so
//! installing one is the person's own action at the keyboard: confirmed
//! with Touch ID or the account password on macOS, Windows Hello on
//! Windows (`settings::confirm`, the path a personal tool's install takes),
//! and only then does Columbus's own server do the work
//! (`columbus-mcp toolbox install|add … --yes`): the download, the check
//! against the pinned hash, the unpacking, and the listing of its tools in
//! the sandbox. The page shows what will be downloaded before any of it.
//! The assistant has no way to call any of this: only the app's own main
//! window may (capabilities/main.json).
//!
//! Listing, searching and describing only read the files beside each
//! toolbox (`columbus_shared::toolboxes`), so the page shows exactly what
//! the assistant is offered.

use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use columbus_shared::paths;
use columbus_shared::toolboxes as tbx;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::chat::MAIN;
use crate::settings::{confirm, confirm_with, policy_now, writable};

/// How long an install may take: the download, then the listing.
const INSTALL_LIMIT: Duration = Duration::from_secs(40 * 60);
/// Most tools one search shows.
const SHOWN: usize = 40;

/// One install or add at a time.
static BUSY: AtomicBool = AtomicBool::new(false);

struct Busy;

impl Busy {
    fn take() -> Result<Busy, String> {
        if BUSY.swap(true, Ordering::SeqCst) {
            return Err("Another toolbox is being installed: wait for it to finish.".into());
        }
        Ok(Busy)
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

/// The installed toolboxes, what the catalogue offers on this system, and
/// what the organization's settings allow.
#[tauri::command]
pub async fn toolboxes_state() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(state)
        .await
        .map_err(|e| e.to_string())
}

fn state() -> Value {
    let policy = policy_now();
    let dir = tbx::toolboxes_dir();
    let installed = tbx::list(&dir);
    let ids: Vec<&str> = installed.iter().map(|v| v.id.as_str()).collect();
    let catalogue: Vec<Value> = tbx::catalogue_json()
        .into_iter()
        .map(|mut e| {
            let on = e["id"].as_str().is_some_and(|id| ids.contains(&id));
            e["installed"] = json!(on);
            e
        })
        .collect();
    json!({
        "dir": dir.display().to_string(),
        "allowed": policy.allow_toolboxes,
        "catalogue_allowed": policy.toolboxes.catalogue,
        // Whether the policy file is the person's to change (else it is
        // the organization's).
        "managed": !writable(&paths::policy_file()),
        "confirm_with": confirm_with(),
        "can_confirm": can_confirm(),
        "installed": installed,
        "catalogue": catalogue,
    })
}

/// Whether this system can ask the person at the keyboard to confirm
/// (Touch ID or the password on a Mac, Windows Hello on Windows). Where it
/// cannot, the page shows the terminal's command instead of a button. A
/// test build that was told how to answer counts as able.
fn can_confirm() -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os("COLUMBUS_APP_TEST_CONFIRM").is_some() {
        return true;
    }
    cfg!(any(target_os = "macos", windows))
}

/// The last line that says why a command failed.
fn why(errs: &str, out: &str) -> String {
    errs.lines()
        .rev()
        .chain(out.lines().rev())
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('{'))
        .map(|l| l.strip_prefix("Error: ").unwrap_or(l).to_string())
        .unwrap_or_else(|| "Columbus did not say why".into())
}

/// Run `columbus-mcp toolbox … --json`, passing each step it reports to
/// the page (`toolbox-progress`), and return its last line: what was
/// installed.
fn run_steps(
    app: &AppHandle,
    server: &Path,
    engine: Option<&Path>,
    args: &[&str],
    id: &str,
) -> Result<Value, String> {
    let mut cmd = std::process::Command::new(server);
    cmd.arg("toolbox")
        .args(args)
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
    let start = Instant::now();
    let mut done = None;
    let mut said = String::new();
    let take =
        |line: String, done: &mut Option<Value>, said: &mut String| match serde_json::from_str::<
            Value,
        >(&line)
        {
            Ok(mut v) if v.is_object() => {
                if v["step"] == "done" {
                    *done = Some(v.clone());
                }
                v["id"] = json!(id);
                let _ = app.emit_to(MAIN, "toolbox-progress", v);
            }
            _ => {
                said.push_str(&line);
                said.push('\n');
            }
        };
    let status = loop {
        while let Ok(line) = rx.try_recv() {
            take(line, &mut done, &mut said);
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() < INSTALL_LIMIT => {
                std::thread::sleep(Duration::from_millis(100))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("It took too long and was stopped; nothing was installed.".into());
            }
        }
    };
    while let Ok(line) = rx.recv_timeout(Duration::from_secs(2)) {
        take(line, &mut done, &mut said);
    }
    let errs = err_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default();
    if !status.success() {
        return Err(why(&errs, &said));
    }
    done.ok_or_else(|| "Columbus did not say what it installed".into())
}

/// Install a toolbox from Columbus's catalogue, once the person confirms
/// what will be downloaded. Takes as long as the download does; the page
/// is told each step.
#[tauri::command]
pub async fn toolbox_install(app: AppHandle, id: String) -> Result<Value, String> {
    let (server, engine) = crate::registry::program(&app)?;
    tauri::async_runtime::spawn_blocking(move || install(&app, &server, engine.as_deref(), &id))
        .await
        .map_err(|e| e.to_string())?
}

fn install(
    app: &AppHandle,
    server: &Path,
    engine: Option<&Path>,
    id: &str,
) -> Result<Value, String> {
    // Refused before anything is asked, as the terminal does.
    let policy = policy_now();
    if !policy.allow_toolboxes {
        return Err("Toolboxes are turned off in your organization's settings (allow_toolboxes), so none can be installed here.".into());
    }
    if !policy.toolboxes.catalogue {
        return Err("Your organization's settings do not let Columbus download toolboxes (toolboxes.catalogue). You can still add a program that is already on this computer.".into());
    }
    let entry = tbx::catalogue_entry(id)
        .ok_or_else(|| format!("Columbus's catalogue has no toolbox {id:?}"))?;
    let Some(dl) = entry.for_this_system() else {
        return Err(format!(
            "{} has no build for this system ({}), so it cannot be installed here.",
            entry.name,
            tbx::this_system_words()
        ));
    };
    let _busy = Busy::take()?;
    confirm(&format!(
        "install the toolbox \"{}\" {} ({}, {} licence), whose tools your assistant may then run on copies of your data without asking",
        entry.name,
        entry.version,
        tbx::size_words(dl.bytes),
        entry.licence
    ))?;
    run_steps(app, server, engine, &["install", id, "--yes", "--json"], id)
}

/// Add a program already on this computer, with its description file, as
/// a toolbox, once the person confirms.
#[tauri::command]
pub async fn toolbox_add(
    app: AppHandle,
    id: String,
    name: String,
    program: String,
    tools: String,
) -> Result<Value, String> {
    let (server, engine) = crate::registry::program(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        add(
            &app,
            &server,
            engine.as_deref(),
            id.trim(),
            name.trim(),
            program.trim(),
            tools.trim(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn add(
    app: &AppHandle,
    server: &Path,
    engine: Option<&Path>,
    id: &str,
    name: &str,
    program: &str,
    tools: &str,
) -> Result<Value, String> {
    if !policy_now().allow_toolboxes {
        return Err("Toolboxes are turned off in your organization's settings (allow_toolboxes), so none can be added here.".into());
    }
    if !tbx::id_ok(id) || tbx::RESERVED_IDS.contains(&id) {
        return Err(format!(
            "{id:?} cannot be a toolbox's short name: use lowercase letters, digits and -, starting with a letter (at most 40)"
        ));
    }
    if tbx::ids(&tbx::toolboxes_dir()).iter().any(|i| i == id) {
        return Err(format!(
            "A toolbox named {id} is already there. Remove it first, or choose another short name."
        ));
    }
    let program_path = Path::new(program);
    if !program_path.is_absolute() || !program_path.is_file() {
        return Err("Choose the program's file (its full path).".into());
    }
    let tools_path = Path::new(tools);
    if !tools_path.is_absolute() || !tools_path.is_file() {
        return Err(
            "Choose the description file (a tools.json that lists the program's tools).".into(),
        );
    }
    // Checked before the person is asked, so the question says what is added.
    let bytes = std::fs::metadata(tools_path)
        .ok()
        .filter(|m| m.len() <= tbx::MAX_TOOLS_JSON)
        .and_then(|_| std::fs::read(tools_path).ok())
        .ok_or("The description file cannot be read, or is too large.")?;
    let listed = tbx::parse_described(&bytes)?;
    let shown = if name.is_empty() { id } else { name };
    let _busy = Busy::take()?;
    confirm(&format!(
        "add the program {program} as the toolbox \"{shown}\" ({} tool{}), which your assistant may then run on copies of your data without asking",
        listed.len(),
        if listed.len() == 1 { "" } else { "s" }
    ))?;
    let mut args = vec![
        "add",
        program,
        "--id",
        id,
        "--kind",
        "described",
        "--tools",
        tools,
    ];
    if !name.is_empty() {
        args.extend(["--name", name]);
    }
    args.extend(["--yes", "--json"]);
    run_steps(app, server, engine, &args, id)
}

/// Remove a toolbox: off from the assistant's next request. Nothing to
/// confirm (it only takes away).
#[tauri::command]
pub async fn toolbox_remove(app: AppHandle, id: String) -> Result<String, String> {
    let (server, engine) = crate::registry::program(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        if !tbx::id_ok(&id) {
            return Err(format!("{id:?} is not a toolbox"));
        }
        // Through Columbus's server, so the removal is in the audit log.
        let (ok, out, errs) = crate::registry::run_raw(
            &server,
            engine.as_deref(),
            &["toolbox", "remove", &id],
            Duration::from_secs(120),
        )?;
        forget(&tbx::toolboxes_dir().join(&id));
        if !ok {
            return Err(why(&errs, &out));
        }
        Ok(out
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("Removed"))
            .unwrap_or("Removed.")
            .to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

type Stamp = Option<(u64, SystemTime)>;

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// Toolboxes whose program this app has already checked in full (a check
/// reads the whole program), by folder, with what their files looked like
/// then. A search reuses the check while they look the same.
static CHECKED: Mutex<Option<HashMap<PathBuf, [Stamp; 3]>>> = Mutex::new(None);

fn forget(folder: &Path) {
    if let Some(m) = CHECKED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(folder);
    }
}

/// A toolbox whose program is the one installed, with its tools as last
/// listed; None for one that is not ready (the list above says why).
fn ready(dir: &Path, id: &str) -> Option<(tbx::Toolbox, tbx::Listing)> {
    let tb = tbx::load(dir, id).ok()?;
    let folder = dir.join(id);
    let seen = [
        stamp(&folder.join(tbx::MANIFEST)),
        stamp(&tb.program_path()),
        stamp(&folder.join(tbx::TOOLS)),
    ];
    let known = CHECKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&folder).cloned());
    if known != Some(seen) {
        if tbx::check(dir, &tb).is_err() {
            forget(&folder);
            return None;
        }
        CHECKED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(folder, seen);
    }
    let listing = tbx::read_cache(dir, &tb)?;
    Some((tb, listing))
}

/// The tools of the installed toolboxes that match the words, best first:
/// what the assistant's own search would offer.
#[tauri::command]
pub async fn toolbox_search(query: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !policy_now().allow_toolboxes {
            return json!({"hits": [], "total": 0});
        }
        let dir = tbx::toolboxes_dir();
        let words = tbx::query_words(&query);
        let mut hits = Vec::new();
        for id in tbx::ids(&dir) {
            if let Some((tb, listing)) = ready(&dir, &id) {
                let (found, _) = tbx::search(&tb, &listing, &words, usize::MAX);
                hits.extend(found.into_iter().map(|h| (tb.name.clone(), h)));
            }
        }
        let mut only: Vec<tbx::Hit> = hits.iter().map(|(_, h)| h.clone()).collect();
        tbx::sort_hits(&mut only);
        let total = only.len();
        let names: HashMap<String, String> = hits
            .into_iter()
            .map(|(name, h)| (h.toolbox, name))
            .collect();
        let shown: Vec<Value> = only
            .into_iter()
            .take(SHOWN)
            .map(|h| {
                json!({"name": h.name, "title": h.title, "summary": h.summary,
                    "toolbox": names.get(&h.toolbox).cloned().unwrap_or_else(|| h.toolbox.clone()),
                    "category": h.category, "cannot_run": h.cannot_run})
            })
            .collect();
        json!({"hits": shown, "total": total})
    })
    .await
    .map_err(|e| e.to_string())
}

/// One tool's description, as the assistant reads it: its parameters with
/// their role and kind.
#[tauri::command]
pub async fn toolbox_tool(name: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (id, tool) = name
            .rsplit_once('.')
            .ok_or("name a tool as <toolbox>.<tool>")?;
        let dir = tbx::toolboxes_dir();
        let (tb, listing) = ready(&dir, id).ok_or_else(|| {
            format!("The toolbox {id} cannot be used as it is: see the list above.")
        })?;
        let t = listing
            .tool(tool)
            .ok_or_else(|| format!("{id} has no tool named {tool}"))?;
        Ok(tbx::tool_json(&tb, t))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Choose the program, or its description file; the chosen path, or None.
#[tauri::command]
pub async fn toolbox_pick(app: AppHandle, kind: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let d = app.dialog().file();
    let picked = match kind.as_str() {
        "program" => d.set_title("Choose the program").blocking_pick_file(),
        "description" => d
            .set_title("Choose the description file (tools.json)")
            .add_filter("Description file", &["json"])
            .blocking_pick_file(),
        _ => return Err("nothing to pick".into()),
    };
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_is_the_commands_last_words() {
        assert_eq!(
            why(
                "Error: the download does not match the hash pinned in Columbus\n",
                ""
            ),
            "the download does not match the hash pinned in Columbus"
        );
        assert_eq!(
            why("", "{\"step\":\"download\"}\nplain words\n"),
            "plain words"
        );
        assert_eq!(why("", ""), "Columbus did not say why");
    }

    #[test]
    fn the_page_is_told_what_the_catalogue_would_download() {
        // The built-in entry, as the Add tools dialog shows it before
        // anything is installed.
        let all = tbx::catalogue_json();
        let e = all
            .iter()
            .find(|e| e["id"] == "geolibre")
            .expect("GeoLibre");
        for key in [
            "name",
            "about",
            "version",
            "licence",
            "publisher",
            "homepage",
            "system",
        ] {
            assert!(e[key].as_str().is_some_and(|s| !s.is_empty()), "{key}");
        }
        assert_eq!(
            (e["version"].as_str(), e["licence"].as_str()),
            (Some("1.8.1"), Some("MIT"))
        );
        if e["available"] == true {
            assert!(e["url"].as_str().unwrap().starts_with(
                "https://github.com/opengeos/geolibre-rust/releases/download/v1.8.1/"
            ));
            assert!(e["size"].as_str().unwrap().ends_with(" MB"));
            assert_eq!(e["sha256"].as_str().unwrap().len(), 64);
        }
    }
}
