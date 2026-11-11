//! `columbus-mcp toolbox …`: the person installs, adds, lists and removes
//! toolboxes at the keyboard (build brief 3, D-17, REQ-38). No MCP tool
//! does any of this: installing a toolbox adds to what the assistant can
//! run without asking, so it is the person's decision (D-16, B4).
//!
//! ```text
//! columbus-mcp toolbox install ID [--yes] [--json]   one from Columbus's catalogue (downloaded, checked, listed)
//! columbus-mcp toolbox add PROGRAM --id ID --kind geolibre|described [--tools tools.json]
//!                      [--name NAME] [--version V] [--licence L] [--reads FOLDER]... [--yes] [--json]
//!                                                    a program already on this computer
//! columbus-mcp toolbox list [--json]                 installed toolboxes, and what the catalogue offers
//! columbus-mcp toolbox tools [WORDS...]              find tools across installed toolboxes
//! columbus-mcp toolbox show TOOLBOX.TOOL             one tool's description
//! columbus-mcp toolbox remove ID                     remove one
//! ```
//!
//! **A catalogue install** downloads the publisher's archive for this
//! system over HTTPS (size-capped, timed), checks its SHA-256 against the
//! hash pinned in Columbus *before* unpacking, takes out only the files it
//! expects (no path from the archive is used; an archive holding a link,
//! an absolute path or `..` is refused), runs the program in the sandbox
//! (level A) to ask for its tools, and only then puts the toolbox in
//! place. Anything that fails leaves nothing behind.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use columbus_shared::audit::AuditRecord;
use columbus_shared::canon::{sha256_file, sha256_hex};
use columbus_shared::toolboxes::{self as tbx, Download, Kind, Listing, Packing, Toolbox};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::server::State;

/// Most a catalogue program may unpack to, and most the licence files may.
const MAX_PROGRAM: u64 = 1024 * 1024 * 1024;
const MAX_NOTICE: u64 = 32 * 1024 * 1024;
/// Most an archive may unpack to in all before it is given up on.
const MAX_UNPACKED: u64 = 4 * 1024 * 1024 * 1024;

const USAGE: &str = "usage: columbus-mcp toolbox install ID [--yes] | add PROGRAM --id ID --kind geolibre|described [--tools tools.json] [--name NAME] [--version V] [--licence L] [--reads FOLDER] [--yes] | list [--json] | tools [WORDS...] | show TOOLBOX.TOOL | remove ID";

pub async fn run(args: &[&str]) -> Result<()> {
    match args {
        ["install", id, flags @ ..] => install(id, flags).await,
        ["add", program, flags @ ..] => add(program, flags).await,
        ["list", flags @ ..] => list(flags),
        ["tools", words @ ..] => tools(words),
        ["show", name] => show(name),
        ["remove", id] => remove(id),
        _ => bail!(USAGE),
    }
}

/// `--yes` and `--json` among the flags; anything else is refused.
fn common_flags(flags: &[&str]) -> Result<(bool, bool)> {
    let (mut yes, mut as_json) = (false, false);
    for f in flags {
        match *f {
            "--yes" => yes = true,
            "--json" => as_json = true,
            other => bail!("unknown argument {other:?}. {USAGE}"),
        }
    }
    Ok((yes, as_json))
}

/// One audit line for something done at the keyboard.
fn record(state: &State, tool: &str, decision: &str, reason: String, tb: Option<&Toolbox>) {
    let _ = state.audit.append(&AuditRecord {
        user: state.user.clone(),
        client: "columbus-mcp toolbox".into(),
        session: Some(state.session.clone()),
        tool: tool.into(),
        decision: decision.into(),
        reason: Some(reason),
        provider: tb.map(|t| t.id.clone()),
        provider_version: tb.map(|t| t.version.clone()),
        tools_hash: tb.map(|t| format!("sha256:{}", t.sha256)),
        ..Default::default()
    });
}

/// What the person at the keyboard is told, or (with `--json`) a line the
/// Columbus app reads.
struct Say {
    as_json: bool,
}

impl Say {
    fn line(&self, text: impl AsRef<str>) {
        if !self.as_json {
            println!("{}", text.as_ref());
        }
    }
    fn step(&self, v: serde_json::Value) {
        if self.as_json {
            println!("{v}");
            let _ = std::io::stdout().flush();
        }
    }
}

/// The folder a toolbox is built in before it is put in place: beside the
/// others, with a name no toolbox can have.
struct Staging(PathBuf);

impl Staging {
    fn new(dir: &Path, id: &str) -> Result<Staging, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(format!(".staging-{id}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Staging(path))
    }

    /// Put the staged toolbox in place as `<dir>/<id>`, replacing one that
    /// is there.
    fn publish(self, dir: &Path, id: &str) -> Result<(), String> {
        let dest = dir.join(id);
        let old = dir.join(format!(".old-{id}-{}", uuid::Uuid::new_v4().simple()));
        let had = std::fs::symlink_metadata(&dest).is_ok();
        if had {
            std::fs::rename(&dest, &old)
                .map_err(|e| format!("could not replace {}: {e}", dest.display()))?;
        }
        // A new program file may be held for a moment by a virus scanner.
        let mut moved = std::fs::rename(&self.0, &dest);
        for _ in 0..10 {
            if moved.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
            moved = std::fs::rename(&self.0, &dest);
        }
        if let Err(e) = moved {
            if had {
                let _ = std::fs::rename(&old, &dest);
            }
            return Err(format!("could not put it in {}: {e}", dest.display()));
        }
        if had {
            let _ = std::fs::remove_dir_all(&old);
        }
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // Still here: the install failed or was refused. Nothing stays.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A path as the sandbox will see it: with links followed on Unix (the
/// sandbox matches real paths), and simply absolute on Windows.
fn real_path(p: &Path) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        p.canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))
    }
    #[cfg(not(unix))]
    {
        std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// The session's own folders for running the program once, to list it.
struct Scratch {
    state: State,
}

impl Scratch {
    fn new() -> Result<Scratch> {
        Ok(Scratch {
            state: State::load(crate::config::Paths::from_env())?,
        })
    }
    fn runs(&self) -> Result<PathBuf, String> {
        let runs = self
            .state
            .sandbox
            .resolve("runs")
            .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&runs).map_err(|e| format!("{}: {e}", runs.display()))?;
        Ok(runs)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // This command's own session folder, with nothing kept in it.
        let root = self.state.sandbox.root().to_path_buf();
        let _ = std::fs::remove_dir(root.join("runs"));
        let _ = std::fs::remove_dir(&root);
    }
}

