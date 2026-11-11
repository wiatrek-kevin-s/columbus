//! Does a file hold something that looks like a secret? Asked in two
//! places, with this one scan:
//!
//! * before a sync sends anything (`columbus-sync`): a hit stops the sync
//!   before the first byte leaves;
//! * before the assistant is given a project file's text, or runs a script
//!   kept there (`project_files`): a file with a hit is not handed over.
//!
//! A finding names the file and the line, never the secret itself.
//!
//! What counts (D-20; widened by the final review of part 3, R-103), kept
//! to what is very likely a secret, so that nobody is stopped for nothing:
//!
//! * the linked server's own access token, anywhere;
//! * a private key (`-----BEGIN … PRIVATE KEY-----`);
//! * a token by its well-known start: GitHub (`ghp_…`, `gho_…`, `ghu_…`,
//!   `ghs_…`, `ghr_…`, `github_pat_…`), Anthropic (`sk-ant-…`), other
//!   `sk-…` keys, GitLab (`glpat-…`), Slack (`xoxb-…` and its kin), an AWS
//!   access key (`AKIA…`), a Google API key (`AIza…`);
//! * a sign-in token of the JWT kind (`eyJ….….…`), which Gitea's and
//!   Forgejo's sign-ins use;
//! * 40 hex digits (a Gitea or Forgejo access token) on a line that also
//!   says token, secret, password, key or authorization: 40 hex digits
//!   alone are far more often a commit or a checksum;
//! * a password written into the file: a quoted value given to a name
//!   ending in `password`, `passwd` or `pwd`, or a long mixed value given
//!   to one ending in `token`, `secret` or `key`; an unquoted value given
//!   to a password name (`password: …` in a settings file, `PWD=…;` or
//!   `Password=…;` in a connection string), when it is six characters or
//!   more with a letter and a digit;
//! * an address with a password in it (`scheme://user:password@host`), or
//!   with a long mixed `token=`, `key=`, `secret=` or `sig=` in it;
//! * a line of a `.pgpass` file (`host:port:database:user:password`) and a
//!   sign-in of a `.netrc` file (`machine … login … password …`).
//!
//! Values that are plainly placeholders (`${…}`, `<…>`, `changeme`,
//! `your-…`) do not count, and neither does a value that is code: a value
//! read from the environment (`os.environ["PGPASSWORD"]`), a variable, a
//! call. That is the way to keep a script's password out of the file.

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

/// Languages where a bare word after `=` is a variable, never a value
/// typed in: there, an unquoted password counts only inside a string (a
/// connection string).
const CODE: &[&str] = &[
    "c", "cc", "cjs", "cpp", "cs", "go", "h", "hpp", "ipynb", "java", "jl", "js", "jsx", "kt",
    "lua", "m", "mjs", "php", "pl", "py", "pyt", "pyw", "r", "rb", "rs", "scala", "swift", "ts",
    "tsx", "vb",
];

/// Whether `bytes` read as text: no NUL byte near the start (Git's own
/// test).
pub fn is_text(bytes: &[u8]) -> bool {
    !bytes[..bytes.len().min(8000)].contains(&0)
}

/// Look through one file. `own` are strings that must never leave (the
/// linked server's token).
pub fn find(path: &str, bytes: &[u8], own: &[String]) -> Vec<Finding> {
    let text = String::from_utf8_lossy(bytes);
    let is_text = is_text(bytes);
    let ext = crate::file_kinds::extension(path.rsplit('/').next().unwrap_or(path));
    let code = CODE.contains(&ext.as_str());
    let notebook = ext == "ipynb";
    let mut out = Vec::new();
    // The line a `.netrc` entry began on (`machine <host>`), when its
    // login and password follow on the next lines.
    let mut machine_at: Option<usize> = None;
    for (i, line) in text.split('\n').enumerate() {
        if out.len() >= MAX_PER_FILE {
            break;
        }
        // A notebook keeps each line of code inside a string of its own.
        let unwrapped;
        let line = if notebook {
            unwrapped = notebook_line(line);
            unwrapped.as_str()
        } else {
            line
        };
        let mut what = in_line(line, own, is_text, code);
        if what.is_none() && is_text {
            let words: Vec<&str> = line.split_whitespace().collect();
            if netrc_machine(&words) {
                machine_at = Some(i);
            } else if machine_at.is_some_and(|at| i - at <= 3) && netrc_fields(&words) {
                if netrc_pairs(&words) {
                    what = Some(NETRC);
                }
            } else if !words.is_empty() {
                machine_at = None;
            }
        }
        if let Some(what) = what {
            out.push(Finding {
                path: path.to_string(),
                line: i + 1,
                what: what.to_string(),
            });
        }
    }
    out
}

