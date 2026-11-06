//! File access inside folders a confined process can write.
//!
//! Servers, plugins and scripts own their folders (`servers/<id>`,
//! `plugins/<id>`, `runs/<run>`), so anything below the workspace root there
//! may be swapped for a link at any moment. The gateway walks such paths one
//! component at a time from the workspace root, never following a link, and
//! opens and creates files relative to the directory it holds open, so a
//! link planted anywhere on the way is refused rather than followed.
//!
//! On Unix this uses `openat` with `O_NOFOLLOW`. On Windows each component
//! is checked not to be a reparse point, files are opened as themselves
//! (`FILE_FLAG_OPEN_REPARSE_POINT`), and every folder and file opened is
//! then checked through its handle: its real path must be exactly the one
//! asked for below the trusted root, and a file must have one name.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

fn plain(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || (cfg!(windows) && name.contains(':'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a plain name"),
        ));
    }
    Ok(())
}

fn refused(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, what.to_string())
}

/// A directory reached from a trusted root without following links.
pub struct Dir {
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    /// Where the folder really is, in the form paths are compared in
    /// (Windows: files opened in it are checked to be there).
    #[cfg(windows)]
    real: String,
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    fn cstr(s: &str) -> io::Result<CString> {
        CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in name"))
    }

    fn open_dir_at(dir: Option<&OwnedFd>, name: &str) -> io::Result<OwnedFd> {
        let c = cstr(name)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = unsafe {
            match dir {
                Some(d) => libc::openat(d.as_raw_fd(), c.as_ptr(), flags),
                None => libc::open(c.as_ptr(), flags),
            }
        };
        if fd < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ELOOP) || e.raw_os_error() == Some(libc::ENOTDIR) {
                return Err(refused(&format!("{name} is a link or not a folder")));
            }
            return Err(e);
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    impl Dir {
        pub fn open_beneath(root: &Path, comps: &[&str], create: bool) -> io::Result<Dir> {
            let root_s = root
                .to_str()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "root is not text"))?;
            let mut fd = open_dir_at(None, root_s)?;
            let mut path = root.to_path_buf();
            for c in comps {
                plain(c)?;
                if create {
                    let cn = cstr(c)?;
                    let r = unsafe { libc::mkdirat(fd.as_raw_fd(), cn.as_ptr(), 0o755) };
                    if r < 0 {
                        let e = io::Error::last_os_error();
                        if e.raw_os_error() != Some(libc::EEXIST) {
                            return Err(e);
                        }
                    }
                }
                fd = open_dir_at(Some(&fd), c)?;
                path.push(c);
            }
            Ok(Dir { path, fd })
        }

        /// Names in the folder (not `.` or `..`).
        pub fn entries(&self) -> io::Result<Vec<String>> {
            let dup = unsafe { libc::dup(self.fd.as_raw_fd()) };
            if dup < 0 {
                return Err(io::Error::last_os_error());
            }
            let d = unsafe { libc::fdopendir(dup) };
            if d.is_null() {
                unsafe { libc::close(dup) };
                return Err(io::Error::last_os_error());
            }
            unsafe { libc::rewinddir(d) };
            let mut out = Vec::new();
            loop {
                let ent = unsafe { libc::readdir(d) };
                if ent.is_null() {
                    break;
                }
                let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
                let n = name.to_string_lossy().into_owned();
                if n != "." && n != ".." {
                    out.push(n);
                }
            }
            unsafe { libc::closedir(d) };
            out.sort();
            Ok(out)
        }

        /// Open a regular file with one name, never a link, FIFO or device.
        pub fn open_file(&self, name: &str) -> io::Result<File> {
            plain(name)?;
            let c = cstr(name)?;
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_RDONLY
                        | libc::O_NOFOLLOW
                        | libc::O_NONBLOCK
                        | libc::O_CLOEXEC
                        | libc::O_NOCTTY,
                )
            };
            if fd < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ELOOP) {
                    return Err(refused(&format!("{name} is a link")));
                }
                return Err(e);
            }
            let f = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
            let m = f.metadata()?;
            use std::os::unix::fs::MetadataExt;
            if !m.file_type().is_file() {
                return Err(refused(&format!("{name} is not a regular file")));
            }
            if m.nlink() > 1 {
                return Err(refused(&format!(
                    "{name} has more than one name (a hard link)"
                )));
            }
            Ok(f)
        }

        /// Create a new file (failing if anything, a link included, is there).
        pub fn create_file(&self, name: &str) -> io::Result<File> {
            plain(name)?;
            let c = cstr(name)?;
            let fd = unsafe {
                libc::openat(
                    self.fd.as_raw_fd(),
                    c.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o644 as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
        }
    }
}

