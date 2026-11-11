//! Check, sync and the conflict rule, against a repository in memory.
//!
//! Two people, Ana and Ben, each have a folder and sync it with the same
//! repository. Every test names the rule of D-20 or R-24 it holds to.

mod support;

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use columbus_sync::hash::ObjectFormat;
use columbus_sync::{Choice, ConflictKind, Kept, Options, What};
use support::{del, put, MemoryHost, Person, Repo, TODAY};

fn repo() -> Arc<Mutex<Repo>> {
    Arc::new(Mutex::new(Repo::new(ObjectFormat::Sha1)))
}

fn paths(items: &[columbus_sync::Item]) -> Vec<(&str, What)> {
    items.iter().map(|i| (i.path.as_str(), i.what)).collect()
}

#[tokio::test]
async fn a_first_sync_sends_everything_as_one_commit_and_a_colleague_gets_it() {
    let r = repo();
    let host_a = MemoryHost::new(r.clone(), "Ana");
    let host_b = MemoryHost::new(r.clone(), "Ben");
    let mut ana = Person::new("ana");
    ana.write("columbus-project.yaml", "name: Roads\n");
    ana.write("scripts/widths.py", "print('widths')\n");
    ana.write("notes.md", "# Notes\n");

    // Check only reads: it says what a sync would send, and sends nothing.
    let c = ana.check(&host_a).await;
    assert_eq!(c.commit, None, "the repository is empty");
    assert_eq!(
        paths(&c.here),
        [
            ("columbus-project.yaml", What::New),
            ("notes.md", What::New),
            ("scripts/widths.py", What::New)
        ]
    );
    assert!(c.server.is_empty() && c.both.is_empty());
    assert!(r.lock().unwrap().is_empty(), "a check changes nothing");
    assert_eq!(host_a.asked.load(Ordering::SeqCst), 0);

    let rep = ana
        .sync_with(&host_a, "First version of the widths check")
        .await;
    assert_eq!(rep.sent.len(), 3);
    assert!(rep.got.is_empty() && rep.conflicts.is_empty() && rep.stopped.is_none());
    {
        let r = r.lock().unwrap();
        let log = r.log("main");
        assert_eq!(log.len(), 1, "one commit");
        assert_eq!(log[0].1.message, "First version of the widths check");
        assert_eq!(log[0].1.who, "Ana");
        assert_eq!(rep.commit.as_deref(), Some(log[0].0.as_str()));
        assert_eq!(r.files("main"), ana.files());
    }
    assert_eq!(ana.state.commit, rep.commit);
    assert_eq!(ana.state.files.len(), 3);

    // Nothing changed since: nothing to do, and nothing is sent.
    let c = ana.check(&host_a).await;
    assert!(c.here.is_empty() && c.server.is_empty() && c.both.is_empty());
    let rep = ana.sync(&host_a).await;
    assert!(rep.sent.is_empty() && rep.commit.is_none());
    assert_eq!(host_a.asked.load(Ordering::SeqCst), 1);

    // Ben links an empty folder and gets everything.
    let mut ben = Person::new("ben");
    let c = ben.check(&host_b).await;
    assert_eq!(c.server.len(), 3);
    assert!(c.server.iter().all(|i| i.what == What::New));
    let rep = ben.sync(&host_b).await;
    assert_eq!(
        rep.got,
        ["columbus-project.yaml", "notes.md", "scripts/widths.py"]
    );
    assert!(rep.sent.is_empty());
    assert_eq!(ben.files(), ana.files());
    assert_eq!(
        r.lock().unwrap().log("main").len(),
        1,
        "getting sends nothing"
    );
}

