//! `columbus-workflow`: what a Windows Task Scheduler entry runs for a
//! scheduled workflow. A windowless program (no console window opens, and
//! none can be closed by mistake mid-run) that runs `columbus-mcp workflow
//! run <id> --scheduled [--python P]` beside it, with no window either, and
//! exits with its code. It passes its arguments on unchanged and checks
//! nothing itself: the runner checks everything.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::{Command, Stdio};

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let exe = if cfg!(windows) {
        "columbus-mcp.exe"
    } else {
        "columbus-mcp"
    };
    let Some(server) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(exe)))
        .filter(|p| p.is_file())
    else {
        std::process::exit(1);
    };
    let mut cmd = Command::new(server);
    cmd.args(["workflow", "run"])
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let code = cmd.status().ok().and_then(|s| s.code()).unwrap_or(1);
    std::process::exit(code);
}
