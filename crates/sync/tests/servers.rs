//! The adapters for GitHub (and GitHub Enterprise Server) and for Gitea
//! (and Forgejo), against stand-in servers shaped like each one's API
//! (T-32's second check): a first sync into an empty repository, a normal
//! sync, a branch that moved during the sync, a wrong token, a server that
//! redirects to another host, 50 items a page, and what else the servers
//! document. Every scenario runs against both kinds.

mod support;

use std::collections::BTreeSet;

use columbus_sync::hash::{content_hash, ObjectFormat};
use columbus_sync::paths;
use columbus_sync::remote::{Address, Kind, Server};
use columbus_sync::{Choice, Error, Host, Tip, What};
use support::{http, put, Lab, Person, Rng, TODAY, TOKEN};

const KINDS: [Kind; 2] = [Kind::GitHub, Kind::Gitea];

#[tokio::test]
async fn connecting_says_whose_token_it_is() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        let server = lab.server();
        server.check().await.unwrap();
        let me = server.whoami().await.unwrap();
        assert_eq!(
            (me.login.as_str(), me.name.as_str()),
            ("ana", "Ana Example"),
            "{kind:?}"
        );
        let w = lab.world.lock().unwrap();
        // How each server wants the token, and what else is sent with it.
        let user = w
            .seen
            .iter()
            .find(|s| s.path.ends_with("/user") || s.path == "/user")
            .unwrap();
        match kind {
            Kind::GitHub => {
                assert_eq!(
                    user.auth.as_deref(),
                    Some(format!("Bearer {TOKEN}").as_str())
                );
                assert_eq!(user.api_version.as_deref(), Some("2022-11-28"));
                // A GitHub Enterprise Server was asked what it is first,
                // without the token (R-104).
                assert_eq!(w.seen[0].path, "/meta");
                assert_eq!(
                    w.seen[0].auth, None,
                    "no token before the server is known to be one"
                );
            }
            Kind::Gitea => {
                assert_eq!(
                    user.auth.as_deref(),
                    Some(format!("token {TOKEN}").as_str())
                );
                // The version was asked first, without the token.
                assert_eq!(w.seen[0].path, "/version");
                assert_eq!(
                    w.seen[0].auth, None,
                    "no token before the server is known to be one"
                );
            }
        }
        // The User-Agent names Columbus, not the person or the computer.
        assert!(
            w.seen.iter().all(|s| s.agent.starts_with("columbus-sync/")),
            "{:?}",
            w.seen[0].agent
        );
    }
}

#[tokio::test]
async fn a_wrong_token_is_said_plainly_and_never_repeated() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        let wrong = "zq7k-wrong-token-9876543210";
        let server = lab.server_with(wrong);
        let err = server.whoami().await.unwrap_err();
        assert!(matches!(err, Error::Token(_)), "{kind:?}: {err:?}");
        let said = err.to_string();
        assert!(said.contains("did not accept the token"), "{said}");
        assert!(said.contains("127.0.0.1"), "it names the server: {said}");
        assert!(
            !said.contains("zq7k") && !format!("{err:?}").contains("zq7k"),
            "{said}"
        );
        // The same for a sync: it stops at the first answer, and nothing
        // in the folder or the record changes.
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "first", "Cy")
        });
        let remote = server.remote("ana", "roads", "main").unwrap();
        let mut ana = Person::new("ana");
        ana.write("mine.md", "mine\n");
        let err = ana.try_sync(&remote, None).await.unwrap_err();
        assert!(matches!(err, Error::Token(_)), "{kind:?}: {err:?}");
        assert!(!err.to_string().contains("zq7k"));
        assert_eq!(ana.files().len(), 1);
        assert!(ana.state.files.is_empty());
        assert!(
            lab.world.lock().unwrap().writes().is_empty(),
            "nothing was sent"
        );
    }
}

/// GitHub Enterprise Server (R-104): the server is asked what it is
/// without the token (`GET /api/v3/meta`), as Gitea is asked its version,
/// so an address that is not a GitHub Enterprise Server's never receives
/// the token.
#[tokio::test]
async fn a_github_enterprise_server_is_asked_what_it_is_before_the_token_is_sent() {
    use std::sync::{Arc, Mutex};
    // A mistyped address: some other web server answers there. It notes
    // the Authorization header of everything it is sent.
    type Sent = Vec<(String, Option<String>)>;
    let sent: Arc<Mutex<Sent>> = Arc::default();
    let log = sent.clone();
    let other = http::serve(axum::Router::new().fallback(axum::routing::any(
        move |req: axum::extract::Request| {
            let log = log.clone();
            async move {
                let auth = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                log.lock()
                    .unwrap()
                    .push((req.uri().path().to_string(), auth));
                (
                    axum::http::StatusCode::NOT_FOUND,
                    "<html><body>Not found</body></html>",
                )
            }
        },
    )))
    .await;
    columbus_sync::remote::allow_loopback_http_for_tests();
    let address = Address::parse(Kind::GitHub, &other.url()).unwrap();
    let server = Server::new(&address, TOKEN, None).unwrap();
    let err = server.check().await.unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    let said = err.to_string();
    assert!(
        said.contains("did not answer as a GitHub Enterprise Server does")
            && said.contains("127.0.0.1")
            && said.contains("Your token was not sent"),
        "{said}"
    );
    assert!(!said.contains("zq7k"), "{said}");
    {
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0], ("/api/v3/meta".to_string(), None), "{sent:?}");
    }
    // The same for a Gitea addressed as a GitHub Enterprise Server.
    let gitea = Lab::new(Kind::Gitea).await;
    let address = Address::parse(Kind::GitHub, &gitea.running.url()).unwrap();
    let err = Server::new(&address, TOKEN, None)
        .unwrap()
        .check()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(gitea
        .world
        .lock()
        .unwrap()
        .seen
        .iter()
        .all(|s| s.auth.is_none()));

    // A GitHub Enterprise Server that answers /meta: known, then asked
    // whose token it is.
    let lab = Lab::new(Kind::GitHub).await;
    lab.server().check().await.unwrap();
    {
        let w = lab.world.lock().unwrap();
        assert_eq!(w.seen.len(), 1);
        assert_eq!((w.seen[0].path.as_str(), &w.seen[0].auth), ("/meta", &None));
    }
    // One that makes everyone sign in first (private mode) refuses /meta
    // without a token, and still names itself in the refusal: known too.
    lab.world.lock().unwrap().knobs.private_mode = true;
    let server = lab.server();
    server.check().await.unwrap();
    assert_eq!(server.whoami().await.unwrap().login, "ana");
    {
        let w = lab.world.lock().unwrap();
        let with_token: Vec<&str> = w
            .seen
            .iter()
            .filter(|s| s.auth.is_some())
            .map(|s| s.path.as_str())
            .collect();
        assert_eq!(with_token, ["/user"], "the token went with /user only");
    }
    // A wrong token is refused by the server it was meant for, after it
    // has said what it is.
    let err = lab
        .server_with("zq7k-wrong-token-9876543210")
        .whoami()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Token(_)), "{err:?}");
}