/// Ask the program for its tools (confined) and write `toolbox.yaml` and
/// the tool list into `staging`. `at`: where the program is now; the
/// file records `tb.program`, where it will be.
async fn list_into(
    scratch: &Scratch,
    dir: &Path,
    staging: &Path,
    tb: &Toolbox,
    at: &Path,
) -> Result<Listing, String> {
    let policy = scratch.state.policy();
    let here = Toolbox {
        program: at.display().to_string(),
        ..tb.clone()
    };
    let launcher = columbus_confine::ready(columbus_confine::Level::A).map_err(|e| {
        format!("a toolbox's program runs only in the sandbox (level A: no network, writes only its own folder), and {e}; Columbus never runs one outside it, so it was not installed")
    });
    let parsed = match tb.kind {
        Kind::Described => {
            let bytes = std::fs::read(staging.join(tbx::TOOLS)).map_err(|e| e.to_string())?;
            tbx::Parsed {
                tools: tbx::parse_described(&bytes)?,
                unreadable: 0,
                notes: vec![],
            }
        }
        Kind::Geolibre => crate::toolbox::list_tools(
            Some(&launcher?),
            dir,
            &here,
            &scratch.runs()?,
            policy.plugins.memory_mb,
        )
        .await
        .map_err(|e| {
            format!(
                "{} could not be listed, so it was not installed: {e}",
                tb.name
            )
        })?,
    };
    let listing = Listing::new(tb, parsed, &columbus_shared::tools::now());
    tbx::write_cache(staging, &listing)?;
    columbus_shared::tools::write_atomic(&staging.join(tbx::MANIFEST), tb.to_yaml()?.as_bytes())?;
    Ok(listing)
}

fn done_words(tb: &Toolbox, listing: &Listing) -> String {
    let cannot = listing
        .tools
        .iter()
        .filter(|t| t.cannot_run.is_some())
        .count();
    let ready = listing.tools.len() - cannot;
    let mut s = format!(
        "Installed {} {}: {} tool{} ready.",
        tb.name,
        tb.version,
        tbx::count_words(ready),
        if ready == 1 { "" } else { "s" }
    );
    if cannot > 0 {
        s.push_str(&format!(
            " {} more {} listed that Columbus cannot run yet.",
            tbx::count_words(cannot),
            if cannot == 1 { "is" } else { "are" }
        ));
    }
    if listing.unreadable > 0 {
        s.push_str(&format!(
            " {} of its list could not be read.",
            if listing.unreadable == 1 {
                "One entry".to_string()
            } else {
                format!("{} entries", listing.unreadable)
            }
        ));
    }
    s.push_str(" Your assistant can use them from its next request, as ");
    s.push_str(&format!("{}.<tool>.", tb.id));
    s
}

fn done_json(tb: &Toolbox, listing: &Listing) -> serde_json::Value {
    json!({"step": "done", "id": tb.id, "name": tb.name, "version": tb.version,
        "tools": listing.tools.len(),
        "cannot_run": listing.tools.iter().filter(|t| t.cannot_run.is_some()).count(),
        "unreadable": listing.unreadable, "message": done_words(tb, listing)})
}

// ---------- install from the catalogue ----------

async fn install(id: &str, flags: &[&str]) -> Result<()> {
    let (yes, as_json) = common_flags(flags)?;
    let say = Say { as_json };
    let scratch = Scratch::new()?;
    let state = &scratch.state;
    let out = install_inner(&scratch, id, yes, &say).await;
    match out {
        Ok((tb, listing)) => {
            record(
                state,
                "toolbox_install",
                "allowed",
                format!(
                    "installed from {} (archive checked against the pinned hash); {} tools",
                    tb.source,
                    listing.tools.len()
                ),
                Some(&tb),
            );
            say.line(done_words(&tb, &listing));
            say.step(done_json(&tb, &listing));
            Ok(())
        }
        Err(e) => {
            record(
                state,
                "toolbox_install",
                "denied",
                format!("{id}: {e}"),
                None,
            );
            Err(anyhow!(e))
        }
    }
}

