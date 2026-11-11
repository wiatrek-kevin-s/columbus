//! Two simulated people, Ana and Ben, working on the same repository for
//! many rounds: random edits, new files, deletions, renames and changes of
//! line endings, syncs in random order, a third colleague (Cy) whose
//! commits land while a sync is under way, and conflicts settled by random
//! choice. Every run has a seed, so a failing one can be run again exactly.
//!
//! What must hold (T-32's first check):
//!
//! 1. **No changed file is ever lost.** Whatever a folder held before a
//!    sync (or before a conflict was settled) is, afterwards, still in the
//!    folder, or in the repository, or kept where removed files go.
//! 2. **Nothing on the server is replaced unseen.** A sync replaces or
//!    removes a file on the server only if the folder had held that very
//!    version of it.
//! 3. **They agree in the end.** Once both have settled their conflicts
//!    and synced twice with no new edits, the two folders are equal, and
//!    equal to the repository.
//!
//! Line endings alone are not a change (D-20), so versions are compared by
//! content with line endings folded, as the engine does.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use columbus_sync::hash::{content_hash, ObjectFormat};
use columbus_sync::{paths, Choice, ConflictKind, Error};
use support::{del, put, Edit, MemoryHost, Person, Repo, Rng};

/// The names files get. Few of them, so that both people often meet on
/// one; none is a folder of another.
const NAMES: &[&str] = &[
    "a.md",
    "b.md",
    "notes/c.md",
    "notes/d.md",
    "scripts/e.py",
    "scripts/f.py",
    "g.txt",
    "deep/er/h.txt",
];

/// Names that never sync: they must stay where they are made.
const LOCAL_ONLY: &[&str] = &["outputs/result.csv", ".env", "data/roads.shp"];

struct World {
    rng: Rng,
    repo: Arc<Mutex<Repo>>,
    /// A counter that makes every written version unique.
    version: u64,
    seen: Seen,
}

/// What the runs went through, so that a run that tests nothing is noticed.
#[derive(Debug, Default)]
struct Seen {
    syncs: usize,
    sent: usize,
    got: usize,
    removed: usize,
    conflicts: usize,
    deleted_on_one_side: usize,
    read_again: usize,
    gave_up: usize,
    kept_mine: usize,
    kept_theirs: usize,
    kept_both: usize,
}

impl Seen {
    fn add(&mut self, o: &Seen) {
        self.syncs += o.syncs;
        self.sent += o.sent;
        self.got += o.got;
        self.removed += o.removed;
        self.conflicts += o.conflicts;
        self.deleted_on_one_side += o.deleted_on_one_side;
        self.read_again += o.read_again;
        self.gave_up += o.gave_up;
        self.kept_mine += o.kept_mine;
        self.kept_theirs += o.kept_theirs;
        self.kept_both += o.kept_both;
    }
}

struct Actor {
    person: Person,
    host: MemoryHost,
    /// Every (path, content) the folder has held, as the engine compares
    /// content.
    held: BTreeSet<(String, String)>,
}

impl Actor {
    fn new(name: &str, repo: &Arc<Mutex<Repo>>) -> Actor {
        Actor {
            person: Person::new(name),
            host: MemoryHost::new(repo.clone(), name),
            held: BTreeSet::new(),
        }
    }

    /// Note what the folder holds now.
    fn look(&mut self) {
        for (path, bytes) in self.person.files() {
            self.held.insert((path, content_hash(&bytes)));
        }
    }

    fn syncable(&self) -> Vec<String> {
        let held = self.person.state.held();
        self.person
            .files()
            .into_keys()
            .filter(|p| paths::ignored(p).is_none() && !held.contains(p))
            .collect()
    }
}

impl World {
    fn text(&mut self, who: &str) -> String {
        self.version += 1;
        // Several lines, so that line endings can differ.
        format!(
            "{who} wrote version {}\nsecond line\nthird line\n",
            self.version
        )
    }

