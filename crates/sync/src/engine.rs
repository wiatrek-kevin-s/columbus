//! Check for changes, sync, and settle a file that needs a look.
//!
//! **Check** only reads: the branch's newest commit and its list of files,
//! and the folder. **Sync** gets the server's changes first, keeps both
//! versions of anything changed on both sides, then sends what only the
//! person changed as one commit, on top of exactly what it read. It never
//! forces: when the server has moved in between, it reads again and
//! repeats (R-24). Nothing is ever merged, and nothing is ever sent unless
//! the person asked for this sync.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::fsx::{self, Kind};
use crate::hash::{blob_id, content_hash, is_lfs_pointer, is_text, ObjectFormat};
use crate::host::{Change, Host, SendError, Tip};
use crate::paths::{self, MAX_FILE};
use crate::plan::{plan, Plan, ServerFile};
use crate::scan::scan;
use crate::secrets::{self, Finding};
use crate::state::{Conflict, ConflictKind, State, Synced};
use crate::trash::{self, Keep, Kept};
use crate::{Error, Skipped};

/// How many times a sync reads the server again when it has moved, before
/// it gives up (never forcing).
pub const MAX_ROUNDS: u32 = 5;

/// The most bytes one sync sends. More waits for the next sync.
pub const MAX_SEND: u64 = 64 * 1024 * 1024;

/// The longest text shown as a before or an after.
pub const MAX_PREVIEW: usize = 256 * 1024;

/// What happened to a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum What {
    New,
    Changed,
    Deleted,
}

/// A path that differs on one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub path: String,
    pub what: What,
    /// For a change here: what the file holds now ([`content_hash`]), or
    /// empty for a deleted one. A sync sends a file only as it was shown.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// For a change on the server: the id of the server's version, or empty
    /// for a deleted one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub blob: String,
}

/// A path that differs on both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BothItem {
    pub path: String,
    pub kind: ConflictKind,
    /// The id of the server's version, or empty when it is deleted there.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub blob: String,
    /// What the file here holds now, or empty when it is deleted here (or
    /// too large to read).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hash: String,
}

/// What Check for changes found. Nothing was changed anywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    /// The branch's newest commit (none: the repository is empty).
    pub commit: Option<String>,
    /// Changed here: what a sync would send.
    pub here: Vec<Item>,
    /// Changed on the server: what a sync would get.
    pub server: Vec<Item>,
    /// Changed on both sides: a sync keeps both versions of each, unless
    /// they turn out to hold the same.
    pub both: Vec<BothItem>,
    /// What does not sync, and why.
    pub skipped: Vec<Skipped>,
    /// Files that already need a look.
    pub needs_look: Vec<Conflict>,
}

/// A file a sync took out of the folder because it was deleted on the
/// server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Removed {
    pub path: String,
    pub kept: Kept,
}

/// A change a sync did not send, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotSent {
    pub path: String,
    pub why: String,
}

/// What a sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Files written from the server (new or changed there).
    pub got: Vec<String>,
    /// Files deleted on the server, taken out of the folder and kept.
    pub removed: Vec<Removed>,
    /// What was sent.
    pub sent: Vec<Item>,
    /// The commit this sync made, if it sent anything.
    pub commit: Option<String>,
    /// Files that need a look, found by this sync: both versions were kept.
    pub conflicts: Vec<Conflict>,
    /// Changes here that were not sent this time.
    pub not_sent: Vec<NotSent>,
    /// What does not sync, and why.
    pub skipped: Vec<Skipped>,
    /// Set when the sync stopped before sending: outgoing files look like
    /// they hold secrets. Nothing was sent.
    pub stopped: Option<Vec<Finding>>,
    /// How many times the server had moved and the sync read it again.
    pub rounds: u32,
}