async fn install_inner(
    scratch: &Scratch,
    id: &str,
    yes: bool,
    say: &Say,
) -> Result<(Toolbox, Listing), String> {
    let policy = scratch.state.policy();
    if !policy.allow_toolboxes {
        return Err("organization policy turns toolboxes off (allow_toolboxes), so none can be installed here".into());
    }
    if !policy.toolboxes.catalogue {
        return Err("organization policy does not let Columbus download toolboxes (toolboxes.catalogue is off); one can still be added from a file already on this computer with `columbus-mcp toolbox add`".into());
    }
    let entry = tbx::catalogue_entry(id).ok_or_else(|| {
        let known: Vec<String> = tbx::catalogue().into_iter().map(|e| e.id).collect();
        format!(
            "Columbus's catalogue has no toolbox {id:?} (it has: {})",
            known.join(", ")
        )
    })?;
    let Some(dl) = entry.for_this_system() else {
        return Err(format!(
            "{} has no build for this system ({}), so it cannot be installed here; its publisher builds it for: {}",
            entry.name,
            tbx::this_system_words(),
            entry.downloads.iter().map(|d| d.target.as_str()).collect::<Vec<_>>().join(", ")
        ));
    };
    check_download_url(&dl.url)?;
    let dir = tbx::toolboxes_dir();
    // One a person added under this id is theirs: never replaced unasked.
    if let Ok(old) = tbx::load(&dir, id) {
        if old.kind != entry.kind || !tbx::program_inside(&dir, &old) {
            return Err(format!(
                "a toolbox {id:?} is already there ({}, added from {}); remove it first with `columbus-mcp toolbox remove {id}`",
                old.name, old.source
            ));
        }
    }
    say.line(format!("{} {}", entry.name, entry.version));
    say.line(format!("  {}", entry.about));
    say.line(format!(
        "  Publisher: {} ({})",
        entry.publisher, entry.homepage
    ));
    say.line(format!("  Licence:   {}", entry.licence));
    say.line(format!("  Download:  {}", dl.url));
    say.line(format!("  Size:      {}", tbx::size_words(dl.bytes)));
    say.line(format!(
        "  SHA-256:   {} (pinned in Columbus; the download must match it)",
        dl.sha256
    ));
    say.line("  Its tools will run in the sandbox: no network; they read only the copies they are given and write only their own run folder. Your assistant may then run them without asking.");
    if !yes {
        let typed = crate::review_cli::confirm_typed(&format!(
            "\nInstall {}? Type the toolbox id ({id}) to confirm: ",
            entry.name
        ))
        .map_err(|e| e.to_string())?;
        if typed.trim() != id {
            return Err("not confirmed; nothing was installed".into());
        }
    }
    let staging = Staging::new(&dir, id)?;
    let archive = staging.0.join(format!("download.{}", dl.packing.ext()));
    say.line(format!("Downloading {}...", tbx::size_words(dl.bytes)));
    download(dl, &archive, say).await?;
    say.step(json!({"step": "check"}));
    say.line("The download matches the pinned hash. Unpacking...");
    // The program keeps its own file name; where it goes is Columbus's choice.
    let exe = dl.program.rsplit('/').next().unwrap_or("").to_string();
    if !crate::export_tools::valid_name(&exe) {
        return Err(format!(
            "the catalogue names a program Columbus will not write ({})",
            dl.program
        ));
    }
    let bin = staging.0.join(tbx::BIN);
    std::fs::create_dir(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let mut wanted = vec![Wanted {
        entry: dl.program.clone(),
        to: bin.join(&exe),
        cap: MAX_PROGRAM,
        required: true,
    }];
    for k in &dl.keep {
        let name = k.rsplit('/').next().unwrap_or(k);
        if !crate::export_tools::valid_name(name) {
            return Err(format!(
                "the catalogue names a file Columbus will not write ({k})"
            ));
        }
        wanted.push(Wanted {
            entry: k.clone(),
            to: bin.join(name),
            cap: MAX_NOTICE,
            required: false,
        });
    }
    say.step(json!({"step": "unpack"}));
    unpack(&archive, dl.packing, &wanted)?;
    let _ = std::fs::remove_file(&archive);
    let program = bin.join(&exe);
    make_executable(&program)?;
    let (sha256, _) = sha256_file(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    // Where it will be once in place.
    let final_dir = real_path(&dir)?.join(id);
    let tb = Toolbox {
        id: id.to_string(),
        name: entry.name.clone(),
        kind: entry.kind,
        program: final_dir.join(tbx::BIN).join(&exe).display().to_string(),
        sha256,
        version: entry.version.clone(),
        source: dl.url.clone(),
        licence: entry.licence.clone(),
        installed: columbus_shared::tools::now(),
        tools_sha256: None,
        reads: vec![],
    };
    tb.check()?;
    say.step(json!({"step": "list"}));
    say.line("Asking it for its tools (in the sandbox)...");
    let listing = list_into(scratch, &dir, &staging.0, &tb, &program).await?;
    staging.publish(&dir, id)?;
    Ok((tb, listing))
}

#[cfg(unix)]
fn make_executable(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(not(unix))]
fn make_executable(_p: &Path) -> Result<(), String> {
    Ok(())
}

/// A catalogue download comes over HTTPS. Only a test build using a test
/// catalogue may fetch from this computer over plain HTTP.
fn check_download_url(url: &str) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
    let local = tbx::test_catalogue() && u.scheme() == "http" && u.host_str() == Some("127.0.0.1");
    if u.scheme() != "https" && !local {
        return Err(format!("{url}: toolboxes are downloaded over https only"));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("the download address carries credentials".into());
    }
    Ok(u)
}

/// Download `dl` to `to` (a new file): at most its stated size, within the
/// time limits, and only if its SHA-256 is the pinned one. The publisher's
/// address redirects to another host; each hop must be https too, and the
/// hash is what counts.
async fn download(dl: &Download, to: &Path, say: &Say) -> Result<(), String> {
    let url = check_download_url(&dl.url)?;
    let test = tbx::test_catalogue();
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let ok = attempt.url().scheme() == "https"
                || (test
                    && attempt.url().scheme() == "http"
                    && attempt.url().host_str() == Some("127.0.0.1"));
            if attempt.previous().len() > 5 {
                attempt.error("too many redirects")
            } else if ok {
                attempt.follow()
            } else {
                attempt.error("a redirect left https")
            }
        }))
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30 * 60))
        .user_agent(concat!("columbus-mcp/", env!("CARGO_PKG_VERSION")));
    if test {
        // The test's own server is on this computer: never through a proxy.
        builder = builder.no_proxy();
    }
    let client = builder.build().map_err(|e| e.to_string())?;
    let host = url.host_str().unwrap_or_default().to_string();
    let mut resp = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            format!("{host} did not answer in time. Check your network, then try again")
        } else {
            format!("downloading from {host}: {e}")
        }
    })?;
    if !resp.status().is_success() {
        return Err(format!(
            "downloading {}: the server answered {}",
            dl.url,
            resp.status()
        ));
    }
    if let Some(n) = resp.content_length().filter(|n| *n != dl.bytes) {
        return Err(format!(
            "the download is {n} bytes and Columbus expects {}; nothing was installed",
            dl.bytes
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    let mut hash = Sha256::new();
    let mut got: u64 = 0;
    let mut last = std::time::Instant::now();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(120), resp.chunk())
            .await
            .map_err(|_| format!("{host} stopped sending; nothing was installed"))?
            .map_err(|e| format!("downloading from {host}: {e}"))?;
        let Some(chunk) = chunk else { break };
        got += chunk.len() as u64;
        if got > dl.bytes {
            return Err(format!(
                "the download is larger than the {} bytes Columbus expects; nothing was installed",
                dl.bytes
            ));
        }
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        if last.elapsed() >= Duration::from_millis(250) {
            last = std::time::Instant::now();
            say.step(json!({"step": "download", "bytes": got, "total": dl.bytes}));
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    say.step(json!({"step": "download", "bytes": got, "total": dl.bytes}));
    let found = hex(&hash.finalize());
    if got != dl.bytes || found != dl.sha256 {
        return Err(format!(
            "the download does not match the hash pinned in Columbus (expected {} bytes with SHA-256 {}, got {got} bytes with {found}), so it was not unpacked and nothing was installed",
            dl.bytes, dl.sha256
        ));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------- unpacking ----------

/// One file to take out of an archive.
pub(crate) struct Wanted {
    /// Its path inside the archive, exactly.
    pub(crate) entry: String,
    /// Where it goes: a new file whose name Columbus chose.
    pub(crate) to: PathBuf,
    pub(crate) cap: u64,
    pub(crate) required: bool,
}

/// Whether a path from an archive is one Columbus accepts seeing there: a
/// plain relative path. It is never used to write anything; an archive
/// that holds an absolute path, a `..` or a drive is refused whole.
fn entry_name_ok(name: &str) -> bool {
    let n = name.trim_end_matches('/');
    !n.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.contains(':')
        && n.split('/')
            .all(|part| !part.is_empty() && part != ".." && part != ".")
}

fn bad_entry(name: &str, why: &str) -> String {
    let shown: String = name.chars().take(120).collect();
    format!("the archive holds {why} ({shown:?}), which Columbus does not accept; nothing was installed")
}

/// Copy at most `cap` bytes of `from` into the new file `to`.
fn write_new_capped(from: &mut impl Read, to: &Path, size: u64, cap: u64) -> Result<(), String> {
    if size > cap {
        return Err(format!(
            "{} would unpack to more than {} MB",
            to.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            cap / 1024 / 1024
        ));
    }
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    let copied = std::io::copy(&mut from.take(size), &mut out).map_err(|e| e.to_string())?;
    if copied != size {
        return Err("the archive ends before its last file does".into());
    }
    out.sync_all().map_err(|e| e.to_string())
}

/// Take `wanted` out of the archive, and nothing else. No path from the
/// archive is trusted: each entry's name is only compared with the names
/// expected, and an archive that holds a link, a device, an absolute path
/// or `..` is refused whole.
pub(crate) fn unpack(archive: &Path, packing: Packing, wanted: &[Wanted]) -> Result<(), String> {
    let mut found = vec![false; wanted.len()];
    match packing {
        Packing::TarGz => unpack_tar_gz(archive, wanted, &mut found)?,
        Packing::Zip => unpack_zip(archive, wanted, &mut found)?,
    }
    for (w, f) in wanted.iter().zip(&found) {
        if w.required && !f {
            return Err(format!(
                "the archive does not hold {}; nothing was installed",
                w.entry
            ));
        }
    }
    Ok(())
}

/// A NUL-ended text field of a tar header.
fn tar_text(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// A tar header's number: octal text, or (GNU) binary with the top bit set.
fn tar_number(field: &[u8]) -> Result<u64, String> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Ok(field[1..].iter().fold(0u64, |n, b| (n << 8) | *b as u64));
    }
    let text = tar_text(field);
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8)
        .map_err(|_| "the archive is damaged (a size is not a number)".to_string())
}

/// The `path=` of a pax extended header (`<len> path=<value>\n` records).
fn pax_path(data: &[u8]) -> Option<String> {
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest.iter().position(|b| *b == b' ')?;
        let len: usize = std::str::from_utf8(&rest[..space]).ok()?.parse().ok()?;
        if len <= space || len > rest.len() {
            return None;
        }
        let record = &rest[space + 1..len];
        if let Some(v) = record.strip_prefix(b"path=") {
            return Some(
                String::from_utf8_lossy(v)
                    .trim_end_matches('\n')
                    .to_string(),
            );
        }
        rest = &rest[len..];
    }
    None
}

fn unpack_tar_gz(archive: &Path, wanted: &[Wanted], found: &mut [bool]) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut tar = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let damaged =
        |e: std::io::Error| format!("the archive is damaged ({e}); nothing was installed");
    let mut unpacked: u64 = 0;
    // A long name given by the entry before (GNU 'L', or pax 'x').
    let mut long_name: Option<String> = None;
    loop {
        let mut h = [0u8; 512];
        match tar.read_exact(&mut h) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(damaged(e)),
        }
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let size = tar_number(&h[124..136])?;
        let padded = size.div_ceil(512) * 512;
        unpacked = unpacked.saturating_add(512 + padded);
        if unpacked > MAX_UNPACKED {
            return Err("the archive unpacks to more than 4 GB; nothing was installed".into());
        }
        let flag = h[156];
        let skip = |tar: &mut flate2::read::GzDecoder<_>, n: u64| -> Result<(), String> {
            let copied = std::io::copy(&mut tar.take(n), &mut std::io::sink()).map_err(damaged)?;
            if copied != n {
                return Err("the archive ends before its last file does".into());
            }
            Ok(())
        };
        // Names that go with the next entry.
        if flag == b'L' || flag == b'x' {
            if size > 64 * 1024 {
                return Err(bad_entry("", "a name longer than Columbus reads"));
            }
            let mut data = vec![0u8; padded as usize];
            tar.read_exact(&mut data).map_err(damaged)?;
            data.truncate(size as usize);
            long_name = if flag == b'L' {
                Some(tar_text(&data))
            } else {
                pax_path(&data).or(long_name)
            };
            continue;
        }
        if flag == b'g' {
            skip(&mut tar, padded)?;
            continue;
        }
        let name = long_name.take().unwrap_or_else(|| {
            let name = tar_text(&h[0..100]);
            let prefix = if &h[257..262] == b"ustar" && h[263] != b' ' {
                tar_text(&h[345..500])
            } else {
                String::new()
            };
            if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            }
        });
        if !entry_name_ok(&name) {
            return Err(bad_entry(
                &name,
                "a path that is absolute or climbs out of its folder",
            ));
        }
        match flag {
            // A regular file.
            b'0' | 0 | b'7' => match wanted.iter().position(|w| w.entry == name) {
                Some(i) if found[i] => {
                    return Err(bad_entry(&name, "the same file twice"));
                }
                Some(i) => {
                    write_new_capped(&mut tar, &wanted[i].to, size, wanted[i].cap)?;
                    found[i] = true;
                    skip(&mut tar, padded - size)?;
                }
                None => skip(&mut tar, padded)?,
            },
            // A folder.
            b'5' => skip(&mut tar, padded)?,
            b'1' | b'2' => return Err(bad_entry(&name, "a link")),
            _ => return Err(bad_entry(&name, "something that is not a file or a folder")),
        }
    }
    Ok(())
}

