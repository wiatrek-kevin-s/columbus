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
        let s = "b = bytearray(1024 * 1024 * 1024)\nfor i in range(0, len(b), 4096): b[i] = 1\nprint('big: ok')\n";
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
