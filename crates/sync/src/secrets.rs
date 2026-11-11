//! Before anything is sent: does an outgoing file hold something that
//! looks like a secret? A hit stops the sync before the first byte leaves,
//! and names the file and the line, never the secret itself.
//!
//! What counts (D-20), kept to what is very likely a secret, so that a sync
//! is not stopped for nothing:
//!
//! * the linked server's own access token, anywhere;
//! * a private key (`-----BEGIN … PRIVATE KEY-----`);
//! * a GitHub token (`ghp_…`, `gho_…`, `ghu_…`, `ghs_…`, `ghr_…`,
//!   `github_pat_…`);
//! * a sign-in token of the JWT kind (`eyJ….….…`), which Gitea's and
//!   Forgejo's sign-ins use;
//! * 40 hex digits (a Gitea or Forgejo access token) on a line that also
//!   says token, secret, password, key or authorization: 40 hex digits
//!   alone are far more often a commit or a checksum;
//! * a password written into the file: a quoted value given to a name
//!   ending in `password`, `passwd` or `pwd`, or a long mixed value given
//!   to one ending in `token`, `secret` or `key`; and an address with a
//!   password in it (`scheme://user:password@host`). Values that are
//!   plainly placeholders (`${…}`, `<…>`, `changeme`, `your-…`) do not count.
//!
//! A value read from the environment (`os.environ["PGPASSWORD"]`) is not a
//! quoted value, so it never counts: that is the way to keep a script's
//! password out of the file.

use serde::Serialize;

/// The most findings reported for one file.
const MAX_PER_FILE: usize = 20;

/// Something that looks like a secret: where, and what kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub path: String,
    /// The line, counted from 1.
    pub line: usize,
    /// What it looks like, in plain words (never the text itself).
    pub what: String,
}

/// Look through one outgoing file. `own` are strings that must never be
/// sent (the linked server's token).
pub fn find(path: &str, bytes: &[u8], own: &[String]) -> Vec<Finding> {
    let text = String::from_utf8_lossy(bytes);
    let is_text = crate::hash::is_text(bytes);
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if out.len() >= MAX_PER_FILE {
            break;
        }
        if let Some(what) = in_line(line, own, is_text) {
            out.push(Finding {
                path: path.to_string(),
                line: i + 1,
                what: what.to_string(),
            });
        }
    }
    out
}

fn in_line(line: &str, own: &[String], is_text: bool) -> Option<&'static str> {
    if own
        .iter()
        .any(|t| t.len() >= 8 && line.contains(t.as_str()))
    {
        return Some("this server's own access token");
    }
    if private_key(line) {
        return Some("a private key");
    }
    if github_token(line) {
        return Some("a GitHub token");
    }
    if jwt(line) {
        return Some("a sign-in token");
    }
    if hex40_beside_a_secret_word(line) {
        return Some("an access token (40 hex digits beside a word like token or key)");
    }
    if is_text {
        if password_assignment(line) {
            return Some("a password or key written into the file (a quoted value)");
        }
        if url_with_password(line) {
            return Some("an address with a password in it");
        }
    }
    None
}

fn private_key(line: &str) -> bool {
    let mut rest = line;
    while let Some(i) = rest.find("-----BEGIN ") {
        rest = &rest[i + 11..];
        if let Some(j) = rest.find("-----") {
            if rest[..j].contains("PRIVATE KEY") {
                return true;
            }
        }
    }
    false
}

fn word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Whether `line` has `prefix` at the start of a word, followed by at
/// least `min` characters that `body` accepts.
fn prefixed(line: &str, prefix: &str, min: usize, body: fn(u8) -> bool) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(i) = line[from..].find(prefix) {
        let start = from + i;
        let after = start + prefix.len();
        let at_word_start = start == 0 || !word_char(bytes[start - 1]);
        let run = bytes[after..].iter().take_while(|b| body(**b)).count();
        if at_word_start && run >= min {
            return true;
        }
        from = after;
    }
    false
}

fn github_token(line: &str) -> bool {
    ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
        .iter()
        .any(|p| prefixed(line, p, 36, |b| b.is_ascii_alphanumeric()))
        || prefixed(line, "github_pat_", 40, word_char)
}

fn jwt(line: &str) -> bool {
    let b64 = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(i) = line[from..].find("eyJ") {
        let start = from + i;
        from = start + 3;
        if start > 0 && word_char(bytes[start - 1]) {
            continue;
        }
        let mut at = start;
        let mut parts = 0;
        loop {
            let run = bytes[at..].iter().take_while(|b| b64(**b)).count();
            if run < 10 {
                break;
            }
            parts += 1;
            at += run;
            if parts == 3 || bytes.get(at) != Some(&b'.') {
                break;
            }
            at += 1;
        }
        if parts == 3 {
            return true;
        }
    }
    false
}

