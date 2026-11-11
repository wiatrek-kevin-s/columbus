//! Linux confinement, for development and CI (the product runs on Windows).
//!
//! The launcher confines itself and then replaces itself with the program,
//! so the gateway's child process is the confined program:
//!
//! * Files: Landlock. Levels A and B may read the system's program folders
//!   (`/usr`, `/lib`, `/bin`, `/opt` ...), a short list of names in `/etc`
//!   (what the loader, the C library and language runtimes need to start:
//!   never the rest of `/etc`, where the organization's Columbus folder and
//!   other programs' connection settings live), the read-only folders given
//!   and the read-write folders given, and write only the read-write ones.
//!   Nothing in `/proc` can be opened, so other processes cannot be listed
//!   and their command lines cannot be read; Landlock itself refuses a
//!   confined program the memory, environment and open files of every
//!   process outside its sandbox. Level C may read everything and write
//!   only the read-write folders.
//! * Network: level A makes no sockets at all but stream socket pairs;
//!   level B may make internet sockets (IPv4, IPv6) and nothing else. A
//!   datagram socket pair is refused at both: it can be pointed at any Unix
//!   datagram socket outside the sandbox. io_uring, which could make
//!   sockets behind the filter, is refused at every level.
//! * Other processes: where the kernel has Landlock's scopes (6.12 or
//!   later), a confined program cannot signal a process outside its
//!   sandbox. Levels A and B cannot use the kernel's key store, System V
//!   shared memory or message queues of either kind, which other programs
//!   of the same person may share.
//! * Permissions, owners, times and extended attributes: Landlock has no
//!   say over changing these, so at levels A and B the calls do nothing
//!   (and report success, so copying a file still works): no file's
//!   permissions or times change, outside the sandbox or in it.
//! * The filter cannot be stepped around: a call made by its 32-bit number
//!   ends the program, and one made by its x32 number is refused.
//! * Open files: anything the launcher was handed besides its input and
//!   output is closed before the program starts.
//! * Memory: `RLIMIT_AS`.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use landlock::{
    Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, Scope, ABI,
};

use crate::{Error, Level, Spec};

/// Read-only system folders a program needs to start (the loader, libraries
/// and interpreters) and the processor list.
const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/lib",
    "/lib64",
    "/lib32",
    "/bin",
    "/sbin",
    "/opt",
    "/sys/devices/system/cpu",
];

/// The names in `/etc` levels A and B may read: what the loader, the C
/// library, certificate stores and language runtimes need. Everything else
/// in `/etc` stays closed: `/etc/columbus` (the organization's policy,
/// audit log and server settings) and other programs' own settings, which
/// can hold database logins (`odbc.ini`, `pg_service.conf`, `fstab`).
const ETC_READ: &[&str] = &[
    // The loader.
    "ld.so.cache",
    "ld.so.preload",
    "ld.so.conf",
    "ld.so.conf.d",
    // Time zones and names (user names, never passwords: those are in
    // files only root reads).
    "localtime",
    "timezone",
    "nsswitch.conf",
    "passwd",
    "group",
    // Certificates and the TLS libraries' own settings.
    "ssl",
    "pki",
    "crypto-policies",
    "ca-certificates",
    "ca-certificates.conf",
    // Which system this is.
    "os-release",
    "lsb-release",
    "debian_version",
    "redhat-release",
    "system-release",
    // Shared tables programs look things up in.
    "mime.types",
    "magic",
    "services",
    "protocols",
    "locale.alias",
    "fonts",
    "alternatives",
    // R's settings (Debian links R's own folder here).
    "R",
];

/// Folders in `/etc` whose names start with these belong to a language
/// runtime that Debian and Ubuntu link from the runtime's own folder
/// (`/etc/python3.12/sitecustomize.py`, `/etc/java-17-openjdk`).
const ETC_READ_PREFIXES: &[&str] = &["python", "java", "perl", "matplotlib"];

/// What level B also reads, to look names up and connect: the resolver's
/// settings (often a link into `/run`, so those folders too).
const NETWORK_READ: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/host.conf",
    "/etc/gai.conf",
    "/etc/netconfig",
    "/run/systemd/resolve",
    "/run/resolvconf",
    "/run/NetworkManager/resolv.conf",
    "/run/NetworkManager/no-stub-resolv.conf",
];