/// How to sync.
pub struct Options<'a> {
    /// The person's note ("What did you change?"): the commit's message.
    pub note: Option<&'a str>,
    /// The changes the person was shown: each path of [`Changes::here`]
    /// and [`Changes::both`] with its `hash` (empty for a deleted file).
    /// When given, a change is sent only if the file is still exactly that;
    /// anything newer waits and is reported in [`Report::not_sent`]. `None`
    /// sends every change.
    pub seen: Option<&'a BTreeMap<String, String>>,
    /// Where files taken out of the folder are kept.
    pub keep: &'a Keep,
    /// Today (`YYYY-MM-DD`), for the names of kept copies.
    pub today: &'a str,
    /// Strings that must never be sent inside a file (the server's token).
    pub never_send: &'a [String],
}

/// The person's choice for a file that needs a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Choice {
    /// My version takes the name (and is sent by the next sync).
    Mine,
    /// The server's version stays; my copy is taken away (kept in the trash).
    Theirs,
    /// Both stay: my copy becomes a file of its own.
    Both,
}

fn server_format(tip: &Tip) -> Option<ObjectFormat> {
    tip.commit().and_then(ObjectFormat::of_id)
}

async fn read_server<H: Host>(host: &H) -> Result<(Tip, Vec<crate::host::TreeEntry>), Error> {
    let tip = host.tip().await?;
    let entries = match &tip {
        Tip::At(c) => host.tree(c).await?,
        Tip::Empty => Vec::new(),
    };
    Ok((tip, entries))
}

fn make_plan(
    root: &Path,
    state: &State,
    tip: &Tip,
    entries: &[crate::host::TreeEntry],
) -> Result<(Plan, crate::scan::Scan), Error> {
    let local = scan(root, &state.held())?;
    // A repository with no commit at all, where files were synced before:
    // it was made anew (emptying one by deleting files leaves commits).
    // What was synced then says nothing about it, so nothing here is
    // taken for "deleted on the server": every file is new to it.
    let fresh;
    let state = if *tip == Tip::Empty && !state.files.is_empty() {
        fresh = State {
            files: BTreeMap::new(),
            ..state.clone()
        };
        &fresh
    } else {
        state
    };
    let format = server_format(tip);
    let local_blob = |path: &str| -> Option<String> {
        let format = format?;
        let (bytes, _) = fsx::read(root, path, MAX_FILE).ok()??;
        Some(blob_id(&bytes, format))
    };
    let p = plan(state, &local, entries, &local_blob);
    Ok((p, local))
}

/// Check for changes: compare the last synced state, the folder and the
/// server. Reads only.
pub async fn check<H: Host>(root: &Path, host: &H, state: &State) -> Result<Changes, Error> {
    let (tip, entries) = read_server(host).await?;
    let (p, local) = make_plan(root, state, &tip, &entries)?;
    let hash_of = |path: &str| {
        local
            .files
            .get(path)
            .and_then(|l| l.hash())
            .unwrap_or_default()
            .to_string()
    };
    let mut c = Changes {
        commit: tip.commit().map(str::to_string),
        skipped: p.skipped.clone(),
        needs_look: state.conflicts.clone(),
        ..Changes::default()
    };
    let here = |path: &str, what: What| Item {
        path: path.to_string(),
        what,
        hash: if what == What::Deleted {
            String::new()
        } else {
            hash_of(path)
        },
        blob: String::new(),
    };
    for path in &p.create {
        c.here.push(here(path, What::New));
    }
    for (path, _) in &p.update {
        c.here.push(here(path, What::Changed));
    }
    for (path, _) in &p.delete {
        c.here.push(here(path, What::Deleted));
    }
    for (path, s) in &p.write {
        c.server.push(Item {
            path: path.clone(),
            what: if state.files.contains_key(path) {
                What::Changed
            } else {
                What::New
            },
            hash: String::new(),
            blob: s.blob.clone(),
        });
    }
    for path in &p.remove {
        c.server.push(Item {
            path: path.clone(),
            what: What::Deleted,
            hash: String::new(),
            blob: String::new(),
        });
    }
    for (path, s) in &p.both {
        c.both.push(BothItem {
            path: path.clone(),
            kind: ConflictKind::BothChanged,
            blob: s.blob.clone(),
            hash: hash_of(path),
        });
    }
    for path in &p.deleted_on_server {
        c.both.push(BothItem {
            path: path.clone(),
            kind: ConflictKind::DeletedOnServer,
            blob: String::new(),
            hash: hash_of(path),
        });
    }
    for (path, s) in &p.deleted_here {
        c.both.push(BothItem {
            path: path.clone(),
            kind: ConflictKind::DeletedHere,
            blob: s.blob.clone(),
            hash: String::new(),
        });
    }
    c.here.sort_by(|a, b| a.path.cmp(&b.path));
    c.server.sort_by(|a, b| a.path.cmp(&b.path));
    c.both.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(c)
}

