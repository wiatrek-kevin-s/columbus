//! Environments Columbus creates (build brief Part 5 "Creating one",
//! REQ-14): the assistant proposes one with `propose_environment` (a
//! Python version and exact-pinned packages, with a reason); the proposal
//! waits here until the person approves it at the keyboard; only then does
//! Columbus run its own pinned `uv` (installed in the engine from
//! `workers/python/requirements-uv.txt`) to create it under
//! `<Columbus data>/envs/<id>/` and register it governed, `created_by:
//! columbus`.
//!
//! ```text
//! environments/.proposals/<id>.json   one waiting proposal (internal)
//! ```
//!
//! How creation is held to the rules:
//! - wheels only (`--no-build`): no package's build code ever runs;
//! - a Python already on the machine (`UV_PYTHON_DOWNLOADS=never`,
//!   `--no-python-downloads`): the engine's own, or another uv finds;
//! - uv's configuration is ignored (`--no-config`, `UV_NO_CONFIG=1`) and it
//!   gets a cleared environment (no `UV_*`, `PIP_*` or proxy settings of the
//!   person's or anyone else's), so nothing can redirect the index;
//! - the index is `https://<first index host>/simple/`, and every
//!   connection uv makes goes through a filtering proxy Columbus runs for
//!   the creation only, which lets through exactly `environments.index_hosts`
//!   (redirects and file links to any other host are refused there, and
//!   the creation fails naming that host);
//! - it installs only into the folder it has just made, never into
//!   `arcgispro-py3`, a Pro clone or any registered environment.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::canon::{canonical_json, sha256_hex};
use crate::environments::{self as envs, Confinement, CreatedBy, Environment, Registration, Trust};
use crate::policy::EnvironmentsPolicy;

/// The waiting proposals, inside the environments folder (not an id).
pub const PROPOSALS: &str = ".proposals";
/// Most proposals waiting at once.
pub const MAX_PROPOSALS: usize = 20;
/// Most packages in one proposal.
pub const MAX_PACKAGES: usize = 50;
/// How long making the folder may take.
const VENV_SECONDS: u64 = 120;
/// How long the install may take.
const INSTALL_SECONDS: u64 = 900;

/// One proposal, as `propose_environment` files it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub id: String,
    /// `3.N` or `3.N.M`; none: the engine's own version.
    #[serde(default)]
    pub python_version: Option<String>,
    /// Exact pins, `name==version`.
    pub packages: Vec<String>,
    pub reason: String,
    #[serde(default)]
    pub proposed: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub client: String,
}

impl Proposal {
    /// What the person approves: its id, Python, packages and reason.
    pub fn fingerprint(&self) -> String {
        let v = json!({
            "id": self.id,
            "python_version": self.python_version,
            "packages": self.packages,
            "reason": self.reason,
        });
        format!("sha256:{}", sha256_hex(canonical_json(&v).as_bytes()))
    }
}

// ---------- what a proposal may ask for ----------

