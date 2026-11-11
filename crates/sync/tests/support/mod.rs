//! Stand-ins for a Git server, for the tests and for the desktop app's
//! end-to-end script (`examples/fake_git_server.rs`).
//!
//! * [`Repo`]: a repository in memory, with Git's own ids for files (so a
//!   file's id here is the id real Git gives it), commits, branches and a
//!   history. Nothing in it is ever lost: every file version ever stored
//!   stays in `blobs`.
//! * [`MemoryHost`]: [`columbus_sync::Host`] straight on a `Repo`, for the
//!   test with two simulated people.
//! * [`github`] and [`gitea`]: HTTP servers shaped like GitHub's and
//!   Gitea's web APIs, written from their published API descriptions, with
//!   the refusals those document.
//! * [`Person`]: a folder, its sync record, and a place for removed files.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use columbus_sync::hash::{blob_id, ObjectFormat};
use columbus_sync::{
    Change, Changes, Commit, EntryKind, Error, Host, Keep, Options, Report, SendError, State, Tip,
    TreeEntry,
};

/// One path of a commit: its mode (`100644` a file, `100755` a program,
/// `120000` a link, `160000` a submodule) and the id of what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub mode: String,
    pub id: String,
}

/// Every file of a commit, by path.
pub type Tree = BTreeMap<String, Entry>;

#[derive(Debug, Clone)]
pub struct CommitObj {
    pub tree: String,
    pub parents: Vec<String>,
    pub message: String,
    pub who: String,
    pub when: String,
}

/// A change to a repository's files.
#[derive(Debug, Clone)]
pub enum Edit {
    Put {
        path: String,
        bytes: Vec<u8>,
    },
    /// A path with a mode of its own (a link, a submodule, a program).
    Raw {
        path: String,
        mode: String,
        bytes: Vec<u8>,
    },
    Delete {
        path: String,
    },
}

pub fn put(path: &str, bytes: &[u8]) -> Edit {
    Edit::Put {
        path: path.into(),
        bytes: bytes.to_vec(),
    }
}

pub fn del(path: &str) -> Edit {
    Edit::Delete { path: path.into() }
}

/// A repository in memory.
#[derive(Debug)]
pub struct Repo {
    pub format: ObjectFormat,
    pub blobs: HashMap<String, Vec<u8>>,
    pub trees: HashMap<String, Tree>,
    pub commits: HashMap<String, CommitObj>,
    pub branches: BTreeMap<String, String>,
    pub default_branch: String,
    pub private: bool,
    seq: u64,
}

impl Repo {
    pub fn new(format: ObjectFormat) -> Repo {
        Repo {
            format,
            blobs: HashMap::new(),
            trees: HashMap::new(),
            commits: HashMap::new(),
            branches: BTreeMap::new(),
            default_branch: "main".into(),
            private: true,
            seq: 0,
        }
    }

    /// An id in the repository's format for anything that is not a file.
    fn id_of(&self, kind: &str, body: &str) -> String {
        // The same hash as a file's, over other bytes: unique, and of the
        // right length.
        blob_id(format!("{kind}\n{body}").as_bytes(), self.format)
    }

    pub fn blob(&mut self, bytes: &[u8]) -> String {
        let id = blob_id(bytes, self.format);
        self.blobs.insert(id.clone(), bytes.to_vec());
        id
    }

    pub fn tree(&mut self, tree: Tree) -> String {
        let body: String = tree
            .iter()
            .map(|(p, e)| format!("{} {} {}\n", e.mode, e.id, p))
            .collect();
        let id = self.id_of("tree", &body);
        self.trees.insert(id.clone(), tree);
        id
    }

    /// The id of a folder inside a tree (GitHub lists folders too).
    pub fn subtree_id(&self, tree: &Tree, folder: &str) -> String {
        let prefix = format!("{folder}/");
        let body: String = tree
            .iter()
            .filter(|(p, _)| p.starts_with(&prefix))
            .map(|(p, e)| format!("{} {} {}\n", e.mode, e.id, &p[prefix.len()..]))
            .collect();
        self.id_of("tree", &body)
    }

