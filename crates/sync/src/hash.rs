//! Hashes: what a file holds (so a change is found by content, never by a
//! time alone), and the id Git gives the same bytes (so a file can be
//! compared with the server's without fetching it).

use sha1::Sha1;
use sha2::{Digest, Sha256};

/// How a repository names its objects. Gitea can make repositories whose
/// ids are SHA-256 (`object_format_name`), so an id is never taken to be 40
/// digits: the format is read from the ids the server gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    Sha1,
    Sha256,
}

impl ObjectFormat {
    /// The format an object id is in, by its length, if it is one Columbus
    /// knows (40 or 64 hex digits).
    pub fn of_id(id: &str) -> Option<ObjectFormat> {
        if !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        match id.len() {
            40 => Some(ObjectFormat::Sha1),
            64 => Some(ObjectFormat::Sha256),
            _ => None,
        }
    }
}

/// The id Git gives a file holding `bytes` (a blob), in `format`.
pub fn blob_id(bytes: &[u8], format: ObjectFormat) -> String {
    let head = format!("blob {}\0", bytes.len());
    match format {
        ObjectFormat::Sha1 => {
            let mut h = Sha1::new();
            h.update(head.as_bytes());
            h.update(bytes);
            hex::encode(h.finalize())
        }
        ObjectFormat::Sha256 => {
            let mut h = Sha256::new();
            h.update(head.as_bytes());
            h.update(bytes);
            hex::encode(h.finalize())
        }
    }
}

/// Whether `bytes` read as text: no NUL byte near the start (Git's own
/// test). Line endings are compared loosely for text only.
pub fn is_text(bytes: &[u8]) -> bool {
    !bytes[..bytes.len().min(8000)].contains(&0)
}

/// What a file holds, as `sha256:<hex>`. For text, every CRLF counts as LF:
/// two files that differ only in their line endings have the same hash, so
/// a difference in line endings alone is never a change (D-20).
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    if is_text(bytes) && bytes.contains(&b'\r') {
        let mut start = 0;
        for i in 0..bytes.len() {
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                h.update(&bytes[start..i]);
                start = i + 1;
            }
        }
        h.update(&bytes[start..]);
    } else {
        h.update(bytes);
    }
    format!("sha256:{}", hex::encode(h.finalize()))
}

/// Whether `bytes` are a Git LFS pointer: the small text file a repository
/// holds in place of a large one kept elsewhere. Columbus does not fetch
/// what it points to.
pub fn is_lfs_pointer(bytes: &[u8]) -> bool {
    bytes.len() < 1024
        && bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\n")
        && bytes.windows(12).any(|w| w == b"\noid sha256:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_blob_ids() {
        // `git hash-object` of an empty file and of "hello\n".
        assert_eq!(
            blob_id(b"", ObjectFormat::Sha1),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            blob_id(b"hello\n", ObjectFormat::Sha1),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        // The same with `--object-format=sha256`.
        assert_eq!(
            blob_id(b"", ObjectFormat::Sha256),
            "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813"
        );
        assert_eq!(
            ObjectFormat::of_id(&"a".repeat(40)),
            Some(ObjectFormat::Sha1)
        );
        assert_eq!(
            ObjectFormat::of_id(&"0".repeat(64)),
            Some(ObjectFormat::Sha256)
        );
        assert_eq!(ObjectFormat::of_id("abc"), None);
        assert_eq!(ObjectFormat::of_id(&"z".repeat(40)), None);
    }

    #[test]
    fn line_endings_alone_do_not_change_the_hash() {
        let lf = b"one\ntwo\nthree\n";
        let crlf = b"one\r\ntwo\r\nthree\r\n";
        let mixed = b"one\r\ntwo\nthree\r\n";
        assert_eq!(content_hash(lf), content_hash(crlf));
        assert_eq!(content_hash(lf), content_hash(mixed));
        assert_ne!(content_hash(lf), content_hash(b"one\ntwo\nthree"));
        assert_ne!(content_hash(lf), content_hash(b"one\ntwo\nthreE\n"));
        // A lone CR is not a line ending Columbus folds.
        assert_ne!(content_hash(b"a\rb"), content_hash(b"a\nb"));
        assert_ne!(content_hash(b"a\r\rb\n"), content_hash(b"a\rb\n"));
        // Binary files are compared byte for byte.
        let bin_a = b"\x00\x01\r\n\x02";
        let bin_b = b"\x00\x01\n\x02";
        assert_ne!(content_hash(bin_a), content_hash(bin_b));
        assert!(content_hash(lf).starts_with("sha256:"));
    }

    #[test]
    fn lfs_pointers() {
        let p = b"version https://git-lfs.github.com/spec/v1\noid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\nsize 12345\n";
        assert!(is_lfs_pointer(p));
        assert!(!is_lfs_pointer(b"version 1\n"));
        assert!(!is_lfs_pointer(b""));
    }
}
