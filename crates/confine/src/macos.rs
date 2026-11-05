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
//! * Other processes: a confined program cannot list other processes. It
//!   can read the arguments and environment other processes of the same
//!   user started with (`kern.procargs2`): no sandbox rule covers that, so
//!   secrets in a local server's environment are not hidden from it.
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

/// One launch's sandbox, told apart from every other process by two empty
/// files the launcher creates: the profile lets this sandbox write `tag`
/// and nothing else lets it write `probe` beside it. (The system's check
/// answers only for paths that exist.)
struct Tag {
    tag: CString,
    probe: CString,
}

impl Tag {
    /// Create the two files, in a folder of the launcher's own.
    fn new() -> Result<Tag, Error> {
        let mut b = [0u8; 16];
        let n = unsafe { libc::getentropy(b.as_mut_ptr() as *mut libc::c_void, b.len()) };
        if n != 0 {
            return Err(Error::Setup("no randomness for the sandbox tag".into()));
        }
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let dir = canonical(&user_temp_dir()?).join("columbus-confine");
        let setup =
            |e: std::io::Error| Error::Setup(format!("the sandbox tag in {}: {e}", dir.display()));
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(setup(e)),
        }
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // Ours alone: another user must not be able to remove the files
            // (the launcher would then lose sight of the sandbox).
            let m = std::fs::symlink_metadata(&dir).map_err(setup)?;
            if !m.is_dir() || m.uid() != unsafe { libc::getuid() } {
                return Err(Error::Setup(format!(
                    "{} is not a folder of this user's",
                    dir.display()
                )));
            }
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(setup)?;
        }
        let tag = dir.join(format!("{hex}.tag"));
        let probe = dir.join(format!("{hex}.probe"));
        for f in [&tag, &probe] {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(f)
                .map_err(setup)?;
        }
        Tag::from_paths(&tag.display().to_string(), &probe.display().to_string())
    }

    /// As passed to the guard: two files beside each other, named alike.
    fn from_paths(tag: &str, probe: &str) -> Result<Tag, Error> {
        let bad = || Error::Usage("bad sandbox tag".into());
        let hex = tag.strip_suffix(".tag").ok_or_else(bad)?;
        let name = Path::new(hex)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(bad)?;
        if !tag.starts_with('/')
            || name.len() != 32
            || !name.bytes().all(|c| c.is_ascii_hexdigit())
            || probe != format!("{hex}.probe")
        {
            return Err(bad());
        }
        Ok(Tag {
            tag: CString::new(tag).map_err(|_| bad())?,
            probe: CString::new(probe).map_err(|_| bad())?,
        })
    }

    fn tag_path(&self) -> &str {
        self.tag.to_str().expect("tag")
    }

    fn probe_path(&self) -> &str {
        self.probe.to_str().expect("probe")
    }

    fn may_write(pid: libc::pid_t, path: &CStr) -> libc::c_int {
        unsafe {
            sandbox_check(
                pid,
                c"file-write-data".as_ptr(),
                SANDBOX_FILTER_PATH | SANDBOX_CHECK_NO_REPORT,
                path.as_ptr(),
            )
        }
    }

    /// Whether `pid` runs in this launch's sandbox.
    fn holds(&self, pid: libc::pid_t) -> bool {
        // Most processes are not sandboxed: one check rules them out.
        let sandboxed = unsafe { sandbox_check(pid, std::ptr::null(), 0) } == 1;
        sandboxed && Tag::may_write(pid, &self.tag) == 0 && Tag::may_write(pid, &self.probe) == 1
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

    fn remove(&self) {
        let _ = std::fs::remove_file(self.tag_path());
        let _ = std::fs::remove_file(self.probe_path());
    }
}

/// This user's own temporary folder as macOS assigns it (not `TMPDIR`,
/// which the gateway points into the program's own folder).
fn user_temp_dir() -> Result<PathBuf, Error> {
    let mut buf = vec![0u8; 1024];
    let n = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if n == 0 || n > buf.len() {
        return Err(Error::Setup(
            "macOS gave no temporary folder for this user".into(),
        ));
    }
    buf.truncate(n - 1);
    let s = String::from_utf8(buf)
        .map_err(|_| Error::Setup("this user's temporary folder has an unreadable name".into()))?;
    Ok(PathBuf::from(s))
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
    cmd.arg("-D").arg(format!("TAG={}", tag.tag_path()));
    cmd
}