    pub fn commit_tree(
        &mut self,
        tree: &str,
        parents: &[String],
        message: &str,
        who: &str,
    ) -> String {
        self.seq += 1;
        let when = format!(
            "2026-11-{:02}T{:02}:{:02}:00Z",
            1 + (self.seq / 1440) % 28,
            (self.seq / 60) % 24,
            self.seq % 60
        );
        let id = self.id_of(
            "commit",
            &format!("{tree}\n{parents:?}\n{message}\n{who}\n{}", self.seq),
        );
        self.commits.insert(
            id.clone(),
            CommitObj {
                tree: tree.to_string(),
                parents: parents.to_vec(),
                message: message.to_string(),
                who: who.to_string(),
                when,
            },
        );
        id
    }

    pub fn is_empty(&self) -> bool {
        self.branches.is_empty()
    }

    pub fn head(&self, branch: &str) -> Option<&str> {
        self.branches.get(branch).map(String::as_str)
    }

    pub fn tree_at(&self, commit: &str) -> Tree {
        self.commits
            .get(commit)
            .and_then(|c| self.trees.get(&c.tree))
            .cloned()
            .unwrap_or_default()
    }

    /// The plain files of a branch, with their bytes.
    pub fn files(&self, branch: &str) -> BTreeMap<String, Vec<u8>> {
        let Some(head) = self.head(branch) else {
            return BTreeMap::new();
        };
        self.tree_at(head)
            .into_iter()
            .filter(|(_, e)| e.mode == "100644" || e.mode == "100755")
            .map(|(p, e)| (p, self.blobs.get(&e.id).cloned().unwrap_or_default()))
            .collect()
    }

    /// A tree with edits applied. `Err(path)` for a deletion of a path that
    /// is not there.
    pub fn edited(&mut self, mut tree: Tree, edits: &[Edit]) -> Result<Tree, String> {
        for e in edits {
            match e {
                Edit::Put { path, bytes } => {
                    let mode = tree
                        .get(path)
                        .map(|e| e.mode.clone())
                        .filter(|m| m == "100755");
                    let id = self.blob(bytes);
                    tree.insert(
                        path.clone(),
                        Entry {
                            mode: mode.unwrap_or_else(|| "100644".into()),
                            id,
                        },
                    );
                }
                Edit::Raw { path, mode, bytes } => {
                    let id = if mode == "160000" {
                        self.id_of("commit", &String::from_utf8_lossy(bytes))
                    } else {
                        self.blob(bytes)
                    };
                    tree.insert(
                        path.clone(),
                        Entry {
                            mode: mode.clone(),
                            id,
                        },
                    );
                }
                Edit::Delete { path } => {
                    if tree.remove(path).is_none() {
                        return Err(path.clone());
                    }
                }
            }
        }
        Ok(tree)
    }

    /// Commit edits on top of a branch, as a colleague working with real
    /// Git would (the branch is made if the repository is empty).
    pub fn commit(&mut self, branch: &str, edits: &[Edit], message: &str, who: &str) -> String {
        let head = self.head(branch).map(str::to_string);
        let base = head.as_deref().map(|h| self.tree_at(h)).unwrap_or_default();
        let tree = self.edited(base, edits).expect("the path is there");
        let tree = self.tree(tree);
        let parents: Vec<String> = head.into_iter().collect();
        let id = self.commit_tree(&tree, &parents, message, who);
        self.branches.insert(branch.to_string(), id.clone());
        id
    }

    /// A branch's commits, newest first.
    pub fn log(&self, branch: &str) -> Vec<(String, CommitObj)> {
        let mut out = Vec::new();
        let mut at = self.head(branch).map(str::to_string);
        while let Some(id) = at {
            let Some(c) = self.commits.get(&id) else {
                break;
            };
            at = c.parents.first().cloned();
            out.push((id, c.clone()));
        }
        out
    }

    /// Whether some version of some file ever stored here holds `needle`.
    pub fn ever_held(&self, needle: &[u8]) -> bool {
        self.blobs
            .values()
            .any(|b| b.windows(needle.len().max(1)).any(|w| w == needle))
    }
}