/// Devices programs open for reading and writing. Not `/dev/tty`: where
/// the gateway was started from a terminal, a confined program could read
/// what the person types there, or type for them.
const DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"];

/// The kernel's Landlock version (ABI), or 0 without Landlock.
fn landlock_abi() -> i64 {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
    unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u32,
        )
    }
}

pub fn available(_level: Level) -> Result<(), Error> {
    if landlock_abi() < 1 {
        return Err(Error::Unsupported(
            "this Linux kernel does not have Landlock enabled".into(),
        ));
    }
    Ok(())
}

/// The names in `/etc` a confined program may read, as they are now.
fn etc_read() -> Vec<PathBuf> {
    let etc = Path::new("/etc");
    let mut out: Vec<PathBuf> = ETC_READ.iter().map(|n| etc.join(n)).collect();
    if let Ok(entries) = std::fs::read_dir(etc) {
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if ETC_READ_PREFIXES.iter().any(|p| name.starts_with(p)) {
                out.push(etc.join(name));
            }
        }
    }
    out
}

fn add_paths(
    mut ruleset: landlock::RulesetCreated,
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
    access: landlock::BitFlags<AccessFs>,
    required: bool,
) -> Result<landlock::RulesetCreated, Error> {
    for p in paths {
        let p = p.as_ref();
        let fd = match PathFd::new(p) {
            Ok(fd) => fd,
            Err(_) if !required => continue,
            Err(e) => return Err(Error::Setup(format!("{}: {e}", p.display()))),
        };
        // A file (rather than a folder) takes only the file rights.
        let rights = if p.is_dir() {
            access
        } else {
            access & AccessFs::from_file(ABI::V3)
        };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, rights))
            .map_err(|e| Error::Setup(format!("{}: {e}", p.display())))?;
    }
    Ok(ruleset)
}

fn restrict_files(spec: &Spec) -> Result<(), Error> {
    let abi = ABI::V3;
    let read = AccessFs::from_read(abi);
    let all = AccessFs::from_all(abi);
    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(all)
        .map_err(|e| Error::Setup(e.to_string()))?;
    let ruleset = match spec.level {
        // Where the kernel can (6.12 or later): no signals to processes
        // outside the sandbox, and no abstract Unix sockets made outside
        // it. Not at level C, whose servers drive desktop apps.
        Level::A | Level::B => ruleset
            .scope(Scope::from_all(ABI::V6))
            .map_err(|e| Error::Setup(e.to_string()))?,
        Level::C => ruleset,
    };
    let ruleset = ruleset.create().map_err(|e| Error::Setup(e.to_string()))?;
    let ruleset = match spec.level {
        // Level C reads anywhere; writes are what it is kept from.
        Level::C => add_paths(ruleset, ["/"], read, true)?,
        Level::A | Level::B => {
            let r = add_paths(ruleset, SYSTEM_READ, read, false)?;
            let r = add_paths(r, etc_read(), read, false)?;
            let r = if spec.level == Level::B {
                add_paths(r, NETWORK_READ, read, false)?
            } else {
                r
            };
            add_paths(r, &spec.read_only, read, true)?
        }
    };
    let mut devices = DEVICES.to_vec();
    if spec.level == Level::C {
        devices.push("/dev/tty");
    }
    let ruleset = add_paths(ruleset, devices, AccessFs::from_file(abi), false)?;
    let ruleset = add_paths(ruleset, &spec.read_write, all, true)?;
    let status = ruleset
        .restrict_self()
        .map_err(|e| Error::Setup(e.to_string()))?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err(Error::Unsupported(
            "Landlock is present but did not enforce the rules".into(),
        ));
    }
    Ok(())
}

