//! The `columbus-confine` launcher at each level (design change 1, Rule 5),
//! on whatever this machine is: Linux (Landlock, seccomp), macOS
//! (Seatbelt) or Windows (AppContainer, restricted token, job object). Uses Python as the
//! confined program so the same checks run on both.

use std::path::{Path, PathBuf};
use std::process::Command;

fn python() -> Option<String> {
    ["python3", "python"]
        .into_iter()
        .map(String::from)
        .find(|p| {
            Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
}

fn launcher() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_columbus-confine"))
}

fn can_confine() -> bool {
    Command::new(launcher())
        .args(["--check", "A"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The Python installation's folders, readable inside the confinement.
fn python_dirs(py: &str) -> Vec<String> {
    let out = Command::new(py)
        .args([
            "-I",
            "-c",
            "import sys; print(sys.base_prefix); print(sys.prefix)",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// One launch at a time: the first launch at a level grants the test
/// container access to the Python folders, which rewrites their ACLs, and a
/// process starting during that rewrite can fail to load its DLLs.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(level: &str, rw: &Path, extra: &[&str], py: &str, script: &str) -> Run {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut args: Vec<String> = vec![
        "--level".into(),
        level.into(),
        "--name".into(),
        "test.confine".into(),
        "--rw".into(),
        rw.display().to_string(),
    ];
    for d in python_dirs(py) {
        args.push("--ro".into());
        args.push(d);
    }
    args.extend(extra.iter().map(|s| s.to_string()));
    args.push("--".into());
    args.push(py.into());
    args.extend(["-I".into(), "-c".into(), script.into()]);
    let out = Command::new(launcher())
        .args(&args)
        .current_dir(rw)
        .env("TEMP", rw)
        .env("TMP", rw)
        .env("TMPDIR", rw)
        .output()
        .unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A Python one-liner that tries `expr` and prints ok or refused.
fn attempt(label: &str, expr: &str) -> String {
    format!(
        "\ntry:\n    {expr}\n    print('{label}: ok')\nexcept Exception as e:\n    print('{label}: refused', type(e).__name__)\n"
    )
}

struct Dirs {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    outside: PathBuf,
}

fn dirs() -> Dirs {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "the secret").unwrap();
    std::fs::write(ws.join("inside.txt"), "inside").unwrap();
    Dirs {
        _tmp: tmp,
        ws: ws.canonicalize().unwrap(),
        outside: outside.canonicalize().unwrap(),
    }
}

fn script(d: &Dirs, port: u16) -> String {
    let p = |p: PathBuf| format!("{:?}", p.display().to_string());
    [
        attempt(
            "read_inside",
            &format!("open({}).read()", p(d.ws.join("inside.txt"))),
        ),
        attempt(
            "write_inside",
            &format!("open({}, 'w').write('x')", p(d.ws.join("new.txt"))),
        ),
        attempt(
            "read_outside",
            &format!("open({}).read()", p(d.outside.join("secret.txt"))),
        ),
        attempt(
            "write_outside",
            &format!("open({}, 'w').write('x')", p(d.outside.join("new.txt"))),
        ),
        format!(
            "import socket\n{}",
            attempt(
                "network",
                &format!("socket.create_connection(('127.0.0.1', {port}), timeout=3).close()")
            )
        ),
    ]
    .concat()
}

fn has(out: &Run, line: &str) -> bool {
    out.stdout.lines().any(|l| l.starts_with(line))
}

#[test]
fn level_a_keeps_files_and_network_in() {
    let Some(py) = python() else { return };
    if !can_confine() {
        eprintln!("skipping: this machine cannot confine");
        return;
    }
    let d = dirs();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let out = run("A", &d.ws, &[], &py, &script(&d, port));
    assert_eq!(out.code, 0, "{} {}", out.stdout, out.stderr);
    for l in [
        "read_inside: ok",
        "write_inside: ok",
        "read_outside: refused",
        "write_outside: refused",
        "network: refused",
    ] {
        assert!(
            has(&out, l),
            "expected {l:?} (exit code {}):\n{}{}",
            out.code,
            out.stdout,
            out.stderr
        );
    }
    assert!(!d.outside.join("new.txt").exists());
}

#[test]
fn level_b_allows_network_but_not_files() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    // Level B is for public APIs; on Windows AppContainers still cannot
    // reach loopback, so only check that sockets can be made here.
    let s = [
        attempt(
            "read_outside",
            &format!(
                "open({:?}).read()",
                d.outside.join("secret.txt").display().to_string()
            ),
        ),
        format!(
            "import socket\n{}",
            attempt("socket", "socket.socket().close()")
        ),
    ]
    .concat();
    let out = run("B", &d.ws, &[], &py, &s);
    assert!(
        has(&out, "read_outside: refused"),
        "{}{}",
        out.stdout,
        out.stderr
    );
    assert!(has(&out, "socket: ok"), "{}{}", out.stdout, out.stderr);
}

#[test]
fn level_c_reads_anywhere_but_writes_only_granted_folders() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    if cfg!(windows) {
        // Not available on Windows yet: refused, never run unconfined.
        let check = Command::new(launcher())
            .args(["--check", "C"])
            .output()
            .unwrap();
        assert!(!check.status.success());
        let out = run("C", &d.ws, &[], &py, "print('ran')");
        assert!(!out.stdout.contains("ran"), "{}", out.stdout);
        assert!(
            out.stderr.contains("not available on Windows"),
            "{}",
            out.stderr
        );
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let out = run("C", &d.ws, &[], &py, &script(&d, port));
    for l in [
        "read_inside: ok",
        "write_inside: ok",
        "read_outside: ok",
        "write_outside: refused",
        "network: ok",
    ] {
        assert!(
            has(&out, l),
            "expected {l:?} (exit code {}):\n{}{}",
            out.code,
            out.stdout,
            out.stderr
        );
    }
    assert!(!d.outside.join("new.txt").exists());
}

#[test]
fn memory_limit_applies() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    if cfg!(target_os = "macos") {
        // macOS does not refuse the allocation; the launcher stops the
        // program once it is using the memory.
        // Polled, not enforced by the kernel: the program holds the memory
        // for a moment, as real work does.
        let s = "import time\nb = bytearray(1024 * 1024 * 1024)\nfor i in range(0, len(b), 4096): b[i] = 1\ntime.sleep(2)\nprint('big: ok')\n";
        let out = run("A", &d.ws, &["--memory-mb", "256"], &py, s);
        assert!(!has(&out, "big: ok"), "{}{}", out.stdout, out.stderr);
        assert_eq!(out.code, 137, "{}", out.stderr);
        assert!(out.stderr.contains("more than 256 MB"), "{}", out.stderr);
        return;
    }
    let s = attempt("big", "bytearray(1024 * 1024 * 1024)");
    let out = run("A", &d.ws, &["--memory-mb", "256"], &py, &s);
    assert!(has(&out, "big: refused"), "{}{}", out.stdout, out.stderr);
}

#[test]
fn exit_codes_and_bad_arguments() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let out = run("A", &d.ws, &[], &py, "import sys; sys.exit(7)");
    assert_eq!(out.code, 7, "the program's exit code comes back");
    let bad = Command::new(launcher())
        .args(["--level", "Z", "--name", "x", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(
        bad.status.code(),
        Some(columbus_confine::EXIT_CONFINE_FAILED)
    );
    let rel = Command::new(launcher())
        .args([
            "--level",
            "A",
            "--name",
            "x",
            "--rw",
            "relative/path",
            "--",
            "true",
        ])
        .output()
        .unwrap();
    assert_eq!(
        rel.status.code(),
        Some(columbus_confine::EXIT_CONFINE_FAILED)
    );
}

#[test]
fn level_a_starts_with_the_small_environment_plugins_get() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let keep = [
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "SystemDrive",
        "windir",
        "COMSPEC",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "ProgramData",
        "ALLUSERSPROFILE",
        "PUBLIC",
        "USERNAME",
        "USERDOMAIN",
        "HOMEDRIVE",
        "HOMEPATH",
        "ProgramFiles",
        "CommonProgramFiles",
    ];
    let mut args: Vec<String> = vec![
        "--level".into(),
        "A".into(),
        "--name".into(),
        "test.env".into(),
        "--rw".into(),
        d.ws.display().to_string(),
    ];
    for dir in python_dirs(&py) {
        args.push("--ro".into());
        args.push(dir);
    }
    args.extend([
        "--".into(),
        py.clone(),
        "-I".into(),
        "-c".into(),
        "print('started')".into(),
    ]);
    let mut cmd = Command::new(launcher());
    cmd.args(&args).env_clear().current_dir(&d.ws);
    for k in keep {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let o = cmd.output().unwrap();
    assert!(
        String::from_utf8_lossy(&o.stdout).contains("started"),
        "exit {:?}: {}",
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Level A on macOS: no Unix socket outside, no system services, no
/// reading the person's home folder outside the grant.
#[cfg(target_os = "macos")]
#[test]
fn level_a_on_macos_reaches_no_socket_service_or_home_folder() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let sock = d.outside.join("bus.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    // A local TCP service outside. Seatbelt refuses the connection, not the
    // socket call, so the test connects.
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_port = tcp.local_addr().unwrap().port();
    let home = std::env::var("HOME").unwrap();
    let s = [
        format!(
            "import socket, os, subprocess\n{}",
            attempt(
                "unix",
                &format!(
                    "socket.socket(socket.AF_UNIX).connect({:?})",
                    sock.display().to_string()
                )
            )
        ),
        attempt("pair", "socket.socketpair()"),
        attempt("home", &format!("os.listdir({home:?})")),
        attempt(
            "keychains",
            &format!("os.listdir({:?})", format!("{home}/Library/Keychains")),
        ),
        attempt(
            "tcp",
            &format!("socket.create_connection(('127.0.0.1', {tcp_port}), timeout=3)"),
        ),
        attempt(
            "exec_outside",
            "subprocess.run(['/usr/bin/true'], check=True)",
        ),
        MAC_PROBES.to_string(),
    ]
    .concat();
    let out = run("A", &d.ws, &[], &py, &s);
    for l in [
        "unix: refused",
        "pair: ok",
        "home: refused",
        "keychains: refused",
        "tcp: refused",
        "exec_outside: ok",
        // The certificate service fetches from the network for its caller.
        "svc com.apple.trustd: refused",
        "svc com.apple.coreservices.launchservicesd: refused",
        "svc com.apple.coreservices.appleevents: refused",
        "svc com.apple.pasteboard.1: refused",
        "svc com.apple.SecurityServer: refused",
        // Not "procargs parent": macOS's sandbox does not cover reading
        // the arguments and environment other processes of the same user
        // started with (a known limit, in the README).
        "proc list: refused",
    ] {
        assert!(
            has(&out, l),
            "expected {l:?}:\n{}{}",
            out.stdout,
            out.stderr
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn level_a_has_no_unix_sockets_and_cannot_leave_its_process_group() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    // A listening Unix socket outside the confinement.
    let sock = d.outside.join("bus.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let s = [
        format!(
            "import socket, os\n{}",
            attempt(
                "unix",
                &format!(
                    "socket.socket(socket.AF_UNIX).connect({:?})",
                    sock.display().to_string()
                )
            )
        ),
        attempt("abstract", "socket.socket(socket.AF_UNIX).connect('\\0x')"),
        attempt("setsid", "os.setsid()"),
        attempt("pair", "socket.socketpair()"),
    ]
    .concat();
    let out = run("A", &d.ws, &[], &py, &s);
    for l in [
        "unix: refused",
        "abstract: refused",
        "setsid: refused",
        "pair: ok",
    ] {
        assert!(
            has(&out, l),
            "expected {l:?}:\n{}{}",
            out.stdout,
            out.stderr
        );
    }
}

/// Python run inside the confinement on a Mac: reaches system services,
/// other processes' arguments and launchd.
#[cfg(target_os = "macos")]
const MAC_PROBES: &str = r#"
import ctypes, os
_lib = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
_bp = ctypes.c_uint.in_dll(_lib, "bootstrap_port")
for _n in ["com.apple.trustd", "com.apple.coreservices.launchservicesd", "com.apple.coreservices.appleevents",
           "com.apple.pasteboard.1", "com.apple.SecurityServer", "com.apple.windowserver.active",
           "com.apple.dock.server", "com.apple.backgroundtaskmanagement.agent", "com.apple.securityd"]:
    _port = ctypes.c_uint(0)
    _kr = _lib.bootstrap_look_up(_bp, _n.encode(), ctypes.byref(_port))
    print(f"svc {_n}:", "ok" if _kr == 0 else "refused")
def _sysctl(mib):
    m = (ctypes.c_int * len(mib))(*mib)
    size = ctypes.c_size_t(0)
    if _lib.sysctl(m, len(mib), None, ctypes.byref(size), None, 0) != 0 or size.value == 0:
        return "refused"
    buf = ctypes.create_string_buffer(size.value)
    return "ok" if _lib.sysctl(m, len(mib), buf, ctypes.byref(size), None, 0) == 0 else "refused"
print("procargs parent:", _sysctl([1, 49, os.getppid()]))
print("proc list:", _sysctl([1, 14, 0]))
"#;

/// Launch with a program that leaves a process behind: it double-forks,
/// leaves the process group (setsid) and keeps writing a heartbeat.
#[cfg(target_os = "macos")]
fn escapee(beat: &Path, main_sleep: f64) -> String {
    format!(
        r#"
import os, time
if os.fork() == 0:
    os.setsid()
    if os.fork() == 0:
        for i in range(600):
            with open({beat:?}, "a") as f: f.write("x")
            time.sleep(0.1)
        os._exit(0)
    os._exit(0)
time.sleep({main_sleep})
print("main: done")
"#,
        beat = beat.display().to_string()
    )
}

#[cfg(target_os = "macos")]
fn still_beating(beat: &Path) -> bool {
    let a = std::fs::metadata(beat).map(|m| m.len()).unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let b = std::fs::metadata(beat).map(|m| m.len()).unwrap_or(0);
    b > a
}

#[cfg(target_os = "macos")]
#[test]
fn on_macos_nothing_outlives_the_program() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let beat = d.ws.join("beat.txt");
    let out = run("A", &d.ws, &[], &py, &escapee(&beat, 1.0));
    assert!(has(&out, "main: done"), "{}{}", out.stdout, out.stderr);
    assert!(beat.exists(), "the escapee ran: {}", out.stderr);
    assert!(
        !still_beating(&beat),
        "a process that left the group was stopped with the program"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn on_macos_killing_the_launcher_stops_the_sandbox() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let d = dirs();
    let beat = d.ws.join("beat.txt");
    let mut args: Vec<String> = vec![
        "--level".into(),
        "A".into(),
        "--name".into(),
        "test.confine".into(),
        "--rw".into(),
        d.ws.display().to_string(),
    ];
    for p in python_dirs(&py) {
        args.push("--ro".into());
        args.push(p);
    }
    args.extend([
        "--".into(),
        py.clone(),
        "-I".into(),
        "-c".into(),
        escapee(&beat, 60.0),
    ]);
    let mut child = Command::new(launcher())
        .args(&args)
        .current_dir(&d.ws)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !beat.exists() && start.elapsed().as_secs() < 20 {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(beat.exists(), "the escapee ran");
    // As the gateway stops a plugin: SIGKILL to the launcher alone.
    child.kill().unwrap();
    child.wait().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(800));
    assert!(
        !still_beating(&beat),
        "the guard stopped what the killed launcher left"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn on_macos_processes_and_memory_count_the_whole_tree() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let many = "import os, time\nfor i in range(12):\n    if os.fork() == 0:\n        time.sleep(20); os._exit(0)\ntime.sleep(20)\nprint('many: ok')\n";
    // The program cannot lift the kernel's limit itself.
    let lift = "import os, resource, time\nsoft, hard = resource.getrlimit(resource.RLIMIT_NPROC)\ntry:\n    resource.setrlimit(resource.RLIMIT_NPROC, (hard + 1000, hard + 1000))\n    print('raise: ok')\nexcept (ValueError, OSError):\n    print('raise: refused')\nn = 0\nfor i in range(200):\n    try:\n        if os.fork() == 0:\n            time.sleep(20); os._exit(0)\n        n += 1\n    except OSError:\n        break\nprint('forked', n, flush=True)\ntime.sleep(20)\n";
    let out = run("A", &d.ws, &["--processes", "5"], &py, lift);
    assert!(has(&out, "raise: refused"), "{}{}", out.stdout, out.stderr);
    let forked: usize = out
        .stdout
        .lines()
        .find_map(|l| {
            l.strip_prefix("forked ")
                .and_then(|n| n.trim().parse().ok())
        })
        .unwrap_or(0);
    assert!(
        forked < 200,
        "the kernel refused forks past the limit: {}",
        out.stdout
    );
    let out = run("A", &d.ws, &["--processes", "5"], &py, many);
    assert!(!has(&out, "many: ok"), "{}{}", out.stdout, out.stderr);
    assert_eq!(out.code, 137, "{}", out.stderr);
    assert!(
        out.stderr.contains("more than 5 processes"),
        "{}",
        out.stderr
    );
    // Three children of 120 MB each: none alone passes 256 MB, together they do.
    let wide = "import os, time\nfor i in range(3):\n    if os.fork() == 0:\n        b = bytearray(120 * 1024 * 1024)\n        for j in range(0, len(b), 4096): b[j] = 1\n        time.sleep(20); os._exit(0)\ntime.sleep(20)\nprint('wide: ok')\n";
    let out = run("A", &d.ws, &["--memory-mb", "256"], &py, wide);
    assert!(!has(&out, "wide: ok"), "{}{}", out.stdout, out.stderr);
    assert_eq!(out.code, 137, "{}", out.stderr);
    assert!(out.stderr.contains("more than 256 MB"), "{}", out.stderr);
}

#[cfg(target_os = "macos")]
#[test]
fn on_macos_level_b_reaches_the_internet_side_only() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = tcp.local_addr().unwrap().port();
    let tcp6 = std::net::TcpListener::bind("[::1]:0").unwrap();
    let port6 = tcp6.local_addr().unwrap().port();
    let s = [
        "import socket\n".to_string(),
        attempt(
            "loopback6",
            &format!("socket.create_connection(('::1', {port6}), timeout=3)"),
        ),
        attempt(
            "any",
            &format!("socket.create_connection(('0.0.0.0', {port}), timeout=3)"),
        ),
        attempt(
            "mapped",
            &format!("socket.create_connection(('::ffff:127.0.0.1', {port}), timeout=3)"),
        ),
        attempt(
            "loopback",
            &format!("socket.create_connection(('127.0.0.1', {port}), timeout=3)"),
        ),
        attempt(
            "localhost",
            &format!("socket.create_connection(('localhost', {port}), timeout=3)"),
        ),
        attempt(
            "udp",
            "socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(b'x', ('127.0.0.1', 9))",
        ),
    ]
    .concat();
    let out = run("B", &d.ws, &[], &py, &s);
    for l in [
        "loopback: refused",
        "localhost: refused",
        "loopback6: refused",
        "mapped: refused",
        "any: refused",
        "udp: refused",
    ] {
        assert!(
            has(&out, l),
            "expected {l:?}:\n{}{}",
            out.stdout,
            out.stderr
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn on_macos_level_c_cannot_act_through_other_programs() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let marker = d.outside.join("launchd-wrote-this");
    let s = format!(
        "{MAC_PROBES}\n{TAG_WRITE}\nimport subprocess, time\nr = subprocess.run(['/bin/launchctl', 'submit', '-l', 'io.columbus.test-escape', '--', '/usr/bin/touch', {m:?}], capture_output=True)\nprint('launchctl rc', r.returncode)\ntime.sleep(2)\nsubprocess.run(['/bin/launchctl', 'remove', 'io.columbus.test-escape'], capture_output=True)\n",
        m = marker.display().to_string()
    );
    let out = run("C", &d.ws, &[], &py, &s);
    for l in [
        "svc com.apple.coreservices.launchservicesd: refused",
        "svc com.apple.coreservices.appleevents: refused",
        "svc com.apple.pasteboard.1: refused",
        "svc com.apple.SecurityServer: refused",
        "svc com.apple.windowserver.active: refused",
        "svc com.apple.dock.server: refused",
        "svc com.apple.backgroundtaskmanagement.agent: refused",
        "svc com.apple.securityd: refused",
        // Level C reads everything, so it can find its own tag; it still
        // cannot write into it.
        "tag write: refused",
    ] {
        assert!(
            has(&out, l),
            "expected {l:?}:\n{}{}",
            out.stdout,
            out.stderr
        );
    }
    let _ = Command::new("/bin/launchctl")
        .args(["remove", "io.columbus.test-escape"])
        .output();
    assert!(
        !marker.exists(),
        "launchd ran a job for the confined program outside its sandbox:\n{}{}",
        out.stdout,
        out.stderr
    );
}

/// Level C finds the launch's tag files and tries to write into one.
#[cfg(target_os = "macos")]
const TAG_WRITE: &str = r#"
import ctypes, glob, os
_buf = ctypes.create_string_buffer(1024)
_lib.confstr(65537, _buf, 1024)
_tags = glob.glob(os.path.join(os.path.realpath(_buf.value.decode()), "columbus-confine", "*.tag"))
try:
    with open(_tags[0], "a") as _f:
        _f.write("x")
    print("tag write: ok")
except Exception as e:
    print("tag write: refused", type(e).__name__, len(_tags))
"#;

#[cfg(target_os = "macos")]
#[test]
fn on_macos_a_fork_bomb_leaves_nothing_behind() {
    let Some(py) = python() else { return };
    if !can_confine() {
        return;
    }
    let d = dirs();
    let marker = format!("columbus-fork-bomb-{}", std::process::id());
    let s = format!(
        // Bounded (at most 64 processes), so a failure cannot take the
        // machine down: six rounds in which every process forks.
        "# {marker}\nimport os\nfor _ in range(6):\n    try:\n        os.fork()\n    except OSError:\n        pass\nimport time\ntime.sleep(30)\n"
    );
    let start = std::time::Instant::now();
    let out = run("A", &d.ws, &["--processes", "20"], &py, &s);
    assert_eq!(out.code, 137, "{}{}", out.stdout, out.stderr);
    assert!(
        start.elapsed().as_secs() < 25,
        "stopped well before its sleep ended"
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    let left = Command::new("/usr/bin/pgrep")
        .args(["-f", &marker])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&left.stdout).trim().is_empty(),
        "processes left: {}",
        String::from_utf8_lossy(&left.stdout)
    );
}