#[tokio::test]
async fn an_old_gitea_is_refused_by_its_version_before_the_token_is_sent() {
    let lab = Lab::new(Kind::Gitea).await;
    lab.world.lock().unwrap().knobs.version = "1.19.4".into();
    let err = lab.server().check().await.unwrap_err();
    assert!(matches!(err, Error::TooOld(_)), "{err:?}");
    let said = err.to_string();
    assert!(said.contains("1.19.4") && said.contains("1.20"), "{said}");
    {
        let w = lab.world.lock().unwrap();
        assert_eq!(w.seen.len(), 1);
        assert_eq!(w.seen[0].auth, None, "the token was not sent to it");
    }
    // A Forgejo says its own number and the Gitea it matches.
    lab.world.lock().unwrap().knobs.version = "9.0.3+gitea-1.22.0".into();
    lab.server().check().await.unwrap();
    // A server that was updated backwards after linking: the files call is
    // not there, and the sync says why.
    lab.repo("roads", |r| {
        r.commit("main", &[put("a.md", b"a\n")], "first", "Cy")
    });
    lab.world.lock().unwrap().knobs.version = "1.19.4".into();
    let mut ana = Person::new("ana");
    ana.write("mine.md", "mine\n");
    let err = ana.try_sync(&lab.remote("roads"), None).await.unwrap_err();
    assert!(matches!(err, Error::TooOld(_)), "{err:?}");
    // Something that is not a Gitea at all.
    let other = Lab::new(Kind::GitHub).await;
    let address = Address::parse(Kind::Gitea, &other.running.url()).unwrap();
    let err = Server::new(&address, TOKEN, None)
        .unwrap()
        .check()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
    assert!(other
        .world
        .lock()
        .unwrap()
        .seen
        .iter()
        .all(|s| s.auth.is_none()));
}

#[tokio::test]
async fn pick_a_repository_or_create_a_private_one() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        // More repositories than one page holds (GitHub 100, Gitea 50).
        let many = if kind == Kind::GitHub { 230 } else { 120 };
        for i in 0..many {
            lab.repo(&format!("repo-{i:03}"), |_| ());
        }
        if kind == Kind::Gitea {
            lab.world.lock().unwrap().knobs.page_max = 50;
        }
        let server = lab.server();
        let repos = server.repos().await.unwrap();
        assert_eq!(repos.len(), many, "{kind:?}: every page was read");
        assert_eq!(repos[0].name, "repo-000");
        assert!(repos
            .iter()
            .all(|r| r.owner == "ana" && r.private && r.can_send));
        let pages = lab
            .requests()
            .iter()
            .filter(|r| r.ends_with("/user/repos"))
            .count();
        assert!(pages >= 3, "{kind:?}: {pages} pages");

        let made = server.create_repo("roads-qc", "Roadway QC").await.unwrap();
        assert_eq!(
            (made.owner.as_str(), made.name.as_str()),
            ("ana", "roads-qc")
        );
        assert!(made.private, "a new repository is private");
        assert_eq!(made.default_branch, "main");
        {
            let w = lab.world.lock().unwrap();
            let repo = w.get("ana", "roads-qc").unwrap();
            assert!(
                repo.private && repo.is_empty(),
                "private, and nothing in it yet"
            );
            let sent = w.writes();
            assert_eq!(sent.len(), 1);
            assert!(
                sent[0].body.contains("\"private\":true")
                    && sent[0].body.contains("\"auto_init\":false"),
                "{}",
                sent[0].body
            );
        }
        assert!(server.branches("ana", "roads-qc").await.unwrap().is_empty());
        // The name is taken now.
        let err = server.create_repo("roads-qc", "").await.unwrap_err();
        assert!(matches!(err, Error::Refused(_)), "{kind:?}: {err:?}");
        // Names that are not names are refused before any request.
        let before = lab.requests().len();
        for bad in ["", "a/b", "../x", "x.git", "a b"] {
            assert!(server.create_repo(bad, "").await.is_err(), "{bad}");
            assert!(server.repo("ana", bad).await.is_err(), "{bad}");
        }
        assert_eq!(lab.requests().len(), before);
        // A repository that is not there, and one's branches.
        let err = server.repo("ana", "nope").await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        lab.repo("repo-000", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "first", "Cy");
            let head = r.head("main").unwrap().to_string();
            for i in 0..70 {
                r.branches.insert(format!("work/{i:02}"), head.clone());
            }
        });
        let branches = server.branches("ana", "repo-000").await.unwrap();
        assert_eq!(branches.len(), 71, "{kind:?}: every page of branches");
        assert!(
            branches.contains(&"main".to_string()) && branches.contains(&"work/69".to_string())
        );
        // A token that may not create.
        lab.world.lock().unwrap().knobs.read_only.push(TOKEN.into());
        let err = server.create_repo("another", "").await.unwrap_err();
        assert!(matches!(err, Error::Permission(_)), "{kind:?}: {err:?}");
    }
}