/// A change to send, before its bytes are read.
enum Out {
    Create(String),
    Update(String, ServerFile),
    Delete(String, String),
}

/// Fetch a file of the server's, checked against the id the server listed
/// for it. `Ok(None)`: it does not sync (said in `skipped`).
async fn fetch<H: Host>(
    host: &H,
    path: &str,
    s: &ServerFile,
    format: Option<ObjectFormat>,
    state: &mut State,
    skipped: &mut Vec<Skipped>,
) -> Result<Option<Vec<u8>>, Error> {
    let bytes = host.blob(&s.blob).await?;
    if let Some(f) = format {
        if blob_id(&bytes, f) != s.blob {
            return Err(Error::Server(format!(
                "the server sent something other than it listed for {path}; nothing was written for it"
            )));
        }
    }
    let why = if is_lfs_pointer(&bytes) {
        state.lfs.insert(path.to_string(), s.blob.clone());
        "the server keeps it with Git LFS, which Columbus does not fetch"
    } else if bytes.len() as u64 > MAX_FILE {
        "it is larger than 5 MB on the server"
    } else {
        return Ok(Some(bytes));
    };
    skipped.push(Skipped {
        path: path.to_string(),
        why: why.to_string(),
        on_server: true,
    });
    Ok(None)
}

/// A free name for the person's version of `path`, beside it.
fn copy_place(root: &Path, path: &str, today: &str) -> Result<String, Error> {
    for n in 1..1000 {
        let name = paths::copy_name(path, today, n);
        if fsx::kind(root, &name)? == Kind::Nothing {
            return Ok(name);
        }
    }
    Err(Error::Refused(format!(
        "there are too many copies of {path} already"
    )))
}

fn record(state: &mut State, path: &str, blob: &str, stat: fsx::Stat, hash: String) {
    state.files.insert(
        path.to_string(),
        Synced {
            blob: blob.to_string(),
            size: stat.size,
            mtime: stat.mtime,
            hash,
        },
    );
}