/// The system calls each level refuses (with EACCES).
///
/// - Level A: every socket (internet, Unix, netlink, packet, and the rest:
///   virtual machine sockets, Bluetooth, SMC, which reaches TCP by another
///   name). Stream socket pairs stay.
/// - Level B: every socket but IPv4 and IPv6 ones. A Unix socket could
///   reach the session bus, a Docker socket or an ssh agent outside the
///   sandbox.
/// - Levels A and B: datagram socket pairs (one can be pointed at any Unix
///   datagram socket outside the sandbox), leaving the process group
///   (setsid, setpgid), so stopping the program stops everything it
///   started, the kernel's key store (other programs' keys and tickets) and
///   System V shared memory and message queues (other programs' data).
/// - Level C: raw packet sockets.
/// - io_uring everywhere confined: it can open sockets without socket().
/// - `truncate` on kernels whose Landlock cannot yet refuse it (before
///   6.2): it would empty any file the person may write.
///
/// Two more filters follow it: at levels A and B, calls that change a
/// file's permissions, owner, times or extended attributes do nothing
/// ([`METADATA_CALLS`]); and on x86-64, at every level, a call made by its
/// x32 number is refused ([`refuse_x32`]), or every rule here could be
/// stepped around on kernels that have that table.
fn syscall_filter(level: Level, abi: i64) -> Result<(), Error> {
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|_| Error::Unsupported(format!("seccomp on {}", std::env::consts::ARCH)))?;
    let setup = |e: seccompiler::BackendError| Error::Setup(e.to_string());
    let arg = |index: u8, op: SeccompCmpOp, value: u64| {
        SeccompCondition::new(index, SeccompCmpArgLen::Dword, op, value).map_err(setup)
    };
    let mut rules = std::collections::BTreeMap::new();
    let socket_rules = match level {
        // No condition: every call matches.
        Level::A => vec![],
        // Refused unless the family is IPv4 or IPv6.
        Level::B => vec![SeccompRule::new(vec![
            arg(0, SeccompCmpOp::Ne, libc::AF_INET as u64)?,
            arg(0, SeccompCmpOp::Ne, libc::AF_INET6 as u64)?,
        ])
        .map_err(setup)?],
        Level::C => {
            vec![
                SeccompRule::new(vec![arg(0, SeccompCmpOp::Eq, libc::AF_PACKET as u64)?])
                    .map_err(setup)?,
            ]
        }
    };
    rules.insert(libc::SYS_socket, socket_rules);
    rules.insert(libc::SYS_io_uring_setup, vec![]);
    if matches!(level, Level::A | Level::B) {
        // The socket type is the low bits; SOCK_NONBLOCK and SOCK_CLOEXEC
        // ride above them.
        rules.insert(
            libc::SYS_socketpair,
            vec![SeccompRule::new(vec![arg(
                1,
                SeccompCmpOp::MaskedEq(0xf),
                libc::SOCK_DGRAM as u64,
            )?])
            .map_err(setup)?],
        );
        for call in [
            libc::SYS_setsid,
            libc::SYS_setpgid,
            libc::SYS_keyctl,
            libc::SYS_add_key,
            libc::SYS_request_key,
            libc::SYS_shmget,
            libc::SYS_shmat,
            libc::SYS_shmctl,
            libc::SYS_msgget,
            libc::SYS_msgsnd,
            libc::SYS_msgrcv,
            libc::SYS_msgctl,
            // POSIX message queues are named for the whole computer too.
            libc::SYS_mq_open,
            libc::SYS_mq_unlink,
        ] {
            rules.insert(call, vec![]);
        }
    }
    if abi < 3 {
        rules.insert(libc::SYS_truncate, vec![]);
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EACCES as u32),
        arch,
    )
    .map_err(setup)?;
    let program: seccompiler::BpfProgram = filter.try_into().map_err(setup)?;
    seccompiler::apply_filter(&program).map_err(|e| Error::Setup(e.to_string()))?;

    if matches!(level, Level::A | Level::B) {
        let mut quiet = std::collections::BTreeMap::new();
        for call in METADATA_CALLS {
            quiet.insert(*call, vec![]);
        }
        // "Errno 0": the call is not made, and the program is told it was.
        let filter = SeccompFilter::new(quiet, SeccompAction::Allow, SeccompAction::Errno(0), arch)
            .map_err(setup)?;
        let program: seccompiler::BpfProgram = filter.try_into().map_err(setup)?;
        seccompiler::apply_filter(&program).map_err(|e| Error::Setup(e.to_string()))?;
    }
    refuse_x32()
}

