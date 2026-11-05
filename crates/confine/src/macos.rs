//! macOS confinement: the system sandbox (Seatbelt), through
//! `/usr/bin/sandbox-exec`, with a profile Columbus writes for each launch.
//!
//! * Files: levels A and B may read the system's own folders (`/System`,
//!   `/usr/lib`, `/usr/bin` and the like, `/bin`, `/sbin`, the program
//!   folders of Homebrew but not its data, `/Library/Frameworks`,
//!   `/private/etc`, the dyld cache and time zones), the read-only folders
//!   given and the read-write folders given, and write only the read-write
//!   ones. Level C may read everything and write only the read-write
//!   folders. Any process may look up file metadata (names, sizes, dates)
//!   everywhere, as the loader and Python need to resolve paths; contents
//!   stay closed.
//! * Network: level A makes no sockets but Unix socket pairs. Level B may
//!   open internet connections and look up names, but not reach this
//!   computer (localhost). Level C may use the network as it likes.
//! * System services: Apple's base rules (`system.sb`) let any sandboxed
//!   program reach a few services (preferences, logging, notifications,
//!   user and group names). On top, every level is refused the services
//!   that would act for it outside the sandbox: LaunchServices (`open`),
//!   Apple Events (`osascript`), the pasteboard and the keychain. Level A
//!   is also refused the certificate service (it fetches from the network)
//!   and group membership lookups.
//! * Other processes: a confined program cannot read other processes'
//!   arguments or environment, nor list them.
//! * Processes and memory: macOS offers no job object or cgroup, so the
//!   launcher tags each launch's sandbox with a path only it allows and
//!   finds every process in that sandbox, even one that left the process
//!   group. Several times a second it sums their memory (physical
//!   footprint) and counts them, and stops them all past a limit. When the
//!   program ends, or the launcher's parent goes, every process left in the
//!   sandbox is stopped; a guard process does the same if the launcher
//!   itself is killed.
//!
//! The profile takes every path as a parameter (`-D`), never spliced into
//! the profile text, so no path can change the rules.

use std::ffi::{CStr, CString};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::{Error, Level, Spec};

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Read-only system folders a program needs to start. Homebrew's data
/// (`/opt/homebrew/var`: local databases) and `/usr/local` stay closed.
const SYSTEM_READ: &[&str] = &[
    "/System",
    "/usr/lib",
    "/usr/libexec",
    "/usr/share",
    "/usr/bin",
    "/usr/sbin",
    "/bin",
    "/sbin",
    "/opt/homebrew/Cellar",
    "/opt/homebrew/opt",
    "/opt/homebrew/lib",
    "/opt/homebrew/Frameworks",
    "/opt/homebrew/share",
    "/Library/Frameworks",
    "/Library/Apple",
    "/private/etc",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/Applications/Xcode.app/Contents/Developer",
    "/Library/Developer/CommandLineTools",
];

/// Services no level may reach: each acts for the caller outside the
/// sandbox (starts apps, sends Apple Events, reads the pasteboard or the
/// keychain).
const DENIED_SERVICES: &str = "(deny mach-lookup \
     (global-name \"com.apple.coreservices.launchservicesd\") \
     (global-name-prefix \"com.apple.lsd.\") \
     (global-name \"com.apple.coreservices.appleevents\") \
     (global-name \"com.apple.pasteboard.1\") \
     (global-name-prefix \"com.apple.pasteboard\") \
     (global-name \"com.apple.SecurityServer\") \
     (global-name \"com.apple.securityd.xpc\") \
     (global-name \"com.apple.security.agent\") \
     (global-name \"com.apple.tccd\") \
     (global-name \"com.apple.tccd.system\"))\n";

/// What no Columbus profile grants: a path under a folder nobody may write.
/// A process that may not write it is sandboxed; with the launch's own tag
/// writable, it is this launch's.
const PROBE: &str = "/private/var/empty/columbus-confine-probe";

extern "C" {
    fn sandbox_check(
        pid: libc::pid_t,
        operation: *const libc::c_char,
        filter_type: libc::c_int,
        ...
    ) -> libc::c_int;
}
const SANDBOX_FILTER_PATH: libc::c_int = 1;
const SANDBOX_CHECK_NO_REPORT: libc::c_int = 0x4000_0000;

