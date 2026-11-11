//! What never syncs, and what stops a sync (T-32's third check): the
//! ignore list, the 5 MB cap, text that looks like a token or a private
//! key, the paths F-17 refuses, links, submodules and Git LFS. (The
//! conflict rule and line endings are in `engine.rs`.)

mod support;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use columbus_sync::hash::ObjectFormat;
use columbus_sync::paths::MAX_FILE;
use columbus_sync::{Options, Skipped};
use support::{put, Edit, MemoryHost, Person, Repo, TODAY};

fn repo() -> Arc<Mutex<Repo>> {
    Arc::new(Mutex::new(Repo::new(ObjectFormat::Sha1)))
}

/// Why `path` does not sync, as the report says (every reason given).
fn why(skipped: &[Skipped], path: &str) -> String {
    let said: Vec<&str> = skipped
        .iter()
        .filter(|s| s.path == path)
        .map(|s| s.why.as_str())
        .collect();
    assert!(!said.is_empty(), "{path} is not named among {skipped:?}");
    said.join("; ")
}

#[tokio::test]
async fn data_secrets_and_outputs_in_the_folder_are_skipped_and_named() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    ana.write("scripts/widths.py", "print(1)\n");
    for (path, _) in IGNORED {
        ana.write(path, "x");
    }
    let c = ana.check(&host).await;
    assert_eq!(c.here.len(), 1, "{:?}", c.here);
    for (path, word) in IGNORED {
        // A folder on the list is named once, not file by file.
        let named = match path.split_once('/') {
            Some((folder, _)) if c.skipped.iter().any(|s| s.path == format!("{folder}/")) => {
                format!("{folder}/")
            }
            _ => path.to_string(),
        };
        let w = why(&c.skipped, &named);
        assert!(w.contains(word), "{path}: {w}");
    }
    let rep = ana.sync(&host).await;
    assert_eq!(rep.sent.len(), 1);
    assert_eq!(rep.skipped, c.skipped);
    let on_server = r.lock().unwrap().files("main");
    assert_eq!(on_server.keys().collect::<Vec<_>>(), ["scripts/widths.py"]);
    // They are all still in the folder.
    for (path, _) in IGNORED {
        assert!(ana.read(path).is_some(), "{path}");
    }
}

const IGNORED: &[(&str, &str)] = &[
    (".env", ".env"),
    (".env.production", ".env"),
    ("server.pem", "keys"),
    ("id.key", "keys"),
    ("cert.pfx", "keys"),
    ("store.p12", "keys"),
    ("prod.sde", "connection"),
    ("gis.ags", "connection"),
    ("__pycache__/widths.cpython-312.pyc", "__pycache__"),
    (".venv/pyvenv.cfg", ".venv"),
    ("roads.gdb/a00000001.gdbtable", "geodatabase"),
    ("parcels.gpkg", "geodata"),
    ("roads.shp", "geodata"),
    ("roads.shx", "geodata"),
    ("roads.dbf", "geodata"),
    ("roads.prj", "geodata"),
    ("roads.cpg", "geodata"),
    ("roads.sbn", "geodata"),
    ("roads.sbx", "geodata"),
    ("dem.tif", "geodata"),
    ("dem2.tiff", "geodata"),
    ("cloud.las", "geodata"),
    ("cloud.laz", "geodata"),
    ("bundle.zip", "archives"),
    ("outputs/report.csv", "outputs"),
    // R-103: data formats Columbus reads as layers or tables.
    ("codes.csv", "tables"),
    ("codes.tsv", "tables"),
    ("book.xlsx", "tables"),
    ("old.xls", "tables"),
    ("big.parquet", "tables"),
    ("cache.sqlite", "tables"),
    ("cache.duckdb", "tables"),
    ("access.mdb", "tables"),
    ("roads.geojson", "geodata"),
    ("roads.fgb", "geodata"),
    ("places.kml", "geodata"),
    ("places.kmz", "geodata"),
    ("track.gpx", "geodata"),
    ("parcels.gml", "geodata"),
    // R-103: the names the assistant is kept from.
    (".pgpass", "password"),
    (".netrc", "password"),
    ("aws/credentials", "password"),
    ("id_rsa", "keys"),
    ("store.jks", "keys"),
    ("vault.kdbx", "keys"),
    ("putty.ppk", "keys"),
    ("prod.dsn", "connection"),
    ("prod.udl", "connection"),
    ("cube.odc", "connection"),
    // The kinds the assistant never writes because they run when opened.
    ("open me.bat", "runs or follows"),
];