/// Write the server's version of `path` into the folder and record it.
/// A folder or a link of that name is said in `skipped`, not an error.
fn put(
    root: &Path,
    path: &str,
    s: &ServerFile,
    bytes: &[u8],
    state: &mut State,
    skipped: &mut Vec<Skipped>,
) -> Result<bool, Error> {
    match fsx::write(root, path, bytes, s.exec) {
        Ok(stat) => {
            record(state, path, &s.blob, stat, content_hash(bytes));
            Ok(true)
        }
        Err(Error::Refused(why)) => {
            skipped.push(Skipped {
                path: path.to_string(),
                why: format!("it could not be written here: {why}"),
                on_server: true,
            });
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// One round of a sync: the folder and the record it works on, and what
/// it has done so far.
struct Round<'a, H: Host> {
    root: &'a Path,
    host: &'a H,
    state: &'a mut State,
    opts: &'a Options<'a>,
    report: &'a mut Report,
    /// How the server names its files, when its ids say.
    format: Option<ObjectFormat>,
    skipped: Vec<Skipped>,
}

impl<H: Host> Round<'_, H> {
    async fn fetch(&mut self, path: &str, s: &ServerFile) -> Result<Option<Vec<u8>>, Error> {
        fetch(
            self.host,
            path,
            s,
            self.format,
            self.state,
            &mut self.skipped,
        )
        .await
    }

    /// Write the server's version of `path` into the folder; whether it
    /// was written.
    fn put(&mut self, path: &str, s: &ServerFile, bytes: &[u8]) -> Result<bool, Error> {
        put(self.root, path, s, bytes, self.state, &mut self.skipped)
    }

    fn needs_look(&mut self, path: &str, copy: Option<String>, kind: ConflictKind) {
        let c = Conflict {
            path: path.to_string(),
            copy,
            kind,
            found: self.opts.today.to_string(),
        };
        self.state.conflicts.push(c.clone());
        self.report.conflicts.push(c);
    }

    /// Move the person's version of `path` to a copy beside it, freeing
    /// the name. `None` when a plain file no longer has the name.
    fn keep_mine_beside(&mut self, path: &str) -> Result<Option<String>, Error> {
        if fsx::kind(self.root, path)? != Kind::File {
            return Ok(None);
        }
        let copy = copy_place(self.root, path, self.opts.today)?;
        fsx::rename_to_new(self.root, path, &copy)?;
        Ok(Some(copy))
    }

    /// A file changed (or new) on the server only. It replaces the file
    /// here only while that is still what was last synced: anything else
    /// is the person's, and waits for the next look.
    async fn get(&mut self, path: &str, s: &ServerFile) -> Result<(), Error> {
        let Some(bytes) = self.fetch(path, s).await? else {
            return Ok(());
        };
        let base = self.state.files.get(path).cloned();
        let mine = fsx::read(self.root, path, MAX_FILE);
        let write = match (base, mine) {
            (Some(base), Ok(Some((mine, stat)))) if content_hash(&mine) == base.hash => {
                if content_hash(&bytes) == base.hash {
                    // Only the line endings differ on the server: the file
                    // here is left as it is.
                    record(self.state, path, &s.blob, stat, base.hash);
                    false
                } else {
                    true
                }
            }
            (None, Ok(None)) => fsx::kind(self.root, path)? == Kind::Nothing,
            // A link or a folder has the name, or is on the way to it.
            (_, Err(Error::Refused(why))) => {
                self.skipped.push(Skipped {
                    path: path.to_string(),
                    why: format!("it could not be written here: {why}"),
                    on_server: true,
                });
                false
            }
            (_, Err(e)) => return Err(e),
            // Changed, made or removed here since the folder was read.
            _ => false,
        };
        if write && self.put(path, s, &bytes)? {
            self.report.got.push(path.to_string());
        }
        Ok(())
    }

    /// A file deleted on the server and untouched here: taken out of the
    /// folder, and kept.
    fn remove(&mut self, path: &str) -> Result<(), Error> {
        let Some(base) = self.state.files.get(path).cloned() else {
            return Ok(());
        };
        match fsx::read(self.root, path, MAX_FILE) {
            Ok(Some((mine, _))) if content_hash(&mine) == base.hash => {
                let kept = trash::remove(self.root, path, self.opts.keep)?;
                fsx::remove_empty_parents(self.root, path);
                self.state.files.remove(path);
                self.report.removed.push(Removed {
                    path: path.to_string(),
                    kept,
                });
            }
            Ok(None) => {
                self.state.files.remove(path);
            }
            // Changed here since the folder was read.
            _ => {}
        }
        Ok(())
    }

    /// A file that differs from what was synced on both sides. The same
    /// content is no conflict; a change of line endings alone is no change;
    /// anything else keeps both versions. Gives the change to send, if the
    /// person's turns out to be the only real one.
    async fn both(&mut self, path: &str, s: &ServerFile) -> Result<Option<Out>, Error> {
        let Some(bytes) = self.fetch(path, s).await? else {
            return Ok(None);
        };
        let theirs = content_hash(&bytes);
        let mine = match fsx::read(self.root, path, MAX_FILE) {
            Ok(Some((mine, stat))) => Some((content_hash(&mine), stat)),
            // Larger than 5 MB: different, whatever it holds.
            Err(Error::Refused(_)) => None,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e),
        };
        let base = self.state.files.get(path).map(|b| b.hash.clone());
        match mine {
            Some((hash, stat)) if hash == theirs => {
                record(self.state, path, &s.blob, stat, hash);
                Ok(None)
            }
            Some(_) if base.as_deref() == Some(theirs.as_str()) => {
                // The server's change is in line endings only: the person's
                // is the only real one.
                if let Some(b) = self.state.files.get_mut(path) {
                    b.blob = s.blob.clone();
                }
                Ok(Some(Out::Update(path.to_string(), s.clone())))
            }
            _ => {
                // The server's version takes the name; the person's is kept
                // beside it.
                if let Some(copy) = self.keep_mine_beside(path)? {
                    self.put(path, s, &bytes)?;
                    self.needs_look(path, Some(copy), ConflictKind::BothChanged);
                }
                Ok(None)
            }
        }
    }

    /// Changed here, deleted on the server: the changed version is kept, as
    /// a copy, and the name is free as it is on the server.
    fn deleted_on_server(&mut self, path: &str) -> Result<(), Error> {
        if let Some(copy) = self.keep_mine_beside(path)? {
            self.state.files.remove(path);
            self.needs_look(path, Some(copy), ConflictKind::DeletedOnServer);
        }
        Ok(())
    }

    /// Deleted here, changed on the server: the changed version is kept, so
    /// the file is back.
    async fn deleted_here(&mut self, path: &str, s: &ServerFile) -> Result<(), Error> {
        if fsx::kind(self.root, path)? != Kind::Nothing {
            return Ok(());
        }
        match self.fetch(path, s).await? {
            None => {
                self.state.files.remove(path);
            }
            Some(bytes) => {
                if self.put(path, s, &bytes)? {
                    self.needs_look(path, None, ConflictKind::DeletedHere);
                }
            }
        }
        Ok(())
    }

    /// Everything that comes before sending: what is only a matter of
    /// record, the server's changes, and the conflict rule. Gives what is
    /// left to send.
    async fn settle(&mut self, p: &Plan) -> Result<Vec<Out>, Error> {
        for path in &p.forget {
            self.state.files.remove(path);
        }
        for (path, s) in &p.adopt {
            if let Some((bytes, stat)) = fsx::read(self.root, path, MAX_FILE)? {
                record(self.state, path, &s.blob, stat, content_hash(&bytes));
            }
        }
        for (path, s) in &p.write {
            self.get(path, s).await?;
        }
        for path in &p.remove {
            self.remove(path)?;
        }
        let mut out = Vec::new();
        for (path, s) in &p.both {
            out.extend(self.both(path, s).await?);
        }
        for path in &p.deleted_on_server {
            self.deleted_on_server(path)?;
        }
        for (path, s) in &p.deleted_here {
            self.deleted_here(path, s).await?;
        }
        out.extend(p.create.iter().cloned().map(Out::Create));
        out.extend(p.update.iter().cloned().map(|(p, s)| Out::Update(p, s)));
        out.extend(p.delete.iter().cloned().map(|(p, b)| Out::Delete(p, b)));
        Ok(out)
    }
}