pub fn available(_level: Level) -> Result<(), Error> {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return Err(Error::Unsupported(
            "this Mac has no /usr/bin/sandbox-exec".into(),
        ));
    }
    static TEST: OnceLock<Result<(), String>> = OnceLock::new();
    TEST.get_or_init(self_test)
        .clone()
        .map_err(Error::Unsupported)
}

/// The profile for a level, with `n_ro` and `n_rw` path parameters (`RO0`…,
/// `RW0`…), the system folders as `SYS0`… and the launch's tag as `TAG`.
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
         (import \"system.sb\")\n\
         (allow process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (deny process-info* (target others))\n\
         (allow process-info* (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (deny sysctl-read (sysctl-name-prefix \"kern.procargs\") (sysctl-name-prefix \"kern.proc.\"))\n\
         (allow file-read-metadata)\n\
         (allow ipc-posix-sem)\n\
         (allow ipc-posix-shm-read* ipc-posix-shm-write-data (ipc-posix-name-prefix \"apple.shm.notification_center\"))\n\
         (allow file-read* file-write-data (literal \"/dev/null\") (literal \"/dev/zero\"))\n\
         (allow file-read* (literal \"/dev/urandom\") (literal \"/dev/random\") (literal \"/dev/dtracehelper\"))\n\
         (allow file-ioctl (literal \"/dev/dtracehelper\"))\n\
         (allow file-write-data (literal (param \"TAG\")))\n\
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
        Level::A => p.push_str(
            "(deny mach-lookup (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\") \
              (global-name \"com.apple.system.opendirectoryd.membership\"))\n",
        ),
        Level::B => p.push_str(
            "(allow system-socket (socket-domain AF_INET) (socket-domain AF_INET6))\n\
             (allow network-outbound (remote ip))\n\
             (deny network-outbound (remote ip \"localhost:*\"))\n\
             (allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n\
             (allow mach-lookup (global-name \"com.apple.dnssd.service\") (global-name \"com.apple.trustd\") \
              (global-name \"com.apple.trustd.agent\") \
              (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.notification_center\"))\n",
        ),
        Level::C => p.push_str(
            "(allow system-socket)\n\
             (allow network*)\n\
             (allow mach-lookup)\n",
        ),
    }
    // Last, so it wins over every allow above.
    p.push_str(DENIED_SERVICES);
    p
}

fn canonical(p: &Path) -> PathBuf {
    // Seatbelt matches the resolved path (/tmp is /private/tmp).
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// One launch's sandbox: the path only it may write.
struct Tag {
    tag: CString,
    probe: CString,
    report: bool,
}

impl Tag {
    fn new() -> Result<Tag, Error> {
        let mut b = [0u8; 16];
        let n = unsafe { libc::getentropy(b.as_mut_ptr() as *mut libc::c_void, b.len()) };
        if n != 0 {
            return Err(Error::Setup("no randomness for the sandbox tag".into()));
        }
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        Tag::from_hex(&hex)
    }

    fn from_hex(hex: &str) -> Result<Tag, Error> {
        if hex.len() != 32 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Usage("bad sandbox tag".into()));
        }
        Ok(Tag {
            tag: CString::new(format!("/private/var/empty/columbus-confine-{hex}")).expect("tag"),
            probe: CString::new(PROBE).expect("probe"),
            report: false,
        })
    }

    fn hex(&self) -> &str {
        let s = self.tag.to_str().expect("tag");
        &s[s.len() - 32..]
    }

    fn check(&self, pid: libc::pid_t, path: &CStr) -> libc::c_int {
        let flags = if self.report {
            SANDBOX_FILTER_PATH
        } else {
            SANDBOX_FILTER_PATH | SANDBOX_CHECK_NO_REPORT
        };
        unsafe { sandbox_check(pid, c"file-write-data".as_ptr(), flags, path.as_ptr()) }
    }

    /// Whether `pid` runs in this launch's sandbox.
    fn holds(&self, pid: libc::pid_t) -> bool {
        // Most processes are not sandboxed: one check rules them out.
        self.check(pid, &self.probe) == 1 && self.check(pid, &self.tag) == 0
    }

    /// Every process in this launch's sandbox.
    fn members(&self) -> Vec<libc::pid_t> {
        let me = std::process::id() as libc::pid_t;
        all_pids()
            .into_iter()
            .filter(|&p| p != me && self.holds(p))
            .collect()
    }

    /// Stop every process in the sandbox, again until none is left (a
    /// process may fork while it is being stopped).
    fn stop_all(&self) {
        for _ in 0..200 {
            let m = self.members();
            if m.is_empty() {
                return;
            }
            for p in m {
                unsafe { libc::kill(p, libc::SIGKILL) };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn all_pids() -> Vec<libc::pid_t> {
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return vec![];
    }
    let mut buf: Vec<libc::pid_t> = vec![0; n as usize + 256];
    let size = (buf.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut libc::c_void, size) };
    buf.truncate(n.max(0) as usize);
    buf.retain(|&p| p > 0);
    buf
}

/// The process's physical footprint (what macOS counts against memory,
/// compressed pages included), or 0 once it has gone.
fn footprint(pid: libc::pid_t) -> u64 {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            &mut info as *mut libc::rusage_info_v4 as *mut libc::rusage_info_t,
        )
    };
    if rc == 0 {
        info.ri_phys_footprint
    } else {
        0
    }
}

