//! Confinement for local processes (design change 1, Rule 5 and section 8).
//!
//! Every local connected server and engine plugin starts through the
//! `columbus-confine` launcher, which confines itself or the child and then
//! runs the real program with the gateway's pipes:
//!
//! | Level | Windows | Linux (development and CI) | Files | Network |
//! | --- | --- | --- | --- | --- |
//! | A | AppContainer, no capabilities, job object | Landlock, seccomp | granted folders only | none |
//! | B | AppContainer with internetClient, job object | Landlock | granted folders only | internet |
//! | C | restricted token at low integrity, job object | Landlock (writes only) | read anywhere, write granted folders only | as the program likes (loopback to a desktop app) |
//!
//! "Granted folders" are the session workspace and the folders the manifest
//! declares, read-write, plus read-only folders the program needs to run
//! (its own folder, its interpreter). A level that cannot be provided on this
//! platform is an error, never a silent fallback: the gateway refuses to
//! start the process unless organization policy allows unconfined pilots.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

/// The confinement level, as manifests and plugin.yaml declare it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Level {
    A,
    B,
    C,
}

impl Level {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "A" => Some(Level::A),
            "B" => Some(Level::B),
            "C" => Some(Level::C),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Level::A => "A",
            Level::B => "B",
            Level::C => "C",
        }
    }
}

/// What a confined process may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub level: Level,
    /// A stable name for the confinement profile (on Windows, the
    /// AppContainer's name): letters, digits, `.` and `-`.
    pub name: String,
    /// Folders the process may read and write (the workspace, declared
    /// folders).
    pub read_write: Vec<PathBuf>,
    /// Folders and files it may read and run but not change (its program,
    /// interpreter and libraries).
    pub read_only: Vec<PathBuf>,
    /// Most memory the process (and its children) may commit, in MiB.
    pub memory_mb: Option<u64>,
    /// Most processes at once, the program included.
    pub processes: Option<u32>,
}

