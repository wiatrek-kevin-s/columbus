//! A file a colleague deleted goes to the system's trash (the Recycle Bin
//! on Windows, the Trash on macOS and Linux), where the person can put it
//! back; where the system cannot take it, to Columbus's own folder. Either
//! way it is out of the synced folder, and not erased.
//!
//! One test, in a program of its own, because it tells the system where
//! its trash is (on Linux) before anything else runs.

use columbus_sync::trash::{remove, Keep, Kept};

#[test]
fn a_removed_file_goes_to_the_systems_trash_or_to_columbuss_folder() {
    let d = tempfile::tempdir().unwrap();
    // On Linux the trash is `$XDG_DATA_HOME/Trash`: one made for this test,
    // on the same disk as the file, so the real one is left alone.
    #[cfg(all(unix, not(target_os = "macos")))]
    std::env::set_var("XDG_DATA_HOME", d.path().join("xdg"));
    let root = d.path().join("files");
    let fallback = d.path().join("removed");
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    let text = format!("print('deleted by a colleague, {}')\n", std::process::id());
    std::fs::write(root.join("scripts/old.py"), &text).unwrap();
    std::fs::write(root.join("scripts/stays.py"), "stays\n").unwrap();

    let keep = Keep::SystemTrash {
        fallback: fallback.clone(),
    };
    let kept = remove(&root, "scripts/old.py", &keep).unwrap();
    assert!(
        !root.join("scripts/old.py").exists(),
        "it is out of the folder"
    );
    assert!(
        root.join("scripts/stays.py").exists(),
        "and nothing else is"
    );
    match &kept {
        Kept::Folder(to) => {
            // The system had no trash for it: Columbus's folder has it.
            assert!(std::path::Path::new(to).starts_with(&fallback), "{to}");
            assert_eq!(std::fs::read_to_string(to).unwrap(), text);
        }
        Kept::Trash => {
            assert!(!fallback.exists(), "not in both places");
            // On Linux the trash's folder is known: the file is in it,
            // whole, with a note of where it came from.
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                let trash = d.path().join("xdg").join("Trash");
                let files: Vec<_> = std::fs::read_dir(trash.join("files"))
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect();
                assert_eq!(files.len(), 1, "{files:?}");
                assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), text);
                let info = std::fs::read_dir(trash.join("info")).unwrap().count();
                assert_eq!(info, 1, "it can be put back");
            }
        }
    }
    println!("kept: {kept:?}");
    // On the systems this build knows a trash for, with one at hand, that
    // is where it went.
    #[cfg(all(unix, not(target_os = "macos")))]
    assert_eq!(kept, Kept::Trash);
}
