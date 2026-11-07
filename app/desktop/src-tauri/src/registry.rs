//! The Registry page: searches and entries from the open geospatial
//! registry, asked of Columbus's own server (`columbus-mcp registry query`),
//! so the page sees exactly what an assistant would, checked and audited.
//! Installing or updating the index runs `columbus-mcp registry update`,
//! which checks it against its manifest before it replaces the old one.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tauri::AppHandle;

pub(crate) fn program(app: &AppHandle) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), String> {
    let s = crate::chat::load(app);
    let server = crate::chat::server(&s).ok_or("Columbus's server was not found")?;
    // Only a Columbus server is started, whatever wrote the settings.
    let server = crate::chat::server_ok(&server, None)?;
    let engine = crate::chat::engine_python(&s, Some(&server));
    Ok((server, engine))
}

/// Run the server with `args`, wait at most `limit`, and return its
/// standard output (at most 8 MB).
pub(crate) fn run(
    server: &std::path::Path,
    engine: Option<&std::path::Path>,
    args: &[&str],
    limit: Duration,
) -> Result<String, String> {
    let mut cmd = Command::new(server);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    // The readers report through channels, so a process that keeps a pipe
    // open past the limit (an engine outliving the server) can't hold this up.
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut out).take(8 << 20).read_to_string(&mut s);
        let _ = out_tx.send(s);
    });
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = (&mut err).take(64 << 10).read_to_string(&mut s);
        let _ = err_tx.send(s);
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Columbus took too long to answer".into());
            }
        }
    };
    let rest = |since: Instant| {
        limit
            .saturating_sub(since.elapsed())
            .max(Duration::from_secs(2))
    };
    let text = out_rx.recv_timeout(rest(start)).unwrap_or_default();
    let errs = err_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default();
    if !status.success() {
        let last = errs
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .or_else(|| text.lines().rev().find(|l| !l.trim().is_empty()))
            .unwrap_or("it stopped with an error");
        return Err(last.trim().to_string());
    }
    Ok(text)
}

/// One read-only call through `columbus-mcp app query` (registry_search,
/// registry_get, or listing project databases), optionally in a project.
pub(crate) fn app_query(
    server: &std::path::Path,
    engine: Option<&std::path::Path>,
    request: &Value,
) -> Result<Value, String> {
    let req = request.to_string();
    let text = run(
        server,
        engine,
        &["app", "query", &req],
        Duration::from_secs(90),
    )?;
    let v: Value = serde_json::from_str(text.trim())
        .map_err(|_| "Columbus's answer could not be read".to_string())?;
    if v["ok"] == true {
        Ok(v)
    } else {
        Err(v["error"]
            .as_str()
            .unwrap_or("Columbus could not answer")
            .to_string())
    }
}

/// One registry call: `tool` is registry_search or registry_get.
#[tauri::command]
pub async fn registry_query(app: AppHandle, tool: String, args: Value) -> Result<Value, String> {
    if tool != "registry_search" && tool != "registry_get" {
        return Err("not a registry call".into());
    }
    let (server, engine) = program(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        app_query(
            &server,
            engine.as_deref(),
            &serde_json::json!({"tool": tool, "args": args}),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Install or update the registry index from its published release.
#[tauri::command]
pub async fn registry_update(app: AppHandle) -> Result<String, String> {
    let (server, engine) = program(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let out = run(
            &server,
            engine.as_deref(),
            &["registry", "update"],
            Duration::from_secs(300),
        )?;
        if out.contains("installed") {
            Ok(out.trim().to_string())
        } else {
            Err(if out.trim().is_empty() {
                "the index was not installed".into()
            } else {
                out.trim().to_string()
            })
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