pub fn entry_kind(mode: &str) -> EntryKind {
    match mode {
        "120000" => EntryKind::Link,
        "160000" => EntryKind::Submodule,
        "100755" => EntryKind::File { exec: true },
        _ => EntryKind::File { exec: false },
    }
}

/// Apply what Columbus sends to a tree, as a server does: every file it
/// replaces or removes must still be what Columbus read (`old`), and a new
/// file must not be there yet. `Err(())`: something is not as it was.
pub fn apply_changes(repo: &mut Repo, mut tree: Tree, changes: &[Change]) -> Result<Tree, ()> {
    for c in changes {
        match c {
            Change::Create { path, bytes, exec } => {
                if tree.contains_key(path) {
                    return Err(());
                }
                let id = repo.blob(bytes);
                tree.insert(
                    path.clone(),
                    Entry {
                        mode: mode_of(*exec),
                        id,
                    },
                );
            }
            Change::Update {
                path,
                bytes,
                old,
                exec,
            } => {
                if tree.get(path).map(|e| e.id.as_str()) != Some(old.as_str()) {
                    return Err(());
                }
                let id = repo.blob(bytes);
                tree.insert(
                    path.clone(),
                    Entry {
                        mode: mode_of(*exec),
                        id,
                    },
                );
            }
            Change::Delete { path, old } => {
                if tree.get(path).map(|e| e.id.as_str()) != Some(old.as_str()) {
                    return Err(());
                }
                tree.remove(path);
            }
        }
    }
    Ok(tree)
}

fn mode_of(exec: bool) -> String {
    if exec { "100755" } else { "100644" }.to_string()
}

type Hook = Box<dyn FnOnce(&mut Repo) + Send>;

/// [`Host`] on a [`Repo`] in memory. `before_send` hooks run, one per send,
/// just before the server looks at what was sent: a colleague's commit made
/// there is one that landed while a sync was under way.
#[derive(Clone)]
pub struct MemoryHost {
    pub repo: Arc<Mutex<Repo>>,
    pub branch: String,
    pub who: String,
    pub before_send: Arc<Mutex<Vec<Hook>>>,
    /// How many sends were asked for, and how many changed the repository.
    pub asked: Arc<AtomicUsize>,
    pub landed: Arc<AtomicUsize>,
}

impl MemoryHost {
    pub fn new(repo: Arc<Mutex<Repo>>, who: &str) -> MemoryHost {
        MemoryHost {
            repo,
            branch: "main".into(),
            who: who.into(),
            before_send: Arc::default(),
            asked: Arc::default(),
            landed: Arc::default(),
        }
    }

    pub fn on_next_send(&self, hook: impl FnOnce(&mut Repo) + Send + 'static) {
        self.before_send.lock().unwrap().push(Box::new(hook));
    }
}

impl Host for MemoryHost {
    async fn tip(&self) -> Result<Tip, Error> {
        let r = self.repo.lock().unwrap();
        match r.head(&self.branch) {
            Some(c) => Ok(Tip::At(c.to_string())),
            None if r.is_empty() => Ok(Tip::Empty),
            None => Err(Error::NotFound(format!(
                "there is no branch {}",
                self.branch
            ))),
        }
    }

    async fn tree(&self, commit: &str) -> Result<Vec<TreeEntry>, Error> {
        let r = self.repo.lock().unwrap();
        if !r.commits.contains_key(commit) {
            return Err(Error::NotFound(format!("there is no commit {commit}")));
        }
        Ok(r.tree_at(commit)
            .into_iter()
            .map(|(path, e)| TreeEntry {
                size: r.blobs.get(&e.id).map(|b| b.len() as u64),
                kind: entry_kind(&e.mode),
                blob: e.id,
                path,
            })
            .collect())
    }