#[tokio::test]
async fn a_first_sync_into_an_empty_repository() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        let server = lab.server();
        server.create_repo("roads", "").await.unwrap();
        let remote = lab.remote("roads");
        assert_eq!(remote.tip().await.unwrap(), Tip::Empty, "{kind:?}");
        assert!(remote.history(10).await.unwrap().is_empty());

        let mut ana = Person::new("ana");
        ana.write(
            "columbus-project.yaml",
            "name: Roads\ninstructions: Miles, two decimals.\n",
        );
        ana.write("scripts/widths.py", "print('widths')\n");
        ana.write("notes/ünï cödé.md", "# Notes\n");
        ana.write("logo.bin", [0u8, 159, 146, 150, 0, 255]);
        let c = ana.check(&remote).await;
        assert_eq!(c.commit, None);
        assert_eq!(c.here.len(), 4);
        let rep = ana.sync_with(&remote, "First version").await;
        assert_eq!(rep.sent.len(), 4, "{kind:?}: {rep:?}");
        assert!(rep.sent.iter().all(|i| i.what == What::New));
        {
            let w = lab.world.lock().unwrap();
            let repo = w.get("ana", "roads").unwrap();
            assert_eq!(
                repo.files("main"),
                ana.files(),
                "{kind:?}: the server holds exactly the folder"
            );
            let log = repo.log("main");
            assert!(log
                .iter()
                .all(|(_, c)| c.message == "First version" && c.who == "Ana Example"));
            match kind {
                // GitHub's Git database answers 409 until a first file
                // exists, which goes through the contents API; the rest is
                // one commit.
                Kind::GitHub => {
                    assert_eq!(log.len(), 2);
                    let first = repo.tree_at(&log[1].0);
                    assert_eq!(
                        first.keys().collect::<Vec<_>>(),
                        ["columbus-project.yaml"],
                        "the shared settings first"
                    );
                    let puts: Vec<&http::Seen> =
                        w.seen.iter().filter(|s| s.method == "PUT").collect();
                    assert_eq!(puts.len(), 1);
                    assert_eq!(
                        puts[0].path,
                        "/repos/ana/roads/contents/columbus-project.yaml"
                    );
                }
                // Gitea takes every file in one call, also into an empty
                // repository.
                Kind::Gitea => {
                    assert_eq!(log.len(), 1);
                    let posts: Vec<&http::Seen> = w
                        .seen
                        .iter()
                        .filter(|s| s.method == "POST" && s.path.ends_with("/contents"))
                        .collect();
                    assert_eq!(posts.len(), 1);
                }
            }
            assert_eq!(rep.commit.as_deref(), Some(log[0].0.as_str()));
        }
        assert_eq!(ana.state.files.len(), 4);
        // Nothing left to do, and nothing more is sent.
        let c = ana.check(&remote).await;
        assert!(
            c.here.is_empty() && c.server.is_empty() && c.both.is_empty(),
            "{kind:?}: {c:?}"
        );
        let writes = lab.world.lock().unwrap().writes().len();
        let rep = ana.sync(&remote).await;
        assert!(rep.sent.is_empty());
        assert_eq!(lab.world.lock().unwrap().writes().len(), writes);

        // A colleague links an empty folder and gets it all, byte for byte.
        let mut ben = Person::new("ben");
        let rep = ben.sync(&remote).await;
        assert_eq!(rep.got.len(), 4);
        assert_eq!(ben.files(), ana.files());
        assert_eq!(
            lab.world.lock().unwrap().writes().len(),
            writes,
            "getting sends nothing"
        );
        // History: who, when, the note.
        let history = remote.history(10).await.unwrap();
        assert_eq!(history[0].who, "Ana Example");
        assert_eq!(history[0].note, "First version");
        assert!(
            history[0].when.starts_with("2026-11-"),
            "{}",
            history[0].when
        );
    }
}

#[tokio::test]
async fn a_single_first_file_is_one_commit_on_both() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.server().create_repo("one", "").await.unwrap();
        let remote = lab.remote("one");
        let mut ana = Person::new("ana");
        ana.write("only.md", "only\n");
        let rep = ana.sync(&remote).await;
        assert_eq!(rep.sent.len(), 1);
        let w = lab.world.lock().unwrap();
        let repo = w.get("ana", "one").unwrap();
        assert_eq!(repo.log("main").len(), 1, "{kind:?}");
        assert_eq!(repo.log("main")[0].1.message, "Add only.md");
        assert_eq!(repo.files("main"), ana.files());
    }
}