#[cfg(windows)]
mod imp {
    //! Windows has no `openat` here, so each file is opened by its path and
    //! then checked by its handle: where it really is (the final path,
    //! after any junction or link on the way) must be exactly where it was
    //! asked for, below the trusted root, and it must have one name. A
    //! folder swapped for a junction at any moment is caught, because the
    //! check is of the open file itself, not of the path before opening.
    use super::*;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, GetFileInformationByHandle, GetFinalPathNameByHandleW,
        SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, DELETE, FILE_DISPOSITION_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS, VOLUME_NAME_NT,
    };

    const REPARSE: u32 = 0x400; // FILE_ATTRIBUTE_REPARSE_POINT
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000; // FILE_FLAG_OPEN_REPARSE_POINT

    /// Where an open file or folder really is: as a drive path, or, on a
    /// volume with no drive letter (a RAM disk, some mounted cloud drives),
    /// as the system's device path. A file and its folder are on one
    /// volume, so they are always compared in the same form.
    fn final_path(f: &File) -> io::Result<String> {
        final_path_as(f, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS)
            .or_else(|_| final_path_as(f, FILE_NAME_NORMALIZED | VOLUME_NAME_NT))
    }

    /// The trusted root, followed if it is itself a link (it is the
    /// gateway's own, so where it leads is where it is).
    fn open_root(p: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(p)
    }

    fn open_folder(p: &Path) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
            .open(p)
    }

    fn final_path_as(f: &File, flags: u32) -> io::Result<String> {
        let h = f.as_raw_handle();
        let mut buf = vec![0u16; 1024];
        loop {
            let n = unsafe {
                GetFinalPathNameByHandleW(h as _, buf.as_mut_ptr(), buf.len() as u32, flags)
            } as usize;
            if n == 0 {
                return Err(io::Error::last_os_error());
            }
            if n < buf.len() {
                return Ok(String::from_utf16_lossy(&buf[..n]));
            }
            buf.resize(n + 1, 0);
        }
    }

    /// The form two paths are compared in: no `\\?\` prefix, one kind of
    /// separator, no trailing one, case folded (NTFS names compare so).
    fn key(p: &str) -> String {
        let p = p
            .strip_prefix(r"\\?\UNC\")
            .map(|r| format!(r"\\{r}"))
            .unwrap_or_else(|| p.strip_prefix(r"\\?\").unwrap_or(p).to_string());
        p.replace('/', "\\").trim_end_matches('\\').to_lowercase()
    }

    fn links(f: &File) -> io::Result<u32> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.nNumberOfLinks)
    }

    /// Delete a file through its handle (one just created somewhere else
    /// than asked, through a swapped folder).
    /// Whether that worked; the file goes when its handle closes.
    fn delete_open(f: &File) -> bool {
        let d = FILE_DISPOSITION_INFO { DeleteFile: true };
        unsafe {
            SetFileInformationByHandle(
                f.as_raw_handle() as _,
                FileDispositionInfo,
                (&d as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            ) != 0
        }
    }

    impl Dir {
        pub fn open_beneath(root: &Path, comps: &[&str], create: bool) -> io::Result<Dir> {
            // The root is the gateway's own; where it really is, once.
            let mut want = key(&final_path(&open_root(root)?)?);
            let mut path = root.to_path_buf();
            for c in comps {
                plain(c)?;
                path.push(c);
                want.push('\\');
                want.push_str(&c.to_lowercase());
                if create && std::fs::symlink_metadata(&path).is_err() {
                    std::fs::create_dir(&path).or_else(|e| {
                        if e.kind() == io::ErrorKind::AlreadyExists {
                            Ok(())
                        } else {
                            Err(e)
                        }
                    })?;
                }
                let m = std::fs::symlink_metadata(&path)?;
                if m.file_attributes() & REPARSE != 0 || !m.is_dir() {
                    return Err(refused(&format!("{c} is a link or not a folder")));
                }
            }
            // The folder as opened, wherever the walk led: it must be the one asked for.
            let d = open_folder(&path)?;
            if key(&final_path(&d)?) != want {
                return Err(refused("a folder on the way is a link"));
            }
            Ok(Dir { path, real: want })
        }

        pub fn entries(&self) -> io::Result<Vec<String>> {
            let mut out: Vec<String> = std::fs::read_dir(&self.path)?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            out.sort();
            Ok(out)
        }

        fn is_where_asked(&self, f: &File, name: &str) -> io::Result<bool> {
            Ok(key(&final_path(f)?) == format!("{}\\{}", self.real, name.to_lowercase()))
        }

        pub fn open_file(&self, name: &str) -> io::Result<File> {
            plain(name)?;
            let f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(OPEN_REPARSE_POINT)
                .open(self.path.join(name))?;
            let m = f.metadata()?;
            if m.file_attributes() & REPARSE != 0 || !m.is_file() {
                return Err(refused(&format!("{name} is a link or not a regular file")));
            }
            if !self.is_where_asked(&f, name)? {
                return Err(refused(&format!("{name} was reached through a link")));
            }
            if links(&f)? > 1 {
                return Err(refused(&format!(
                    "{name} has more than one name (a hard link)"
                )));
            }
            Ok(f)
        }

        pub fn create_file(&self, name: &str) -> io::Result<File> {
            plain(name)?;
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .access_mode(0x4000_0000 | DELETE) // GENERIC_WRITE | DELETE
                .custom_flags(OPEN_REPARSE_POINT)
                .open(self.path.join(name))?;
            if !self.is_where_asked(&f, name)? {
                let gone = delete_open(&f);
                return Err(refused(&if gone {
                    format!("{name} would have been made through a link; it was not")
                } else {
                    format!("{name} was made through a link, outside the folder asked for, and could not be removed")
                }));
            }
            Ok(f)
        }
    }
}

impl Dir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Copy `src` (a file the gateway trusts) to a new file `name` in `dir`,
/// returning the SHA-256 of the bytes written.
pub fn copy_into(
    src: &mut File,
    dir: &Dir,
    name: &str,
    cap: Option<u64>,
) -> io::Result<(String, u64)> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let mut dst = dir.create_file(name)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n: u64 = 0;
    loop {
        let k = src.read(&mut buf)?;
        if k == 0 {
            break;
        }
        n += k as u64;
        if cap.is_some_and(|c| n > c) {
            drop(dst);
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "larger than allowed",
            ));
        }
        h.update(&buf[..k]);
        dst.write_all(&buf[..k])?;
    }
    dst.sync_all()?;
    Ok((format!("{:x}", h.finalize()), n))
}