    async fn blob(&self, id: &str) -> Result<Vec<u8>, Error> {
        self.repo
            .lock()
            .unwrap()
            .blobs
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("there is no file {id}")))
    }

    async fn send(
        &self,
        tip: &Tip,
        changes: &[Change],
        message: &str,
    ) -> Result<String, SendError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let hook = self.before_send.lock().unwrap().pop();
        let mut r = self.repo.lock().unwrap();
        if let Some(hook) = hook {
            hook(&mut r);
        }
        let head = r.head(&self.branch).map(str::to_string);
        // Only on top of exactly what was read: never forced.
        if head.as_deref() != tip.commit() {
            return Err(SendError::Moved);
        }
        let base = head.as_deref().map(|h| r.tree_at(h)).unwrap_or_default();
        let tree = apply_changes(&mut r, base, changes).map_err(|()| SendError::Moved)?;
        let tree = r.tree(tree);
        let parents: Vec<String> = head.into_iter().collect();
        let id = r.commit_tree(&tree, &parents, message, &self.who);
        r.branches.insert(self.branch.clone(), id.clone());
        self.landed.fetch_add(1, Ordering::SeqCst);
        Ok(id)
    }

    async fn history(&self, limit: usize) -> Result<Vec<Commit>, Error> {
        let r = self.repo.lock().unwrap();
        Ok(r.log(&self.branch)
            .into_iter()
            .take(limit)
            .map(|(id, c)| Commit {
                id,
                who: c.who,
                when: c.when,
                note: c.message,
            })
            .collect())
    }
}

/// The day the tests run on, for the names of kept copies.
pub const TODAY: &str = "2026-11-11";

/// A person: their folder, what Columbus remembers about it, and where
/// files taken out of the folder are kept.
pub struct Person {
    pub name: String,
    dir: tempfile::TempDir,
    pub root: PathBuf,
    pub kept: PathBuf,
    pub state: State,
}

impl Person {
    pub fn new(name: &str) -> Person {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("files");
        let kept = dir.path().join("removed");
        std::fs::create_dir_all(&root).unwrap();
        Person {
            name: name.into(),
            root,
            kept,
            dir,
            state: State::default(),
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for part in rel.split('/') {
            p.push(part);
        }
        p
    }

    pub fn write(&self, rel: &str, bytes: impl AsRef<[u8]>) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    pub fn delete(&self, rel: &str) {
        std::fs::remove_file(self.path(rel)).unwrap();
    }

    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(rel)).ok()
    }

    pub fn text(&self, rel: &str) -> String {
        String::from_utf8(
            self.read(rel)
                .unwrap_or_else(|| panic!("{rel} is not there")),
        )
        .unwrap()
    }

    /// Every file in the folder, by path.
    pub fn files(&self) -> BTreeMap<String, Vec<u8>> {
        files_under(&self.root)
    }

    /// Every file kept after it was taken out of the folder, by its bytes.
    pub fn kept_files(&self) -> Vec<Vec<u8>> {
        files_under(&self.kept).into_values().collect()
    }

    pub fn keep(&self) -> Keep {
        Keep::Folder(self.kept.clone())
    }

    pub async fn check<H: Host>(&self, host: &H) -> Changes {
        columbus_sync::check(&self.root, host, &self.state)
            .await
            .expect("check")
    }

    pub async fn sync<H: Host>(&mut self, host: &H) -> Report {
        self.try_sync(host, None).await.expect("sync")
    }

    pub async fn sync_with<H: Host>(&mut self, host: &H, note: &str) -> Report {
        self.try_sync(host, Some(note)).await.expect("sync")
    }

    pub async fn try_sync<H: Host>(
        &mut self,
        host: &H,
        note: Option<&str>,
    ) -> Result<Report, Error> {
        let keep = self.keep();
        let opts = Options {
            note,
            seen: None,
            keep: &keep,
            today: TODAY,
            never_send: &[],
        };
        columbus_sync::sync(&self.root, host, &mut self.state, &opts).await
    }

    pub fn resolve(&mut self, path: &str, copy: Option<&str>, choice: columbus_sync::Choice) {
        let keep = self.keep();
        columbus_sync::resolve(&self.root, &mut self.state, path, copy, choice, &keep)
            .expect("resolve");
    }
}

/// Every plain file under a folder (links are not followed), by its path
/// with `/` between parts.
pub fn files_under(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            if m.is_dir() {
                todo.push(p);
            } else if m.is_file() {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// A small random number generator with a seed, so that a failing run can
/// be run again exactly (xorshift64*).
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}