#[tokio::test]
async fn what_is_on_the_ignore_list_on_the_server_is_never_fetched() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ben");
    // A colleague with real Git committed data and a key.
    let edits: Vec<Edit> = IGNORED
        .iter()
        .map(|(p, _)| put(p, b"from the server"))
        .chain([put("notes.md", b"notes\n")])
        .collect();
    r.lock().unwrap().commit("main", &edits, "everything", "Cy");
    let mut ben = Person::new("ben");
    let rep = ben.sync(&host).await;
    assert_eq!(rep.got, ["notes.md"]);
    assert_eq!(ben.files().keys().collect::<Vec<_>>(), ["notes.md"]);
    for (path, word) in IGNORED {
        let w = why(&rep.skipped, path);
        assert!(w.contains(word), "{path}: {w}");
    }
    assert!(rep.skipped.iter().all(|s| s.on_server));
    // Ben's own file of an ignored name is not sent over the server's, and
    // the server's is not deleted because Ben has none.
    ben.write("roads.shp", "Ben's own");
    let rep = ben.sync(&host).await;
    assert!(rep.sent.is_empty());
    assert_eq!(
        r.lock().unwrap().files("main")["roads.shp"],
        b"from the server"
    );
    assert_eq!(ben.text("roads.shp"), "Ben's own");
}

#[tokio::test]
async fn files_over_5_mb_are_skipped_and_named_on_both_sides() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    let big = vec![b'x'; MAX_FILE as usize + 1];
    ana.write("big.txt", &big);
    ana.write("edge.txt", vec![b'y'; MAX_FILE as usize]);
    ana.write("small.txt", "a,b\n");
    let c = ana.check(&host_a).await;
    assert_eq!(c.here.len(), 2);
    assert!(why(&c.skipped, "big.txt").contains("5 MB"));
    let rep = ana.sync(&host_a).await;
    assert!(why(&rep.skipped, "big.txt").contains("5 MB"));
    {
        let r = r.lock().unwrap();
        let files = r.files("main");
        assert_eq!(files.keys().collect::<Vec<_>>(), ["edge.txt", "small.txt"]);
        assert_eq!(
            files["edge.txt"].len() as u64,
            MAX_FILE,
            "5 MB exactly still syncs"
        );
    }
    // A file over 5 MB on the server (committed with real Git) is not
    // fetched.
    r.lock()
        .unwrap()
        .commit("main", &[put("huge.bin", &big)], "huge", "Cy");
    let rep = ben.sync(&host_b).await;
    assert!(why(&rep.skipped, "huge.bin").contains("5 MB"));
    assert!(ben.read("huge.bin").is_none());
    assert_eq!(rep.got, ["edge.txt", "small.txt"]);

    // A synced file that grows past 5 MB here is no longer sent; if the
    // server's then changes, both versions are kept like any conflict.
    ben.write("small.txt", &big);
    let rep = ben.sync(&host_b).await;
    assert!(rep.sent.is_empty());
    assert!(why(&rep.skipped, "small.txt").contains("5 MB"));
    ana.sync(&host_a).await;
    ana.write("small.txt", "a,b\n1,2\n");
    ana.sync(&host_a).await;
    let rep = ben.sync(&host_b).await;
    assert_eq!(rep.conflicts.len(), 1);
    assert_eq!(ben.text("small.txt"), "a,b\n1,2\n");
    assert_eq!(
        ben.read(&format!("small (my copy {TODAY}).txt"))
            .unwrap()
            .len(),
        big.len()
    );
}

