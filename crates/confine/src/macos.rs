//! macOS confinement: the system sandbox (Seatbelt), through
//! `/usr/bin/sandbox-exec`, with a profile Columbus writes for each launch.
//!
//! * Files: levels A and B may read the system's own folders (`/System`,
//!   `/usr`, `/bin`, `/sbin`, `/opt`, `/Library/Frameworks`, `/private/etc`,
//!   the dyld cache and time zones), the read-only folders given and the
//!   read-write folders given, and write only the read-write ones. Level C
//!   may read everything and write only the read-write folders. Any process
//!   may look up file metadata (names, sizes, dates) everywhere, as the
//!   loader and Python need to resolve paths; contents stay closed.
//! * Network and services: level A makes no sockets but Unix socket pairs,
//!   and reaches no system service. Level B may open internet connections
//!   and look up names (the system resolver and certificate services). Level
//!   C may use the network as it likes.
//! * Memory: macOS does not enforce memory limits on a process, so the
//!   launcher stays as the program's parent and stops it when its resident
//!   memory passes the limit (checked several times a second).
//! * Processes: not capped (macOS counts processes per user, not per tree).
//!
//! The profile takes every path as a parameter (`-D`), never spliced into
//! the profile text, so no path can change the rules.

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::{Error, Level, Spec};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Read-only system folders a program needs to start.
const SYSTEM_READ: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/opt",
    "/Library/Frameworks",
    "/Library/Apple",
    "/private/etc",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/Applications/Xcode.app/Contents/Developer",
    "/Library/Developer/CommandLineTools",
];

pub fn available(_level: Level) -> Result<(), Error> {
    if Path::new(SANDBOX_EXEC).is_file() {
        Ok(())
    } else {
        Err(Error::Unsupported(
            "this Mac has no /usr/bin/sandbox-exec".into(),
        ))
    }
}

/// The Seatbelt profile for a level, with `n_ro` and `n_rw` path
/// parameters (`RO0`…, `RW0`…) and the system folders as `SYS0`….
pub(crate) fn profile(level: Level, n_ro: usize, n_rw: usize) -> String {
    let params = |prefix: &str, n: usize| -> String {
        (0..n)
            .map(|i| format!("(subpath (param \"{prefix}{i}\"))"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let sys = params("SYS", SYSTEM_READ.len());
    let ro = params("RO", n_ro);
    let rw = params("RW", n_rw);
    let mut p = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow process-info* (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow file-read-metadata)\n\
         (allow ipc-posix-sem)\n\
         (allow ipc-posix-shm-read* ipc-posix-shm-write-data (ipc-posix-name-prefix \"apple.shm.notification_center\"))\n\
         (allow file-read* file-write-data (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\"))\n\
         (allow file-read* (literal \"/dev/urandom\") (literal \"/dev/random\") (literal \"/dev/dtracehelper\"))\n\
         (allow file-ioctl (literal \"/dev/dtracehelper\"))\n\
         (allow system-socket (socket-domain AF_UNIX))\n",
    );
    match level {
        Level::C => p.push_str("(allow file-read*)\n(allow process-exec)\n"),
        _ => {
            p.push_str(&format!("(allow file-read* {sys} {ro} {rw})\n"));
            p.push_str(&format!("(allow process-exec {sys} {ro} {rw})\n"));
        }
    }
    if n_rw > 0 {
        p.push_str(&format!("(allow file-write* {rw})\n"));
    }
    match level {
        Level::A => {}
        Level::B => p.push_str(
            "(allow system-socket)\n\
             (allow network-outbound (remote ip))\n\
             (allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n\
             (allow mach-lookup (global-name \"com.apple.dnssd.service\") (global-name \"com.apple.trustd\") \
              (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.SecurityServer\") \
              (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.notification_center\"))\n",
        ),
        Level::C => p.push_str(
            "(allow system-socket)\n\
             (allow network*)\n\
             (allow mach-lookup)\n",
        ),
    }
    p
}

fn canonical(p: &Path) -> PathBuf {
    // Seatbelt matches the resolved path (/tmp is /private/tmp).
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

pub fn launch(spec: &Spec, program: &str, args: &[String]) -> Result<i32, Error> {
    let ro: Vec<PathBuf> = spec.read_only.iter().map(|p| canonical(p)).collect();
    let rw: Vec<PathBuf> = spec.read_write.iter().map(|p| canonical(p)).collect();
    let mut cmd = Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(profile(spec.level, ro.len(), rw.len()));
    for (i, s) in SYSTEM_READ.iter().enumerate() {
        cmd.arg("-D").arg(format!("SYS{i}={s}"));
    }
    for (i, p) in ro.iter().enumerate() {
        cmd.arg("-D").arg(format!("RO{i}={}", p.display()));
    }
    for (i, p) in rw.iter().enumerate() {
        cmd.arg("-D").arg(format!("RW{i}={}", p.display()));
    }
    cmd.arg("--").arg(program).args(args);
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Setup(format!("{SANDBOX_EXEC}: {e}")))?;
    let pid = child.id() as i32;
    let limit = spec.memory_mb.map(|m| m.saturating_mul(1024 * 1024));
    let over = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(limit) = limit {
        let over = over.clone();
        std::thread::spawn(move || loop {
            match resident(pid) {
                Some(r) if r > limit => {
                    over.store(true, std::sync::atomic::Ordering::SeqCst);
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                    return;
                }
                Some(_) => std::thread::sleep(Duration::from_millis(150)),
                None => return,
            }
        });
    }
    let status = child
        .wait()
        .map_err(|e| Error::Setup(format!("waiting for the program: {e}")))?;
    if over.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!(
            "columbus-confine: stopped the program: it used more than {} MB of memory",
            spec.memory_mb.unwrap_or(0)
        );
        return Ok(137);
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// The process's resident memory in bytes, or None once it has gone.
fn resident(pid: i32) -> Option<u64> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    (n == size).then_some(info.pti_resident_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_take_paths_only_as_parameters() {
        let p = profile(Level::A, 2, 1);
        assert!(p.contains("(param \"RO1\")") && p.contains("(param \"RW0\")"));
        assert!(!p.contains("network-outbound"), "level A has no network");
        assert!(
            !p.contains("(allow mach-lookup"),
            "level A reaches no service"
        );
        let b = profile(Level::B, 0, 1);
        assert!(b.contains("(remote ip)"));
        let c = profile(Level::C, 0, 1);
        assert!(c.contains("(allow file-read*)\n"));
        assert!(c.contains("(allow file-write* (subpath (param \"RW0\")))"));
    }
}