/// The calls that change a file's permissions, owner, times or extended
/// attributes. Landlock does not cover them (as of Linux 6.18), so a
/// confined program could make the person's private files readable by
/// everyone on the computer, lock them out of their own files, or change
/// when a file says it was written, anywhere the person owns files. At
/// levels A and B these calls do nothing and report success: `shutil.copy`
/// and `copy2`, which make them on the copy, go on working, and nothing
/// changes that way, outside the sandbox or in it. (A new file still gets
/// the permissions it is created with.)
const METADATA_CALLS: &[libc::c_long] = &[
    libc::SYS_fchmod,
    libc::SYS_fchmodat,
    // fchmodat2 (Linux 6.6), the same number everywhere.
    452,
    libc::SYS_fchown,
    libc::SYS_fchownat,
    libc::SYS_utimensat,
    libc::SYS_setxattr,
    libc::SYS_lsetxattr,
    libc::SYS_fsetxattr,
    libc::SYS_removexattr,
    libc::SYS_lremovexattr,
    libc::SYS_fremovexattr,
    // setxattrat and removexattrat (Linux 6.13), likewise.
    463,
    466,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chmod,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chown,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_lchown,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_utime,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_utimes,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_futimesat,
];

/// On x86-64 a kernel may also answer the "x32" table of system calls,
/// whose numbers are the usual ones with one high bit set. The filters
/// above name calls by their usual numbers, so without this a program
/// could make any refused call by its other number. Nothing Columbus runs
/// uses that table: every call in it is refused.
#[cfg(target_arch = "x86_64")]
fn refuse_x32() -> Result<(), Error> {
    use seccompiler::sock_filter;
    // BPF: load a word of the call's data; jump if greater or equal; return.
    const LOAD_WORD: u16 = 0x20;
    const JUMP_IF_GE: u16 = 0x35;
    const RETURN: u16 = 0x06;
    const X32_BIT: u32 = 0x4000_0000;
    let step = |code, jt, jf, k| sock_filter { code, jt, jf, k };
    let program = [
        // The call's number is the first word.
        step(LOAD_WORD, 0, 0, 0),
        step(JUMP_IF_GE, 0, 1, X32_BIT),
        step(RETURN, 0, 0, libc::SECCOMP_RET_ERRNO | libc::EACCES as u32),
        step(RETURN, 0, 0, libc::SECCOMP_RET_ALLOW),
    ];
    seccompiler::apply_filter(&program).map_err(|e| Error::Setup(e.to_string()))
}

#[cfg(not(target_arch = "x86_64"))]
fn refuse_x32() -> Result<(), Error> {
    Ok(())
}

/// Close every file the launcher was handed besides its input, output and
/// error streams: an open file, folder or socket of the gateway's (or of
/// the AI app that started it) must not reach the confined program.
fn close_inherited() {
    let closed = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
    if closed == 0 {
        return;
    }
    // Before Linux 5.9: one by one, from the list of what is open.
    let mut open: Vec<i32> = std::fs::read_dir("/proc/self/fd")
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok()))
                .collect()
        })
        .unwrap_or_else(|_| (3..1024).collect());
    open.retain(|fd| *fd > 2);
    for fd in open {
        unsafe { libc::close(fd) };
    }
}

/// Run as root, a confined program would keep every capability (reading
/// any process's memory and environment, loading kernel modules). Drop them
/// all, including the bounding and ambient sets, before anything else.
fn drop_capabilities() -> Result<(), Error> {
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        );
        for cap in 0..64 {
            libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0);
        }
    }
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let mut h = Header {
        version: 0x2008_0522, // _LINUX_CAPABILITY_VERSION_3
        pid: 0,
    };
    let d = [Data::default(); 2];
    let r = unsafe { libc::syscall(libc::SYS_capset, &mut h as *mut Header, d.as_ptr()) };
    if r != 0 {
        return Err(Error::Setup(format!(
            "dropping capabilities: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn limit_memory(mb: u64) -> Result<(), Error> {
    let bytes = mb.saturating_mul(1024 * 1024);
    let lim = libc::rlimit {
        rlim_cur: bytes as libc::rlim_t,
        rlim_max: bytes as libc::rlim_t,
    };
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &lim) } != 0 {
        return Err(Error::Setup(format!(
            "memory limit: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

pub fn launch(spec: &Spec, program: &str, args: &[String]) -> Result<i32, Error> {
    available(spec.level)?;
    let abi = landlock_abi();
    close_inherited();
    if let Some(mb) = spec.memory_mb {
        limit_memory(mb)?;
    }
    drop_capabilities()?;
    restrict_files(spec)?;
    syscall_filter(spec.level, abi)?;
    // Replace this process with the program: it inherits the pipes, the
    // rules and the limits, and nothing can lift them.
    let err = std::process::Command::new(program).args(args).exec();
    Err(Error::Setup(format!("could not start {program}: {err}")))
}