/// Before the first launch: can this Mac's sandbox tell the launcher which
/// processes it confines? Without that, a program could outlive its limits,
/// so confinement is refused.
fn self_test() -> Result<(), String> {
    let tag = Tag::new().map_err(|e| e.to_string())?;
    let result = (|| {
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
        let mut seen = false;
        while Instant::now() < deadline && !seen {
            seen = tag.holds(pid);
            if !seen {
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        let me = std::process::id() as libc::pid_t;
        let ok = seen && !tag.holds(me) && !tag.holds(1);
        let _ = child.kill();
        let _ = child.wait();
        if ok {
            Ok(())
        } else {
            Err("this Mac's sandbox cannot tell Columbus which processes it confines, so it could not stop them all; confinement is refused".to_string())
        }
    })();
    tag.remove();
    result
}

pub fn launch(spec: &Spec, program: &str, args: &[String]) -> Result<i32, Error> {
    available(spec.level)?;
    let ro: Vec<PathBuf> = spec.read_only.iter().map(|p| canonical(p)).collect();
    let rw: Vec<PathBuf> = spec.read_write.iter().map(|p| canonical(p)).collect();
    let tag = Arc::new(Tag::new()?);
    // A program that could write beside its tag would hide from the check.
    let tag_dir = Path::new(tag.tag_path()).parent().map(Path::to_path_buf);
    if let Some(d) = tag_dir.filter(|d| rw.iter().any(|f| d.starts_with(f))) {
        tag.remove();
        return Err(Error::Setup(format!(
            "a writable folder contains the launcher's own folder {}",
            d.display()
        )));
    }

    // The guard stops the sandbox's processes if this launcher is killed.
    // It runs in its own process group, so a group kill aimed at the
    // launcher does not take it too.
    let exe = std::env::current_exe().map_err(|e| Error::Setup(format!("the launcher: {e}")))?;
    let mut guard = Command::new(exe);
    guard
        .arg("--guard")
        .arg(tag.tag_path())
        .arg(tag.probe_path())
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .process_group(0);
    let mut guard = match guard.spawn() {
        Ok(g) => g,
        Err(e) => {
            tag.remove();
            return Err(Error::Setup(format!("the sandbox guard: {e}")));
        }
    };

    let child = sandbox_command(spec.level, &ro, &rw, &tag)
        .arg("--")
        .arg(program)
        .args(args)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            let _ = guard.kill();
            let _ = guard.wait();
            tag.remove();
            return Err(Error::Setup(format!("{SANDBOX_EXEC}: {e}")));
        }
    };
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
    tag.remove();
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

/// The guard (`columbus-confine --guard <tag> <probe> <launcher pid>`):
/// waits while the launcher lives, then stops whatever is left in its
/// sandbox.
pub fn guard(args: &[String]) -> i32 {
    let (Some(tag), Some(probe), Some(pid)) = (args.first(), args.get(1), args.get(2)) else {
        return crate::EXIT_CONFINE_FAILED;
    };
    let Ok(tag) = Tag::from_paths(tag, probe) else {
        return crate::EXIT_CONFINE_FAILED;
    };
    let Ok(launcher) = pid.parse::<libc::pid_t>() else {
        return crate::EXIT_CONFINE_FAILED;
    };
    while unsafe { libc::getppid() } == launcher {
        std::thread::sleep(Duration::from_millis(200));
    }
    tag.stop_all();
    tag.remove();
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
        let h = "/t/columbus-confine/00112233445566778899aabbccddeeff";
        assert!(Tag::from_paths(&format!("{h}.tag"), &format!("{h}.probe")).is_ok());
        assert!(Tag::from_paths(&format!("{h}.tag"), "/etc/passwd").is_err());
        assert!(Tag::from_paths("/t/../../etc.tag", "/t/../../etc.probe").is_err());
        assert!(Tag::from_paths(
            "rel/00112233445566778899aabbccddeeff.tag",
            "rel/00112233445566778899aabbccddeeff.probe"
        )
        .is_err());
    }
}