/// How a confinement attempt failed.
#[derive(Debug)]
pub enum Error {
    /// This platform, or this machine, cannot provide the level.
    Unsupported(String),
    /// The arguments are wrong.
    Usage(String),
    /// Setting up the confinement failed.
    Setup(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(m) => write!(f, "confinement not available: {m}"),
            Error::Usage(m) => write!(f, "{m}"),
            Error::Setup(m) => write!(f, "could not set up confinement: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// Exit code the launcher uses when it refused or failed to confine; the
/// gateway reports it as a confinement failure rather than a program error.
pub const EXIT_CONFINE_FAILED: i32 = 97;

fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

impl Spec {
    /// The launcher's arguments for this spec, followed by `--`, the
    /// program and its arguments.
    pub fn launcher_args(&self, program: &str, args: &[String]) -> Vec<String> {
        let mut out = vec![
            "--level".to_string(),
            self.level.as_str().to_string(),
            "--name".to_string(),
            self.name.clone(),
        ];
        for p in &self.read_write {
            out.push("--rw".into());
            out.push(p.display().to_string());
        }
        for p in &self.read_only {
            out.push("--ro".into());
            out.push(p.display().to_string());
        }
        if let Some(m) = self.memory_mb {
            out.push("--memory-mb".into());
            out.push(m.to_string());
        }
        if let Some(n) = self.processes {
            out.push("--processes".into());
            out.push(n.to_string());
        }
        out.push("--".into());
        out.push(program.to_string());
        out.extend(args.iter().cloned());
        out
    }

    /// Parse the launcher's arguments: the spec, then the program and its
    /// arguments after `--`.
    pub fn parse_args(args: &[String]) -> Result<(Spec, String, Vec<String>), Error> {
        let mut level = None;
        let mut name = None;
        let mut spec_rw = Vec::new();
        let mut spec_ro = Vec::new();
        let mut memory_mb = None;
        let mut processes = None;
        let mut i = 0;
        let value = |i: usize| -> Result<&String, Error> {
            args.get(i + 1)
                .ok_or_else(|| Error::Usage(format!("{} needs a value", args[i])))
        };
        while i < args.len() {
            match args[i].as_str() {
                "--" => {
                    let program = args
                        .get(i + 1)
                        .ok_or_else(|| Error::Usage("no program after --".into()))?
                        .clone();
                    let rest = args[i + 2..].to_vec();
                    let level = level.ok_or_else(|| Error::Usage("--level is required".into()))?;
                    let name: String =
                        name.ok_or_else(|| Error::Usage("--name is required".into()))?;
                    if !name_ok(&name) {
                        return Err(Error::Usage(
                            "--name may hold only letters, digits, '.' and '-' (64 at most)".into(),
                        ));
                    }
                    return Ok((
                        Spec {
                            level,
                            name,
                            read_write: spec_rw,
                            read_only: spec_ro,
                            memory_mb,
                            processes,
                        },
                        program,
                        rest,
                    ));
                }
                "--level" => {
                    level = Some(
                        Level::parse(value(i)?)
                            .ok_or_else(|| Error::Usage("--level must be A, B or C".into()))?,
                    );
                    i += 2;
                }
                "--name" => {
                    name = Some(value(i)?.clone());
                    i += 2;
                }
                "--rw" => {
                    spec_rw.push(PathBuf::from(value(i)?));
                    i += 2;
                }
                "--ro" => {
                    spec_ro.push(PathBuf::from(value(i)?));
                    i += 2;
                }
                "--memory-mb" => {
                    memory_mb = Some(
                        value(i)?
                            .parse()
                            .map_err(|_| Error::Usage("--memory-mb must be a number".into()))?,
                    );
                    i += 2;
                }
                "--processes" => {
                    processes = Some(
                        value(i)?
                            .parse()
                            .map_err(|_| Error::Usage("--processes must be a number".into()))?,
                    );
                    i += 2;
                }
                other => return Err(Error::Usage(format!("unknown argument {other:?}"))),
            }
        }
        Err(Error::Usage("expected -- and a program".into()))
    }
}

/// Whether this build can provide `level` on this machine. The gateway
/// asks before starting anything, so a missing capability is reported as
/// a clear status instead of a failed start.
pub fn available(level: Level) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        linux::available(level)
    }
    #[cfg(windows)]
    {
        let _ = level;
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(Error::Unsupported(format!(
            "level {} is not implemented on this operating system",
            level.as_str()
        )))
    }
}

/// The `columbus-confine` launcher: `COLUMBUS_CONFINE` when set, else the
/// file next to the running program (where the installer puts both).
pub fn find_launcher() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COLUMBUS_CONFINE") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let name = if cfg!(windows) {
        "columbus-confine.exe"
    } else {
        "columbus-confine"
    };
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    // Cargo puts test binaries one folder down, in deps/.
    let candidates = [
        Some(dir.join(name)),
        (dir.file_name()? == "deps")
            .then(|| dir.parent().map(|d| d.join(name)))
            .flatten(),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// Whether a process can be started confined at `level` here: the
/// launcher is installed and the operating system supports the level.
pub fn ready(level: Level) -> Result<PathBuf, Error> {
    let launcher = find_launcher().ok_or_else(|| {
        Error::Unsupported(
            "the columbus-confine launcher is not installed next to columbus-mcp".into(),
        )
    })?;
    available(level)?;
    Ok(launcher)
}

/// The folder of the program as the launcher will run it (found on PATH
/// when given without one), which must be readable inside the confinement.
fn program_dir(program: &str) -> Option<PathBuf> {
    let p = std::path::Path::new(program);
    if p.components().count() > 1 {
        return p.parent().map(PathBuf::from);
    }
    let exts: &[&str] = if cfg!(windows) { &["", ".exe"] } else { &[""] };
    std::env::split_paths(&std::env::var_os("PATH")?).find(|d| {
        exts.iter()
            .any(|e| d.join(format!("{program}{e}")).is_file())
    })
}

/// The launcher: confine, then run the program. Returns only on failure
/// (on Linux the program replaces this process) or, on Windows, with the
/// program's exit code once it ends.
pub fn launch(spec: &Spec, program: &str, args: &[String]) -> Result<i32, Error> {
    for p in spec.read_write.iter().chain(&spec.read_only) {
        if !p.is_absolute() {
            return Err(Error::Usage(format!(
                "{} is not an absolute path",
                p.display()
            )));
        }
    }
    let mut spec = spec.clone();
    if let Some(dir) = program_dir(program).filter(|d| d.is_absolute()) {
        spec.read_only.push(dir);
    }
    let spec = &spec;
    #[cfg(target_os = "linux")]
    {
        linux::launch(spec, program, args)
    }
    #[cfg(windows)]
    {
        windows::launch(spec, program, args)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (program, args);
        Err(Error::Unsupported(format!(
            "level {} is not implemented on this operating system",
            spec.level.as_str()
        )))
    }
}

/// Entry point for the `columbus-confine` binary.
pub fn main_cli() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--check") {
        let level = args
            .get(1)
            .and_then(|l| Level::parse(l))
            .unwrap_or(Level::A);
        return match available(level) {
            Ok(()) => {
                println!("level {} available", level.as_str());
                0
            }
            Err(e) => {
                eprintln!("columbus-confine: {e}");
                EXIT_CONFINE_FAILED
            }
        };
    }
    let (spec, program, rest) = match Spec::parse_args(&args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("columbus-confine: {e}");
            return EXIT_CONFINE_FAILED;
        }
    };
    match launch(&spec, &program, &rest) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("columbus-confine: {e}");
            EXIT_CONFINE_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip() {
        let spec = Spec {
            level: Level::B,
            name: "columbus.plugin.lrs-check".into(),
            read_write: vec![PathBuf::from("/ws")],
            read_only: vec![PathBuf::from("/opt/py"), PathBuf::from("/plug")],
            memory_mb: Some(512),
            processes: Some(8),
        };
        let a = spec.launcher_args(
            "/opt/py/bin/python",
            &["-m".into(), "x".into(), "--".into()],
        );
        let (back, prog, rest) = Spec::parse_args(&a).unwrap();
        assert_eq!(back, spec);
        assert_eq!(prog, "/opt/py/bin/python");
        assert_eq!(rest, vec!["-m", "x", "--"]);
    }

    #[test]
    fn bad_args_are_refused() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(Spec::parse_args(&s(&["--level", "D", "--name", "x", "--", "p"])).is_err());
        assert!(Spec::parse_args(&s(&["--name", "x", "--", "p"])).is_err());
        assert!(Spec::parse_args(&s(&["--level", "A", "--name", "a b", "--", "p"])).is_err());
        assert!(Spec::parse_args(&s(&["--level", "A", "--name", "x"])).is_err());
        assert!(
            Spec::parse_args(&s(&["--level", "A", "--name", "x", "--bogus", "--", "p"])).is_err()
        );
    }
}