const SECRET_WORDS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "api-key",
    "access_key",
    "access-key",
    "authorization",
    "bearer",
];

fn hex40_beside_a_secret_word(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut found = false;
    while i < bytes.len() {
        if bytes[i].is_ascii_hexdigit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let run = bytes[i..]
                .iter()
                .take_while(|b| b.is_ascii_hexdigit())
                .count();
            let ends = bytes
                .get(i + run)
                .is_none_or(|b| !b.is_ascii_alphanumeric());
            // All digits is a number, not a token.
            let mixed = bytes[i..i + run].iter().any(|b| b.is_ascii_alphabetic());
            if run == 40 && ends && mixed {
                found = true;
                break;
            }
            i += run.max(1);
        } else {
            i += 1;
        }
    }
    if !found {
        return false;
    }
    let lower = line.to_lowercase();
    SECRET_WORDS.iter().any(|w| lower.contains(w))
}

/// A value nobody would use: a template, a stand-in, one letter repeated.
fn placeholder(v: &str) -> bool {
    let l = v.to_lowercase();
    v.contains(['$', '{', '}', '<', '>', '%', '*'])
        || [
            "your",
            "example",
            "placeholder",
            "changeme",
            "change_me",
            "change-me",
            "redacted",
            "dummy",
            "xxxx",
            "todo",
            "secret",
            "password",
            "passwd",
        ]
        .iter()
        .any(|w| l.contains(w))
        || l.chars()
            .next()
            .is_some_and(|first| l.chars().all(|c| c == first))
}

/// `name = "value"` (or `:`, or `=>`), with `name` one that holds a
/// password or a key.
fn password_assignment(line: &str) -> bool {
    let bytes = line.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'=' && *b != b':' {
            continue;
        }
        // The name before it: letters, digits, _ and -, perhaps in quotes.
        let mut end = i;
        while end > 0 && bytes[end - 1] == b' ' {
            end -= 1;
        }
        if end > 0 && (bytes[end - 1] == b'"' || bytes[end - 1] == b'\'') {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (word_char(bytes[start - 1]) || bytes[start - 1] == b'-') {
            start -= 1;
        }
        if start == end {
            continue;
        }
        let name = line[start..end].to_lowercase();
        let password = ["password", "passwd", "pwd"]
            .iter()
            .any(|w| name.ends_with(w));
        let key = [
            "token",
            "secret",
            "apikey",
            "api_key",
            "api-key",
            "access_key",
            "access-key",
            "secret_key",
            "secret-key",
            "private_key",
            "private-key",
        ]
        .iter()
        .any(|w| name.ends_with(w));
        if !password && !key {
            continue;
        }
        // The value after it: a quoted run with no space in it.
        let mut at = i + 1;
        while at < bytes.len() && matches!(bytes[at], b' ' | b'=' | b'>' | b':') {
            at += 1;
        }
        let Some(quote) = bytes.get(at).copied().filter(|q| *q == b'"' || *q == b'\'') else {
            continue;
        };
        let rest = &line[at + 1..];
        let Some(close) = rest.find(quote as char) else {
            continue;
        };
        let value = &rest[..close];
        if value.chars().any(char::is_whitespace) || placeholder(value) {
            continue;
        }
        let n = value.chars().count();
        let mixed =
            value.chars().any(|c| c.is_ascii_digit()) && value.chars().any(|c| c.is_alphabetic());
        if (password && n >= 6) || (key && n >= 16 && mixed) {
            return true;
        }
    }
    false
}