    /// One random change to a person's folder.
    fn edit(&mut self, a: &mut Actor) {
        let existing = a.syncable();
        let roll = self.rng.below(100);
        let who = a.person.name.clone();
        if roll < 40 && !existing.is_empty() {
            let path = self.rng.pick(&existing).clone();
            let text = self.text(&who);
            a.person.write(&path, text);
        } else if roll < 60 {
            let path = self.rng.pick(NAMES).to_string();
            let text = self.text(&who);
            a.person.write(&path, text);
        } else if roll < 75 && !existing.is_empty() {
            let path = self.rng.pick(&existing).clone();
            a.person.delete(&path);
        } else if roll < 85 && !existing.is_empty() {
            // A rename: the same content under another name.
            let from = self.rng.pick(&existing).clone();
            let to = self.rng.pick(NAMES).to_string();
            if from != to {
                let bytes = a.person.read(&from).unwrap();
                a.person.delete(&from);
                a.person.write(&to, bytes);
            }
        } else if roll < 93 && !existing.is_empty() {
            // The editor saves with other line endings: not a change.
            let path = self.rng.pick(&existing).clone();
            let text = String::from_utf8(a.person.read(&path).unwrap()).unwrap();
            let other = if text.contains("\r\n") {
                text.replace("\r\n", "\n")
            } else {
                text.replace('\n', "\r\n")
            };
            a.person.write(&path, other);
        } else {
            // Something that never syncs.
            let path = self.rng.pick(LOCAL_ONLY).to_string();
            let text = self.text(&who);
            a.person.write(&path, text);
        }
        a.look();
    }

    /// A commit by a colleague with real Git, straight on the server.
    fn cy_edits(&mut self) -> Vec<Edit> {
        let n = 1 + self.rng.below(2);
        let mut edits = Vec::new();
        let on_server: Vec<String> = self
            .repo
            .lock()
            .unwrap()
            .files("main")
            .into_keys()
            .collect();
        for _ in 0..n {
            if self.rng.chance(25) && !on_server.is_empty() {
                let path = self.rng.pick(&on_server).clone();
                if !edits.iter().any(|e| matches!(e, Edit::Delete { path: p } | Edit::Put { path: p, .. } if *p == path)) {
                    edits.push(del(&path));
                }
            } else {
                let path = self.rng.pick(NAMES).to_string();
                if !edits.iter().any(|e| matches!(e, Edit::Delete { path: p } | Edit::Put { path: p, .. } if *p == path)) {
                    let text = self.text("Cy");
                    edits.push(put(&path, text.as_bytes()));
                }
            }
        }
        edits
    }

    /// Sync, checking rules 1 and 2 around it.
    async fn sync(&mut self, a: &mut Actor, seed: u64, round: usize) {
        a.look();
        let before: Vec<(String, String)> = a
            .person
            .files()
            .into_iter()
            .map(|(p, b)| (p, content_hash(&b)))
            .collect();
        // Sometimes Cy's commit lands while this sync is under way.
        if self.rng.chance(30) {
            let edits = self.cy_edits();
            if !edits.is_empty() {
                a.host.on_next_send(move |repo| {
                    repo.commit("main", &edits, "Cy, meanwhile", "Cy");
                });
            }
        }
        self.seen.syncs += 1;
        match a.person.try_sync(&a.host, None).await {
            Ok(r) => {
                self.seen.sent += r.sent.len();
                self.seen.got += r.got.len();
                self.seen.removed += r.removed.len();
                self.seen.conflicts += r.conflicts.len();
                self.seen.deleted_on_one_side += r
                    .conflicts
                    .iter()
                    .filter(|c| c.kind != ConflictKind::BothChanged)
                    .count();
                self.seen.read_again += r.rounds as usize;
            }
            // Never forced: a sync may give up when the server keeps moving.
            Err(Error::Busy(_)) => self.seen.gave_up += 1,
            Err(e) => panic!(
                "seed {seed}, round {round}: {} could not sync: {e}",
                a.person.name
            ),
        }
        a.host.before_send.lock().unwrap().clear();
        a.look();
        self.nothing_lost(a, &before, seed, round, "a sync");
        // Rule 2: what this sync replaced or removed on the server, the
        // folder had held.
        let repo = self.repo.lock().unwrap();
        for (path, old) in a.host.replaced.lock().unwrap().drain(..) {
            let version = content_hash(&repo.blobs[&old]);
            assert!(
                a.held.contains(&(path.clone(), version)),
                "seed {seed}, round {round}: {} replaced a version of {path} on the server that its folder never held",
                a.person.name
            );
        }
    }

