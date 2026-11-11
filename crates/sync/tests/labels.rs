//! A file that may hold data read from a source is held, not sent (final
//! review of part 3, R-103): the data labels kept beside a project's files
//! (`files-labels.json`), the person's choice to send a file anyway, and
//! the organization's flow rules.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use columbus_shared::policy::{FlowAction, FlowRule};
use columbus_sync::hash::ObjectFormat;
use columbus_sync::{Choice, DataLabels, What};
use support::{put, MemoryHost, Person, Repo, TODAY};

const HOST: &str = "git.example.org";
/// A value only the labelled file holds, so it can be looked for on the
/// server.
const ROW: &str = "parcel 4471, owner M. Okafor";

fn repo() -> Arc<Mutex<Repo>> {
    Arc::new(Mutex::new(Repo::new(ObjectFormat::Sha1)))
}

/// Write the record the server keeps beside a project's files, and read it
/// the way the app does.
fn record(ana: &mut Person, labels: &[(&str, &[&str])], rules: &[FlowRule]) {
    let map: BTreeMap<&str, &[&str]> = labels.iter().copied().collect();
    std::fs::write(
        ana.project().join("files-labels.json"),
        serde_json::to_string(&map).unwrap(),
    )
    .unwrap();
    ana.labels = DataLabels::load(ana.project(), rules, HOST);
}

fn deny(from: &str, to: &str) -> FlowRule {
    FlowRule {
        from: vec![from.into()],
        to: vec![to.into()],
        action: FlowAction::Deny,
    }
}

#[tokio::test]
async fn a_labelled_file_is_held_released_and_held_again_when_it_changes() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    ana.write("scripts/widths.py", "print('widths')\n");
    ana.write("notes/summary.md", format!("# Summary\n{ROW}\n"));
    record(
        &mut ana,
        &[("notes/summary.md", &["connection:roads-prod"])],
        &[],
    );

    // Check lists it as held, with the source's name, not as a change a
    // sync would send.
    let c = ana.check(&host).await;
    assert_eq!(
        c.here.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
        ["scripts/widths.py"]
    );
    assert_eq!(c.held.len(), 1, "{:?}", c.held);
    let held = c.held[0].clone();
    assert_eq!(
        (held.path.as_str(), held.what),
        ("notes/summary.md", What::New)
    );
    assert_eq!(held.sources, ["roads-prod"]);
    assert!(held.choice, "the person may choose to send it");
    assert!(
        held.why.contains("it may hold data read from roads-prod"),
        "{}",
        held.why
    );

    // A sync sends the rest and says what it held. The file never leaves.
    let rep = ana.sync(&host).await;
    assert_eq!(
        rep.sent.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
        ["scripts/widths.py"]
    );
    assert_eq!(rep.held, c.held);
    assert!(rep.stopped.is_none() && rep.not_sent.is_empty());
    assert!(!r.lock().unwrap().ever_held(ROW.as_bytes()));
    assert_eq!(
        r.lock().unwrap().files("main").keys().collect::<Vec<_>>(),
        ["scripts/widths.py"]
    );
    // Sync after sync, until the person chooses.
    let rep = ana.sync(&host).await;
    assert!(rep.sent.is_empty() && rep.held.len() == 1);
    assert!(!r.lock().unwrap().ever_held(ROW.as_bytes()));

    // Send it anyway: for that file as it is now.
    ana.state.send_anyway(&held.path, &held.hash);
    let c = ana.check(&host).await;
    assert!(c.held.is_empty());
    assert_eq!(c.here.len(), 1);
    assert_eq!(c.here[0].path, "notes/summary.md");
    assert_eq!(
        c.here[0].may_hold,
        ["roads-prod"],
        "still said, beside the change"
    );
    let rep = ana.sync(&host).await;
    assert_eq!(rep.sent.len(), 1);
    assert!(rep.held.is_empty());
    assert!(r.lock().unwrap().ever_held(ROW.as_bytes()));

    // The file changes: the choice was for what it held then, so it is
    // held again, and the new text does not leave.
    ana.write(
        "notes/summary.md",
        format!("# Summary\n{ROW}\nparcel 4472, owner J. Lindqvist\n"),
    );
    let c = ana.check(&host).await;
    assert!(c.here.is_empty(), "{:?}", c.here);
    assert_eq!(c.held.len(), 1);
    assert_eq!(c.held[0].what, What::Changed);
    assert_ne!(c.held[0].hash, held.hash);
    let asked = host.asked.load(Ordering::SeqCst);
    let rep = ana.sync(&host).await;
    assert!(rep.sent.is_empty() && rep.held.len() == 1);
    assert_eq!(
        host.asked.load(Ordering::SeqCst),
        asked,
        "nothing was offered to the server"
    );
    assert!(!r.lock().unwrap().ever_held(b"Lindqvist"));

    // Back to exactly what the person chose to send: that content may go
    // (it is on the server already, so there is nothing to send).
    ana.write("notes/summary.md", format!("# Summary\n{ROW}\n"));
    let c = ana.check(&host).await;
    assert!(c.held.is_empty() && c.here.is_empty());

    // The choice can be taken back before a sync.
    ana.write("notes/summary.md", "# Summary\nchanged once more\n");
    let c = ana.check(&host).await;
    ana.state.send_anyway(&c.held[0].path, &c.held[0].hash);
    assert!(ana.check(&host).await.held.is_empty());
    ana.state.hold_again("notes/summary.md");
    assert_eq!(ana.check(&host).await.held.len(), 1);
    let rep = ana.sync(&host).await;
    assert!(rep.sent.is_empty());
    assert!(!r.lock().unwrap().ever_held(b"changed once more"));

    // Deleting a labelled file is no data: the deletion goes.
    ana.delete("notes/summary.md");
    let rep = ana.sync(&host).await;
    assert_eq!(
        rep.sent
            .iter()
            .map(|i| (i.path.as_str(), i.what))
            .collect::<Vec<_>>(),
        [("notes/summary.md", What::Deleted)]
    );
}