fn token() -> String {
    // Shaped like a GitHub token; made up here so that no line of this
    // file looks like one.
    format!("gh{}_{}", "p", "A1b2C3d4E5".repeat(4))
}

#[tokio::test]
async fn text_that_looks_like_a_token_or_a_key_stops_the_sync_before_anything_is_sent() {
    let r = repo();
    let (host_a, host_b) = (
        MemoryHost::new(r.clone(), "Ana"),
        MemoryHost::new(r.clone(), "Ben"),
    );
    let (mut ana, mut ben) = (Person::new("ana"), Person::new("ben"));
    ana.write("ok.md", "fine\n");
    ana.sync(&host_a).await;
    ben.sync(&host_b).await;
    ana.write("from-ana.md", "Ana's news\n");
    ana.sync(&host_a).await;
    let commits = r.lock().unwrap().log("main").len();

    let token = token();
    ben.write("ok.md", "fine, changed\n");
    ben.write(
        "scripts/load.py",
        format!("import os\n\nTOKEN = \"{token}\"\n"),
    );
    ben.write(
        "deploy/key.txt",
        format!("first line\n-----BEGIN {}PRIVATE KEY-----\nMIIB\n", "RSA "),
    );
    let asked = host_b.asked.load(Ordering::SeqCst);
    let rep = ben.sync(&host_b).await;
    let found = rep.stopped.as_ref().expect("the sync stopped");
    let at: Vec<(&str, usize)> = found.iter().map(|f| (f.path.as_str(), f.line)).collect();
    assert_eq!(at, [("deploy/key.txt", 2), ("scripts/load.py", 3)]);
    assert!(found[0].what.contains("private key") && found[1].what.contains("GitHub token"));
    // Nothing was sent: not the files with secrets, not the harmless one.
    assert!(rep.sent.is_empty() && rep.commit.is_none());
    assert_eq!(
        host_b.asked.load(Ordering::SeqCst),
        asked,
        "the server was never asked to take anything"
    );
    assert_eq!(r.lock().unwrap().log("main").len(), commits);
    assert!(!r.lock().unwrap().ever_held(token.as_bytes()));
    // The report names the file and the line, never the secret.
    let said = format!("{rep:?} {}", serde_json::to_string(&rep).unwrap());
    assert!(
        !said.contains(&token) && !said.contains(&token[4..20]),
        "{said}"
    );
    // What the server had was still fetched.
    assert_eq!(rep.got, ["from-ana.md"]);
    assert_eq!(ben.text("from-ana.md"), "Ana's news\n");

    // The secret taken out (read from the environment instead), the same
    // sync goes through.
    ben.write(
        "scripts/load.py",
        "import os\n\nTOKEN = os.environ[\"GH_TOKEN\"]\n",
    );
    ben.delete("deploy/key.txt");
    let rep = ben.sync(&host_b).await;
    assert!(rep.stopped.is_none());
    assert_eq!(rep.sent.len(), 2);
    assert!(!r.lock().unwrap().ever_held(token.as_bytes()));
}

#[tokio::test]
async fn the_linked_servers_own_token_never_travels_inside_a_file() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    // Not shaped like any known token: only Columbus knows it is one.
    let own = "kx91-made-up-server-token-0042".to_string();
    ana.write(
        "notes.md",
        format!("how I connected:\npasted {own} into Settings\n"),
    );
    let keep = ana.keep();
    let never = [own.clone()];
    let opts = Options {
        note: None,
        seen: None,
        keep: &keep,
        today: TODAY,
        never_send: &never,
        labels: &ana.labels,
    };
    let rep = columbus_sync::sync(&ana.root, &host, &mut ana.state, &opts)
        .await
        .unwrap();
    let found = rep.stopped.expect("stopped");
    assert_eq!((found[0].path.as_str(), found[0].line), ("notes.md", 2));
    assert!(found[0].what.contains("this server's own access token"));
    assert!(r.lock().unwrap().is_empty());
    assert!(!format!("{found:?}").contains(&own));
}