#[tokio::test]
async fn a_normal_sync_gets_and_sends_and_keeps_both_on_a_conflict() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit(
                "main",
                &[
                    put("a.md", b"a\n"),
                    put("b.md", b"b\n"),
                    put("old.md", b"old\n"),
                    put("dir/deep/c.md", b"c\n"),
                ],
                "start",
                "Cy",
            )
        });
        let remote = lab.remote("roads");
        let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
        ana.sync(&remote).await;
        ben.sync(&remote).await;
        assert_eq!(ana.files().len(), 4);

        // Ana: one changed, one new, one deleted, one large text file, one
        // file that is not text. One commit.
        let commits = lab.repo("roads", |r| r.log("main").len());
        ana.write("a.md", "a, changed\n");
        ana.write("new/file.md", "new\n");
        ana.delete("old.md");
        ana.write("long.txt", "0123456789abcdef\n".repeat(20_000));
        ana.write(
            "data.bin",
            (0..=255u8).cycle().take(3000).collect::<Vec<u8>>(),
        );
        let rep = ana.sync_with(&remote, "Ana's round").await;
        assert_eq!(rep.sent.len(), 5, "{kind:?}: {rep:?}");
        lab.repo("roads", |r| {
            let log = r.log("main");
            assert_eq!(
                log.len(),
                commits + 1,
                "{kind:?}: one commit for five files"
            );
            assert_eq!(log[0].1.message, "Ana's round");
            assert_eq!(r.files("main"), ana.files());
        });

        // Ben: gets all of it, sends his own change, and a file both
        // changed keeps both versions.
        ben.write("b.md", "b, by Ben\n");
        ben.write("a.md", "a, by Ben\n");
        let c = ben.check(&remote).await;
        assert_eq!(
            c.here.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            ["b.md"]
        );
        assert_eq!(
            c.both.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            ["a.md"]
        );
        assert_eq!(c.server.len(), 4, "{kind:?}: {:?}", c.server);
        let rep = ben.sync(&remote).await;
        assert_eq!(rep.got, ["data.bin", "long.txt", "new/file.md"]);
        assert_eq!(rep.removed.len(), 1);
        assert_eq!(rep.conflicts.len(), 1);
        assert_eq!(
            rep.sent.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            ["b.md"]
        );
        let copy = format!("a (my copy {TODAY}).md");
        assert_eq!(ben.text("a.md"), "a, changed\n");
        assert_eq!(ben.text(&copy), "a, by Ben\n");
        assert_eq!(ben.read("long.txt"), ana.read("long.txt"));
        assert_eq!(ben.read("data.bin"), ana.read("data.bin"));
        lab.repo("roads", |r| {
            let files = r.files("main");
            assert_eq!(
                files["a.md"], b"a, changed\n",
                "{kind:?}: Ana's was not overwritten"
            );
            assert!(!files.contains_key(&copy));
        });
        // Keep mine, and it travels.
        ben.resolve("a.md", Some(&copy), Choice::Mine);
        ben.sync(&remote).await;
        ana.sync(&remote).await;
        assert_eq!(ana.text("a.md"), "a, by Ben\n");
        assert_eq!(ana.files(), ben.files());
        lab.repo("roads", |r| assert_eq!(r.files("main"), ana.files()));
        assert_eq!(remote.history(3).await.unwrap().len(), 3);
    }
}

#[tokio::test]
async fn a_branch_that_moved_during_the_sync_is_read_again_and_never_forced() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit(
                "main",
                &[put("a.md", b"a\n"), put("shared.md", b"shared\n")],
                "start",
                "Cy",
            )
        });
        let remote = lab.remote("roads");
        let mut ana = Person::new("ana");
        ana.sync(&remote).await;

        // While Ana's sync is under way, a colleague's commit lands: a new
        // file, and a change to a file Ana is sending too.
        ana.write("mine.md", "Ana's\n");
        ana.write("shared.md", "Ana's shared\n");
        lab.land_during_next_send(
            "roads",
            vec![
                put("theirs.md", b"Cy's\n"),
                put("shared.md", b"Cy's shared\n"),
            ],
        );
        let rep = ana.sync(&remote).await;
        assert_eq!(rep.rounds, 1, "{kind:?}: read again once");
        assert_eq!(
            rep.sent.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            ["mine.md"]
        );
        assert_eq!(rep.conflicts.len(), 1);
        assert_eq!(ana.text("theirs.md"), "Cy's\n");
        assert_eq!(ana.text("shared.md"), "Cy's shared\n");
        assert_eq!(
            ana.text(&format!("shared (my copy {TODAY}).md")),
            "Ana's shared\n"
        );
        lab.repo("roads", |r| {
            let log = r.log("main");
            assert_eq!(log[1].1.message, "Cy, meanwhile");
            assert_eq!(
                log[0].1.parents,
                [log[1].0.clone()],
                "{kind:?}: on top of it, not in place of it"
            );
            let files = r.files("main");
            assert_eq!(
                files["shared.md"], b"Cy's shared\n",
                "Cy's change was not overwritten"
            );
            assert_eq!(files["mine.md"], b"Ana's\n");
            // Every commit of Cy's is in the branch's one line of history.
            let cys = log.iter().filter(|(_, c)| c.who == "Cy").count();
            assert_eq!(
                cys,
                r.commits.values().filter(|c| c.who == "Cy").count(),
                "{kind:?}: no commit was pushed aside"
            );
        });

        // A colleague's commit that touches other files only.
        ana.write("mine.md", "Ana's, again\n");
        lab.land_during_next_send("roads", vec![put("more.md", b"more of Cy's\n")]);
        let rep = ana.sync(&remote).await;
        assert_eq!(rep.sent.len(), 1);
        match kind {
            // GitHub refuses a branch update that is not a fast-forward,
            // so the sync reads again.
            Kind::GitHub => assert_eq!(rep.rounds, 1),
            // Gitea checks each file sent against the version read: no
            // file of Ana's changed underneath her, so it takes her commit
            // on top of Cy's.
            Kind::Gitea => assert_eq!(rep.rounds, 0),
        }
        lab.repo("roads", |r| {
            let files = r.files("main");
            assert_eq!(files["more.md"], b"more of Cy's\n");
            assert_eq!(files["mine.md"], b"Ana's, again\n");
            let cys = r.log("main").iter().filter(|(_, c)| c.who == "Cy").count();
            assert_eq!(cys, r.commits.values().filter(|c| c.who == "Cy").count());
        });
        // Either way Ana has Cy's file after her next sync at the latest.
        ana.sync(&remote).await;
        assert_eq!(ana.text("more.md"), "more of Cy's\n");
        lab.repo("roads", |r| {
            let mine: std::collections::BTreeMap<_, _> = ana
                .files()
                .into_iter()
                .filter(|(p, _)| !p.contains("my copy"))
                .collect();
            assert_eq!(r.files("main"), mine);
        });

        // Columbus never asked either server to force anything.
        let w = lab.world.lock().unwrap();
        for s in w.writes() {
            assert!(!s.body.contains("\"force\":true"), "{}", s.body);
            assert!(
                !s.body.contains("force_push") && !s.body.contains("new_branch"),
                "{}",
                s.body
            );
        }
        if kind == Kind::GitHub {
            let moves: Vec<&http::Seen> = w.seen.iter().filter(|s| s.method == "PATCH").collect();
            assert_eq!(
                moves.len(),
                4,
                "two tries for each of the two syncs that sent"
            );
            assert!(
                moves.iter().all(|s| s.body.contains("\"force\":false")),
                "always said, never left to a default"
            );
        }
    }
}