/// A line of a notebook's code as it was typed: `"x = 1\n",` is `x = 1`.
fn notebook_line(line: &str) -> String {
    let t = line.trim();
    let Some(inner) = t.strip_prefix('"') else {
        return line.to_string();
    };
    let inner = inner.strip_suffix(',').unwrap_or(inner);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    let inner = inner.strip_suffix("\\n").unwrap_or(inner);
    inner.replace("\\\"", "\"")
}

const NETRC: &str = "a sign-in of a password file (.netrc: machine, login, password)";

fn in_line(line: &str, own: &[String], is_text: bool, code: bool) -> Option<&'static str> {
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
    if let Some(what) = known_token(line) {
        return Some(what);
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
        if unquoted_password(line, code) {
            return Some("a password written into the file");
        }
        if url_with_password(line) {
            return Some("an address with a password in it");
        }
        if url_with_secret(line) {
            return Some("an address with a token, key or secret in it");
        }
        if pgpass_line(line) {
            return Some(
                "a line of a password file (.pgpass: host, port, database, user, password)",
            );
        }
        if netrc_line(&line.split_whitespace().collect::<Vec<_>>()) {
            return Some(NETRC);
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

/// The run of characters `body` accepts after each `prefix` that starts a
/// word in `line`.
fn runs_after<'a>(
    line: &'a str,
    prefix: &'a str,
    body: fn(u8) -> bool,
) -> impl Iterator<Item = &'a str> + 'a {
    let bytes = line.as_bytes();
    line.match_indices(prefix).filter_map(move |(start, _)| {
        if start > 0 && word_char(bytes[start - 1]) {
            return None;
        }
        let after = start + prefix.len();
        let run = bytes[after..].iter().take_while(|b| body(**b)).count();
        Some(&line[after..after + run])
    })
}

fn has_letter_and_digit(v: &str) -> bool {
    v.bytes().any(|b| b.is_ascii_digit()) && v.bytes().any(|b| b.is_ascii_alphabetic())
}

/// A token by its well-known start (besides GitHub's, above).
fn known_token(line: &str) -> Option<&'static str> {
    let dashed = |b: u8| word_char(b) || b == b'-';
    if runs_after(line, "sk-ant-", dashed).any(|r| r.len() >= 20 && has_letter_and_digit(r)) {
        return Some("an Anthropic API key");
    }
    if runs_after(line, "sk-", dashed).any(|r| r.len() >= 20 && has_letter_and_digit(r)) {
        return Some("an API key (it starts with sk-)");
    }
    if runs_after(line, "glpat-", dashed).any(|r| r.len() >= 20) {
        return Some("a GitLab token");
    }
    for kind in ["xoxa-", "xoxb-", "xoxp-", "xoxr-", "xoxs-"] {
        if runs_after(line, kind, dashed).any(|r| r.len() >= 10 && has_letter_and_digit(r)) {
            return Some("a Slack token");
        }
    }
    // An AWS access key id: AKIA (or ASIA) and exactly sixteen capitals
    // and digits.
    let capitals = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    for kind in ["AKIA", "ASIA"] {
        let found = line.match_indices(kind).any(|(start, _)| {
            let bytes = line.as_bytes();
            let after = start + kind.len();
            let run = bytes[after..].iter().take_while(|b| capitals(**b)).count();
            let ends = bytes.get(after + run).is_none_or(|b| !word_char(*b));
            (start == 0 || !word_char(bytes[start - 1])) && run == 16 && ends
        });
        if found {
            return Some("an AWS access key");
        }
    }
    if runs_after(line, "AIza", dashed).any(|r| r.len() == 35) {
        return Some("a Google API key");
    }
    None
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

fn identifier(v: &str) -> bool {
    let mut chars = v.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `name = value` or `name: value` with no quotes, `name` one that holds a
/// password: a settings file (`password: …`), a shell line
/// (`export PGPASSWORD=…`), a connection string (`PWD=…;`,
/// `Password=…;`, `password=… dbname=…`). The value counts when it is six
/// characters or more with a letter and a digit, and is not code: in a
/// language where a bare word is a variable (`code`), only a value inside
/// a string counts.
fn unquoted_password(line: &str, code: bool) -> bool {
    let bytes = line.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b'=' && *b != b':' {
            continue;
        }
        let mut end = i;
        while end > 0 && bytes[end - 1] == b' ' {
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
        if !["password", "passwd", "pwd"]
            .iter()
            .any(|w| name.ends_with(w))
        {
            continue;
        }
        let mut at = i + 1;
        while at < bytes.len() && (bytes[at] == b' ' || bytes[at] == b'\t') {
            at += 1;
        }
        let rest = &line[at..];
        // A quoted value is the other rule's.
        if rest.starts_with(['"', '\'']) {
            continue;
        }
        let stop = rest
            .find(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | '&' | '"' | '\'' | ')'))
            .unwrap_or(rest.len());
        let value = &rest[..stop];
        if value.chars().count() < 6 || placeholder(value) || !has_letter_and_digit(value) {
            continue;
        }
        // A path, an address, a call, an index: not a value typed in.
        if value.contains(['/', '\\', '(', '[', ']', '|', '`']) || value.contains("::") {
            continue;
        }
        // `args.password`, `self.pw1`: a field of something.
        let fields: Vec<&str> = value.split('.').collect();
        if fields.len() > 1 && fields.iter().all(|f| identifier(f)) {
            continue;
        }
        if code && identifier(value) {
            let before = &line[..start];
            let in_string =
                before.matches('"').count() % 2 == 1 || before.matches('\'').count() % 2 == 1;
            if !in_string {
                continue;
            }
        }
        return true;
    }
    false
}

/// The names of an address's parts that carry a secret (`?token=…`), with
/// `-` and `_` taken out.
const SECRET_PARTS: &[&str] = &[
    "accesskey",
    "accesstoken",
    "apikey",
    "apisecret",
    "apitoken",
    "authtoken",
    "clientsecret",
    "key",
    "passwd",
    "password",
    "pwd",
    "secret",
    "sig",
    "signature",
    "subscriptionkey",
    "token",
];

/// An address with a long mixed token, key or secret among its parts:
/// `https://host/path?f=json&token=…`.
fn url_with_secret(line: &str) -> bool {
    let mut rest = line;
    while let Some(i) = rest.find("://") {
        rest = &rest[i + 3..];
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(rest.len());
        let address = &rest[..end];
        let Some((_, query)) = address.split_once('?') else {
            continue;
        };
        for part in query.split(['&', ';', '#']) {
            let Some((name, value)) = part.split_once('=') else {
                continue;
            };
            let name: String = name
                .to_lowercase()
                .chars()
                .filter(|c| *c != '-' && *c != '_')
                .collect();
            if SECRET_PARTS.contains(&name.as_str())
                && value.chars().count() >= 16
                && has_letter_and_digit(value)
                && !placeholder(value)
            {
                return true;
            }
        }
    }
    false
}

/// A line of a `.pgpass` file: `host:port:database:user:password`, and
/// nothing else on the line. The host is a name with a dot in it,
/// `localhost` or `*`; the port is a number or `*`.
fn pgpass_line(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.contains(char::is_whitespace) {
        return false;
    }
    let fields: Vec<&str> = line.split(':').collect();
    let [host, port, database, user, password] = fields[..] else {
        return false;
    };
    let host_ok = host == "*"
        || host.eq_ignore_ascii_case("localhost")
        || (host.contains('.')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
            && !host.starts_with('.')
            && !host.ends_with('.'));
    let port_ok =
        port == "*" || ((2..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit()));
    let named = |v: &str| {
        v == "*"
            || v.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    };
    host_ok
        && port_ok
        && named(database)
        && named(user)
        && password.chars().count() >= 3
        && !placeholder(password)
}

/// A value that could be a password: not a stand-in, and with a digit in
/// it (so that prose such as "password reset" is not one).
fn netrc_value(v: &str) -> bool {
    v.chars().count() >= 3 && !placeholder(v) && has_letter_and_digit(v)
}

/// `machine <host>` at the start of a line, the host a name with a dot in
/// it (or `localhost`): where a `.netrc` entry begins.
fn netrc_machine(words: &[&str]) -> bool {
    matches!(words, ["machine", host, ..] if host.contains('.') || *host == "localhost")
}

/// The whole entry on one line: `machine <host> login <user> password
/// <value>` (or `default login … password …`).
fn netrc_line(words: &[&str]) -> bool {
    let rest = match words {
        ["default", rest @ ..] => rest,
        _ if netrc_machine(words) => &words[2..],
        _ => return false,
    };
    netrc_pairs(rest)
}

/// Pairs of a `.netrc` entry and nothing else: `login ana`,
/// `login ana password …`.
fn netrc_fields(words: &[&str]) -> bool {
    !words.is_empty()
        && words.len().is_multiple_of(2)
        && words
            .chunks(2)
            .all(|p| matches!(p[0], "login" | "password" | "account"))
}

/// The same, one of the pairs a password.
fn netrc_pairs(words: &[&str]) -> bool {
    netrc_fields(words)
        && words
            .chunks(2)
            .any(|p| p[0] == "password" && netrc_value(p[1]))
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

    fn hit_in(path: &str, text: &str) -> Option<(usize, String)> {
        find(path, text.as_bytes(), &[])
            .first()
            .map(|f| (f.line, f.what.clone()))
    }

    // More made-up values, put together here for the same reason.
    const PW: &str = "Wint3r2026";
    fn mixed(n: usize) -> String {
        "a1B2c3D4e5F6g7H8i9J0".repeat(n / 20 + 1)[..n].to_string()
    }
    fn anthropic() -> String {
        format!("sk{}{}", "-ant-", mixed(48))
    }
    fn sk() -> String {
        format!("sk{}{}", "-", mixed(48))
    }
    fn gitlab() -> String {
        format!("gl{}{}", "pat-", mixed(20))
    }
    fn slack() -> String {
        format!("xox{}-{}-{}", "b", "123456789012", mixed(24))
    }
    fn aws() -> String {
        format!("AK{}{}", "IA", "Q7ZP2M4X8N6B1V3C")
    }
    fn google() -> String {
        format!("AI{}{}", "za", mixed(35))
    }

    /// The shapes the final review found the scan missed (R-103), each on
    /// its own and with something ordinary around it.
    #[test]
    fn the_wider_scan_stops_these() {
        let (m20, m32) = (mixed(20), mixed(32));
        for (path, text, what) in [
            // An unquoted password in a settings file.
            ("conf/db.yaml", format!("user: gis\npassword: {PW}\n"), "a password"),
            ("conf/db.yml", format!("  db_password: {PW}  # prod"), "a password"),
            ("db.ini", format!("[prod]\npasswd = {PW}"), "a password"),
            ("db.properties", format!("jdbc.pwd={PW}"), "a password"),
            ("notes.md", format!("the login is gis, password: {PW}"), "a password"),
            ("run.sh", format!("export PGPASSWORD={PW}"), "a password"),
            // A password inside a connection string.
            (
                "load.py",
                format!("c = pyodbc.connect('DRIVER={{SQL Server}};SERVER=db;UID=gis;PWD={PW};')"),
                "a password",
            ),
            (
                "load.py",
                format!("c = \"Server=db;Database=gis;User Id=gis;Password={PW};\""),
                "a password",
            ),
            (
                "load.py",
                format!("conn = psycopg2.connect(\"host=db user=gis password={PW} dbname=gis\")"),
                "a password",
            ),
            ("notes.txt", format!("Data Source=db;Pwd={PW}"), "a password"),
            // A .pgpass line and a .netrc entry, pasted into another file.
            (
                "notes.md",
                format!("db.corp.example:5432:gis:gis_reader:{PW}"),
                ".pgpass",
            ),
            ("setup.txt", format!("*:*:*:gis_reader:{PW}"), ".pgpass"),
            ("setup.txt", format!("localhost:5432:gis:gis:{PW}"), ".pgpass"),
            (
                "notes.md",
                format!("machine git.corp.example login ana password {PW}"),
                ".netrc",
            ),
            (
                "notes.md",
                format!("machine git.corp.example\n  login ana\n  password {PW}\n"),
                ".netrc",
            ),
            ("notes.md", format!("default login ana password {PW}"), ".netrc"),
            // Well-known token shapes.
            ("a.py", format!("KEY = os.getenv('X') or {}", anthropic()), "Anthropic"),
            ("a.txt", format!("key {}", sk()), "API key"),
            ("a.txt", format!("gitlab: {}", gitlab()), "GitLab"),
            ("a.txt", format!("slack {}", slack()), "Slack"),
            ("a.txt", format!("aws_access_key_id = {}", aws()), "AWS"),
            ("a.txt", format!("maps key: {}", google()), "Google"),
            // A token, key or secret in an address.
            (
                "fetch.py",
                format!("url = 'https://gis.corp.example/arcgis/rest/services/Roads/FeatureServer/0/query?f=json&token={m32}'"),
                "address",
            ),
            ("fetch.py", format!("u = f\"https://api.example.org/v1/tiles?api_key={m20}&z=3\""), "address"),
            ("notes.md", format!("see https://hooks.example.org/in?secret={m20}"), "address"),
            ("notes.md", format!("https://maps.example.org/js?key={m32}"), "address"),
        ] {
            let got = hit_in(path, &text);
            assert!(
                got.as_ref().is_some_and(|(_, w)| w.contains(what)),
                "{path} should stop for {what}: {text} -> {got:?}"
            );
        }
        // The line is the one that holds it.
        let text = format!("machine git.corp.example\n  login ana\n  password {PW}\n");
        assert_eq!(hit_in("notes.md", &text).unwrap().0, 3);
        let text = format!("user: gis\n\npassword: {PW}\n");
        assert_eq!(hit_in("db.yaml", &text).unwrap().0, 3);
        // And what is said never holds the value found.
        for f in find("db.yaml", text.as_bytes(), &[]) {
            assert!(!format!("{f:?}").contains(PW));
        }
    }

    /// Near misses of each new shape: ordinary code and prose that must not
    /// stop a sync, or keep the assistant from a file.
    #[test]
    fn the_wider_scan_does_not_stop_these() {
        let m20 = mixed(20);
        for (path, text) in [
            // Code that handles a password without holding one.
            ("a.py", "password: str = None".to_string()),
            (
                "a.py",
                "def connect(user: str, password: Optional[str] = None):".to_string(),
            ),
            ("a.py", "password = args.password".to_string()),
            (
                "a.py",
                "password = config[\"db\"][\"password\"]".to_string(),
            ),
            (
                "a.py",
                "conn = connect(user=user, password=pw2026x, host=host)".to_string(),
            ),
            ("a.py", "self.password = password1".to_string()),
            ("a.py", "pwd = os.getcwd()".to_string()),
            (
                "a.py",
                "password = keyring.get_password('columbus', user)".to_string(),
            ),
            ("a.ts", "password: string;".to_string()),
            ("a.rs", "password: &str,".to_string()),
            ("a.sh", "PWD=$(pwd)".to_string()),
            ("a.sh", "pwd=/home/me/data2026".to_string()),
            ("a.sh", "export PGPASSWORD=$1".to_string()),
            ("a.sh", "mysql --password=$DB_PASS1".to_string()),
            (
                "a.py",
                "dsn = f\"host=db user=gis password={password1} dbname=gis\"".to_string(),
            ),
            (
                "a.py",
                "c = 'DRIVER={SQL Server};SERVER=db;UID=gis;PWD=' + pwd1234 + ';'".to_string(),
            ),
            (
                "a.py",
                "c = \"Server=db;User Id=gis;Password=%s;\" % pw".to_string(),
            ),
            (
                "a.cs",
                "var c = \"Server=db;Password=\" + secret1234;".to_string(),
            ),
            // Settings and notes that name a password without giving one.
            ("db.yaml", "password: null".to_string()),
            ("db.yaml", "password: ${DB_PASSWORD}".to_string()),
            ("db.yaml", "password: <your password>".to_string()),
            ("db.yaml", "password: changeme123".to_string()),
            ("db.yaml", "password: required".to_string()),
            ("db.yaml", "password_file: /run/secrets/db1".to_string()),
            ("db.ini", "password =".to_string()),
            ("db.ini", "pwd = abc12".to_string()),
            ("notes.md", "Password: ask Kevin for it".to_string()),
            (
                "notes.md",
                "The password is kept in the credential store, never in a file.".to_string(),
            ),
            (
                "notes.md",
                "Reset your password: https://id.example.org/reset".to_string(),
            ),
            (
                "notes.md",
                "password=PASSWORD123 is only an example".to_string(),
            ),
            // Lines with colons that are not a .pgpass line.
            ("notes.md", "10:30:00:15:45".to_string()),
            ("notes.md", "roads:2024:north:main:line3".to_string()),
            ("notes.md", "fe80:1234:abcd:ef01:2345".to_string()),
            (
                "notes.md",
                "db.corp.example:5432:gis:gis_reader".to_string(),
            ),
            (
                "notes.md",
                "db.corp.example:5432:gis:gis_reader:<password>".to_string(),
            ),
            (
                "notes.md",
                "see http://db.corp.example:5432/gis:public:roads".to_string(),
            ),
            ("a.py", "x = {'a': 1, 'b': 2, 'c': 3, 'd': 4}".to_string()),
            // Words of a .netrc without its shape.
            (
                "notes.md",
                "the machine needs a login and a password".to_string(),
            ),
            (
                "notes.md",
                "machine learning login page password reset".to_string(),
            ),
            ("notes.md", "password required".to_string()),
            (
                "notes.md",
                "login ana\npassword is in the vault".to_string(),
            ),
            (
                "notes.md",
                "machine git.corp.example login ana password <password>".to_string(),
            ),
            // Near the token shapes.
            ("a.py", "from sklearn import sk-learn".to_string()),
            (
                "a.md",
                "sk-means-clustering-of-all-the-points-here".to_string(),
            ),
            ("a.md", format!("sk{}short", "-ant-")),
            ("a.md", format!("task{}{}", "-ant-", mixed(48))),
            ("a.md", format!("gl{}short", "pat-")),
            ("a.md", format!("xox{}-", "b")),
            ("a.md", format!("AK{}SHORT", "IA")),
            ("a.md", format!("MAK{}{}", "IA", "Q7ZP2M4X8N6B1V3C")),
            ("a.md", format!("AK{}{}", "IA", "q7zp2m4x8n6b1v3c")),
            ("a.md", format!("AI{}tooShort", "za")),
            // Addresses whose token, key or secret is a stand-in or short.
            (
                "a.py",
                "u = f\"https://x.example/api?f=json&token={token}\"".to_string(),
            ),
            (
                "a.md",
                "https://x.example/api?token=YOUR_TOKEN_GOES_HERE_1234".to_string(),
            ),
            ("a.md", "https://x.example/api?key=value".to_string()),
            (
                "a.md",
                "https://x.example/list?page_token=abc123&sort_key=name".to_string(),
            ),
            ("a.md", "https://x.example/api?f=json&token=".to_string()),
            (
                "a.md",
                "https://x.example/api?apikey=<your-key>".to_string(),
            ),
            ("a.md", format!("not an address: token={m20}")),
        ] {
            assert_eq!(hit_in(path, &text), None, "{path} should not stop: {text}");
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