/// Record what was sent, from what the server lists for the new commit.
async fn record_sent<H: Host>(
    host: &H,
    state: &mut State,
    report: &mut Report,
    commit: String,
    changes: &[Change],
    hashes: &BTreeMap<String, (String, fsx::Stat)>,
) {
    // If the list cannot be read now, the ids are worked out here instead;
    // the next look compares with the server anyway.
    let listed: BTreeMap<String, String> = match host.tree(&commit).await {
        Ok(t) => t.into_iter().map(|e| (e.path, e.blob)).collect(),
        Err(_) => BTreeMap::new(),
    };
    let format = ObjectFormat::of_id(&commit);
    for c in changes {
        let path = c.path();
        let Some(bytes) = c.bytes() else {
            state.files.remove(path);
            report.sent.push(item(path, What::Deleted));
            continue;
        };
        let blob = listed
            .get(path)
            .cloned()
            .or_else(|| format.map(|f| blob_id(bytes, f)));
        if let (Some(blob), Some((hash, stat))) = (blob, hashes.get(path)) {
            record(state, path, &blob, *stat, hash.clone());
        }
        let what = if matches!(c, Change::Create { .. }) {
            What::New
        } else {
            What::Changed
        };
        report.sent.push(item(path, what));
    }
    state.commit = Some(commit.clone());
    report.commit = Some(commit);
}