    /// Rule 1: every version in `before` is still somewhere.
    fn nothing_lost(
        &self,
        a: &Actor,
        before: &[(String, String)],
        seed: u64,
        round: usize,
        after: &str,
    ) {
        let here: BTreeSet<String> = a.person.files().values().map(|b| content_hash(b)).collect();
        let kept: BTreeSet<String> = a
            .person
            .kept_files()
            .iter()
            .map(|b| content_hash(b))
            .collect();
        let repo = self.repo.lock().unwrap();
        let there: BTreeSet<String> = repo.blobs.values().map(|b| content_hash(b)).collect();
        for (path, version) in before {
            assert!(
                here.contains(version) || there.contains(version) || kept.contains(version),
                "seed {seed}, round {round}: after {after}, the version of {path} that {} had is nowhere",
                a.person.name
            );
        }
        // And nothing half-written is left behind.
        assert!(
            !a.person
                .files()
                .keys()
                .any(|p| p.contains(".columbus-sync-")),
            "seed {seed}: a temporary file was left in the folder"
        );
    }

    /// Settle every file that needs a look, by random choice (or, to end
    /// the run, by keeping the server's).
    fn settle(&mut self, a: &mut Actor, seed: u64, round: usize, give_way: bool) {
        a.look();
        let before: Vec<(String, String)> = a
            .person
            .files()
            .into_iter()
            .map(|(p, b)| (p, content_hash(&b)))
            .collect();
        for c in a.person.state.conflicts.clone() {
            let choice = if give_way {
                Choice::Theirs
            } else {
                *self.rng.pick(&[Choice::Mine, Choice::Theirs, Choice::Both])
            };
            match choice {
                Choice::Mine => self.seen.kept_mine += 1,
                Choice::Theirs => self.seen.kept_theirs += 1,
                Choice::Both => self.seen.kept_both += 1,
            }
            a.person.resolve(&c.path, c.copy.as_deref(), choice);
        }
        a.look();
        self.nothing_lost(a, &before, seed, round, "settling a conflict");
    }
}

fn syncable_files(p: &Person) -> BTreeMap<String, Vec<u8>> {
    p.files()
        .into_iter()
        .filter(|(path, _)| paths::ignored(path).is_none())
        .collect()
}

/// Folders compared as the engine compares content: line endings folded.
fn by_content(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(p, b)| (p.clone(), content_hash(b)))
        .collect()
}