/// `scheme://user:password@host`.
fn url_with_password(line: &str) -> bool {
    let mut rest = line;
    while let Some(i) = rest.find("://") {
        rest = &rest[i + 3..];
        let end = rest
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c.is_whitespace() || c == '"' || c == '\''
            })
            .unwrap_or(rest.len());
        let authority = &rest[..end];
        if let Some((userinfo, _host)) = authority.rsplit_once('@') {
            if let Some((_user, pass)) = userinfo.split_once(':') {
                if pass.chars().count() >= 3 && !placeholder(pass) && pass.to_lowercase() != "pass"
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(line: &str) -> Option<String> {
        find("f.py", line.as_bytes(), &[])
            .first()
            .map(|f| f.what.clone())
    }

    // Made up for these tests, and put together here so that no line of
    // this file looks like a credential: none is real.
    fn gh() -> String {
        format!("gh{}_{}", "p", "0123456789abcdefghijABCDEFGHIJ012345")
    }
    fn pat() -> String {
        format!("github{}{}", "_pat_", "1A2b3C4d5E".repeat(8))
    }
    fn hex40() -> String {
        "0123456789abcdef".repeat(3)[..40].to_string()
    }
    fn jwt_like() -> String {
        [
            "eyJ",
            "hbGciOiJub25lIn0",
            ".",
            "eyJzdWIiOiJ0ZXN0In0",
            ".",
            "c2lnbmF0dXJlLW9mLW5vdGhpbmc",
        ]
        .concat()
    }
    fn key_line(kind: &str) -> String {
        format!("-----BEGIN {kind}{} KEY-----", "PRIVATE")
    }

    #[test]
    fn what_stops_a_sync() {
        let (gh, pat, hex, jwt) = (gh(), pat(), hex40(), jwt_like());
        for line in [
            key_line("RSA "),
            key_line(""),
            format!("key = \"\"\"{}", key_line("OPENSSH ")),
            format!("{} BLOCK-----", key_line("PGP ").trim_end_matches('-')),
            format!("TOKEN={gh}"),
            format!("curl -H 'Authorization: Bearer {gh}' https://api.github.com/user"),
            format!("x = '{pat}'"),
            format!("gitea_token: {hex}"),
            format!("Authorization: token {hex}"),
            format!("auth = {jwt}"),
            format!("password = \"{}\"", "hunter2-prod"),
            format!("DB_PASSWORD='{}'", "S3cure!Pass"),
            format!("  \"password\": \"{}\",", "Tr0ub4dor&3"),
            format!("pwd => '{}'", "opensesame"),
            format!("api_key = \"{}\"", "a8f5f167f44f4964e6c998dee827110c"),
            format!("client_secret: \"{}\"", "9bX2kQ7mLp4Zt1Vw8Ry3"),
            format!(
                "conn = 'postgresql://gis:{}@db.corp.example/gis'",
                "Wint3r2026"
            ),
        ] {
            assert!(hit(&line).is_some(), "should stop: {line}");
        }
    }

    #[test]
    fn what_does_not() {
        let hex = hex40();
        for line in [
            "password = os.environ[\"PGPASSWORD\"]".to_string(),
            "connect(user=user, password=password)".to_string(),
            "password = getpass.getpass()".to_string(),
            "password = \"${DB_PASSWORD}\"".to_string(),
            "password: \"<your password>\"".to_string(),
            "password = \"changeme\"".to_string(),
            "password = \"\"".to_string(),
            "password = \"abc\"".to_string(),
            "token = \"IDENTIFIER\"".to_string(),
            "next_token = \"end-of-statement\"".to_string(),
            "The password is kept in the credential store.".to_string(),
            format!("commit {hex}"),
            format!("sha1: {hex}"),
            format!("checksum = \"{hex}\""),
            format!("{} token", "0".repeat(40)),
            "-----BEGIN CERTIFICATE-----".to_string(),
            "-----BEGIN PUBLIC KEY-----".to_string(),
            "see https://github.com/owner/repo".to_string(),
            "url = \"postgresql://gis@db.corp.example/gis\"".to_string(),
            "url = \"postgresql://user:password@host/db\"".to_string(),
            "url = \"postgresql://{user}:{password}@{host}/db\"".to_string(),
            format!("gh{}_short", "p"),
            "eyJ is how a JWT starts".to_string(),
            "widths = [3.6, 3.7]  # token widths".to_string(),
        ] {
            assert_eq!(hit(&line), None, "should not stop: {line}");
        }
    }

    #[test]
    fn the_servers_own_token_is_found_anywhere_and_never_repeated() {
        let own = vec!["zq7k-FAKE-token-0001".to_string()];
        let text = "line one\nnotes: zq7k-FAKE-token-0001 was pasted here\nline three\n";
        let found = find("notes.md", text.as_bytes(), &own);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].path.as_str(), found[0].line), ("notes.md", 2));
        let shown = format!("{found:?} {}", serde_json::to_string(&found).unwrap());
        assert!(!shown.contains("zq7k"), "{shown}");
        // In a file that is not text, too.
        let mut bin = vec![0u8, 1, 2];
        bin.extend_from_slice(b"\nzq7k-FAKE-token-0001\n");
        assert_eq!(find("x.bin", &bin, &own).len(), 1);
        // And what a finding says never holds the secret it found.
        let gh = gh();
        for f in find("k.py", format!("a = 1\nTOKEN={gh}\n").as_bytes(), &[]) {
            assert_eq!(f.line, 2);
            assert!(!format!("{f:?}").contains(&gh[..8]));
        }
    }

    #[test]
    fn a_file_full_of_hits_is_reported_in_part() {
        let text = format!("password = \"{}\"\n", "hunter2-prod").repeat(50);
        assert_eq!(find("f", text.as_bytes(), &[]).len(), MAX_PER_FILE);
    }
}