#[tokio::test]
async fn a_github_branch_that_never_stops_moving_ends_the_sync_with_nothing_forced() {
    let lab = Lab::new(Kind::GitHub).await;
    lab.repo("roads", |r| {
        r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
    });
    let remote = lab.remote("roads");
    let mut ana = Person::new("ana");
    ana.sync(&remote).await;
    ana.write("mine.md", "Ana's\n");
    for i in 0..10 {
        lab.land_during_next_send("roads", vec![put(&format!("busy-{i}.md"), b"busy\n")]);
    }
    let err = ana.try_sync(&remote, None).await.unwrap_err();
    assert!(matches!(err, Error::Busy(_)), "{err:?}");
    assert!(err.to_string().contains("Nothing was overwritten"), "{err}");
    lab.repo("roads", |r| {
        assert!(
            !r.files("main").contains_key("mine.md"),
            "nothing of Ana's landed"
        );
        // Every commit of Cy's is still in the branch's history.
        let in_history = r.log("main").iter().filter(|(_, c)| c.who == "Cy").count();
        assert_eq!(
            in_history,
            r.commits.values().filter(|c| c.who == "Cy").count()
        );
        assert_eq!(in_history, 1 + columbus_sync::engine::MAX_ROUNDS as usize);
    });
    assert_eq!(ana.text("mine.md"), "Ana's\n", "and her file is still hers");
    // What the server had was fetched meanwhile.
    assert_eq!(ana.text("busy-9.md"), "busy\n");
    // Once it is quiet, the same sync goes through.
    lab.world.lock().unwrap().knobs.before_landing.clear();
    let rep = ana.sync(&remote).await;
    assert_eq!(rep.sent.len(), 1);
    lab.repo("roads", |r| {
        assert_eq!(r.files("main")["mine.md"], b"Ana's\n")
    });
    let w = lab.world.lock().unwrap();
    assert!(w
        .writes()
        .iter()
        .all(|s| !s.body.contains("\"force\":true")));
}

#[tokio::test]
async fn a_server_that_redirects_to_another_host_gets_no_further_and_the_token_goes_nowhere_else() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        let elsewhere = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
        });
        elsewhere.repo("roads", |r| {
            r.commit(
                "main",
                &[put("evil.md", b"from elsewhere\n")],
                "start",
                "Mallory",
            )
        });
        // "localhost" and "127.0.0.1" are different hosts to a client, and
        // the other server is on another port besides.
        let other = format!(
            "http://localhost:{}{}",
            elsewhere.running.addr.port(),
            if kind == Kind::GitHub {
                "/api/v3"
            } else {
                "/api/v1"
            }
        );
        lab.world.lock().unwrap().knobs.redirect_to = Some(other);

        let server = lab.server();
        let err = server.whoami().await.unwrap_err();
        assert!(matches!(err, Error::Redirected(_)), "{kind:?}: {err:?}");
        let said = err.to_string();
        assert!(
            said.contains("localhost") && said.contains("does not follow"),
            "{said}"
        );
        assert!(!said.contains("zq7k"));
        let mut ana = Person::new("ana");
        ana.write("mine.md", "mine\n");
        let err = ana.try_sync(&lab.remote("roads"), None).await.unwrap_err();
        assert!(matches!(err, Error::Redirected(_)), "{kind:?}: {err:?}");
        assert!(ana.read("evil.md").is_none());
        // The other host was never asked anything, so it never saw the
        // token (or anything else).
        let seen = elsewhere.world.lock().unwrap().seen.len();
        assert_eq!(seen, 0, "{kind:?}: the other host got {seen} requests");
    }
}

#[tokio::test]
async fn a_redirect_on_the_same_host_is_followed_with_the_token() {
    // A repository that was renamed: GitHub answers with a redirect to its
    // new address, on the same host.
    let lab = Lab::new(Kind::GitHub).await;
    lab.repo("new-name", |r| {
        r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
    });
    let app = axum::Router::new()
        .route(
            "/api/v3/repos/ana/old-name",
            axum::routing::get(|| async {
                (
                    axum::http::StatusCode::MOVED_PERMANENTLY,
                    [("location", "/api/v3/repos/ana/new-name")],
                )
            }),
        )
        .merge(support::github::enterprise(lab.world.clone()));
    let running = http::serve(app).await;
    let address = Address::parse(Kind::GitHub, &running.url()).unwrap();
    let server = Server::new(&address, TOKEN, None).unwrap();
    let info = server.repo("ana", "old-name").await.unwrap();
    assert_eq!(info.name, "new-name");
    let w = lab.world.lock().unwrap();
    assert_eq!(w.seen.len(), 1);
    assert_eq!(
        w.seen[0].auth.as_deref(),
        Some(format!("Bearer {TOKEN}").as_str()),
        "same host: the token goes along"
    );
}