async fn run(seed: u64, rounds: usize, format: ObjectFormat) -> Seen {
    let repo = Arc::new(Mutex::new(Repo::new(format)));
    let mut w = World {
        rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
        repo: repo.clone(),
        version: 0,
        seen: Seen::default(),
    };
    let mut ana = Actor::new("Ana", &repo);
    let mut ben = Actor::new("Ben", &repo);

    for round in 0..rounds {
        for a in [&mut ana, &mut ben] {
            for _ in 0..w.rng.below(4) {
                w.edit(a);
            }
            if w.rng.chance(25) {
                w.settle(a, seed, round, false);
            }
        }
        // Cy sometimes commits between syncs, too.
        if w.rng.chance(15) {
            let edits = w.cy_edits();
            if !edits.is_empty() {
                repo.lock().unwrap().commit("main", &edits, "Cy", "Cy");
            }
        }
        let ana_first = w.rng.chance(50);
        for turn in 0..2 {
            let a = if (turn == 0) == ana_first {
                &mut ana
            } else {
                &mut ben
            };
            if w.rng.chance(70) {
                w.sync(a, seed, round).await;
            }
        }
    }

    // The end: everyone settles what needs a look and syncs, until nothing
    // needs one. After a few tries they give way, so that this ends.
    for attempt in 0.. {
        assert!(attempt < 30, "seed {seed}: the two never settled");
        for a in [&mut ana, &mut ben] {
            w.settle(a, seed, rounds, attempt >= 4);
            w.sync(a, seed, rounds).await;
        }
        if ana.person.state.conflicts.is_empty() && ben.person.state.conflicts.is_empty() {
            break;
        }
    }
    // Both sync twice, with no new edits (and no Cy).
    for _ in 0..2 {
        for a in [&mut ana, &mut ben] {
            let before: Vec<(String, String)> = a
                .person
                .files()
                .into_iter()
                .map(|(p, b)| (p, content_hash(&b)))
                .collect();
            let report = a.person.sync(&a.host).await;
            assert!(
                report.conflicts.is_empty(),
                "seed {seed}: a conflict with no new edits: {report:?}"
            );
            w.nothing_lost(a, &before, seed, rounds, "the last syncs");
        }
    }

    // Rule 3: the folders are equal, and equal to the repository.
    let (fa, fb) = (syncable_files(&ana.person), syncable_files(&ben.person));
    assert_eq!(
        by_content(&fa),
        by_content(&fb),
        "seed {seed}: the two folders differ"
    );
    let on_server = repo.lock().unwrap().files("main");
    assert_eq!(
        by_content(&fa),
        by_content(&on_server),
        "seed {seed}: the folders and the repository differ"
    );
    // And both see nothing left to do.
    for a in [&ana, &ben] {
        let c = a.person.check(&a.host).await;
        assert!(
            c.here.is_empty()
                && c.server.is_empty()
                && c.both.is_empty()
                && c.needs_look.is_empty(),
            "seed {seed}: {} still sees changes: {c:?}",
            a.person.name
        );
    }
    // What never syncs never reached the repository, in any commit.
    let repo = repo.lock().unwrap();
    for tree in repo.trees.values() {
        for path in tree.keys() {
            assert!(
                paths::ignored(path).is_none(),
                "seed {seed}: {path} reached the repository"
            );
        }
    }
    // Nothing was ever forced: every commit of the branch has its place in
    // one line of history (Cy's commits included).
    let log = repo.log("main");
    let cy = log.iter().filter(|(_, c)| c.who == "Cy").count();
    let made_by_cy = repo.commits.values().filter(|c| c.who == "Cy").count();
    assert_eq!(
        cy, made_by_cy,
        "seed {seed}: a commit of Cy's is no longer in the history"
    );
    assert_eq!(
        log.len(),
        repo.commits.len(),
        "seed {seed}: a commit fell out of the history"
    );
    w.seen
}

#[tokio::test]
async fn two_people_never_lose_a_change_and_agree_in_the_end() {
    let mut seen = Seen::default();
    for seed in 1..=24 {
        seen.add(&run(seed, 40, ObjectFormat::Sha1).await);
    }
    // The runs went through what they are meant to: files sent, fetched
    // and removed, conflicts of every kind settled every way, branches that
    // moved during a sync.
    println!("{seen:?}");
    assert!(
        seen.syncs > 1000 && seen.sent > 500 && seen.got > 500,
        "{seen:?}"
    );
    assert!(
        seen.removed > 20 && seen.conflicts > 100 && seen.deleted_on_one_side > 20,
        "{seen:?}"
    );
    assert!(seen.read_again > 50, "{seen:?}");
    assert!(
        seen.kept_mine > 20 && seen.kept_theirs > 20 && seen.kept_both > 20,
        "{seen:?}"
    );
}

#[tokio::test]
async fn two_people_on_a_repository_with_sha256_ids() {
    let mut seen = Seen::default();
    for seed in 100..=105 {
        seen.add(&run(seed, 40, ObjectFormat::Sha256).await);
    }
    assert!(seen.conflicts > 10 && seen.read_again > 5, "{seen:?}");
}

#[tokio::test]
async fn a_long_run() {
    let seen = run(7, 400, ObjectFormat::Sha1).await;
    assert!(seen.conflicts > 50, "{seen:?}");
}