fn sandbox_command(level: Level, ro: &[PathBuf], rw: &[PathBuf], tag: &Tag) -> Command {
    let mut cmd = Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(profile(level, ro.len(), rw.len()));
    for (i, s) in SYSTEM_READ.iter().enumerate() {
        cmd.arg("-D").arg(format!("SYS{i}={s}"));
    }
    for (i, p) in ro.iter().enumerate() {
        cmd.arg("-D").arg(format!("RO{i}={}", p.display()));
    }
    for (i, p) in rw.iter().enumerate() {
        cmd.arg("-D").arg(format!("RW{i}={}", p.display()));
    }
    cmd.arg("-D")
        .arg(format!("TAG={}", tag.tag.to_str().expect("tag")));
    cmd
}

/// Before the first launch: can this Mac's sandbox tell the launcher which
/// processes it confines? Without that, a program could outlive its limits,
/// so confinement is refused.
fn self_test() -> Result<(), String> {
    let mut tag = Tag::new().map_err(|e| e.to_string())?;
    let mut child = sandbox_command(Level::A, &[], &[], &tag)
        .arg("--")
        .arg("/bin/sleep")
        .arg("10")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{SANDBOX_EXEC}: {e}"))?;
    let pid = child.id() as libc::pid_t;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut ok = false;
    while Instant::now() < deadline {
        if tag.holds(pid) {
            ok = true;
            break;
        }
        // Some versions refuse the no-report flag; try without it.
        tag.report = !tag.report;
        if tag.holds(pid) {
            ok = true;
            break;
        }
        tag.report = !tag.report;
        std::thread::sleep(Duration::from_millis(50));
    }
    let me = unsafe { libc::getpid() };
    let unsandboxed = !tag.holds(me);
    let _ = child.kill();
    let _ = child.wait();
    if ok && unsandboxed {
        if tag.report {
            REPORT.store(1, Ordering::SeqCst);
        }
        Ok(())
    } else {
        Err("this Mac's sandbox cannot tell Columbus which processes it confines, so it could not stop them all; confinement is refused".into())
    }
}

/// 1 when the self-test found the no-report flag refused.
static REPORT: AtomicU8 = AtomicU8::new(0);