#[tokio::test]
async fn fifty_items_a_page_and_lists_of_files_longer_than_a_page() {
    // Gitea gives 50 items a page at most, and a list of files a page at a
    // time; GitHub cuts a list of files off beyond a limit.
    let lab = Lab::new(Kind::Gitea).await;
    {
        let mut w = lab.world.lock().unwrap();
        w.knobs.page_max = 50;
        w.knobs.tree_page_max = 50;
    }
    let edits: Vec<_> = (0..180)
        .map(|i| put(&format!("f/{:03}.md", i), format!("{i}\n").as_bytes()))
        .collect();
    lab.repo("many", |r| r.commit("main", &edits, "many files", "Cy"));
    let remote = lab.remote("many");
    let tip = remote.tip().await.unwrap();
    let tree = remote.tree(tip.commit().unwrap()).await.unwrap();
    assert_eq!(tree.len(), 180, "every page of the list of files");
    let pages = lab
        .requests()
        .iter()
        .filter(|r| r.contains("/git/trees/"))
        .count();
    assert!(
        pages >= 4,
        "{pages} pages (180 files and their folder, 50 a page)"
    );
    let mut ana = Person::new("ana");
    let rep = ana.sync(&remote).await;
    assert_eq!(rep.got.len(), 180);
    assert_eq!(lab.repo("many", |r| r.files("main")), ana.files());
    // History, too, is asked for within the limit.
    for i in 0..60 {
        lab.repo("many", |r| {
            r.commit(
                "main",
                &[put("f/000.md", format!("v{i}\n").as_bytes())],
                &format!("change {i}"),
                "Cy",
            )
        });
    }
    let history = remote.history(30).await.unwrap();
    assert_eq!(history.len(), 30);
    assert_eq!(history[0].note, "change 59");

    // GitHub: a list of files that was cut off cannot be synced with, and
    // nothing is taken for deleted because it was not listed.
    let lab = Lab::new(Kind::GitHub).await;
    lab.repo("huge", |r| r.commit("main", &edits, "many files", "Cy"));
    let remote = lab.remote("huge");
    let mut ben = Person::new("ben");
    ben.sync(&remote).await;
    assert_eq!(ben.files().len(), 180);
    lab.world.lock().unwrap().knobs.tree_limit = 100;
    let err = ben.try_sync(&remote, None).await.unwrap_err();
    assert!(matches!(err, Error::Refused(_)), "{err:?}");
    assert!(
        err.to_string()
            .contains("more files than GitHub lists at once"),
        "{err}"
    );
    assert_eq!(ben.files().len(), 180, "nothing was removed");
    assert!(lab.world.lock().unwrap().writes().is_empty());
}

#[tokio::test]
async fn a_server_that_asks_to_wait_is_said_plainly() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
        });
        lab.world.lock().unwrap().knobs.rate_limit_next = true;
        let mut ana = Person::new("ana");
        let err = ana.try_sync(&lab.remote("roads"), None).await.unwrap_err();
        assert!(matches!(err, Error::RateLimited(_)), "{kind:?}: {err:?}");
        assert!(err.to_string().contains("asks Columbus to wait"), "{err}");
        // The next try goes through.
        ana.sync(&lab.remote("roads")).await;
        assert_eq!(ana.text("a.md"), "a\n");
    }
}

#[tokio::test]
async fn a_server_that_does_not_answer_is_given_up_on_in_plain_words() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
        });
        // It answers, but far too slowly.
        lab.world.lock().unwrap().knobs.delay_ms = 3_000;
        let server = lab
            .server()
            .with_timeout(std::time::Duration::from_millis(300));
        let started = std::time::Instant::now();
        let err = server.whoami().await.unwrap_err();
        assert!(matches!(err, Error::Network(_)), "{kind:?}: {err:?}");
        assert!(err.to_string().contains("did not answer within"), "{err}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "it did not wait for the answer"
        );
        let mut ana = Person::new("ana");
        ana.write("mine.md", "mine\n");
        let remote = server.remote("ana", "roads", "main").unwrap();
        let err = ana.try_sync(&remote, None).await.unwrap_err();
        assert!(matches!(err, Error::Network(_)), "{kind:?}: {err:?}");
        assert!(ana.state.files.is_empty());

        // Nothing listens at the address at all.
        let gone = format!("http://127.0.0.1:{}", free_port());
        let address = Address::parse(kind, &gone).unwrap();
        let err = Server::new(&address, TOKEN, None)
            .unwrap()
            .whoami()
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Network(_)), "{kind:?}: {err:?}");
        assert!(
            err.to_string().contains("could not reach 127.0.0.1"),
            "{err}"
        );
        assert!(!err.to_string().contains("zq7k"));
    }
}

/// A port nothing listens on (one that was free a moment ago).
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

