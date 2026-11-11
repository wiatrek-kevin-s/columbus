//! The comparison at the heart of sync: for every path, three things are
//! compared (what was last synced, what the folder holds now, what the
//! server holds now), and the path is put in exactly one list. Nothing here
//! reads the network or changes a file.

use std::collections::{BTreeMap, BTreeSet};

use crate::host::{EntryKind, TreeEntry};
use crate::paths::{self, MAX_FILE};
use crate::scan::{Local, Scan};
use crate::state::State;
use crate::Skipped;

/// A file as the server has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerFile {
    pub blob: String,
    pub exec: bool,
}

/// What to do, path by path.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// Changed (or new) on the server only: to be written into the folder.
    pub write: Vec<(String, ServerFile)>,
    /// Deleted on the server, untouched here: to be removed (kept in the
    /// trash).
    pub remove: Vec<String>,
    /// New here: to be sent.
    pub create: Vec<String>,
    /// Changed here only: to be sent in place of the server's (its id).
    pub update: Vec<(String, ServerFile)>,
    /// Deleted here, untouched on the server (its id): to be sent.
    pub delete: Vec<(String, String)>,
    /// Different from what was synced on both sides. Whether that is a
    /// conflict is known once the two contents are compared.
    pub both: Vec<(String, ServerFile)>,
    /// Changed here, deleted on the server: a conflict.
    pub deleted_on_server: Vec<String>,
    /// Deleted here, changed on the server: a conflict.
    pub deleted_here: Vec<(String, ServerFile)>,
    /// The same on both sides already (though not recorded so): only the
    /// record changes.
    pub adopt: Vec<(String, ServerFile)>,
    /// Gone on both sides: only the record changes.
    pub forget: Vec<String>,
    /// What does not sync, and why (the folder's and the server's).
    pub skipped: Vec<Skipped>,
}