/// A package name (PEP 508): letters, digits, `.`, `_`, `-`, starting and
/// ending with a letter or digit.
pub fn name_ok(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 100
        && b.first().is_some_and(u8::is_ascii_alphanumeric)
        && b.last().is_some_and(u8::is_ascii_alphanumeric)
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// The name as indexes compare it (PEP 503).
pub fn normalize(n: &str) -> String {
    let mut out = String::new();
    let mut sep = false;
    for c in n.chars() {
        if matches!(c, '-' | '_' | '.') {
            sep = true;
        } else {
            if sep && !out.is_empty() {
                out.push('-');
            }
            sep = false;
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// An exact public version (PEP 440): `1.17.0`, `2.0rc1`, `1.0.post2`,
/// `3.1.dev4`. No wildcards, ranges or local parts.
pub fn version_ok(v: &str) -> bool {
    fn digits(s: &str) -> Option<&str> {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        (n > 0 && n <= 12).then(|| &s[n..])
    }
    if v.is_empty() || v.len() > 64 {
        return false;
    }
    let mut rest = match digits(v) {
        Some(r) => r,
        None => return false,
    };
    while let Some(r) = rest.strip_prefix('.') {
        match digits(r) {
            Some(r2) => rest = r2,
            None => break,
        }
    }
    for pre in ["rc", "a", "b"] {
        if let Some(r) = rest.strip_prefix(pre) {
            match digits(r) {
                Some(r2) => rest = r2,
                None => return false,
            }
            break;
        }
    }
    if let Some(r) = rest.strip_prefix(".post") {
        match digits(r) {
            Some(r2) => rest = r2,
            None => return false,
        }
    }
    if let Some(r) = rest.strip_prefix(".dev") {
        match digits(r) {
            Some(r2) => rest = r2,
            None => return false,
        }
    }
    rest.is_empty()
}

/// The host a URL-like requirement points at, if it has one.
fn url_host(spec: &str) -> Option<String> {
    let after = spec.split("://").nth(1)?;
    let hostport = after.split(['/', '?', '#']).next()?;
    let host = hostport.rsplit('@').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// One requirement, checked: `name==version`, or `name[extra,…]==version`.
/// Returns the normalized name.
pub fn check_package(spec: &str, index_hosts: &[String]) -> Result<String, String> {
    let s = spec.trim();
    let hosts = index_hosts.join(", ");
    if s.contains("://") || s.contains(" @ ") || s.contains('@') {
        let host = url_host(s).unwrap_or_else(|| "another place".into());
        return Err(format!(
            "{s:?} points to {host}: packages come only from the organization's package index (environments.index_hosts: {hosts}), named as name==version"
        ));
    }
    let bad = || {
        format!(
            "{s:?} is not an exact pin: give every package as name==version (for example six==1.17.0); version ranges, wildcards, URLs, file paths, VCS links and markers are refused"
        )
    };
    let (left, version) = s.split_once("==").ok_or_else(bad)?;
    if version.starts_with('=') || !version_ok(version) {
        return Err(bad());
    }
    let (name, extras) = match left.split_once('[') {
        Some((n, e)) => (n, Some(e.strip_suffix(']').ok_or_else(bad)?)),
        None => (left, None),
    };
    if !name_ok(name) {
        return Err(format!(
            "{name:?} is not a package name (letters, digits, '.', '_' and '-', starting and ending with a letter or digit)"
        ));
    }
    if let Some(e) = extras {
        if e.split(',').any(|x| !name_ok(x.trim())) {
            return Err(bad());
        }
    }
    Ok(normalize(name))
}

/// `3.N` or `3.N.M`.
pub fn python_version_ok(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (parts.len() == 2 || parts.len() == 3)
        && parts[0] == "3"
        && parts[1..]
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Every rule a proposal meets, when it is filed and again when approved.
pub fn check_proposal(p: &Proposal, policy: &EnvironmentsPolicy) -> Result<(), String> {
    if !policy.allow {
        return Err("organization policy does not allow environments (environments.allow is off), so none can be created".into());
    }
    if !envs::id_ok(&p.id) {
        return Err(format!(
            "{:?} is not an environment id (lowercase letters, digits and -, at most 40)",
            p.id
        ));
    }
    if let Some(v) = &p.python_version {
        if !python_version_ok(v) {
            return Err(format!(
                "python_version {v:?} must be 3.N or 3.N.M, for example 3.12"
            ));
        }
    }
    if p.packages.is_empty() || p.packages.len() > MAX_PACKAGES {
        return Err(format!(
            "give between 1 and {MAX_PACKAGES} packages, each as name==version"
        ));
    }
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for spec in &p.packages {
        match check_package(spec, &policy.index_hosts) {
            Ok(n) => {
                if !seen.insert(n.clone()) {
                    problems.push(format!("{n} is listed twice"));
                }
            }
            Err(e) => problems.push(e),
        }
    }
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    let r = p.reason.trim();
    if r.chars().count() < 10 || r.chars().count() > 2000 {
        return Err(
            "give a reason of 10 to 2000 characters: what the environment is for, in the person's words"
                .into(),
        );
    }
    Ok(())
}

// ---------- filing, reading, declining ----------

fn proposal_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(PROPOSALS).join(format!("{id}.json"))
}

/// File a proposal (the server). It replaces a waiting proposal of the same
/// id. Refused when that id is registered or its folder exists, or too many
/// others are waiting. Returns its fingerprint.
pub fn write_proposal(
    dir: &Path,
    envs_root: &Path,
    policy: &EnvironmentsPolicy,
    p: &Proposal,
) -> Result<String, String> {
    check_proposal(p, policy)?;
    if std::fs::symlink_metadata(envs::file_for(dir, &p.id)).is_ok() {
        return Err(format!(
            "an environment {:?} is already registered; choose another id",
            p.id
        ));
    }
    if std::fs::symlink_metadata(envs_root.join(&p.id)).is_ok() {
        return Err(format!(
            "{} already exists; choose another id",
            envs_root.join(&p.id).display()
        ));
    }
    let others = list_proposals(dir)
        .into_iter()
        .filter(|(q, _)| q.id != p.id)
        .count();
    if others >= MAX_PROPOSALS {
        return Err(format!(
            "{others} environments are already waiting for the person; ask them to approve or decline those first"
        ));
    }
    let folder = dir.join(PROPOSALS);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let bytes = serde_json::to_vec_pretty(p).map_err(|e| e.to_string())?;
    crate::tools::write_atomic(&proposal_file(dir, &p.id), &bytes)?;
    Ok(p.fingerprint())
}

/// One waiting proposal and its fingerprint.
pub fn read_proposal(dir: &Path, id: &str) -> Result<(Proposal, String), String> {
    if !envs::id_ok(id) {
        return Err(format!("{id:?} is not an environment id"));
    }
    let path = proposal_file(dir, id);
    let md = std::fs::symlink_metadata(&path)
        .map_err(|_| format!("there is no environment {id:?} waiting"))?;
    if !md.file_type().is_file() || md.len() > 256 * 1024 {
        return Err(format!("{} is not a plain file", path.display()));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let p: Proposal =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if p.id != id {
        return Err(format!("{} names another id", path.display()));
    }
    let fp = p.fingerprint();
    Ok((p, fp))
}

/// Every waiting proposal, by id.
pub fn list_proposals(dir: &Path) -> Vec<(Proposal, String)> {
    let Ok(entries) = std::fs::read_dir(dir.join(PROPOSALS)) else {
        return vec![];
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_str()?.to_string();
            let id = n.strip_suffix(".json")?.to_string();
            envs::id_ok(&id).then_some(id)
        })
        .collect();
    ids.sort();
    ids.into_iter()
        .filter_map(|id| read_proposal(dir, &id).ok())
        .collect()
}

/// Decline a waiting proposal: it is deleted. Nothing else changes.
pub fn decline(dir: &Path, id: &str) -> Result<(), String> {
    read_proposal(dir, id)?;
    let path = proposal_file(dir, id);
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
}

// ---------- Columbus's own uv ----------

/// What the engine says about itself and its uv.
#[derive(Debug, Clone, Deserialize)]
pub struct Engine {
    /// The engine's interpreter, as `sys.executable`.
    pub executable: String,
    pub version: String,
    /// The uv installed in the engine (`uv.find_uv_bin()`), if any.
    pub uv: Option<String>,
}

const ENGINE_PY: &str = r#"import sys, json
r = {"executable": sys.executable, "version": "%d.%d.%d" % tuple(sys.version_info[:3]), "uv": None}
try:
    import uv
    r["uv"] = uv.find_uv_bin()
except Exception:
    pass
sys.stdout.write("\nCOLUMBUS-ENGINE " + json.dumps(r) + "\n")
"#;

/// How to install Columbus's uv into the engine.
pub const INSTALL_UV: &str = "install it into the engine with: <engine python> -m pip install --require-hashes --no-deps --only-binary :all: -r workers/python/requirements-uv.txt (the engine setup does this)";

/// Ask the engine's Python (isolated, `-I`) for its version and the uv
/// installed in it. Never a uv on PATH.
pub fn engine_info(engine_python: &Path) -> Result<Engine, String> {
    let mut cmd = std::process::Command::new(engine_python);
    cmd.args(["-I", "-B", "-c", ENGINE_PY])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let shown = engine_python.display();
    let out = match envs::run_limited(cmd, Duration::from_secs(60)) {
        envs::Ran::Done { stdout, .. } => stdout,
        envs::Ran::TimedOut => return Err(format!("the engine {shown} did not answer in 60 s")),
        envs::Ran::Failed(e) => return Err(format!("the engine {shown} could not start: {e}")),
    };
    let line = out
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("COLUMBUS-ENGINE "))
        .ok_or_else(|| format!("the engine {shown} did not answer"))?;
    serde_json::from_str(line).map_err(|e| format!("the engine's answer: {e}"))
}

/// The uv Columbus runs: the one in the engine, checked to be a uv program.
pub fn engine_uv(engine: &Engine) -> Result<PathBuf, String> {
    let uv = engine.uv.as_deref().map(PathBuf::from).ok_or_else(|| {
        format!(
            "Columbus's uv is not installed in the engine ({}); {INSTALL_UV}",
            engine.executable
        )
    })?;
    let name = uv
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !(name == "uv" || name == "uv.exe") || !uv.is_absolute() || !uv.is_file() {
        return Err(format!(
            "the engine's uv ({}) is not a uv program; {INSTALL_UV}",
            uv.display()
        ));
    }
    Ok(uv)
}

// ---------- the filtering proxy ----------

/// One allowed host, from `environments.index_hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRule {
    pub host: String,
    pub port: Option<u16>,
}

impl HostRule {
    /// `host` or `host:port`: letters, digits, `.` and `-`.
    pub fn parse(s: &str) -> Result<HostRule, String> {
        let s = s.trim().to_ascii_lowercase();
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                Some(
                    p.parse::<u16>()
                        .map_err(|_| format!("index host {s:?}: bad port"))?,
                ),
            ),
            None => (s.clone(), None),
        };
        let ok = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            && !host.starts_with(['.', '-']);
        if !ok {
            return Err(format!(
                "index host {s:?} is not a host name (environments.index_hosts lists host names, such as pypi.org)"
            ));
        }
        Ok(HostRule { host, port })
    }

    fn loopback(&self) -> bool {
        is_loopback(&self.host)
    }

    /// The index URL when this is the first host: `https://host/simple/`,
    /// or `http://` for a mirror on this computer (127.0.0.1, localhost).
    pub fn index_url(&self) -> String {
        let scheme = if self.loopback() { "http" } else { "https" };
        match self.port {
            Some(p) => format!("{scheme}://{}:{p}/simple/", self.host),
            None => format!("{scheme}://{}/simple/", self.host),
        }
    }
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|a| a.is_loopback())
}