#[tokio::test]
async fn a_repository_made_anew_takes_nothing_away_from_the_folder() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.server().create_repo("roads", "").await.unwrap();
        let remote = lab.remote("roads");
        let mut ana = Person::new("ana");
        ana.write("a.md", "a\n");
        ana.write("b.md", "b\n");
        ana.sync(&remote).await;
        assert_eq!(ana.state.files.len(), 2);
        // The repository is deleted on the server and made again, empty,
        // under the same name.
        {
            let mut w = lab.world.lock().unwrap();
            w.repos.remove(&("ana".to_string(), "roads".to_string()));
        }
        lab.server().create_repo("roads", "").await.unwrap();
        // Nothing here is taken for "deleted on the server".
        let c = ana.check(&remote).await;
        assert!(c.server.is_empty() && c.both.is_empty(), "{kind:?}: {c:?}");
        assert_eq!(c.here.len(), 2);
        assert!(c.here.iter().all(|i| i.what == What::New));
        let rep = ana.sync(&remote).await;
        assert!(rep.removed.is_empty());
        assert_eq!(rep.sent.len(), 2);
        assert_eq!(ana.files().len(), 2);
        lab.repo("roads", |r| assert_eq!(r.files("main"), ana.files()));
    }
}

#[tokio::test]
async fn a_token_that_may_not_send_is_said_plainly_and_changes_nothing() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
        });
        lab.world.lock().unwrap().knobs.read_only.push(TOKEN.into());
        let remote = lab.remote("roads");
        let mut ana = Person::new("ana");
        // Getting works with a token that only reads.
        ana.sync(&remote).await;
        ana.write("mine.md", "mine\n");
        let err = ana.try_sync(&remote, None).await.unwrap_err();
        assert!(matches!(err, Error::Permission(_)), "{kind:?}: {err:?}");
        assert!(err.to_string().contains("not allowed"), "{err}");
        lab.repo("roads", |r| assert_eq!(r.files("main").len(), 1));
        assert_eq!(ana.text("mine.md"), "mine\n");
    }
}

#[tokio::test]
async fn a_missing_branch_or_repository_is_said_plainly() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
        });
        let server = lab.server();
        let gone = server.remote("ana", "roads", "release/2027").unwrap();
        let err = gone.tip().await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{kind:?}: {err:?}");
        assert!(err.to_string().contains("release/2027"), "{err}");
        let none = server.remote("ana", "nothing-here", "main").unwrap();
        let err = none.tip().await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{kind:?}: {err:?}");
        // A branch with a slash in its name is found.
        lab.repo("roads", |r| {
            let head = r.head("main").unwrap().to_string();
            r.branches.insert("release/2027".into(), head);
        });
        assert!(matches!(gone.tip().await.unwrap(), Tip::At(_)));
        let mut ana = Person::new("ana");
        ana.sync(&gone).await;
        ana.write("b.md", "b\n");
        ana.sync(&gone).await;
        lab.repo("roads", |r| {
            assert_eq!(r.files("release/2027").len(), 2);
            assert_eq!(r.files("main").len(), 1, "only the linked branch");
        });
    }
}

#[tokio::test]
async fn a_gitea_repository_with_sha256_ids() {
    let lab = Lab::new(Kind::Gitea).await;
    lab.world.lock().unwrap().format = ObjectFormat::Sha256;
    lab.server().create_repo("roads", "").await.unwrap();
    let remote = lab.remote("roads");
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("a.md", "one\n");
    let rep = ana.sync(&remote).await;
    assert_eq!(rep.commit.unwrap().len(), 64);
    assert_eq!(ana.state.files["a.md"].blob.len(), 64);
    ben.write("a.md", "one\n");
    let c = ben.check(&remote).await;
    assert!(
        c.both.is_empty(),
        "the same file is recognized by its 64-digit id"
    );
    ben.sync(&remote).await;
    ben.write("a.md", "two\n");
    ben.sync(&remote).await;
    ana.sync(&remote).await;
    assert_eq!(ana.text("a.md"), "two\n");
}

#[tokio::test]
async fn a_server_that_sends_something_other_than_it_listed_is_not_believed() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"what was listed\n")], "start", "Cy");
            // The server (or something between) hands out other bytes
            // under the listed id.
            let id = r.tree_at(r.head("main").unwrap())["a.md"].id.clone();
            r.blobs.insert(id, b"something else\n".to_vec());
        });
        let mut ana = Person::new("ana");
        let err = ana.try_sync(&lab.remote("roads"), None).await.unwrap_err();
        assert!(matches!(err, Error::Server(_)), "{kind:?}: {err:?}");
        assert!(err.to_string().contains("other than it listed"), "{err}");
        assert!(ana.files().is_empty(), "nothing was written");
    }
}