#[tokio::test]
async fn changes_on_each_side_travel_and_a_default_note_says_what_changed() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("a.py", "a = 1\n");
    ana.write("b.py", "b = 1\n");
    ana.write("old.py", "old\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;

    // Ana changes one file, adds one and deletes one; Ben changes another.
    ana.write("a.py", "a = 2\n");
    ana.write("new/c.py", "c = 1\n");
    ana.delete("old.py");
    ben.write("b.py", "b = 2\n");

    let c = ana.check(&host_a).await;
    assert_eq!(
        paths(&c.here),
        [
            ("a.py", What::Changed),
            ("new/c.py", What::New),
            ("old.py", What::Deleted)
        ]
    );
    let rep = ana.sync(&host_a).await;
    assert_eq!(rep.sent.len(), 3);
    assert_eq!(
        r.lock().unwrap().log("main")[0].1.message,
        "Update 3 files (1 new, 1 changed, 1 removed)"
    );

    // Ben sees both directions before he syncs.
    let c = ben.check(&host_b).await;
    assert_eq!(paths(&c.here), [("b.py", What::Changed)]);
    assert_eq!(
        paths(&c.server),
        [
            ("a.py", What::Changed),
            ("new/c.py", What::New),
            ("old.py", What::Deleted)
        ]
    );
    let rep = ben.sync(&host_b).await;
    assert_eq!(rep.got, ["a.py", "new/c.py"]);
    assert_eq!(rep.removed.len(), 1);
    assert_eq!(rep.removed[0].path, "old.py");
    // The file a colleague deleted is kept, not erased.
    let Kept::Folder(kept) = &rep.removed[0].kept else {
        panic!("kept in the test's folder")
    };
    assert_eq!(std::fs::read(kept).unwrap(), b"old\n");
    assert_eq!(paths(&rep.sent), [("b.py", What::Changed)]);
    assert_eq!(r.lock().unwrap().log("main")[0].1.message, "Update b.py");

    ana.sync(&host_a).await;
    assert_eq!(ana.files(), ben.files());
    assert_eq!(ana.text("b.py"), "b = 2\n");
    assert!(ana.read("old.py").is_none() && ben.read("old.py").is_none());
}

#[tokio::test]
async fn a_file_changed_on_both_sides_keeps_both_versions_and_waits_for_the_person() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("notes.md", "shared\n");
    ana.write("other.md", "other\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;

    ana.write("notes.md", "Ana's version\n");
    ana.sync(&host_a).await;
    ben.write("notes.md", "Ben's version\n");
    ben.write("other.md", "Ben's other\n");

    let c = ben.check(&host_b).await;
    assert_eq!(c.both.len(), 1);
    assert_eq!(
        (c.both[0].path.as_str(), c.both[0].kind),
        ("notes.md", ConflictKind::BothChanged)
    );
    assert_eq!(paths(&c.here), [("other.md", What::Changed)]);

    let before = r.lock().unwrap().log("main").len();
    let rep = ben.sync(&host_b).await;
    // The server's version has the name; Ben's is beside it, untouched.
    let copy = format!("notes (my copy {TODAY}).md");
    assert_eq!(ben.text("notes.md"), "Ana's version\n");
    assert_eq!(ben.text(&copy), "Ben's version\n");
    assert_eq!(rep.conflicts.len(), 1);
    assert_eq!(rep.conflicts[0].copy.as_deref(), Some(copy.as_str()));
    assert_eq!(ben.state.conflicts, rep.conflicts);
    // What only Ben changed was still sent; the copy was not, and the
    // server's file was not overwritten.
    assert_eq!(paths(&rep.sent), [("other.md", What::Changed)]);
    {
        let r = r.lock().unwrap();
        assert_eq!(r.log("main").len(), before + 1);
        let files = r.files("main");
        assert_eq!(files["notes.md"], b"Ana's version\n");
        assert!(
            !files.contains_key(&copy),
            "a copy stays out of sync until the person has chosen"
        );
    }
    // It stays out, sync after sync, and Check lists it as needing a look.
    let rep = ben.sync(&host_b).await;
    assert!(rep.sent.is_empty() && rep.conflicts.is_empty());
    let c = ben.check(&host_b).await;
    assert!(c.here.is_empty() && c.both.is_empty());
    assert_eq!(c.needs_look.len(), 1);
    assert!(!c.skipped.iter().any(|s| s.path == copy));
}