/// Compare. `local_blob` gives the id Git would give a file of the folder
/// (so equal files are recognized without fetching), or `None` when it
/// cannot be told.
pub(crate) fn plan(
    state: &State,
    local: &Scan,
    server: &[TreeEntry],
    local_blob: &dyn Fn(&str) -> Option<String>,
) -> Plan {
    let mut out = Plan {
        skipped: local.skipped.clone(),
        ..Plan::default()
    };
    // Paths that are left alone on both sides.
    let mut blocked: BTreeSet<String> = BTreeSet::new();
    fn skip(out: &mut Plan, path: &str, why: String, on_server: bool) {
        out.skipped.push(Skipped {
            path: path.to_string(),
            why,
            on_server,
        });
    }

    // What the server holds that may sync.
    let mut theirs: BTreeMap<&str, ServerFile> = BTreeMap::new();
    for e in server {
        let why = match e.kind {
            EntryKind::Link => {
                Some("it is a link on the server (links are never followed)".to_string())
            }
            EntryKind::Submodule => {
                Some("it is another repository inside this one (a submodule)".to_string())
            }
            EntryKind::File { .. } => paths::refused(&e.path)
                .or_else(|| paths::ignored(&e.path))
                .map(str::to_string)
                .or_else(|| {
                    (e.size.is_some_and(|s| s > MAX_FILE))
                        .then(|| "it is larger than 5 MB on the server".to_string())
                })
                .or_else(|| {
                    (state.lfs.get(&e.path) == Some(&e.blob)).then(|| {
                        "the server keeps it with Git LFS, which Columbus does not fetch"
                            .to_string()
                    })
                }),
        };
        match (why, e.kind) {
            (Some(why), _) => {
                skip(&mut out, &e.path, why, true);
                blocked.insert(e.path.clone());
            }
            (None, EntryKind::File { exec }) => {
                theirs.insert(
                    &e.path,
                    ServerFile {
                        blob: e.blob.clone(),
                        exec,
                    },
                );
            }
            (None, _) => {}
        }
    }

    // Something in the folder that does not sync (a link, say) where the
    // server has a file, or a folder of files: the server's are left alone
    // too, so nothing is ever written onto or through it.
    for mine in &local.skipped {
        if local.files.contains_key(&mine.path) {
            // A file too large to send: the conflict rule covers it.
            continue;
        }
        let name = mine.path.trim_end_matches('/');
        let inside = format!("{name}/");
        let hit: Vec<&str> = theirs
            .keys()
            .copied()
            .filter(|p| *p == name || p.starts_with(&inside))
            .collect();
        for p in hit {
            theirs.remove(p);
            if blocked.insert(p.to_string()) {
                skip(
                    &mut out,
                    p,
                    format!("{name} in the folder here is in the way: {}", mine.why),
                    true,
                );
            }
        }
    }

    // Names that differ only by letter case cannot share a folder on
    // Windows or macOS: neither syncs.
    let mut by_case: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let everything = || {
        theirs
            .keys()
            .copied()
            .chain(local.files.keys().map(String::as_str))
            .chain(state.files.keys().map(String::as_str))
    };
    for p in everything() {
        by_case.entry(paths::case_key(p)).or_default().insert(p);
    }
    for names in by_case.values().filter(|n| n.len() > 1) {
        for p in names {
            let other = names.iter().find(|o| *o != p).copied().unwrap_or("");
            if theirs.contains_key(p) || local.files.contains_key(*p) {
                skip(
                    &mut out,
                    p,
                    format!("its name differs from {other} only by capital letters"),
                    !local.files.contains_key(*p),
                );
            }
            blocked.insert(p.to_string());
        }
    }

    // A file on one side where the other side has a folder.
    let files_now: BTreeSet<&str> = theirs
        .keys()
        .copied()
        .chain(local.files.keys().map(String::as_str))
        .collect();
    for p in &files_now {
        let mut end = 0;
        while let Some(i) = p[end..].find('/') {
            end += i;
            let folder = &p[..end];
            if files_now.contains(folder) {
                for q in [*p, folder] {
                    if blocked.insert(q.to_string()) {
                        skip(
                            &mut out,
                            q,
                            format!("{folder} is a file on one side and a folder on the other"),
                            !local.files.contains_key(q),
                        );
                    }
                }
            }
            end += 1;
        }
    }

    for p in everything().collect::<BTreeSet<_>>() {
        if blocked.contains(p) {
            continue;
        }
        let base = state.files.get(p);
        let mine = local.files.get(p);
        let them = theirs.get(p);
        let local_changed = match (base, mine) {
            (None, None) => false,
            (None, Some(_)) | (Some(_), None) => true,
            (Some(b), Some(Local::File { hash, .. })) => *hash != b.hash,
            (Some(_), Some(Local::TooBig { .. })) => true,
        };
        let server_changed = match (base, them) {
            (None, None) => false,
            (None, Some(_)) | (Some(_), None) => true,
            (Some(b), Some(s)) => s.blob != b.blob,
        };
        let path = p.to_string();
        match (local_changed, server_changed) {
            (false, false) => {}
            (true, false) => match (mine, them) {
                (Some(Local::File { .. }), None) => out.create.push(path),
                (Some(Local::File { .. }), Some(s)) => out.update.push((path, s.clone())),
                (None, Some(s)) => out.delete.push((path, s.blob.clone())),
                // Too large to send; the scan has said so.
                (Some(Local::TooBig { .. }), _) => {}
                (None, None) => {}
            },
            (false, true) => match (mine, them) {
                (_, Some(s)) => out.write.push((path, s.clone())),
                (Some(_), None) => out.remove.push(path),
                (None, None) => {}
            },
            (true, true) => match (mine, them) {
                (None, None) => out.forget.push(path),
                (Some(l), Some(s)) => {
                    let same = matches!(l, Local::File { .. })
                        && local_blob(p).is_some_and(|id| id == s.blob);
                    if same {
                        out.adopt.push((path, s.clone()));
                    } else {
                        out.both.push((path, s.clone()));
                    }
                }
                (Some(_), None) => out.deleted_on_server.push(path),
                (None, Some(s)) => out.deleted_here.push((path, s.clone())),
            },
        }
    }
    out.skipped.sort();
    out.skipped.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsx::Stat;
    use crate::state::Synced;

    fn file(hash: &str) -> Local {
        Local::File {
            stat: Stat {
                size: 1,
                mtime: 1,
                exec: false,
            },
            hash: hash.into(),
        }
    }

    fn entry(path: &str, blob: &str) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            blob: blob.into(),
            size: Some(10),
            kind: EntryKind::File { exec: false },
        }
    }

    fn synced(blob: &str, hash: &str) -> Synced {
        Synced {
            blob: blob.into(),
            size: 1,
            mtime: 1,
            hash: hash.into(),
        }
    }

    fn sf(blob: &str) -> ServerFile {
        ServerFile {
            blob: blob.into(),
            exec: false,
        }
    }

    /// One path in every combination of (last synced, here, server).
    #[test]
    fn every_case_of_one_path() {
        let none = |_: &str| None;
        let run = |base: Option<(&str, &str)>, here: Option<&str>, there: Option<&str>| {
            let mut state = State::default();
            if let Some((blob, hash)) = base {
                state.files.insert("f".into(), synced(blob, hash));
            }
            let mut local = Scan::default();
            if let Some(h) = here {
                local.files.insert("f".into(), file(h));
            }
            let server: Vec<TreeEntry> = there.iter().map(|b| entry("f", b)).collect();
            plan(&state, &local, &server, &none)
        };
        let p = |f: &dyn Fn(&mut Plan)| {
            let mut x = Plan::default();
            f(&mut x);
            x
        };
        let f = || "f".to_string();
        // Never synced.
        assert_eq!(run(None, None, None), Plan::default());
        assert_eq!(run(None, Some("h1"), None), p(&|x| x.create.push(f())));
        assert_eq!(
            run(None, None, Some("b1")),
            p(&|x| x.write.push((f(), sf("b1"))))
        );
        assert_eq!(
            run(None, Some("h1"), Some("b1")),
            p(&|x| x.both.push((f(), sf("b1"))))
        );
        // Synced as (b1, h1).
        let base = Some(("b1", "h1"));
        assert_eq!(run(base, Some("h1"), Some("b1")), Plan::default());
        assert_eq!(
            run(base, Some("h2"), Some("b1")),
            p(&|x| x.update.push((f(), sf("b1"))))
        );
        assert_eq!(
            run(base, None, Some("b1")),
            p(&|x| x.delete.push((f(), "b1".into())))
        );
        assert_eq!(
            run(base, Some("h1"), Some("b2")),
            p(&|x| x.write.push((f(), sf("b2"))))
        );
        assert_eq!(run(base, Some("h1"), None), p(&|x| x.remove.push(f())));
        assert_eq!(
            run(base, Some("h2"), Some("b2")),
            p(&|x| x.both.push((f(), sf("b2"))))
        );
        assert_eq!(
            run(base, Some("h2"), None),
            p(&|x| x.deleted_on_server.push(f()))
        );
        assert_eq!(
            run(base, None, Some("b2")),
            p(&|x| x.deleted_here.push((f(), sf("b2"))))
        );
        assert_eq!(run(base, None, None), p(&|x| x.forget.push(f())));
    }

    #[test]
    fn equal_files_are_recognized_without_fetching() {
        let mut local = Scan::default();
        local.files.insert("f".into(), file("h1"));
        let server = [entry("f", "b1")];
        let same = |_: &str| Some("b1".to_string());
        let got = plan(&State::default(), &local, &server, &same);
        assert_eq!(got.adopt, [("f".to_string(), sf("b1"))]);
        assert!(got.both.is_empty());
    }

    #[test]
    fn what_the_server_holds_that_never_syncs() {
        let mut state = State::default();
        state.lfs.insert("model.bin".into(), "b9".into());
        let mut server = vec![
            entry("ok.py", "b1"),
            entry("roads.shp", "b2"),
            entry(".git/config", "b3"),
            entry("NUL.txt", "b4"),
            entry("model.bin", "b9"),
            entry("big.csv", "b5"),
            entry("link", "b6"),
            entry("vendor/lib", "b7"),
            entry("Readme.md", "b8"),
            entry("README.md", "b8"),
        ];
        server[5].size = Some(MAX_FILE + 1);
        server[6].kind = EntryKind::Link;
        server[7].kind = EntryKind::Submodule;
        let got = plan(
            &State::default().clone(),
            &Scan::default(),
            &server,
            &|_| None,
        );
        // Without the LFS record the pointer would be fetched once to find out.
        assert!(got.write.iter().any(|(p, _)| p == "model.bin"));
        let got = plan(&state, &Scan::default(), &server, &|_| None);
        assert_eq!(got.write, [("ok.py".to_string(), sf("b1"))]);
        let why: BTreeMap<&str, &str> = got
            .skipped
            .iter()
            .map(|s| (s.path.as_str(), s.why.as_str()))
            .collect();
        for (path, word) in [
            ("roads.shp", "geodata"),
            (".git/config", ".git"),
            ("NUL.txt", "Windows"),
            ("model.bin", "LFS"),
            ("big.csv", "5 MB"),
            ("link", "link"),
            ("vendor/lib", "submodule"),
            ("Readme.md", "capital letters"),
            ("README.md", "capital letters"),
        ] {
            assert!(
                why.get(path).is_some_and(|w| w.contains(word)),
                "{path}: {why:?}"
            );
        }
        assert!(got.skipped.iter().all(|s| s.on_server));
    }

    #[test]
    fn a_name_that_differs_only_by_case_from_the_servers_is_left_alone() {
        let mut local = Scan::default();
        local.files.insert("Notes.md".into(), file("h1"));
        let server = [entry("notes.md", "b1")];
        let got = plan(&State::default(), &local, &server, &|_| None);
        assert!(got.create.is_empty() && got.write.is_empty() && got.both.is_empty());
        assert_eq!(got.skipped.len(), 2);
    }

    #[test]
    fn a_file_on_one_side_and_a_folder_on_the_other_is_left_alone() {
        let mut local = Scan::default();
        local.files.insert("a".into(), file("h1"));
        local.files.insert("keep.txt".into(), file("h2"));
        let server = [entry("a/b.txt", "b1")];
        let got = plan(&State::default(), &local, &server, &|_| None);
        assert_eq!(got.create, ["keep.txt"]);
        assert!(got.write.is_empty());
        let named: Vec<&str> = got.skipped.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(named, ["a", "a/b.txt"]);
    }

    #[test]
    fn a_file_too_large_to_send_still_meets_the_conflict_rule() {
        let mut state = State::default();
        state.files.insert("f".into(), synced("b1", "h1"));
        let mut local = Scan::default();
        local
            .files
            .insert("f".into(), Local::TooBig { size: MAX_FILE + 1 });
        // Unchanged on the server: nothing is sent, nothing is fetched.
        let got = plan(&state, &local, &[entry("f", "b1")], &|_| None);
        assert_eq!(got, Plan::default());
        // Changed on the server too: both versions will be kept.
        let got = plan(&state, &local, &[entry("f", "b2")], &|_| Some("b2".into()));
        assert_eq!(got.both, [("f".to_string(), sf("b2"))]);
        let got = plan(&state, &local, &[], &|_| None);
        assert_eq!(got.deleted_on_server, ["f"]);
    }
}