/// Sync: get, keep both where both changed, send, record. `state` is
/// changed as the work goes: save it afterwards, also after an error (what
/// was done until then is recorded in it).
pub async fn sync<H: Host>(
    root: &Path,
    host: &H,
    state: &mut State,
    opts: &Options<'_>,
) -> Result<Report, Error> {
    std::fs::create_dir_all(root).map_err(|e| Error::io(root, e))?;
    let mut report = Report::default();
    loop {
        let (tip, entries) = read_server(host).await?;
        if tip == Tip::Empty {
            // See `make_plan`: an empty repository has no synced files.
            state.files.clear();
        }
        let (p, _) = make_plan(root, state, &tip, &entries)?;
        let mut round = Round {
            root,
            host,
            state: &mut *state,
            opts,
            report: &mut report,
            format: server_format(&tip),
            skipped: p.skipped.clone(),
        };
        let out = round.settle(&p).await?;
        let mut skipped = round.skipped;
        skipped.sort();
        skipped.dedup();
        report.skipped = skipped;
        report.not_sent.clear();
        let (changes, hashes) = outgoing(root, out, opts, &mut report)?;
        if changes.is_empty() {
            state.commit = tip.commit().map(str::to_string);
            break;
        }

        // Before anything is sent: nothing that looks like a secret.
        let found: Vec<Finding> = changes
            .iter()
            .filter_map(|c| {
                c.bytes()
                    .map(|b| secrets::find(c.path(), b, opts.never_send))
            })
            .flatten()
            .collect();
        if !found.is_empty() {
            report.stopped = Some(found);
            return Ok(finish(report));
        }

        match host
            .send(&tip, &changes, &message(opts.note, &changes))
            .await
        {
            Ok(commit) => {
                record_sent(host, state, &mut report, commit, &changes, &hashes).await;
                break;
            }
            // Never forced: read again, and repeat.
            Err(SendError::Moved) => {
                report.rounds += 1;
                if report.rounds >= MAX_ROUNDS {
                    return Err(Error::Busy(format!(
                        "The repository kept changing while Columbus tried to send ({MAX_ROUNDS} tries). Nothing was overwritten, and what the server had was fetched. Press Sync again."
                    )));
                }
            }
            Err(SendError::Failed(e)) => return Err(e),
        }
    }
    state.synced_at = Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    Ok(finish(report))
}

fn finish(mut report: Report) -> Report {
    report.got.sort();
    report.got.dedup();
    report.sent.sort_by(|a, b| a.path.cmp(&b.path));
    report.not_sent.sort_by(|a, b| a.path.cmp(&b.path));
    report.removed.sort_by(|a, b| a.path.cmp(&b.path));
    report
}

fn item(path: &str, what: What) -> Item {
    Item {
        path: path.to_string(),
        what,
        hash: String::new(),
        blob: String::new(),
    }
}