#[tokio::test]
async fn paths_f17_refuses_are_skipped_on_the_server_and_nothing_is_written_outside_the_folder() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ben");
    // A server (or a broken one) lists paths that would leave the folder,
    // reach into .git or .columbus, or cannot exist on Windows.
    let bad = [
        "../escape.txt",
        "a/../../escape2.txt",
        "/abs.txt",
        "C:/drive.txt",
        "back\\slash.txt",
        ".git/config",
        "sub/.git/hooks/pre-commit",
        ".columbus/state.json",
        "NUL.txt",
        "aux",
        "trailing./x.txt",
        "what?.txt",
        "",
    ];
    let mut edits: Vec<Edit> = bad.iter().map(|p| put(p, b"evil")).collect();
    edits.push(put("good.txt", b"good\n"));
    edits.push(put("Readme.md", b"one\n"));
    edits.push(put("README.md", b"two\n"));
    r.lock().unwrap().commit("main", &edits, "odd paths", "Cy");
    let mut ben = Person::new("ben");
    let rep = ben.sync(&host).await;
    assert_eq!(rep.got, ["good.txt"]);
    assert_eq!(ben.files().keys().collect::<Vec<_>>(), ["good.txt"]);
    for p in bad {
        assert!(
            rep.skipped.iter().any(|s| s.path == p && s.on_server),
            "{p:?} is named"
        );
    }
    assert!(why(&rep.skipped, "Readme.md").contains("capital letters"));
    assert!(why(&rep.skipped, "README.md").contains("capital letters"));
    // Nothing beside or above the folder.
    let around: Vec<String> = std::fs::read_dir(ben.root.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(around, ["files"], "only the folder itself: {around:?}");
    assert!(!ben.root.join(".git").exists() && !ben.root.join(".columbus").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn names_windows_cannot_hold_are_not_sent_from_a_system_that_can() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    for name in [
        "NUL.txt",
        "aux",
        "a:b.txt",
        "what?.md",
        "ends.",
        "tab\tname.txt",
    ] {
        ana.write(name, "x");
    }
    ana.write(".git/config", "[core]\n");
    ana.write(".columbus/trash/x", "x");
    ana.write("fine.txt", "x");
    let rep = ana.sync(&host).await;
    assert_eq!(rep.sent.len(), 1);
    assert_eq!(
        r.lock().unwrap().files("main").keys().collect::<Vec<_>>(),
        ["fine.txt"]
    );
    for name in [
        "NUL.txt",
        "aux",
        "a:b.txt",
        "what?.md",
        "ends.",
        "tab\tname.txt",
        ".git",
        ".columbus",
    ] {
        assert!(
            rep.skipped.iter().any(|s| s.path == name && !s.on_server),
            "{name:?} is named"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn links_in_the_folder_are_named_and_never_followed() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    let outside = ana.root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("passwords.txt"), "outside the folder\n").unwrap();
    std::os::unix::fs::symlink(outside.join("passwords.txt"), ana.path("link.txt")).unwrap();
    std::os::unix::fs::symlink(&outside, ana.path("linked-folder")).unwrap();
    ana.write("real.txt", "real\n");
    let rep = ana.sync(&host).await;
    assert_eq!(rep.sent.len(), 1);
    assert!(why(&rep.skipped, "link.txt").contains("link"));
    assert!(why(&rep.skipped, "linked-folder").contains("link"));
    assert!(!r.lock().unwrap().ever_held(b"outside the folder"));

    // The server now has files where the folder has links: nothing is
    // written through them.
    r.lock().unwrap().commit(
        "main",
        &[
            put("linked-folder/new.txt", b"from the server\n"),
            put("link.txt", b"replaced\n"),
        ],
        "through the links?",
        "Cy",
    );
    let rep = ana.sync(&host).await;
    assert!(rep.got.is_empty(), "{rep:?}");
    assert!(!outside.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("passwords.txt")).unwrap(),
        "outside the folder\n"
    );
    assert!(why(&rep.skipped, "linked-folder/new.txt").contains("is in the way: it is a link"));
    assert!(why(&rep.skipped, "link.txt").contains("is in the way: it is a link"));
    // And the link is not sent as if it were a file.
    assert_eq!(r.lock().unwrap().files("main")["link.txt"], b"replaced\n");
}

#[tokio::test]
async fn links_submodules_and_git_lfs_on_the_server_are_skipped_and_named() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ben");
    let pointer = b"version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 73400320\n";
    let raw = |path: &str, mode: &str, bytes: &[u8]| Edit::Raw {
        path: path.into(),
        mode: mode.into(),
        bytes: bytes.to_vec(),
    };
    r.lock().unwrap().commit(
        "main",
        &[
            raw("docs", "120000", b"../somewhere/else"),
            raw("vendor/lib", "160000", b"1234"),
            put("model.bin", pointer),
            raw("run.sh", "100755", b"#!/bin/sh\necho hi\n"),
            put("notes.md", b"notes\n"),
        ],
        "all kinds",
        "Cy",
    );
    let mut ben = Person::new("ben");
    let c = ben.check(&host).await;
    assert!(why(&c.skipped, "docs").contains("link"));
    assert!(why(&c.skipped, "vendor/lib").contains("submodule"));
    let rep = ben.sync(&host).await;
    assert_eq!(rep.got, ["notes.md", "run.sh"]);
    assert!(why(&rep.skipped, "model.bin").contains("Git LFS"));
    assert_eq!(
        ben.files().keys().collect::<Vec<_>>(),
        ["notes.md", "run.sh"]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(ben.path("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert!(mode & 0o100 != 0, "a program on the server is one here");
    }
    // The pointer is not asked for again, and stays named.
    let c = ben.check(&host).await;
    assert!(c.server.is_empty(), "{:?}", c.server);
    assert!(why(&c.skipped, "model.bin").contains("Git LFS"));
    // None of them is deleted from the server because the folder has none.
    ben.write("notes.md", "notes, changed\n");
    let rep = ben.sync(&host).await;
    assert_eq!(rep.sent.len(), 1);
    let r = r.lock().unwrap();
    let tree = r.tree_at(r.head("main").unwrap());
    for p in ["docs", "vendor/lib", "model.bin", "run.sh"] {
        assert!(tree.contains_key(p), "{p} is still on the server");
    }
    assert_eq!(tree["run.sh"].mode, "100755", "still a program");
}

#[tokio::test]
async fn a_file_here_where_the_server_has_a_folder_is_left_alone_on_both_sides() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ben");
    r.lock().unwrap().commit(
        "main",
        &[put("plan/steps.md", b"steps\n")],
        "a folder",
        "Cy",
    );
    let mut ben = Person::new("ben");
    ben.write("plan", "a file named like their folder\n");
    ben.write("other.md", "other\n");
    let rep = ben.sync(&host).await;
    assert_eq!(
        rep.sent.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
        ["other.md"]
    );
    assert!(why(&rep.skipped, "plan").contains("a file on one side and a folder on the other"));
    assert!(
        why(&rep.skipped, "plan/steps.md").contains("a file on one side and a folder on the other")
    );
    assert_eq!(ben.text("plan"), "a file named like their folder\n");
    let files = r.lock().unwrap().files("main");
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        ["other.md", "plan/steps.md"]
    );
}

#[tokio::test]
async fn a_folder_with_far_too_many_files_is_refused() {
    let r = repo();
    let host = MemoryHost::new(r.clone(), "Ana");
    let mut ana = Person::new("ana");
    // Someone unpacked a dataset into the project's files.
    let dir = ana.path("tiles");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..=columbus_sync::MAX_FILES {
        std::fs::write(dir.join(format!("{i}.txt")), b"").unwrap();
    }
    let err = columbus_sync::check(&ana.root, &host, &ana.state, &ana.labels)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("more than 20000 files"), "{err}");
    let err = ana.try_sync(&host, None).await.unwrap_err();
    assert!(matches!(err, columbus_sync::Error::Refused(_)), "{err:?}");
    assert!(r.lock().unwrap().is_empty(), "nothing was sent");
}
