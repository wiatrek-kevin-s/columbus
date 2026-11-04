//! Linux confinement, for development and CI (the product runs on Windows).
//!
//! The launcher confines itself and then replaces itself with the program,
//! so the gateway's child process is the confined program:
//!
//! * Files: Landlock. Levels A and B may read the system's own folders
//!   (`/usr`, `/lib`, `/etc`, ...), the read-only folders given and the
//!   read-write folders given, and write only the read-write ones. Level C
//!   may read everything and write only the read-write folders.
//! * Network: level A refuses creating internet sockets (and io_uring,
//!   which could create them behind the filter) with seccomp.
//! * Memory: `RLIMIT_AS`.

use std::os::unix::process::CommandExt;
use std::path::Path;

use landlock::{
    Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, ABI,
};

use crate::{Error, Level, Spec};

/// Read-only system folders a program needs to start (loader, libraries,
/// interpreters, certificates, time zones).
const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/lib",
    "/lib64",
    "/lib32",
    "/bin",
    "/sbin",
    "/etc",
    "/opt",
    "/proc",
    "/sys/devices/system/cpu",
];
/// Devices programs open for reading and writing.
const DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/urandom",
    "/dev/random",
    "/dev/tty",
];

pub fn available(_level: Level) -> Result<(), Error> {
    // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u32,
        )
    };
    if abi < 1 {
        return Err(Error::Unsupported(
            "this Linux kernel does not have Landlock enabled".into(),
        ));
    }
    Ok(())
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
        .map_err(|e| Error::Setup(e.to_string()))?
        .create()
        .map_err(|e| Error::Setup(e.to_string()))?;
    let ruleset = match spec.level {
        // Level C reads anywhere; writes are what it is kept from.
        Level::C => add_paths(ruleset, ["/"], read, true)?,
        Level::A | Level::B => {
            let r = add_paths(ruleset, SYSTEM_READ, read, false)?;
            add_paths(r, &spec.read_only, read, true)?
        }
    };
    let ruleset = add_paths(ruleset, DEVICES, AccessFs::from_file(abi), false)?;
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

fn deny_network() -> Result<(), Error> {
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };
    let arch = TargetArch::try_from(std::env::consts::ARCH)
        .map_err(|_| Error::Unsupported(format!("seccomp on {}", std::env::consts::ARCH)))?;
    let family = |f: i32| -> Result<SeccompRule, Error> {
        SeccompRule::new(vec![SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Eq,
            f as u64,
        )
        .map_err(|e| Error::Setup(e.to_string()))?])
        .map_err(|e| Error::Setup(e.to_string()))
    };
    let mut rules = std::collections::BTreeMap::new();
    rules.insert(
        libc::SYS_socket,
        vec![
            family(libc::AF_INET)?,
            family(libc::AF_INET6)?,
            family(libc::AF_PACKET)?,
        ],
    );
    // io_uring can open sockets without the socket() system call.
    rules.insert(libc::SYS_io_uring_setup, vec![]);
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EACCES as u32),
        arch,
    )
    .map_err(|e| Error::Setup(e.to_string()))?;
    let program: seccompiler::BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| Error::Setup(e.to_string()))?;
    seccompiler::apply_filter(&program).map_err(|e| Error::Setup(e.to_string()))
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
    if let Some(mb) = spec.memory_mb {
        limit_memory(mb)?;
    }
    restrict_files(spec)?;
    if spec.level == Level::A {
        deny_network()?;
    }
    // Replace this process with the program: it inherits the pipes, the
    // rules and the limits, and nothing can lift them.
    let err = std::process::Command::new(program).args(args).exec();
    Err(Error::Setup(format!("could not start {program}: {err}")))
}