/// The three choices, each from the same conflict.
async fn conflict() -> (
    Arc<Mutex<Repo>>,
    MemoryHost,
    MemoryHost,
    Person,
    Person,
    String,
) {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("notes.md", "shared\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;
    ana.write("notes.md", "Ana's version\n");
    ana.sync(&host_a).await;
    ben.write("notes.md", "Ben's version\n");
    let rep = ben.sync(&host_b).await;
    assert_eq!(rep.conflicts.len(), 1);
    let copy = format!("notes (my copy {TODAY}).md");
    (r, host_a, host_b, ana, ben, copy)
}

#[tokio::test]
async fn keep_mine_puts_my_version_back_and_the_next_sync_sends_it() {
    let (r, host_a, host_b, mut ana, mut ben, copy) = conflict().await;
    ben.resolve("notes.md", Some(&copy), Choice::Mine);
    assert_eq!(ben.text("notes.md"), "Ben's version\n");
    assert!(ben.read(&copy).is_none());
    assert!(ben.state.conflicts.is_empty());
    // Nothing was sent by choosing.
    assert_eq!(
        r.lock().unwrap().files("main")["notes.md"],
        b"Ana's version\n"
    );
    let c = ben.check(&host_b).await;
    assert_eq!(paths(&c.here), [("notes.md", What::Changed)]);
    ben.sync(&host_b).await;
    ana.sync(&host_a).await;
    assert_eq!(ana.text("notes.md"), "Ben's version\n");
    assert_eq!(ana.files(), ben.files());
    // Ana's version is still in the repository's history.
    assert!(r.lock().unwrap().ever_held(b"Ana's version"));
}

#[tokio::test]
async fn keep_theirs_takes_my_copy_away_and_keeps_it_in_the_trash() {
    let (r, host_a, host_b, mut ana, mut ben, copy) = conflict().await;
    ben.resolve("notes.md", Some(&copy), Choice::Theirs);
    assert_eq!(ben.text("notes.md"), "Ana's version\n");
    assert!(ben.read(&copy).is_none());
    assert!(ben.state.conflicts.is_empty());
    assert_eq!(
        ben.kept_files(),
        [b"Ben's version\n".to_vec()],
        "taken away, not erased"
    );
    let rep = ben.sync(&host_b).await;
    assert!(rep.sent.is_empty());
    ana.sync(&host_a).await;
    assert_eq!(ana.files(), ben.files());
    assert_eq!(r.lock().unwrap().files("main").len(), 1);
}

#[tokio::test]
async fn keep_both_makes_my_copy_a_file_of_its_own() {
    let (r, host_a, host_b, mut ana, mut ben, copy) = conflict().await;
    ben.resolve("notes.md", Some(&copy), Choice::Both);
    assert_eq!(ben.text("notes.md"), "Ana's version\n");
    assert_eq!(ben.text(&copy), "Ben's version\n");
    assert!(ben.state.conflicts.is_empty());
    let c = ben.check(&host_b).await;
    assert_eq!(paths(&c.here), [(copy.as_str(), What::New)]);
    ben.sync(&host_b).await;
    ana.sync(&host_a).await;
    assert_eq!(ana.text(&copy), "Ben's version\n");
    assert_eq!(ana.files(), ben.files());
    assert_eq!(r.lock().unwrap().files("main").len(), 2);
}

#[tokio::test]
async fn a_second_conflict_on_the_same_file_gets_a_name_of_its_own() {
    let (_r, host_a, host_b, mut ana, mut ben, copy) = conflict().await;
    // Unsettled, and it happens again the same day.
    ana.write("notes.md", "Ana again\n");
    ana.sync(&host_a).await;
    ben.write("notes.md", "Ben again\n");
    let rep = ben.sync(&host_b).await;
    let second = format!("notes (my copy {TODAY} 2).md");
    assert_eq!(rep.conflicts[0].copy.as_deref(), Some(second.as_str()));
    assert_eq!(ben.text("notes.md"), "Ana again\n");
    assert_eq!(ben.text(&copy), "Ben's version\n");
    assert_eq!(ben.text(&second), "Ben again\n");
    assert_eq!(ben.state.conflicts.len(), 2);
    // Each is settled on its own.
    ben.resolve("notes.md", Some(&second), Choice::Theirs);
    ben.resolve("notes.md", Some(&copy), Choice::Mine);
    assert_eq!(ben.text("notes.md"), "Ben's version\n");
    assert!(ben.state.conflicts.is_empty());
    // "Ana again" was replaced by Ben's choice; it is on the server.
    assert_eq!(ben.kept_files(), [b"Ben again\n".to_vec()]);
}

#[tokio::test]
async fn deleted_on_the_server_and_changed_here_keeps_the_changed_version() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("plan.md", "plan\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;
    ana.delete("plan.md");
    ana.sync(&host_a).await;
    ben.write("plan.md", "Ben's plan\n");

    let c = ben.check(&host_b).await;
    assert_eq!(c.both[0].kind, ConflictKind::DeletedOnServer);
    let rep = ben.sync(&host_b).await;
    let copy = format!("plan (my copy {TODAY}).md");
    assert_eq!(rep.conflicts[0].kind, ConflictKind::DeletedOnServer);
    assert_eq!(
        ben.text(&copy),
        "Ben's plan\n",
        "the changed version is kept"
    );
    assert!(ben.read("plan.md").is_none());
    assert!(rep.sent.is_empty());
    assert!(ben.kept_files().is_empty(), "nothing went to the trash");

    // Keep mine: it has its name again and goes back to the server.
    ben.resolve("plan.md", Some(&copy), Choice::Mine);
    assert_eq!(ben.text("plan.md"), "Ben's plan\n");
    ben.sync(&host_b).await;
    assert_eq!(r.lock().unwrap().files("main")["plan.md"], b"Ben's plan\n");
    ana.sync(&host_a).await;
    assert_eq!(ana.files(), ben.files());
}

#[tokio::test]
async fn deleted_here_and_changed_on_the_server_keeps_the_changed_version() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("plan.md", "plan\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;
    ana.write("plan.md", "Ana's new plan\n");
    ana.sync(&host_a).await;
    ben.delete("plan.md");

    let c = ben.check(&host_b).await;
    assert_eq!(c.both[0].kind, ConflictKind::DeletedHere);
    let rep = ben.sync(&host_b).await;
    assert_eq!(rep.conflicts[0].kind, ConflictKind::DeletedHere);
    assert_eq!(rep.conflicts[0].copy, None);
    assert_eq!(
        ben.text("plan.md"),
        "Ana's new plan\n",
        "the changed version is kept"
    );
    assert!(
        rep.sent.is_empty(),
        "the deletion was not sent over Ana's change"
    );
    assert_eq!(
        r.lock().unwrap().files("main")["plan.md"],
        b"Ana's new plan\n"
    );

    // Keep mine: I still mean to delete it. It is kept in the trash, and
    // the next sync sends the deletion.
    ben.resolve("plan.md", None, Choice::Mine);
    assert!(ben.read("plan.md").is_none());
    assert_eq!(ben.kept_files(), [b"Ana's new plan\n".to_vec()]);
    let rep = ben.sync(&host_b).await;
    assert_eq!(paths(&rep.sent), [("plan.md", What::Deleted)]);
    assert!(r.lock().unwrap().files("main").is_empty());
    assert!(
        r.lock().unwrap().ever_held(b"Ana's new plan"),
        "and it is in the history"
    );
}

#[tokio::test]
async fn deleted_here_and_changed_on_the_server_can_also_be_kept() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("plan.md", "plan\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;
    ana.write("plan.md", "Ana's new plan\n");
    ana.sync(&host_a).await;
    ben.delete("plan.md");
    ben.sync(&host_b).await;
    ben.resolve("plan.md", None, Choice::Theirs);
    assert_eq!(ben.text("plan.md"), "Ana's new plan\n");
    let rep = ben.sync(&host_b).await;
    assert!(rep.sent.is_empty());
    assert_eq!(r.lock().unwrap().files("main").len(), 1);
}

#[tokio::test]
async fn the_same_change_on_both_sides_is_no_conflict() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("a.md", "one\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;
    // Both make the same edit, both delete the same file's twin, and both
    // add the same new file.
    for p in [&ana, &ben] {
        p.write("a.md", "two\n");
        p.write("new.md", "new\n");
    }
    ana.sync(&host_a).await;
    let c = ben.check(&host_b).await;
    assert!(
        c.both.is_empty() && c.here.is_empty() && c.server.is_empty(),
        "{c:?}"
    );
    let rep = ben.sync(&host_b).await;
    assert!(rep.conflicts.is_empty() && rep.sent.is_empty() && rep.got.is_empty());
    // What Columbus remembers is the same for both (ids and contents).
    let synced = |p: &Person| -> Vec<(String, String, String)> {
        p.state
            .files
            .iter()
            .map(|(k, v)| (k.clone(), v.blob.clone(), v.hash.clone()))
            .collect()
    };
    assert_eq!(synced(&ben), synced(&ana));
    // Deleted on both sides: only the record goes.
    ana.delete("a.md");
    ben.delete("a.md");
    ana.sync(&host_a).await;
    let rep = ben.sync(&host_b).await;
    assert!(rep.conflicts.is_empty() && rep.sent.is_empty() && rep.removed.is_empty());
    assert!(!ben.state.files.contains_key("a.md"));
}

#[tokio::test]
async fn a_difference_in_line_endings_only_is_not_a_change() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("s.py", "a = 1\nb = 2\n");
    ana.write("t.py", "t = 1\n");
    ana.write("u.py", "u = 1\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;

    // Ben's editor saves with Windows line endings: nothing to send.
    ben.write("s.py", "a = 1\r\nb = 2\r\n");
    let c = ben.check(&host_b).await;
    assert!(c.here.is_empty(), "{c:?}");
    let before = r.lock().unwrap().log("main").len();
    let rep = ben.sync(&host_b).await;
    assert!(rep.sent.is_empty());
    assert_eq!(r.lock().unwrap().log("main").len(), before);
    assert_eq!(
        ben.text("s.py"),
        "a = 1\r\nb = 2\r\n",
        "his file is left as he saved it"
    );

    // A colleague with real Git changes only the line endings of t.py on
    // the server, while Ben changes its content: Ben's is the only real
    // change, so it is sent and nothing needs a look.
    r.lock()
        .unwrap()
        .commit("main", &[put("t.py", b"t = 1\r\n")], "line endings", "Cy");
    ben.write("t.py", "t = 2\n");
    let rep = ben.sync(&host_b).await;
    assert!(rep.conflicts.is_empty(), "{rep:?}");
    assert_eq!(paths(&rep.sent), [("t.py", What::Changed)]);
    assert_eq!(r.lock().unwrap().files("main")["t.py"], b"t = 2\n");

    // The server changes only the line endings of u.py and Ben did not
    // touch it: his file is left as it is, and it is not asked for again.
    r.lock()
        .unwrap()
        .commit("main", &[put("u.py", b"u = 1\r\n")], "line endings", "Cy");
    let rep = ben.sync(&host_b).await;
    assert!(rep.got.is_empty() && rep.conflicts.is_empty() && rep.sent.is_empty());
    assert_eq!(ben.text("u.py"), "u = 1\n");
    let c = ben.check(&host_b).await;
    assert!(c.server.is_empty() && c.here.is_empty() && c.both.is_empty());

    // Both sides change a file to the same text with different line
    // endings: the same change, not a conflict.
    ana.sync(&host_a).await;
    ana.write("s.py", "a = 1\nb = 3\n");
    ana.sync(&host_a).await;
    ben.write("s.py", "a = 1\r\nb = 3\r\n");
    let rep = ben.sync(&host_b).await;
    assert!(rep.conflicts.is_empty() && rep.sent.is_empty(), "{rep:?}");
    assert_eq!(ben.text("s.py"), "a = 1\r\nb = 3\r\n");

    // A file that is not text is compared byte for byte.
    ben.write("pic.bin", b"\x00\x01\n\x02");
    ben.sync(&host_b).await;
    ben.write("pic.bin", b"\x00\x01\r\n\x02");
    let c = ben.check(&host_b).await;
    assert_eq!(paths(&c.here), [("pic.bin", What::Changed)]);
}

#[tokio::test]
async fn a_branch_that_moved_during_the_sync_is_read_again_never_forced() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("a.md", "a\n");
    ana.write("shared.md", "shared\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;

    // While Ben's sync is under way (after it read the server, before the
    // server takes his commit), a colleague's commit lands: a new file, and
    // a change to a file Ben is sending too.
    ben.write("b.md", "Ben's b\n");
    ben.write("shared.md", "Ben's shared\n");
    host_b.on_next_send(|repo| {
        repo.commit(
            "main",
            &[put("c.md", b"Cy's c\n"), put("shared.md", b"Cy's shared\n")],
            "Cy was faster",
            "Cy",
        );
    });
    let rep = ben.sync(&host_b).await;
    assert_eq!(rep.rounds, 1, "read again once");
    assert_eq!(
        host_b.asked.load(Ordering::SeqCst),
        2,
        "the first try, refused; then the second"
    );
    assert_eq!(host_b.landed.load(Ordering::SeqCst), 1);
    // Cy's commit is still there, under Ben's: nothing was overwritten.
    {
        let r = r.lock().unwrap();
        let log = r.log("main");
        assert_eq!(log[1].1.message, "Cy was faster");
        assert_eq!(
            log[0].1.parents,
            [log[1].0.clone()],
            "on top of it, not in place of it"
        );
        let files = r.files("main");
        assert_eq!(files["c.md"], b"Cy's c\n");
        assert_eq!(files["b.md"], b"Ben's b\n");
        assert_eq!(
            files["shared.md"], b"Cy's shared\n",
            "Cy's change was not overwritten"
        );
    }
    // Ben got Cy's file, and both versions of the one they both changed.
    assert_eq!(ben.text("c.md"), "Cy's c\n");
    assert_eq!(ben.text("shared.md"), "Cy's shared\n");
    assert_eq!(
        ben.text(&format!("shared (my copy {TODAY}).md")),
        "Ben's shared\n"
    );
    assert_eq!(rep.conflicts.len(), 1);
    assert_eq!(paths(&rep.sent), [("b.md", What::New)]);

    // A repository that never stops moving: the sync gives up, having
    // forced nothing, and says so plainly.
    ben.write("d.md", "d\n");
    for i in 0..10 {
        host_b.on_next_send(move |repo| {
            repo.commit(
                "main",
                &[put("busy.md", format!("{i}\n").as_bytes())],
                "busy",
                "Cy",
            );
        });
    }
    let landed = host_b.landed.load(Ordering::SeqCst);
    let err = ben.try_sync(&host_b, None).await.unwrap_err();
    assert!(matches!(err, columbus_sync::Error::Busy(_)), "{err:?}");
    assert!(err.to_string().contains("Nothing was overwritten"), "{err}");
    assert_eq!(
        host_b.landed.load(Ordering::SeqCst),
        landed,
        "nothing of Ben's landed"
    );
    assert!(!r.lock().unwrap().files("main").contains_key("d.md"));
    assert_eq!(ben.text("d.md"), "d\n", "and his file is still his");
    // Once it is quiet again, the same sync goes through.
    host_b.before_send.lock().unwrap().clear();
    let rep = ben.sync(&host_b).await;
    assert_eq!(paths(&rep.sent), [("d.md", What::New)]);
}

#[tokio::test]
async fn a_sync_sends_only_what_the_person_was_shown() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    ana.write("seen.md", "seen\n");
    ana.write("gone.md", "gone\n");
    ana.write("edited.md", "as shown\n");
    ana.sync(&host).await;
    ana.write("seen.md", "seen, changed\n");
    ana.delete("gone.md");
    ana.write("edited.md", "as shown, changed\n");

    // The page shows these three changes…
    let c = ana.check(&host).await;
    let seen: BTreeMap<String, String> = c
        .here
        .iter()
        .map(|i| (i.path.clone(), i.hash.clone()))
        .collect();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen["gone.md"], "");
    // …then, before the person presses Sync, the assistant writes a new
    // file and changes one of the three again.
    ana.write("late.md", "written after the person looked\n");
    ana.write("edited.md", "changed again after the person looked\n");

    let keep = ana.keep();
    let opts = Options {
        note: Some("what I saw"),
        seen: Some(&seen),
        keep: &keep,
        today: TODAY,
        never_send: &[],
    };
    let rep = columbus_sync::sync(&ana.root, &host, &mut ana.state, &opts)
        .await
        .unwrap();
    assert_eq!(
        paths(&rep.sent),
        [("gone.md", What::Deleted), ("seen.md", What::Changed)]
    );
    let waiting: Vec<&str> = rep.not_sent.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(waiting, ["edited.md", "late.md"]);
    assert!(rep
        .not_sent
        .iter()
        .all(|n| n.why.contains("after you looked")));
    {
        let r = r.lock().unwrap();
        let files = r.files("main");
        assert_eq!(
            files["edited.md"], b"as shown\n",
            "not the version nobody saw"
        );
        assert!(!files.contains_key("late.md"));
        assert!(!r.ever_held(b"after the person looked"));
    }
    // The next look shows them, and then they go.
    let c = ana.check(&host).await;
    assert_eq!(
        paths(&c.here),
        [("edited.md", What::Changed), ("late.md", What::New)]
    );
    ana.sync(&host).await;
    assert_eq!(r.lock().unwrap().files("main"), ana.files());
}

#[tokio::test]
async fn a_repository_with_sha256_ids_syncs_the_same_way() {
    let r = Arc::new(Mutex::new(Repo::new(ObjectFormat::Sha256)));
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("a.md", "one\n");
    let rep = ana.sync(&host_a).await;
    assert_eq!(rep.commit.as_ref().unwrap().len(), 64, "not 40 digits");
    assert_eq!(ana.state.files["a.md"].blob.len(), 64);
    // The same file on both sides is recognized by its 64-digit id.
    ben.write("a.md", "one\n");
    let c = ben.check(&host_b).await;
    assert!(
        c.both.is_empty() && c.here.is_empty() && c.server.is_empty(),
        "{c:?}"
    );
    ben.sync(&host_b).await;
    ben.write("a.md", "two\n");
    ben.sync(&host_b).await;
    ana.sync(&host_a).await;
    assert_eq!(ana.text("a.md"), "two\n");
    // And a colleague's deletion with real Git arrives too.
    r.lock()
        .unwrap()
        .commit("main", &[del("a.md")], "remove", "Cy");
    ana.sync(&host_a).await;
    assert!(ana.read("a.md").is_none());
}