fn le16(b: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn le32(b: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as u64)
}

fn unpack_zip(archive: &Path, wanted: &[Wanted], found: &mut [bool]) -> Result<(), String> {
    let damaged = || {
        "the archive is damaged (it does not read as a zip file); nothing was installed".to_string()
    };
    let m = std::fs::metadata(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    if m.len() > 1024 * 1024 * 1024 {
        return Err("the archive is larger than Columbus reads (1 GB)".into());
    }
    let data = std::fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    // The end record, searched for from the end (a comment may follow it).
    let eocd = (0..=data.len().saturating_sub(22))
        .rev()
        .take(66_000)
        .find(|i| data[*i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or_else(damaged)?;
    let count = le16(&data, eocd + 10).ok_or_else(damaged)?;
    let mut at = le32(&data, eocd + 16).ok_or_else(damaged)? as usize;
    if count == 0xFFFF || at == 0xFFFF_FFFF {
        return Err("the archive is a kind of zip file Columbus does not read (zip64)".into());
    }
    let mut unpacked: u64 = 0;
    for _ in 0..count {
        if !data
            .get(at..)
            .is_some_and(|d| d.starts_with(&[0x50, 0x4b, 0x01, 0x02]))
        {
            return Err(damaged());
        }
        let made_by = le16(&data, at + 4).ok_or_else(damaged)?;
        let flags = le16(&data, at + 8).ok_or_else(damaged)?;
        let method = le16(&data, at + 10).ok_or_else(damaged)?;
        let crc = le32(&data, at + 16).ok_or_else(damaged)? as u32;
        let packed = le32(&data, at + 20).ok_or_else(damaged)?;
        let size = le32(&data, at + 24).ok_or_else(damaged)?;
        let name_len = le16(&data, at + 28).ok_or_else(damaged)?;
        let extra_len = le16(&data, at + 30).ok_or_else(damaged)?;
        let comment_len = le16(&data, at + 32).ok_or_else(damaged)?;
        let external = le32(&data, at + 38).ok_or_else(damaged)?;
        let local = le32(&data, at + 42).ok_or_else(damaged)? as usize;
        let name_bytes = data.get(at + 46..at + 46 + name_len).ok_or_else(damaged)?;
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if !entry_name_ok(&name) {
            return Err(bad_entry(
                &name,
                "a path that is absolute or climbs out of its folder",
            ));
        }
        // Made on Unix: the file's kind is in the high bits.
        let unix_kind = if made_by >> 8 == 3 {
            (external >> 16) & 0o170000
        } else {
            0
        };
        if unix_kind == 0o120000 {
            return Err(bad_entry(&name, "a link"));
        }
        if unix_kind != 0 && unix_kind != 0o100000 && unix_kind != 0o040000 {
            return Err(bad_entry(&name, "something that is not a file or a folder"));
        }
        unpacked = unpacked.saturating_add(size);
        if unpacked > MAX_UNPACKED {
            return Err("the archive unpacks to more than 4 GB; nothing was installed".into());
        }
        if name.ends_with('/') {
            continue;
        }
        let Some(i) = wanted.iter().position(|w| w.entry == name) else {
            continue;
        };
        if found[i] {
            return Err(bad_entry(&name, "the same file twice"));
        }
        if flags & 1 != 0 {
            return Err(bad_entry(&name, "an encrypted file"));
        }
        if packed == 0xFFFF_FFFF || size == 0xFFFF_FFFF {
            return Err("the archive is a kind of zip file Columbus does not read (zip64)".into());
        }
        if !data
            .get(local..)
            .is_some_and(|d| d.starts_with(&[0x50, 0x4b, 0x03, 0x04]))
        {
            return Err(damaged());
        }
        let start = local
            + 30
            + le16(&data, local + 26).ok_or_else(damaged)?
            + le16(&data, local + 28).ok_or_else(damaged)?;
        let body = data
            .get(start..start + packed as usize)
            .ok_or_else(damaged)?;
        let w = &wanted[i];
        match method {
            0 if packed == size => write_new_capped(&mut &body[..], &w.to, size, w.cap)?,
            8 => write_new_capped(
                &mut flate2::read::DeflateDecoder::new(body),
                &w.to,
                size,
                w.cap,
            )?,
            _ => {
                return Err(bad_entry(
                    &name,
                    "a file packed in a way Columbus does not read",
                ))
            }
        }
        // What was written is what the archive says it holds.
        let mut check = flate2::Crc::new();
        let mut f = std::fs::File::open(&w.to).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            check.update(&buf[..n]);
        }
        if check.sum() != crc {
            let _ = std::fs::remove_file(&w.to);
            return Err(
                "the archive is damaged (a file's checksum does not match); nothing was installed"
                    .into(),
            );
        }
        found[i] = true;
    }
    Ok(())
}

// ---------- add a program already on this computer ----------

struct AddArgs {
    id: String,
    kind: Kind,
    tools: Option<PathBuf>,
    name: Option<String>,
    version: Option<String>,
    licence: Option<String>,
    reads: Vec<PathBuf>,
    yes: bool,
    as_json: bool,
}

fn add_args(flags: &[&str]) -> Result<AddArgs> {
    let mut a = AddArgs {
        id: String::new(),
        kind: Kind::Described,
        tools: None,
        name: None,
        version: None,
        licence: None,
        reads: vec![],
        yes: false,
        as_json: false,
    };
    let mut kind = None;
    let mut i = 0;
    while i < flags.len() {
        match flags[i] {
            "--yes" => {
                a.yes = true;
                i += 1;
                continue;
            }
            "--json" => {
                a.as_json = true;
                i += 1;
                continue;
            }
            _ => {}
        }
        let value = flags
            .get(i + 1)
            .ok_or_else(|| anyhow!("{} needs a value", flags[i]))?;
        match flags[i] {
            "--id" => a.id = value.to_string(),
            "--kind" => kind = Some(Kind::parse(value).map_err(|e| anyhow!(e))?),
            "--tools" => a.tools = Some(PathBuf::from(value)),
            "--name" => a.name = Some(value.to_string()),
            "--version" => a.version = Some(value.to_string()),
            "--licence" | "--license" => a.licence = Some(value.to_string()),
            "--reads" => a.reads.push(PathBuf::from(value)),
            other => bail!("unknown argument {other:?}. {USAGE}"),
        }
        i += 2;
    }
    if a.id.is_empty() {
        bail!("--id <a short name for the toolbox> is required (lowercase letters, digits and -)");
    }
    a.kind = kind.ok_or_else(|| anyhow!("--kind geolibre|described is required"))?;
    Ok(a)
}

async fn add(program: &str, flags: &[&str]) -> Result<()> {
    let a = add_args(flags)?;
    let say = Say { as_json: a.as_json };
    let scratch = Scratch::new()?;
    let state = &scratch.state;
    let id = a.id.clone();
    match add_inner(&scratch, program, a, &say).await {
        Ok((tb, listing)) => {
            record(
                state,
                "toolbox_add",
                "allowed",
                format!(
                    "added the program {}; {} tools",
                    tb.program,
                    listing.tools.len()
                ),
                Some(&tb),
            );
            say.line(done_words(&tb, &listing));
            say.step(done_json(&tb, &listing));
            Ok(())
        }
        Err(e) => {
            record(state, "toolbox_add", "denied", format!("{id}: {e}"), None);
            Err(anyhow!(e))
        }
    }
}

async fn add_inner(
    scratch: &Scratch,
    program: &str,
    a: AddArgs,
    say: &Say,
) -> Result<(Toolbox, Listing), String> {
    let policy = scratch.state.policy();
    if !policy.allow_toolboxes {
        return Err(
            "organization policy turns toolboxes off (allow_toolboxes), so none can be added here"
                .into(),
        );
    }
    let id = a.id.as_str();
    if !tbx::id_ok(id) {
        return Err(format!(
            "{id:?} is not a toolbox id (lowercase letters, digits and -, starting with a letter, at most 40)"
        ));
    }
    if tbx::RESERVED_IDS.contains(&id) {
        return Err(format!(
            "the id {id:?} is kept for Columbus itself; choose another"
        ));
    }
    if scratch.state.broker.get(id).is_some()
        || scratch.state.plugins.all().iter().any(|p| p.id() == id)
    {
        return Err(format!(
            "the id {id:?} is used by another provider (list_providers); choose another"
        ));
    }
    let dir = tbx::toolboxes_dir();
    if std::fs::symlink_metadata(dir.join(id)).is_ok() {
        return Err(format!(
            "a toolbox {id:?} is already there; remove it first with `columbus-mcp toolbox remove {id}`"
        ));
    }
    let program = real_path(Path::new(program))?;
    let m = std::fs::metadata(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    if !m.is_file() {
        return Err(format!("{} is not a program file", program.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "{} is not marked as a program (it has no execute permission)",
                program.display()
            ));
        }
    }
    // Its folder becomes readable by its tools: it must be a folder of
    // its own, not the person's home, Desktop, Documents or Downloads, a
    // folder of keys and sign-ins there (.ssh, .aws), the top of a drive
    // or a system folder.
    if let Some(why) = tbx::program_folder_problem(&program) {
        return Err(format!("{why}. Nothing was added"));
    }
    let mut reads = Vec::new();
    for r in &a.reads {
        let r = real_path(r)?;
        if !r.is_dir() {
            return Err(format!("--reads {}: not a folder", r.display()));
        }
        reads.push(r.display().to_string());
    }
    // The description, checked before anything is asked or run.
    let description = match (a.kind, &a.tools) {
        (Kind::Described, Some(f)) => {
            let m = std::fs::metadata(f).map_err(|e| format!("{}: {e}", f.display()))?;
            if !m.is_file() || m.len() > tbx::MAX_TOOLS_JSON {
                return Err(format!(
                    "{} is not a description file (a JSON file of at most {} MB)",
                    f.display(),
                    tbx::MAX_TOOLS_JSON / 1024 / 1024
                ));
            }
            let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
            let tools = tbx::parse_described(&bytes).map_err(|e| format!("{}: {e}", f.display()))?;
            Some((bytes, tools.len()))
        }
        (Kind::Described, None) => {
            return Err("a described toolbox needs --tools <its tools.json> (see docs/reference.md, Toolboxes, and examples/toolboxes)".into())
        }
        (Kind::Geolibre, Some(_)) => {
            return Err("--tools applies only to --kind described; the GeoLibre runner lists its own tools".into())
        }
        (Kind::Geolibre, None) => None,
    };
    let (sha256, bytes) =
        sha256_file(&program).map_err(|e| format!("{}: {e}", program.display()))?;
    let mut tb = Toolbox {
        id: id.to_string(),
        name: a.name.clone().unwrap_or_else(|| id.to_string()),
        kind: a.kind,
        program: program.display().to_string(),
        sha256,
        version: a.version.clone().unwrap_or_else(|| "not stated".into()),
        source: format!("a program on this computer ({})", program.display()),
        licence: a.licence.clone().unwrap_or_else(|| "not stated".into()),
        installed: columbus_shared::tools::now(),
        tools_sha256: description.as_ref().map(|(b, _)| sha256_hex(b)),
        reads,
    };
    tb.check()?;
    say.line(format!("Toolbox {id}: {}", tb.name));
    say.line(format!(
        "  Program:  {} ({})",
        tb.program,
        tbx::size_words(bytes)
    ));
    say.line(format!(
        "  SHA-256:  {} (it runs only while its file has this hash)",
        tb.sha256
    ));
    say.line(format!("  Kind:     {}", tb.kind.as_str()));
    if let Some((_, n)) = &description {
        say.line(format!("  Tools:    {n}, from the description file"));
    }
    say.line("  Its tools will run in the sandbox: no network; they write only their own run folder, and read the copies they are given.");
    say.line(format!("  {}.", tbx::reads_words(&program, &tb.reads)));
    say.line("  Your assistant may then run them without asking. Columbus checks that the program is the one you added, not what it does: add only programs you trust.");
    if !a.yes {
        let typed = crate::review_cli::confirm_typed(&format!(
            "\nAdd {id}? Type the toolbox id to confirm: "
        ))
        .map_err(|e| e.to_string())?;
        if typed.trim() != id {
            return Err("not confirmed; nothing was added".into());
        }
    }
    let staging = Staging::new(&dir, id)?;
    if let Some((bytes, _)) = &description {
        columbus_shared::tools::write_atomic(&staging.0.join(tbx::TOOLS), bytes)?;
    }
    if tb.kind == Kind::Geolibre {
        // What version it says it is: nothing before 1.8.1 is used.
        let launcher = columbus_confine::ready(columbus_confine::Level::A).map_err(|e| {
            format!("a toolbox's program runs only in the sandbox, and {e}; nothing was added")
        })?;
        let said = crate::toolbox::geolibre_version(
            &launcher,
            &tb,
            &scratch.runs()?,
            policy.plugins.memory_mb,
        )
        .await
        .map_err(|e| {
            format!(
                "{} could not be asked its version, so it was not added: {e}",
                tb.name
            )
        })?;
        if let Some(v) = &said {
            if tbx::version_triple(v).is_some_and(|t| t < tbx::GEOLIBRE_FIRST) {
                return Err(format!(
                    "this is GeoLibre {v}; Columbus uses 1.8.1 or later (earlier versions contained code under the AGPL licence). Nothing was added"
                ));
            }
            if a.version.is_none() {
                tb.version = v.clone();
            }
        }
        if a.licence.is_none() {
            tb.licence = "MIT (as its publisher states; not checked)".into();
        }
        tb.check()?;
    }
    say.step(json!({"step": "list"}));
    let listing = list_into(scratch, &dir, &staging.0, &tb, &program).await?;
    staging.publish(&dir, id)?;
    Ok((tb, listing))
}

// ---------- list, find, show, remove ----------

fn list(flags: &[&str]) -> Result<()> {
    let (_, as_json) = common_flags(flags)?;
    let paths = crate::config::Paths::from_env();
    let (policy, _) = crate::config::load_policy(&paths)?;
    let dir = tbx::toolboxes_dir();
    let views = tbx::list(&dir);
    if as_json {
        println!(
            "{}",
            json!({"dir": dir.display().to_string(), "allowed": policy.allow_toolboxes,
                "catalogue_allowed": policy.toolboxes.catalogue,
                "installed": views, "catalogue": tbx::catalogue_json()})
        );
        return Ok(());
    }
    println!("Toolboxes in {}", dir.display());
    if !policy.allow_toolboxes {
        println!("note: organization policy turns toolboxes off (allow_toolboxes); none of these are used.");
    }
    if views.is_empty() {
        println!("None installed.");
    }
    for v in &views {
        println!(
            "{:<16} {:<10} {:<10} {:<8} {}",
            v.id,
            v.kind.as_deref().unwrap_or("?"),
            v.version.as_deref().unwrap_or("?"),
            v.state,
            match (&v.reason, v.tools) {
                (Some(r), _) => r.clone(),
                (None, Some(n)) => format!("{} tools; {}", tbx::count_words(n), v.name),
                (None, None) => format!("tools not listed yet; {}", v.name),
            }
        );
    }
    for e in tbx::catalogue() {
        if views.iter().any(|v| v.id == e.id) {
            continue;
        }
        match e.for_this_system() {
            Some(d) => println!(
                "catalogue: {} ({} {}, {}, {}): install it with `columbus-mcp toolbox install {}`",
                e.id,
                e.name,
                e.version,
                e.licence,
                tbx::size_words(d.bytes),
                e.id
            ),
            None => println!(
                "catalogue: {} ({} {}) has no build for this system ({})",
                e.id,
                e.name,
                e.version,
                tbx::this_system_words()
            ),
        }
    }
    Ok(())
}

fn tools(words: &[&str]) -> Result<()> {
    let dir = tbx::toolboxes_dir();
    let words = tbx::query_words(&words.join(" "));
    let mut hits = Vec::new();
    for id in tbx::ids(&dir) {
        let Ok(tb) = tbx::load(&dir, &id) else {
            continue;
        };
        if tbx::check(&dir, &tb).is_err() {
            continue;
        }
        match tbx::read_cache(&dir, &tb) {
            Some(l) => hits.extend(tbx::search(&tb, &l, &words, usize::MAX).0),
            None => println!(
                "{id}: its tools are not listed yet (they are, the first time a session uses it)"
            ),
        }
    }
    tbx::sort_hits(&mut hits);
    let total = hits.len();
    for h in hits.iter().take(50) {
        println!(
            "{:<44} {}{}",
            h.name,
            h.summary,
            if h.cannot_run.is_some() {
                " (Columbus cannot run it yet)"
            } else {
                ""
            }
        );
    }
    if total > 50 {
        println!("... and {} more; add words to narrow it down.", total - 50);
    }
    if total == 0 {
        println!("No tools matched.");
    }
    Ok(())
}

fn show(name: &str) -> Result<()> {
    let (id, tool) = name
        .rsplit_once('.')
        .ok_or_else(|| anyhow!("name a tool as <toolbox>.<tool>"))?;
    let dir = tbx::toolboxes_dir();
    let tb = tbx::load(&dir, id).map_err(|p| anyhow!(p.reason().to_string()))?;
    tbx::check(&dir, &tb)
        .map_err(|p| anyhow!("{}; {}", p.reason(), tbx::again_words(Some(&tb), id)))?;
    let listing =
        tbx::read_cache(&dir, &tb).ok_or_else(|| anyhow!("{id}'s tools are not listed yet"))?;
    let t = listing
        .tool(tool)
        .ok_or_else(|| anyhow!("{id} has no tool named {tool:?}"))?;
    println!("{}", tbx::describe_text(&tb, t));
    Ok(())
}

fn remove(id: &str) -> Result<()> {
    let dir = tbx::toolboxes_dir();
    let tb = tbx::load(&dir, id).ok();
    let gone = tbx::remove(&dir, id).map_err(|e| anyhow!(e))?;
    // On Windows, running its tools wrote a read permission for this
    // toolbox's sandbox onto the program's folder and the folders it was
    // given to read: taken back now, the mirror of that grant. (On macOS
    // and Linux nothing was written onto a folder.)
    let taken_back = tb.as_ref().map(|tb| {
        columbus_confine::forget(
            &crate::toolbox::sandbox_name(&tb.id),
            &crate::toolbox::read_folders(tb),
        )
    });
    // Recorded where the person's other actions are, when that can be.
    if let Ok(state) = State::load(crate::config::Paths::from_env()) {
        record(
            &state,
            "toolbox_remove",
            "allowed",
            format!("removed the toolbox {id}"),
            tb.as_ref(),
        );
        let _ = std::fs::remove_dir(state.sandbox.root());
    }
    println!(
        "Removed {id}{}. Running sessions stop offering its tools from their next call.",
        if gone {
            " and its program".to_string()
        } else {
            match &tb {
                Some(t) => format!(" (its program stays where it is: {})", t.program),
                None => String::new(),
            }
        }
    );
    match (&tb, taken_back) {
        (Some(t), Some(Ok(()))) if cfg!(windows) && !gone => println!(
            "The permission its tools had to read the program's folder ({}) was taken back.",
            crate::toolbox::read_folders(t)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (Some(t), Some(Err(e))) => println!(
            "note: Windows did not take back the permission its tools had to read {}: {e}. It shows in the folder's Properties, under Security, as \"Columbus {}\" (or as an unknown account), and can be removed there.",
            crate::toolbox::read_folders(t)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            crate::toolbox::sandbox_name(&t.id)
        ),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/archives.rs"]
mod archives;

#[cfg(test)]
mod tests {
    use super::archives::{tar_gz, zip, Entry};
    use super::*;

    fn want(dir: &Path) -> Vec<Wanted> {
        vec![
            Wanted {
                entry: "tool-1.0/tool".into(),
                to: dir.join("tool"),
                cap: 1024 * 1024,
                required: true,
            },
            Wanted {
                entry: "tool-1.0/LICENSE".into(),
                to: dir.join("LICENSE"),
                cap: 1024,
                required: false,
            },
        ]
    }

    fn try_unpack(bytes: &[u8], packing: Packing) -> (tempfile::TempDir, Result<(), String>) {
        let d = tempfile::tempdir().unwrap();
        let archive = d.path().join("a");
        std::fs::write(&archive, bytes).unwrap();
        let out = d.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let r = unpack(&archive, packing, &want(&out));
        (d, r)
    }

    #[test]
    fn only_the_expected_files_come_out() {
        let program = vec![7u8; 70_000];
        let entries = [
            Entry::file("tool-1.0/", b""),
            Entry::program("tool-1.0/tool", &program),
            Entry::file("tool-1.0/README.md", b"read me"),
            Entry::file("tool-1.0/LICENSE", b"MIT"),
        ];
        for (bytes, packing) in [
            (tar_gz(&entries), Packing::TarGz),
            (zip(&entries), Packing::Zip),
        ] {
            let (d, r) = try_unpack(&bytes, packing);
            r.unwrap();
            let out = d.path().join("out");
            assert_eq!(std::fs::read(out.join("tool")).unwrap(), program);
            assert_eq!(std::fs::read(out.join("LICENSE")).unwrap(), b"MIT");
            // Nothing the archive names is written anywhere else.
            let names: Vec<String> = std::fs::read_dir(&out)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(names.len(), 2, "{names:?}");
        }
    }

    #[test]
    fn archives_that_reach_out_are_refused_whole() {
        let program = b"program".to_vec();
        let cases: Vec<(Vec<Entry>, &str)> = vec![
            (
                vec![
                    Entry::program("tool-1.0/tool", &program),
                    Entry::file("../evil", b"x"),
                ],
                "climbs out",
            ),
            (
                vec![
                    Entry::program("tool-1.0/tool", &program),
                    Entry::file("tool-1.0/../../evil", b"x"),
                ],
                "climbs out",
            ),
            (
                vec![
                    Entry::program("tool-1.0/tool", &program),
                    Entry::file("/etc/evil", b"x"),
                ],
                "absolute",
            ),
            (
                vec![
                    Entry::program("tool-1.0/tool", &program),
                    Entry::file("C:\\evil", b"x"),
                ],
                "absolute",
            ),
            (
                vec![
                    Entry::program("tool-1.0/tool", &program),
                    Entry::link("tool-1.0/LICENSE", "/etc/passwd"),
                ],
                "a link",
            ),
            (vec![Entry::link("tool-1.0/tool", "/bin/sh")], "a link"),
            (
                vec![
                    Entry::program("tool-1.0/tool", &program),
                    Entry::program("tool-1.0/tool", b"again"),
                ],
                "twice",
            ),
            (
                vec![Entry::file("tool-1.0/LICENSE", b"MIT")],
                "does not hold tool-1.0/tool",
            ),
        ];
        for (entries, wanted) in &cases {
            for (bytes, packing) in [
                (tar_gz(entries), Packing::TarGz),
                (zip(entries), Packing::Zip),
            ] {
                let (_d, r) = try_unpack(&bytes, packing);
                let e = r.expect_err(wanted);
                assert!(e.contains(wanted), "{packing:?}: {e}");
            }
        }
        // Too large for its place, and not an archive at all.
        let big = vec![0u8; 2 * 1024 * 1024];
        let entries = [Entry::program("tool-1.0/tool", &big)];
        for (bytes, packing) in [
            (tar_gz(&entries), Packing::TarGz),
            (zip(&entries), Packing::Zip),
        ] {
            let e = try_unpack(&bytes, packing).1.unwrap_err();
            assert!(e.contains("more than 1 MB"), "{e}");
        }
        for packing in [Packing::TarGz, Packing::Zip] {
            let e = try_unpack(b"this is not an archive", packing)
                .1
                .unwrap_err();
            assert!(e.contains("damaged") || e.contains("does not hold"), "{e}");
        }
    }

    #[test]
    fn names_from_an_archive_are_only_ever_compared() {
        for ok in ["a", "a/b", "tool-1.0/tool", "tool-1.0/", "a b/c.txt"] {
            assert!(entry_name_ok(ok), "{ok}");
        }
        for bad in [
            "", "/a", "../a", "a/../b", "a/./b", "a//b", "C:\\a", "c:/a", "a\\b", "a\0b", "..",
        ] {
            assert!(!entry_name_ok(bad), "{bad:?}");
        }
        // Long names (GNU and pax) are read, and held to the same rule.
        assert_eq!(
            pax_path(b"18 path=dir/a.txt\n").as_deref(),
            Some("dir/a.txt")
        );
        assert_eq!(
            pax_path(b"12 uid=1000\n18 path=dir/a.txt\n").as_deref(),
            Some("dir/a.txt")
        );
        assert_eq!(pax_path(b"garbage"), None);
        assert_eq!(tar_number(b"0000144\0").unwrap(), 100);
        assert_eq!(tar_number(&[0x80, 0, 0, 0, 0, 0, 1, 0]).unwrap(), 256);
    }

    /// With the publisher's real archive on disk (never fetched or run by a
    /// test): its hash is the pinned one, and it unpacks to the program and
    /// the two notices. `COLUMBUS_TEST_GEOLIBRE_ARCHIVE=<the archive for
    /// this system> cargo test -p columbus-mcp --bin columbus-mcp -- --ignored real_archive`
    #[test]
    #[ignore = "needs the real GeoLibre archive on disk (COLUMBUS_TEST_GEOLIBRE_ARCHIVE)"]
    fn the_real_archive_matches_its_pin_and_unpacks() {
        let Some(archive) = std::env::var_os("COLUMBUS_TEST_GEOLIBRE_ARCHIVE") else {
            panic!("set COLUMBUS_TEST_GEOLIBRE_ARCHIVE to the archive for this system");
        };
        let archive = PathBuf::from(archive);
        let entry = tbx::builtin_catalogue().remove(0);
        let dl = entry.for_this_system().expect("a build for this system");
        let (sha, bytes) = sha256_file(&archive).unwrap();
        assert_eq!((sha.as_str(), bytes), (dl.sha256.as_str(), dl.bytes));
        let d = tempfile::tempdir().unwrap();
        let mut wanted = vec![Wanted {
            entry: dl.program.clone(),
            to: d.path().join("program"),
            cap: MAX_PROGRAM,
            required: true,
        }];
        for (i, k) in dl.keep.iter().enumerate() {
            wanted.push(Wanted {
                entry: k.clone(),
                to: d.path().join(format!("kept-{i}")),
                cap: MAX_NOTICE,
                required: true,
            });
        }
        unpack(&archive, dl.packing, &wanted).unwrap();
        let size = |n: &str| std::fs::metadata(d.path().join(n)).unwrap().len();
        assert!(size("program") > 10 * 1024 * 1024, "{}", size("program"));
        assert!(size("kept-0") > 500 && size("kept-1") > 500);
        println!(
            "program {} bytes, sha256 {}",
            size("program"),
            sha256_file(&d.path().join("program")).unwrap().0
        );
    }

    #[test]
    fn downloads_come_over_https_only() {
        assert!(check_download_url(
            "https://github.com/opengeos/geolibre-rust/releases/download/v1.8.1/x.tar.gz"
        )
        .is_ok());
        for bad in [
            "http://github.com/x.tar.gz",
            "ftp://example.org/x",
            "file:///etc/passwd",
            "https://user:pw@example.org/x",
            "not a url",
        ] {
            assert!(check_download_url(bad).is_err(), "{bad}");
        }
        // Plain HTTP to this computer is for a test catalogue only, which a
        // unit test does not have.
        assert!(!tbx::test_catalogue());
        assert!(check_download_url("http://127.0.0.1:8000/x.tar.gz").is_err());
        // Every address the built-in catalogue holds passes.
        for e in tbx::builtin_catalogue() {
            for d in &e.downloads {
                check_download_url(&d.url).unwrap();
            }
        }
    }
}