pub fn launch(spec: &Spec, program: &str, args: &[String]) -> Result<i32, Error> {
    available(spec.level)?;
    let ro: Vec<PathBuf> = spec.read_only.iter().map(|p| canonical(p)).collect();
    let rw: Vec<PathBuf> = spec.read_write.iter().map(|p| canonical(p)).collect();
    let mut tag = Tag::new()?;
    tag.report = REPORT.load(Ordering::SeqCst) == 1;
    let tag = Arc::new(tag);

    // The guard stops the sandbox's processes if this launcher is killed.
    // It runs in its own process group, so a group kill aimed at the
    // launcher does not take it too.
    let exe = std::env::current_exe().map_err(|e| Error::Setup(format!("the launcher: {e}")))?;
    let mut guard = Command::new(exe);
    guard
        .arg("--guard")
        .arg(tag.hex())
        .arg(std::process::id().to_string())
        .arg(if tag.report { "report" } else { "quiet" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .process_group(0);
    let mut guard = guard
        .spawn()
        .map_err(|e| Error::Setup(format!("the sandbox guard: {e}")))?;

    let mut child = sandbox_command(spec.level, &ro, &rw, &tag)
        .arg("--")
        .arg(program)
        .args(args)
        .spawn()
        .map_err(|e| Error::Setup(format!("{SANDBOX_EXEC}: {e}")))?;
    let parent = unsafe { libc::getppid() };
    let limit = spec.memory_mb.map(|m| m.saturating_mul(1024 * 1024));
    let procs = spec.processes;
    let done = Arc::new(AtomicBool::new(false));
    // 1: over memory, 2: over processes.
    let over = Arc::new(AtomicU8::new(0));
    let watch = {
        let (tag, done, over) = (tag.clone(), done.clone(), over.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let m = tag.members();
                let reason = if procs.is_some_and(|n| m.len() > n as usize) {
                    2
                } else if limit.is_some_and(|l| m.iter().map(|&p| footprint(p)).sum::<u64>() > l) {
                    1
                } else {
                    0
                };
                if reason != 0 {
                    over.store(reason, Ordering::SeqCst);
                    tag.stop_all();
                    return;
                }
                // The gateway has gone: nothing may run on unwatched.
                if unsafe { libc::getppid() } != parent {
                    tag.stop_all();
                    return;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        })
    };
    let status = child.wait();
    done.store(true, Ordering::SeqCst);
    // Whatever the program left behind in its sandbox ends with it.
    tag.stop_all();
    let _ = watch.join();
    let _ = guard.kill();
    let _ = guard.wait();
    let status = status.map_err(|e| Error::Setup(format!("waiting for the program: {e}")))?;
    match over.load(Ordering::SeqCst) {
        1 => {
            eprintln!(
                "columbus-confine: stopped the program: it used more than {} MB of memory",
                spec.memory_mb.unwrap_or(0)
            );
            return Ok(137);
        }
        2 => {
            eprintln!(
                "columbus-confine: stopped the program: it ran more than {} processes",
                procs.unwrap_or(0)
            );
            return Ok(137);
        }
        _ => {}
    }
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// The guard (`columbus-confine --guard <tag> <launcher pid> <report|quiet>`):
/// waits while the launcher lives, then stops whatever is left in its
/// sandbox.
pub fn guard(args: &[String]) -> i32 {
    let (Some(hex), Some(pid), Some(mode)) = (args.first(), args.get(1), args.get(2)) else {
        return crate::EXIT_CONFINE_FAILED;
    };
    let Ok(mut tag) = Tag::from_hex(hex) else {
        return crate::EXIT_CONFINE_FAILED;
    };
    tag.report = mode == "report";
    let Ok(launcher) = pid.parse::<libc::pid_t>() else {
        return crate::EXIT_CONFINE_FAILED;
    };
    while unsafe { libc::getppid() } == launcher {
        std::thread::sleep(Duration::from_millis(200));
    }
    tag.stop_all();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_take_paths_only_as_parameters() {
        let p = profile(Level::A, 2, 1);
        assert!(p.contains("(param \"RO1\")") && p.contains("(param \"RW0\")"));
        assert!(!p.contains("network-outbound"), "level A has no network");
        assert!(p.contains("(param \"TAG\")"));
        let b = profile(Level::B, 0, 1);
        assert!(b.contains("(remote ip)") && b.contains("localhost:*"));
        let c = profile(Level::C, 0, 1);
        assert!(c.contains("(allow file-read*)\n"));
        assert!(c.contains("(allow file-write* (subpath (param \"RW0\")))"));
        // The service denials come last at every level, after any allow.
        for p in [p, b, c] {
            assert!(p.ends_with(DENIED_SERVICES), "{p}");
        }
    }

    #[test]
    fn tags_are_checked() {
        assert!(Tag::from_hex("00112233445566778899aabbccddeeff").is_ok());
        assert!(Tag::from_hex("../../etc").is_err());
        assert!(Tag::from_hex("0011223344556677").is_err());
    }
}