/// Two people working through the real adapters for many rounds, with a
/// colleague's commits landing mid-sync: nothing a folder held is lost,
/// and in the end both folders equal the repository.
#[tokio::test]
async fn two_people_through_each_servers_api() {
    for kind in KINDS {
        for seed in [11u64, 12] {
            let lab = Lab::new(kind).await;
            lab.server().create_repo("shared", "").await.unwrap();
            let remote = lab.remote("shared");
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let names = [
                "a.md",
                "b.md",
                "notes/c.md",
                "scripts/d.py",
                "deep/er/e.txt",
            ];
            let mut people = [Person::new("Ana"), Person::new("Ben")];
            let mut version = 0;
            for round in 0..25 {
                for p in people.iter_mut() {
                    for _ in 0..rng.below(3) {
                        version += 1;
                        let name = *rng.pick(&names);
                        if rng.chance(20) && p.read(name).is_some() {
                            p.delete(name);
                        } else {
                            p.write(
                                name,
                                format!("{} wrote version {version}\nsecond line\n", p.name),
                            );
                        }
                    }
                    if rng.chance(30) {
                        for c in p.state.conflicts.clone() {
                            p.resolve(
                                &c.path,
                                c.copy.as_deref(),
                                *rng.pick(&[Choice::Mine, Choice::Theirs, Choice::Both]),
                            );
                        }
                    }
                    if rng.chance(75) {
                        if rng.chance(30) && !lab.repo("shared", |r| r.is_empty()) {
                            version += 1;
                            let name = *rng.pick(&names);
                            lab.land_during_next_send(
                                "shared",
                                vec![put(
                                    name,
                                    format!("Cy wrote version {version}\n").as_bytes(),
                                )],
                            );
                        }
                        let before: Vec<String> =
                            p.files().values().map(|b| content_hash(b)).collect();
                        match p.try_sync(&remote, None).await {
                            Ok(_) | Err(Error::Busy(_)) => {}
                            Err(e) => panic!("{kind:?}, seed {seed}, round {round}: {e}"),
                        }
                        lab.world.lock().unwrap().knobs.before_landing.clear();
                        // Nothing the folder held is lost.
                        let here: BTreeSet<String> =
                            p.files().values().map(|b| content_hash(b)).collect();
                        let kept: BTreeSet<String> =
                            p.kept_files().iter().map(|b| content_hash(b)).collect();
                        let there: BTreeSet<String> = lab.repo("shared", |r| {
                            r.blobs.values().map(|b| content_hash(b)).collect()
                        });
                        for v in before {
                            assert!(
                                here.contains(&v) || kept.contains(&v) || there.contains(&v),
                                "{kind:?}, seed {seed}, round {round}: a version {} had is nowhere",
                                p.name
                            );
                        }
                    }
                }
            }
            // Settle, then both sync twice.
            for attempt in 0..30 {
                for p in people.iter_mut() {
                    for c in p.state.conflicts.clone() {
                        let choice = if attempt >= 4 {
                            Choice::Theirs
                        } else {
                            *rng.pick(&[Choice::Mine, Choice::Theirs, Choice::Both])
                        };
                        p.resolve(&c.path, c.copy.as_deref(), choice);
                    }
                    p.sync(&remote).await;
                }
                if people.iter().all(|p| p.state.conflicts.is_empty()) {
                    break;
                }
            }
            for _ in 0..2 {
                for p in people.iter_mut() {
                    let rep = p.sync(&remote).await;
                    assert!(rep.conflicts.is_empty());
                }
            }
            let (a, b) = (people[0].files(), people[1].files());
            assert_eq!(a, b, "{kind:?}, seed {seed}: the two folders differ");
            lab.repo("shared", |r| {
                assert_eq!(
                    r.files("main"),
                    a,
                    "{kind:?}, seed {seed}: the folders and the repository differ"
                );
                let cys = r.log("main").iter().filter(|(_, c)| c.who == "Cy").count();
                assert_eq!(
                    cys,
                    r.commits.values().filter(|c| c.who == "Cy").count(),
                    "{kind:?}, seed {seed}: a commit of Cy's was pushed aside"
                );
            });
            assert!(a.keys().all(|p| paths::refused(p).is_none()));
            let w = lab.world.lock().unwrap();
            assert!(w
                .writes()
                .iter()
                .all(|s| !s.body.contains("\"force\":true") && !s.body.contains("force_push")));
            // Every request went with the token, to this server.
            assert!(w
                .seen
                .iter()
                .all(|s| s.auth.is_some() || s.path == "/version"));
        }
    }
}

#[tokio::test]
async fn the_token_is_in_no_error_no_report_and_no_record() {
    for kind in KINDS {
        let lab = Lab::new(kind).await;
        lab.repo("roads", |r| {
            r.commit("main", &[put("a.md", b"a\n")], "start", "Cy")
        });
        let remote = lab.remote("roads");
        let mut ana = Person::new("ana");
        ana.write("mine.md", "mine\n");
        let rep = ana.sync(&remote).await;
        let said = format!(
            "{rep:?} {} {:?}",
            serde_json::to_string(&rep).unwrap(),
            ana.state
        );
        assert!(!said.contains("zq7k"), "{said}");
        // A server that repeats the token in its error message.
        let echo = axum::Router::new().fallback(|req: axum::extract::Request| async move {
            let auth = req
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            (
                axum::http::StatusCode::IM_A_TEAPOT,
                axum::Json(serde_json::json!({"message": format!("cannot brew for {auth}")})),
            )
        });
        let running = http::serve(echo).await;
        let address = Address::parse(kind, &running.url()).unwrap();
        let err = Server::new(&address, TOKEN, None)
            .unwrap()
            .whoami()
            .await
            .unwrap_err();
        let said = format!("{err} {err:?}");
        assert!(said.contains("cannot brew"), "{said}");
        assert!(
            !said.contains("zq7k"),
            "the token was taken out of what the server said: {said}"
        );
        assert!(said.contains("[token]"));
    }
}

/// There is no switch to skip the certificate check, and none can slip in
/// unnoticed: no source file of the crate names the calls that would make
/// one, or anything that forces a branch.
#[test]
fn no_source_file_can_skip_the_certificate_check_or_force_a_branch() {
    let sources = [
        ("lib.rs", include_str!("../src/lib.rs")),
        ("engine.rs", include_str!("../src/engine.rs")),
        ("remote/mod.rs", include_str!("../src/remote/mod.rs")),
        ("remote/http.rs", include_str!("../src/remote/http.rs")),
        ("remote/github.rs", include_str!("../src/remote/github.rs")),
        ("remote/gitea.rs", include_str!("../src/remote/gitea.rs")),
    ];
    for (name, text) in sources {
        for banned in [
            "danger_accept",
            "tls_danger",
            "accept_invalid",
            "\"force\": true",
            "\"force_push\"",
            "\"new_branch\"",
        ] {
            assert!(!text.contains(banned), "{name} has {banned}");
        }
    }
    // The one place a branch is moved says "do not force", in so many words.
    assert!(include_str!("../src/remote/github.rs").contains("\"force\": false"));
}