/// Read what is to be sent, as it is now. A change the person was not
/// shown (when `seen` is given), or beyond what one sync sends, waits.
#[allow(clippy::type_complexity)]
fn outgoing(
    root: &Path,
    out: Vec<Out>,
    opts: &Options<'_>,
    report: &mut Report,
) -> Result<(Vec<Change>, BTreeMap<String, (String, fsx::Stat)>), Error> {
    const LATER: &str = "it changed after you looked; check for changes again";
    let mut changes = Vec::new();
    let mut hashes = BTreeMap::new();
    let mut total = 0u64;
    let shown = |path: &str, hash: &str| {
        opts.seen
            .is_none_or(|s| s.get(path).map(String::as_str) == Some(hash))
    };
    let mut wait = |path: &str, why: &str| {
        report.not_sent.push(NotSent {
            path: path.to_string(),
            why: why.to_string(),
        });
    };
    for o in out {
        let (path, old) = match o {
            Out::Delete(path, old) => {
                if fsx::kind(root, &path)? != Kind::Nothing {
                    continue;
                }
                if shown(&path, "") {
                    changes.push(Change::Delete { path, old });
                } else {
                    wait(&path, LATER);
                }
                continue;
            }
            Out::Create(path) => (path, None),
            Out::Update(path, s) => (path, Some(s)),
        };
        let (bytes, stat) = match fsx::read(root, &path, MAX_FILE) {
            Ok(Some(x)) => x,
            // Gone, or grown past 5 MB, since the folder was read.
            Ok(None) | Err(Error::Refused(_)) => continue,
            Err(e) => return Err(e),
        };
        let hash = content_hash(&bytes);
        if !shown(&path, &hash) {
            wait(&path, LATER);
            continue;
        }
        if total + bytes.len() as u64 > MAX_SEND && !changes.is_empty() {
            wait(&path, "this sync is full; sync again to send it");
            continue;
        }
        total += bytes.len() as u64;
        hashes.insert(path.clone(), (hash, stat));
        changes.push(match old {
            None => Change::Create {
                path,
                bytes,
                exec: stat.exec,
            },
            // Windows has no mark for programs: the server's is kept.
            Some(s) => Change::Update {
                path,
                bytes,
                old: s.blob,
                exec: if cfg!(unix) { stat.exec } else { s.exec },
            },
        });
    }
    Ok((changes, hashes))
}