/// Open the regular file at `path` (below `root`) without following a link
/// anywhere on the way.
pub fn open_beneath(root: &Path, path: &Path) -> io::Result<File> {
    let comps = below(root, path)?;
    let (file, dirs) = comps
        .split_last()
        .ok_or_else(|| io::Error::other("no file name"))?;
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    Dir::open_beneath(root, &dirs, false)?.open_file(file)
}

/// Copy an open file to a new file at `to` in a folder only the gateway
/// writes, returning the SHA-256 of the bytes written.
pub fn copy_to_new(src: &mut File, to: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let k = src.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        dst.write_all(&buf[..k])?;
    }
    dst.sync_all()?;
    Ok(format!("{:x}", h.finalize()))
}

/// The components of `path` below `root`, each a plain name.
pub fn below(root: &Path, path: &Path) -> io::Result<Vec<String>> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| refused(&format!("{} is not in the workspace", path.display())))?;
    rel.components()
        .map(|c| match c {
            std::path::Component::Normal(s) => s
                .to_str()
                .map(String::from)
                .ok_or_else(|| refused("a path component is not text")),
            _ => Err(refused("not a plain path")),
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn links_on_the_way_are_refused_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        // A link as a folder on the way.
        std::os::unix::fs::symlink(outside.path(), root.join("out")).unwrap();
        assert!(Dir::open_beneath(&root, &["out"], false).is_err());
        assert!(Dir::open_beneath(&root, &["out", "x"], true).is_err());
        assert!(!outside.path().join("x").exists());
        // A link as the file.
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), root.join("real/f")).unwrap();
        let d = Dir::open_beneath(&root, &["real"], false).unwrap();
        assert!(d.open_file("f").is_err());
        assert!(d.create_file("f").is_err());
        // A hard link.
        std::fs::write(root.join("real/a"), "a").unwrap();
        std::fs::hard_link(root.join("real/a"), root.join("real/b")).unwrap();
        assert!(d.open_file("a").is_err());
        // A FIFO does not block.
        let c = std::ffi::CString::new(root.join("real/p").to_str().unwrap()).unwrap();
        unsafe { libc::mkfifo(c.as_ptr(), 0o644) };
        assert!(d.open_file("p").is_err());
        // Plain files work, and names cannot climb.
        std::fs::write(root.join("real/ok"), "fine").unwrap();
        assert!(d.open_file("ok").is_ok());
        assert!(d.open_file("../real/ok").is_err());
        assert_eq!(d.entries().unwrap(), vec!["a", "b", "f", "ok", "p"]);
        let made = Dir::open_beneath(&root, &["new", "deeper"], true).unwrap();
        assert!(made.path().ends_with("new/deeper"));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn junction(link: &Path, target: &Path) {
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J failed");
    }

    /// A workspace on a substituted drive, or reached as a network share,
    /// still works: the root's real path is taken from its own handle.
    #[test]
    fn roots_on_subst_drives_and_shares_work() {
        let real = tempfile::tempdir().unwrap();
        std::fs::create_dir(real.path().join("w")).unwrap();
        std::fs::write(real.path().join("w").join("a.txt"), "a").unwrap();
        let check = |root: &Path| {
            let d = Dir::open_beneath(root, &["w"], false).unwrap();
            assert!(d.open_file("a.txt").is_ok(), "{}", root.display());
            let made = format!("n{}.txt", std::process::id());
            assert!(d.create_file(&made).is_ok(), "{}", root.display());
            let _ = std::fs::remove_file(real.path().join("w").join(&made));
        };
        check(real.path());
        let mut substituted = false;
        for letter in ['Q', 'R', 'S', 'T', 'U', 'V', 'W'] {
            let drive = format!("{letter}:");
            let ok = std::process::Command::new("subst")
                .arg(&drive)
                .arg(real.path())
                .status()
                .is_ok_and(|s| s.success());
            if ok {
                check(Path::new(&format!("{drive}\\")));
                let _ = std::process::Command::new("subst")
                    .args([&drive, "/D"])
                    .status();
                substituted = true;
                break;
            }
        }
        assert!(
            substituted,
            "no drive letter was free for subst, so nothing was checked"
        );
        // The same folder as an administrative share, where there is one.
        let p = real.path().to_string_lossy().replace(':', "$");
        let unc = std::path::PathBuf::from(format!(r"\\localhost\{p}"));
        if unc.exists() {
            check(&unc);
        } else {
            eprintln!("skipped: no administrative share at {}", unc.display());
        }
        // A root that is itself a junction is followed (it is trusted).
        let link = tempfile::tempdir().unwrap();
        let root_link = link.path().join("root");
        junction(&root_link, real.path());
        check(&root_link);
    }

    #[test]
    fn junctions_and_hard_links_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "x").unwrap();
        std::fs::create_dir(root.path().join("srv")).unwrap();
        junction(&root.path().join("srv").join("out"), outside.path());
        // A junction on the way is refused, as a folder and for its files.
        assert!(Dir::open_beneath(root.path(), &["srv", "out"], false).is_err());
        assert!(open_beneath(root.path(), &root.path().join("srv/out/secret.txt")).is_err());

        // A folder swapped for a junction after it was reached: the file
        // opened or made through it is refused, and nothing is left there.
        std::fs::create_dir(root.path().join("srv").join("in")).unwrap();
        let d = Dir::open_beneath(root.path(), &["srv", "in"], false).unwrap();
        std::fs::remove_dir(root.path().join("srv").join("in")).unwrap();
        junction(&root.path().join("srv").join("in"), outside.path());
        assert!(d.open_file("secret.txt").is_err());
        assert!(d.create_file("made.txt").is_err());
        assert!(
            !outside.path().join("made.txt").exists(),
            "made through the junction"
        );

        // A hard link to a file elsewhere has two names: refused.
        std::fs::create_dir(root.path().join("plain")).unwrap();
        std::fs::hard_link(
            outside.path().join("secret.txt"),
            root.path().join("plain").join("h.txt"),
        )
        .unwrap();
        let p = Dir::open_beneath(root.path(), &["plain"], false).unwrap();
        assert!(p.open_file("h.txt").is_err());
        std::fs::write(root.path().join("plain").join("ok.txt"), "y").unwrap();
        assert!(p.open_file("ok.txt").is_ok());
        assert!(p.create_file("new.txt").is_ok());
    }
}