#[tokio::test]
async fn a_sync_sends_only_what_the_person_chose_even_if_the_file_changed_after() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    ana.write("report.md", "first text\n");
    record(&mut ana, &[("report.md", &["connection:roads-prod"])], &[]);
    let c = ana.check(&host).await;
    ana.state.send_anyway(&c.held[0].path, &c.held[0].hash);
    // Between the choice and the sync, the assistant writes it again.
    ana.write("report.md", format!("second text\n{ROW}\n"));
    let rep = ana.sync(&host).await;
    assert!(rep.sent.is_empty(), "{:?}", rep.sent);
    assert_eq!(rep.held.len(), 1);
    assert!(r.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn where_the_organizations_flow_rules_deny_it_there_is_no_choice() {
    for to in ["git:git.example.org", "git:*", "server:*", "*"] {
        let r = repo();
        let host = MemoryHost::new(r.clone(), "Ana");
        let mut ana = Person::new("ana");
        ana.write("plain.md", "no data\n");
        ana.write("hr.md", format!("{ROW}\n"));
        ana.write("roads.md", "lengths in miles\n");
        record(
            &mut ana,
            &[
                ("hr.md", &["connection:hr", "connection:roads-prod"]),
                ("roads.md", &["connection:roads-prod"]),
            ],
            &[deny("connection:hr", to)],
        );
        let c = ana.check(&host).await;
        let by: BTreeMap<&str, &columbus_sync::Held> =
            c.held.iter().map(|h| (h.path.as_str(), h)).collect();
        assert_eq!(by.len(), 2, "{to}: {:?}", c.held);
        assert!(!by["hr.md"].choice, "{to}: denied, so no choice");
        assert!(
            by["hr.md"].why.contains("your organization's rules") && by["hr.md"].why.contains("hr"),
            "{}",
            by["hr.md"].why
        );
        assert_eq!(by["hr.md"].sources, ["hr", "roads-prod"]);
        assert!(by["roads.md"].choice, "{to}: another source is not denied");

        // Even a recorded choice does not send it.
        ana.state.send_anyway("hr.md", &by["hr.md"].hash);
        ana.state.send_anyway("roads.md", &by["roads.md"].hash);
        let c = ana.check(&host).await;
        assert_eq!(
            c.held.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            ["hr.md"],
            "{to}"
        );
        let rep = ana.sync(&host).await;
        assert_eq!(
            rep.sent.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            ["plain.md", "roads.md"],
            "{to}"
        );
        assert_eq!(rep.held.len(), 1);
        assert!(!r.lock().unwrap().ever_held(ROW.as_bytes()), "{to}");
    }
    // A rule for another server, or one that only asks, leaves the choice.
    for rule in [
        deny("connection:hr", "git:github.com"),
        deny("connection:hr", "folder:*"),
        FlowRule {
            from: vec!["connection:hr".into()],
            to: vec!["git:*".into()],
            action: FlowAction::Approve,
        },
    ] {
        let host = MemoryHost::new(repo(), "Ana");
        let mut ana = Person::new("ana");
        ana.write("hr.md", "x\n");
        record(&mut ana, &[("hr.md", &["connection:hr"])], &[rule]);
        let c = ana.check(&host).await;
        assert!(c.held[0].choice);
    }
}

#[tokio::test]
async fn the_labels_follow_the_copy_kept_for_a_file_changed_on_both_sides() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("report.md", "v1\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;
    ben.write("report.md", "v2 by Ben\n");
    ben.sync(&host_b).await;

    // Ana's assistant rewrites the file after reading a source.
    ana.write("report.md", format!("v2 by Ana\n{ROW}\n"));
    record(&mut ana, &[("report.md", &["connection:roads-prod"])], &[]);
    let rep = ana.sync(&host_a).await;
    assert_eq!(rep.conflicts.len(), 1);
    let copy = format!("report (my copy {TODAY}).md");
    assert_eq!(ana.text("report.md"), "v2 by Ben\n");
    assert!(ana.text(&copy).contains(ROW));

    // Keep both: the copy is a file of its own, and still may hold data.
    ana.resolve("report.md", Some(&copy), Choice::Both);
    let c = ana.check(&host_a).await;
    assert!(c.here.is_empty(), "{:?}", c.here);
    assert_eq!(c.held.len(), 1);
    assert_eq!(c.held[0].path, copy);
    assert_eq!(c.held[0].sources, ["roads-prod"]);
    let rep = ana.sync(&host_a).await;
    assert!(rep.sent.is_empty());
    assert!(!r.lock().unwrap().ever_held(ROW.as_bytes()));

    // Keep mine, in another round: the file has its name and its labels.
    ben.write("report.md", "v3 by Ben\n");
    ben.sync(&host_b).await;
    ana.write("report.md", format!("v3 by Ana\n{ROW}\n"));
    let rep = ana.sync(&host_a).await;
    assert_eq!(rep.conflicts.len(), 1);
    let copy2 = rep.conflicts[0].copy.clone().unwrap();
    ana.resolve("report.md", Some(&copy2), Choice::Mine);
    let c = ana.check(&host_a).await;
    let held: BTreeSet<&str> = c.held.iter().map(|h| h.path.as_str()).collect();
    assert!(held.contains("report.md"), "{:?}", c.held);
    assert!(!ana.state.carried.contains_key(&copy2));
    let rep = ana.sync(&host_a).await;
    assert!(rep.sent.is_empty(), "{:?}", rep.sent);
    assert!(!r.lock().unwrap().ever_held(ROW.as_bytes()));
}

#[tokio::test]
async fn a_record_that_cannot_be_read_holds_every_file() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    ana.write("scripts/widths.py", "print('widths')\n");
    ana.write("notes.md", format!("{ROW}\n"));
    std::fs::write(ana.project().join("files-labels.json"), "{ not json").unwrap();
    ana.labels = DataLabels::load(ana.project(), &[], HOST);
    let c = ana.check(&host).await;
    assert!(c.here.is_empty());
    assert_eq!(c.held.len(), 2);
    assert!(c
        .held
        .iter()
        .all(|h| !h.choice && h.why.contains("files-labels.json")));
    ana.state.send_anyway("notes.md", &c.held[0].hash);
    let rep = ana.sync(&host).await;
    assert!(rep.sent.is_empty() && rep.held.len() == 2);
    assert!(r.lock().unwrap().is_empty());

    // What the server has still arrives: only sending waits.
    r.lock()
        .unwrap()
        .commit("main", &[put("from-cy.md", b"hello\n")], "hello", "Cy");
    let rep = ana.sync(&host).await;
    assert_eq!(rep.got, ["from-cy.md"]);

    // No record at all: nothing is labelled.
    std::fs::remove_file(ana.project().join("files-labels.json")).unwrap();
    ana.labels = DataLabels::load(ana.project(), &[], HOST);
    let rep = ana.sync(&host).await;
    assert_eq!(rep.sent.len(), 2);
    assert!(rep.held.is_empty());
}

#[tokio::test]
async fn files_without_labels_and_files_from_the_server_are_not_held() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    // The record names a path Ana does not have, and one the server sends.
    record(
        &mut ana,
        &[
            ("gone.md", &["connection:roads-prod"]),
            ("from-ben.md", &["connection:roads-prod"]),
        ],
        &[],
    );
    ben.write("from-ben.md", "Ben's\n");
    ben.sync(&host_b).await;
    ana.write("mine.md", "mine\n");
    let rep = ana.sync(&host_a).await;
    assert_eq!(rep.got, ["from-ben.md"]);
    assert_eq!(rep.sent.len(), 1);
    assert!(rep.held.is_empty(), "nothing of Ana's is labelled: {rep:?}");
}