/// The commit's message: the person's note, or a line saying what changed.
fn message(note: Option<&str>, changes: &[Change]) -> String {
    if let Some(n) = note.map(str::trim).filter(|n| !n.is_empty()) {
        return n.chars().take(2000).collect();
    }
    let count = |f: fn(&Change) -> bool| changes.iter().filter(|c| f(c)).count();
    let new = count(|c| matches!(c, Change::Create { .. }));
    let changed = count(|c| matches!(c, Change::Update { .. }));
    let removed = count(|c| matches!(c, Change::Delete { .. }));
    if let [one] = changes {
        let verb = match one {
            Change::Create { .. } => "Add",
            Change::Update { .. } => "Update",
            Change::Delete { .. } => "Remove",
        };
        return format!("{verb} {}", one.path());
    }
    let mut parts = Vec::new();
    if new > 0 {
        parts.push(format!("{new} new"));
    }
    if changed > 0 {
        parts.push(format!("{changed} changed"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }
    format!("Update {} files ({})", changes.len(), parts.join(", "))
}

/// Settle a file that needs a look, as the person chose. Nothing is sent;
/// what "keep mine" or "keep both" leaves in the folder goes with the next
/// sync. `copy` names which record is meant (a file can need a look twice).
pub fn resolve(
    root: &Path,
    state: &mut State,
    path: &str,
    copy: Option<&str>,
    choice: Choice,
    keep: &Keep,
) -> Result<(), Error> {
    let Some(i) = state
        .conflicts
        .iter()
        .position(|c| c.path == path && c.copy.as_deref() == copy)
    else {
        return Err(Error::Refused(format!("{path} no longer needs a look")));
    };
    let c = state.conflicts[i].clone();
    match (c.kind, choice, c.copy.as_deref()) {
        // My version takes the name. What has the name now is the server's
        // version, which the server still has; if it was changed since, it
        // is kept in the trash.
        (ConflictKind::BothChanged | ConflictKind::DeletedOnServer, Choice::Mine, Some(copy)) => {
            if fsx::kind(root, copy)? == Kind::File {
                match fsx::kind(root, path)? {
                    Kind::Nothing => {}
                    Kind::File => {
                        let as_synced =
                            match (fsx::read(root, path, MAX_FILE), state.files.get(path)) {
                                (Ok(Some((bytes, _))), Some(base)) => {
                                    content_hash(&bytes) == base.hash
                                }
                                _ => false,
                            };
                        if as_synced {
                            let full = fsx::full(root, path);
                            std::fs::remove_file(&full).map_err(|e| Error::io(&full, e))?;
                        } else {
                            trash::remove(root, path, keep)?;
                        }
                    }
                    _ => {
                        return Err(Error::Refused(format!(
                            "a folder or a link is named {path} now; move it away first"
                        )))
                    }
                }
                fsx::rename_to_new(root, copy, path)?;
            }
        }
        // The server's version stays; my copy is taken away, and kept.
        (ConflictKind::BothChanged | ConflictKind::DeletedOnServer, Choice::Theirs, Some(copy)) => {
            take_away(root, copy, keep)?
        }
        // I deleted it and still mean to: the server's new version is taken
        // away (kept), and the next sync sends the deletion.
        (ConflictKind::DeletedHere, Choice::Mine, _) => take_away(root, path, keep)?,
        // Both stay as they are: the copy is a file of its own from now on.
        _ => {}
    }
    state.conflicts.remove(i);
    Ok(())
}

/// Take a plain file out of the folder, keeping it; nothing happens if no
/// plain file has the name.
fn take_away(root: &Path, path: &str, keep: &Keep) -> Result<(), Error> {
    if fsx::kind(root, path)? == Kind::File {
        trash::remove(root, path, keep)?;
        fsx::remove_empty_parents(root, path);
    }
    Ok(())
}

/// One side of a before-and-after.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Side {
    /// The text, when it is text and short enough to show.
    pub text: Option<String>,
    /// Why there is no text ("not there", "not a text file", …).
    pub note: Option<String>,
}

impl Side {
    fn of(bytes: Option<Vec<u8>>) -> Side {
        let note = |n: &str| Side {
            text: None,
            note: Some(n.to_string()),
        };
        let Some(bytes) = bytes else {
            return note("not there");
        };
        if !is_text(&bytes) {
            return note("not a text file");
        }
        if bytes.len() > MAX_PREVIEW {
            return note("too long to show here");
        }
        match String::from_utf8(bytes) {
            Ok(text) => Side {
                text: Some(text),
                note: None,
            },
            Err(_) => note("not a text file"),
        }
    }
}

/// A file's two versions, for What changed: the one in the folder (`mine`,
/// a path) and the server's (`theirs`, the id Check gave). Reads only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Preview {
    pub mine: Side,
    pub theirs: Side,
}

pub async fn preview<H: Host>(
    root: &Path,
    host: &H,
    mine: Option<&str>,
    theirs: Option<&str>,
) -> Result<Preview, Error> {
    let mine = match mine {
        None => Side::of(None),
        Some(path) => {
            if let Some(why) = paths::refused(path) {
                return Err(Error::Refused(format!("{path} is not shown: {why}")));
            }
            match fsx::read(root, path, MAX_FILE) {
                Ok(x) => Side::of(x.map(|(bytes, _)| bytes)),
                Err(Error::Refused(_)) => Side {
                    text: None,
                    note: Some("too long to show here".into()),
                },
                Err(e) => return Err(e),
            }
        }
    };
    let theirs = match theirs {
        None => Side::of(None),
        Some(id) => {
            if ObjectFormat::of_id(id).is_none() {
                return Err(Error::Refused("that is not a file of the server's".into()));
            }
            Side::of(Some(host.blob(id).await?))
        }
    };
    Ok(Preview { mine, theirs })
}