/// Whether a connection to `host:port` may go ahead (`tls`: a CONNECT;
/// otherwise plain HTTP, which only a mirror on this computer may use).
fn allowed(rules: &[HostRule], host: &str, port: u16, tls: bool) -> bool {
    if !tls && !is_loopback(host) {
        return false;
    }
    let default = if tls { 443 } else { 80 };
    rules
        .iter()
        .any(|r| r.host == host && r.port.unwrap_or(default) == port)
}

fn shown_host(host: &str, port: u16, tls: bool) -> String {
    if port == if tls { 443 } else { 80 } {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

/// A proxy on 127.0.0.1 for one creation: it lets uv reach only the index
/// hosts, and remembers every other host uv tried.
pub struct Gate {
    addr: SocketAddr,
    refused: Arc<Mutex<BTreeSet<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Gate {
    pub fn start(rules: Vec<HostRule>) -> Result<Gate, String> {
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|e| format!("starting the proxy: {e}"))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("starting the proxy: {e}"))?;
        let refused = Arc::new(Mutex::new(BTreeSet::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let rules = Arc::new(rules);
        let upstream = Arc::new(Upstream::from_env());
        let (r2, s2) = (refused.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((c, _)) => {
                        let (rules, refused, upstream) =
                            (rules.clone(), r2.clone(), upstream.clone());
                        std::thread::spawn(move || {
                            let _ = c.set_nonblocking(false);
                            serve(c, &rules, &refused, &upstream);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        Ok(Gate {
            addr,
            refused,
            stop,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The hosts refused so far.
    pub fn refused(&self) -> Vec<String> {
        self.refused
            .lock()
            .map(|r| r.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The proxy Columbus itself goes through, if the person's computer has
/// one (`HTTPS_PROXY`, an `http://` address), and the hosts `NO_PROXY`
/// exempts.
struct Upstream {
    proxy: Option<(String, Option<String>)>,
    no_proxy: Vec<String>,
}

impl Upstream {
    fn from_env() -> Upstream {
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .or_else(|| std::env::var(k.to_ascii_lowercase()).ok())
                .filter(|v| !v.trim().is_empty())
        };
        let proxy = get("HTTPS_PROXY")
            .or_else(|| get("ALL_PROXY"))
            .and_then(|u| {
                let rest = u.trim().strip_prefix("http://")?;
                let hostport = rest.split('/').next()?;
                let (auth, hp) = match hostport.rsplit_once('@') {
                    Some((a, h)) => (Some(base64(a.as_bytes())), h),
                    None => (None, hostport),
                };
                let hp = if hp.contains(':') {
                    hp.to_string()
                } else {
                    format!("{hp}:80")
                };
                Some((hp, auth))
            });
        let no_proxy = get("NO_PROXY")
            .map(|v| {
                v.split(',')
                    .map(|s| {
                        s.trim()
                            .trim_start_matches("*.")
                            .trim_start_matches('.')
                            .to_ascii_lowercase()
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Upstream { proxy, no_proxy }
    }

    fn for_host(&self, host: &str) -> Option<&(String, Option<String>)> {
        if is_loopback(host)
            || self
                .no_proxy
                .iter()
                .any(|n| n == "*" || n == host || host.ends_with(&format!(".{n}")))
        {
            return None;
        }
        self.proxy.as_ref()
    }
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Read a request or response head, up to the blank line (at most 16 KB).
fn read_head(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if buf.len() > 16 * 1024 || s.read(&mut byte).ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
    }
    String::from_utf8(buf).ok()
}

fn connect(hostport: &str) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::other("no address");
    for a in hostport.to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, Duration::from_secs(20)) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn splice(a: TcpStream, b: TcpStream) {
    let _ = a.set_read_timeout(Some(Duration::from_secs(300)));
    let _ = b.set_read_timeout(Some(Duration::from_secs(300)));
    let (Ok(mut a2), Ok(mut b2)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let t = std::thread::spawn(move || {
        let mut a = a;
        let mut b = b;
        let _ = std::io::copy(&mut a, &mut b);
        let _ = b.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut b2, &mut a2);
    let _ = a2.shutdown(Shutdown::Write);
    let _ = t.join();
}

fn refuse(c: &mut TcpStream, refused: &Mutex<BTreeSet<String>>, shown: String) {
    let body = format!("Columbus: {shown} is not one of environments.index_hosts\n");
    if let Ok(mut r) = refused.lock() {
        r.insert(shown);
    }
    let _ = write!(
        c,
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// Split `host:port` (or `[v6]:port`), with a default port.
fn host_port(s: &str, default: u16) -> Option<(String, u16)> {
    let s = s.to_ascii_lowercase();
    if let Some(rest) = s.strip_prefix('[') {
        let (h, p) = rest.split_once(']')?;
        let port = match p.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default,
        };
        return Some((format!("[{h}]"), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) => Some((h.to_string(), p.parse().ok()?)),
        None => Some((s, default)),
    }
}

fn serve(
    mut c: TcpStream,
    rules: &[HostRule],
    refused: &Mutex<BTreeSet<String>>,
    upstream: &Upstream,
) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(30)));
    let Some(head) = read_head(&mut c) else {
        return;
    };
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or("HTTP/1.1"),
    );
    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) = host_port(target, 443) else {
            return;
        };
        if !allowed(rules, &host, port, true) {
            return refuse(&mut c, refused, shown_host(&host, port, true));
        }
        let hostport = format!("{host}:{port}");
        let up = match upstream.for_host(&host) {
            None => connect(&hostport),
            Some((proxy, auth)) => connect(proxy).and_then(|mut p| {
                let auth = auth
                    .as_ref()
                    .map(|a| format!("Proxy-Authorization: Basic {a}\r\n"))
                    .unwrap_or_default();
                write!(
                    p,
                    "CONNECT {hostport} HTTP/1.1\r\nHost: {hostport}\r\n{auth}\r\n"
                )?;
                let ok = read_head(&mut p)
                    .is_some_and(|h| h.split(' ').nth(1).is_some_and(|code| code == "200"));
                if ok {
                    Ok(p)
                } else {
                    Err(std::io::Error::other("the upstream proxy refused"))
                }
            }),
        };
        match up {
            Ok(up) => {
                if c.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .is_ok()
                {
                    let _ = c.set_read_timeout(None);
                    splice(c, up);
                }
            }
            Err(e) => {
                let _ = write!(
                    c,
                    "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = e;
            }
        }
        return;
    }
    // Plain HTTP, absolute form: only to a mirror on this computer.
    let Some(rest) = target.strip_prefix("http://") else {
        let _ = write!(
            c,
            "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        return;
    };
    let (hp, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let Some((host, port)) = host_port(hp, 80) else {
        return;
    };
    if !allowed(rules, &host, port, false) {
        return refuse(&mut c, refused, shown_host(&host, port, false));
    }
    let Ok(mut up) = connect(&format!("{host}:{port}")) else {
        let _ = write!(
            c,
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        return;
    };
    let mut out = format!("{method} {path} {version}\r\n");
    for l in lines.filter(|l| !l.is_empty()) {
        let k = l
            .split(':')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if k.starts_with("proxy-") || k == "connection" || k == "keep-alive" {
            continue;
        }
        out.push_str(l);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    if up.write_all(out.as_bytes()).is_ok() {
        let _ = c.set_read_timeout(None);
        splice(c, up);
    }
}

// ---------- creating ----------

/// The environment uv gets: nothing of the person's settings for uv, pip
/// or proxies; only what programs need to run on each system.
fn uv_command(uv: &Path, gate: &Gate, cwd: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(uv);
    cmd.env_clear();
    for k in [
        "PATH",
        "HOME",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "PROGRAMDATA",
        "PROGRAMFILES",
        "HOMEDRIVE",
        "HOMEPATH",
        "LANG",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let proxy = gate.url();
    cmd.env("UV_NO_CONFIG", "1")
        .env("UV_PYTHON_DOWNLOADS", "never")
        .env("UV_NO_PROGRESS", "1")
        .env("UV_HTTP_RETRIES", "1")
        .env("NO_COLOR", "1")
        .env("HTTP_PROXY", &proxy)
        .env("HTTPS_PROXY", &proxy)
        .env("ALL_PROXY", &proxy)
        .env("http_proxy", &proxy)
        .env("https_proxy", &proxy)
        .env("all_proxy", &proxy)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd
}

struct UvRun {
    ok: bool,
    output: String,
}

fn run_uv(cmd: std::process::Command, limit: u64, what: &str) -> Result<UvRun, String> {
    match envs::run_limited(cmd, Duration::from_secs(limit)) {
        envs::Ran::Done {
            stdout,
            stderr,
            success,
        } => Ok(UvRun {
            ok: success,
            output: format!("{stdout}\n{stderr}"),
        }),
        envs::Ran::TimedOut => Err(format!("{what} ran longer than {limit} s and was stopped")),
        envs::Ran::Failed(e) => Err(format!("{what}: uv could not start: {e}")),
    }
}

/// The last lines uv printed, for the person.
fn tail(s: &str) -> String {
    let lines: Vec<&str> = s
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let from = lines.len().saturating_sub(8);
    let t = lines[from..].join("\n");
    t.chars().take(1500).collect()
}

/// The interpreter inside a venv folder.
pub fn venv_python(folder: &Path) -> PathBuf {
    if cfg!(windows) {
        folder.join("Scripts").join("python.exe")
    } else {
        folder.join("bin").join("python")
    }
}

/// Why creation failed, in plain words, with the hosts refused named first.
fn failure(gate: &Gate, out: &str, hosts: &[String]) -> String {
    let refused = gate.refused();
    if !refused.is_empty() {
        return format!(
            "creating the environment needed {}, which {} not in environments.index_hosts ({}); nothing was installed. Ask the organization to add the host if it is trusted.\nuv said:\n{}",
            refused.join(", "),
            if refused.len() == 1 { "is" } else { "are" },
            hosts.join(", "),
            tail(out)
        );
    }
    let lower = out.to_ascii_lowercase();
    if lower.contains("no-build")
        || lower.contains("building source distributions is disabled")
        || lower.contains("building from source is disabled")
        || lower.contains("no usable wheels")
        || lower.contains("no wheels")
    {
        return format!(
            "a package has no wheel for this system, and Columbus installs wheels only (it never builds from source, so no package's build code runs); nothing was installed. Choose a version that publishes a wheel for this system.\nuv said:\n{}",
            tail(out)
        );
    }
    format!(
        "uv could not create the environment; nothing was installed.\nuv said:\n{}",
        tail(out)
    )
}

/// Create the proposed environment and register it, after the person
/// approved it at the keyboard (the app after Touch ID or Windows Hello;
/// the terminal's `columbus-mcp env approve <id>` after they typed the
/// id): `want` is the fingerprint they were shown, `typed` what they typed
/// (the app passes the id). The proposal is gone once it is registered;
/// on any failure the new folder is deleted and the proposal stays.
#[allow(clippy::too_many_arguments)]
pub fn approve(
    dir: &Path,
    envs_root: &Path,
    policy: &EnvironmentsPolicy,
    engine_python: &Path,
    id: &str,
    want: &str,
    typed: &str,
    now: &str,
) -> Result<Environment, String> {
    let (p, fp) = read_proposal(dir, id)?;
    if fp != want {
        return Err(
            "the proposal changed after you looked at it; nothing was created. Look again".into(),
        );
    }
    if typed.trim() != id {
        return Err("not confirmed; nothing changed".into());
    }
    check_proposal(&p, policy)?;
    let rules = policy
        .index_hosts
        .iter()
        .map(|h| HostRule::parse(h))
        .collect::<Result<Vec<_>, _>>()?;
    let index = rules
        .first()
        .ok_or("environments.index_hosts is empty, so there is no package index to install from")?
        .index_url();
    if std::fs::symlink_metadata(envs::file_for(dir, id)).is_ok() {
        return Err(format!("an environment {id:?} is already registered"));
    }
    let engine = engine_info(engine_python)?;
    let uv = engine_uv(&engine)?;
    // The engine's own Python when its version is the one asked for (or
    // none was); otherwise one uv finds on this computer. Never downloaded.
    let base = match &p.python_version {
        None => engine.executable.clone(),
        Some(v) if engine.version == *v || engine.version.starts_with(&format!("{v}.")) => {
            engine.executable.clone()
        }
        Some(v) => v.clone(),
    };
    std::fs::create_dir_all(envs_root).map_err(|e| format!("{}: {e}", envs_root.display()))?;
    let folder = envs_root.join(id);
    // Only a folder made now, by this creation.
    std::fs::create_dir(&folder).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{} already exists; nothing was created", folder.display())
        } else {
            format!("{}: {e}", folder.display())
        }
    })?;
    let made = (|| {
        let gate = Gate::start(rules.clone())?;
        let mut cmd = uv_command(&uv, &gate, envs_root);
        cmd.args([
            "venv",
            "--no-config",
            "--no-project",
            "--no-python-downloads",
        ])
        .arg("--python")
        .arg(&base)
        .arg("--allow-existing")
        .arg(&folder);
        let r = run_uv(cmd, VENV_SECONDS, "making the environment's folder")?;
        if !r.ok {
            return Err(format!(
                "uv could not make the environment with Python {base} (Columbus never downloads a Python; it uses one already on this computer).\nuv said:\n{}",
                tail(&r.output)
            ));
        }
        let python = venv_python(&folder);
        // Installs go only into the folder made here, really under envs.
        if envs::columbus_folder(envs_root, id, &python).is_none()
            || !folder.join("pyvenv.cfg").is_file()
        {
            return Err(format!(
                "{} is not a new environment under {}; nothing was installed",
                folder.display(),
                envs_root.display()
            ));
        }
        let mut cmd = uv_command(&uv, &gate, envs_root);
        cmd.args([
            "pip",
            "install",
            "--no-config",
            "--no-build",
            "--no-python-downloads",
            "--no-cache",
            "--system-certs",
            "--index-strategy",
            "first-index",
            "--default-index",
            &index,
            "--python",
        ])
        .arg(&python)
        .arg("--")
        .args(p.packages.iter().map(|s| s.trim()));
        let r = run_uv(cmd, INSTALL_SECONDS, "installing the packages")?;
        if !r.ok || !gate.refused().is_empty() {
            return Err(failure(&gate, &r.output, &policy.index_hosts));
        }
        drop(gate);
        let found = envs::probe(&python, Duration::from_secs(envs::PROBE_SECONDS))?;
        // What was asked for is what is there.
        let have: BTreeMap<String, String> = found
            .packages
            .iter()
            .filter_map(|l| l.split_once("=="))
            .map(|(n, v)| (normalize(n), v.to_string()))
            .collect();
        for spec in &p.packages {
            let (left, v) = spec.trim().split_once("==").unwrap_or((spec, ""));
            let n = normalize(left.split('[').next().unwrap_or(left));
            if have.get(&n).map(String::as_str) != Some(v) {
                return Err(format!(
                    "after installing, {n} is not at version {v}; nothing was registered"
                ));
            }
        }
        let first_line: String = p
            .reason
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        let reg = Registration {
            id: id.to_string(),
            python: python.clone(),
            trust: Trust::Governed,
            confinement: Confinement::A,
            created_by: CreatedBy::Columbus,
            description: Some(format!("Created by Columbus: {first_line}")),
        };
        envs::register(dir, envs_root, policy, &reg, &found, id, now)
    })();
    match made {
        Ok(env) => {
            let _ = std::fs::remove_file(proposal_file(dir, id));
            Ok(env)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&folder);
            Err(e)
        }
    }
}

/// A proposal as the app and the terminal show it.
pub fn describe(p: &Proposal) -> Value {
    json!({
        "id": p.id,
        "python_version": p.python_version,
        "packages": p.packages,
        "reason": p.reason,
        "proposed": p.proposed,
        "client": p.client,
        "fingerprint": p.fingerprint(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<String> {
        vec!["pypi.org".into(), "files.pythonhosted.org".into()]
    }

    #[test]
    fn only_exact_pins_of_real_names_pass() {
        for ok in [
            "six==1.17.0",
            "whitebox==2.3.6",
            "Shapely==2.1.0",
            "zope.interface==7.0",
            "a_b-c==1.0rc1",
            "x==1.0.post2",
            "x==1.0.dev3",
            "rasterio[s3]==1.4.3",
        ] {
            assert!(check_package(ok, &hosts()).is_ok(), "{ok}");
        }
        for bad in [
            "six",
            "six>=1.0",
            "six~=1.17",
            "six==1.*",
            "six===1.17.0",
            "six!=1.0",
            "six==1.17.0; python_version>'3'",
            "six==1.0+local",
            "-e .",
            "./six",
            "/tmp/six-1.0.whl",
            "--index-url=https://evil.example/simple",
            "six[../x]==1.0",
            "-six==1.0",
        ] {
            assert!(check_package(bad, &hosts()).is_err(), "{bad}");
        }
        for (url, host) in [
            (
                "six @ https://evil.example/six-1.17.0-py3-none-any.whl",
                "evil.example",
            ),
            ("git+https://github.com/x/y.git", "github.com"),
            ("https://user@files.example.org/x.whl", "files.example.org"),
        ] {
            let e = check_package(url, &hosts()).unwrap_err();
            assert!(e.contains(host), "{e}");
        }
        assert_eq!(normalize("Zope.Interface__x"), "zope-interface-x");
    }

    #[test]
    fn hosts_and_index_urls() {
        assert_eq!(
            HostRule::parse("pypi.org").unwrap().index_url(),
            "https://pypi.org/simple/"
        );
        assert_eq!(
            HostRule::parse("127.0.0.1:8123").unwrap().index_url(),
            "http://127.0.0.1:8123/simple/"
        );
        assert!(HostRule::parse("https://pypi.org").is_err());
        assert!(HostRule::parse("pypi.org/simple").is_err());
        let rules = vec![
            HostRule::parse("pypi.org").unwrap(),
            HostRule::parse("127.0.0.1:8123").unwrap(),
        ];
        assert!(allowed(&rules, "pypi.org", 443, true));
        assert!(!allowed(&rules, "pypi.org", 80, false));
        assert!(!allowed(&rules, "evil.pypi.org", 443, true));
        assert!(!allowed(&rules, "pypi.org", 8443, true));
        assert!(allowed(&rules, "127.0.0.1", 8123, false));
        assert!(!allowed(&rules, "localhost", 8123, false));
        assert_eq!(base64(b"user:pw"), "dXNlcjpwdw==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn the_proxy_refuses_other_hosts_and_names_them() {
        let gate = Gate::start(vec![HostRule::parse("127.0.0.1:9").unwrap()]).unwrap();
        let addr = gate.url().trim_start_matches("http://").to_string();
        let mut s = TcpStream::connect(&addr).unwrap();
        write!(
            s,
            "CONNECT evil.example:443 HTTP/1.1\r\nHost: evil.example:443\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");
        let mut s = TcpStream::connect(&addr).unwrap();
        write!(
            s,
            "GET http://localhost:9/simple/x/ HTTP/1.1\r\nHost: localhost:9\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");
        assert_eq!(gate.refused(), vec!["evil.example", "localhost:9"]);
    }

    #[test]
    fn a_proposal_is_checked_whole() {
        let policy = EnvironmentsPolicy {
            allow: true,
            ..Default::default()
        };
        let p = Proposal {
            id: "whitebox".into(),
            python_version: Some("3.12".into()),
            packages: vec!["whitebox==2.3.6".into()],
            reason: "Run WhiteboxTools hydrology on DEMs".into(),
            proposed: String::new(),
            session: String::new(),
            client: String::new(),
        };
        assert!(check_proposal(&p, &policy).is_ok());
        assert!(check_proposal(&p, &EnvironmentsPolicy::default()).is_err());
        let mut q = p.clone();
        q.packages.push("Whitebox==2.3.6".into());
        assert!(check_proposal(&q, &policy).unwrap_err().contains("twice"));
        let mut q = p.clone();
        q.python_version = Some("2.7".into());
        assert!(check_proposal(&q, &policy).is_err());
        let mut q = p.clone();
        q.id = "Bad_Id".into();
        assert!(check_proposal(&q, &policy).is_err());
        // The fingerprint covers what the person approves, not when.
        let mut q = p.clone();
        q.proposed = "later".into();
        assert_eq!(q.fingerprint(), p.fingerprint());
        q.packages = vec!["whitebox==2.3.5".into()];
        assert_ne!(q.fingerprint(), p.fingerprint());
    }
}
