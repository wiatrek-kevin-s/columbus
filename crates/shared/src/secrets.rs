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
//! What counts (D-20; widened by the final review of part 3, R-103, and by
//! its re-check, R-105), kept to what is very likely a secret, so that
//! nobody is stopped for nothing:
//!
//! * the linked server's own access token, anywhere;
//! * a private key (`-----BEGIN … PRIVATE KEY-----`);
//! * a token by its well-known start: GitHub (`ghp_…`, `gho_…`, `ghu_…`,
//!   `ghs_…`, `ghr_…`, `github_pat_…`), Anthropic (`sk-ant-…`), other
//!   `sk-…` keys, GitLab (`glpat-…`), Slack (`xoxb-…` and its kin), an AWS
//!   access key (`AKIA…`), a Google API key (`AIza…`);
//! * a sign-in token of the JWT kind (`eyJ….….…`), which Gitea's and
//!   Forgejo's sign-ins use, and a long mixed token after the word
//!   `Bearer`;
//! * 40 hex digits (a Gitea or Forgejo access token) on a line that also
//!   says token, secret, password, key or authorization: 40 hex digits
//!   alone are far more often a commit or a checksum;
//! * a password given to a name that holds one (a name ending in
//!   `password`, `passwd`, `pwd` or `passphrase`, and `pass` alone or as in
//!   `db_pass`), or a long mixed value given to a name ending in `token`,
//!   `secret` or `key`:
//!   * in quotes, with `=`, `:`, `=>`, `:=` or R's `<-`, the name plain, in
//!     quotes or in a subscript (`cfg["password"] = "…"`,
//!     `os.environ["PGPASSWORD"] = "…"`), with a type between
//!     (`password: str = "…"`) and a letter before the quote (`b"…"`);
//!   * without quotes, when it is six characters or more with a letter and
//!     a digit (sixteen for a token, secret or key): `password: …` in a
//!     settings file or a note (also as `**Password:** …` and
//!     ``Password: `…` ``), `PWD=…;` or `Password=…;` in a connection
//!     string, `export PGPASSWORD=…`, `token: …`, `api_key = …`;
//!   * in a sentence (`the password is …`), in a row of a table whose
//!     heading names it, and between tags (`<password>…</password>`);
//!   * in a database command (`PASSWORD '…'`, `IDENTIFIED BY …`) and on a
//!     command line (`--password …`, `-P …` beside `-U …`, curl's
//!     `-u user:password`);
//! * an address with a password in it (`scheme://user:password@host`), or
//!   with a long mixed `token=`, `key=`, `secret=` or `sig=` in it;
//! * a line of a `.pgpass` file (`host:port:database:user:password`) and a
//!   sign-in of a `.netrc` file (`machine … login … password …`), also
//!   after a comment sign or a list mark.
//!
//! Values that are plainly placeholders (`${…}`, `<…>`, `changeme`,
//! `your-…`) do not count, and neither does a value that is a path, a
//! column's type (`varchar2`) or code: a value read from the environment
//! (`os.environ["PGPASSWORD"]`), a variable, a call. That is the way to
//! keep a script's password out of the file. In a script, a bare word
//! after `=` is a variable; in its comments and docstrings, as in a note,
//! it is something the person typed.

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

/// How a language marks a person's own words in a line of code: its
/// comment signs, and strings that run over several lines.
#[derive(Debug, Clone, Copy, Default)]
struct Lang {
    /// `# …`
    hash: bool,
    /// `// …` and `/* … */`
    slashes: bool,
    /// `-- …`
    dashes: bool,
    /// `% …`
    percent: bool,
    /// `' …` (Visual Basic, whose strings are in double quotes only)
    apostrophe: bool,
    /// `"""…"""` and `'''…'''`, Python's docstrings
    triple: bool,
    /// `` `…` `` is a string
    backtick: bool,
}

/// Languages where a bare word after `=` is a variable, never a value
/// typed in: there, an unquoted password counts only inside a string (a
/// connection string) or among the person's own words (a comment, a
/// docstring). `None` for every other file, which is all the person's own
/// words: a note, a settings file, a shell script.
fn lang(ext: &str) -> Option<Lang> {
    let none = Lang::default();
    Some(match ext {
        "ipynb" | "py" | "pyt" | "pyw" => Lang {
            hash: true,
            triple: true,
            ..none
        },
        "jl" | "pl" | "r" | "rb" => Lang { hash: true, ..none },
        "php" => Lang {
            hash: true,
            slashes: true,
            ..none
        },
        "c" | "cc" | "cpp" | "cs" | "h" | "hpp" | "java" | "kt" | "rs" | "scala" | "swift" => {
            Lang {
                slashes: true,
                ..none
            }
        }
        "cjs" | "go" | "js" | "jsx" | "mjs" | "ts" | "tsx" => Lang {
            slashes: true,
            backtick: true,
            ..none
        },
        "lua" => Lang {
            dashes: true,
            ..none
        },
        "m" => Lang {
            percent: true,
            slashes: true,
            ..none
        },
        "vb" => Lang {
            apostrophe: true,
            ..none
        },
        _ => return None,
    })
}

/// What the lines before left open in a file of code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Open {
    #[default]
    Nothing,
    /// A string over several lines (a docstring), until this sign comes
    /// again.
    Text(&'static str),
    /// A `/* … */` comment.
    Comment,
}

/// Where on a line a person's own words are. In a note or a settings file
/// that is all of it. In a line of code it is the comments and the
/// docstrings: elsewhere there, a bare word after `=` is a variable.
#[derive(Debug)]
enum Prose {
    All,
    /// The parts of a line of code that are comments or docstrings, as
    /// byte ranges.
    Parts(Vec<(usize, usize)>),
}

impl Prose {
    /// Whether the byte at `i` is among the person's own words.
    fn at(&self, i: usize) -> bool {
        match self {
            Prose::All => true,
            Prose::Parts(parts) => parts.iter().any(|(from, to)| (*from..*to).contains(&i)),
        }
    }
}

/// Where `sign` next is in `bytes` at or after `from`.
fn find_from(bytes: &[u8], from: usize, sign: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(sign.len())
        .position(|w| w == sign)
        .map(|i| from + i)
}

/// The parts of a line of code that are a person's own words: its
/// comments, and what lies inside a string that runs over several lines.
/// `open` is what the lines before left open, and is brought up to date.
/// A sign inside an ordinary string is not a comment. Where a line cannot
/// be made out (a quote that never closes), less is taken for words, never
/// more.
fn words_in_code(line: &str, lang: Lang, open: &mut Open) -> Vec<(usize, usize)> {
    let b = line.as_bytes();
    let mut parts = Vec::new();
    let mut i = 0;
    let ends = match *open {
        Open::Nothing => None,
        Open::Text(sign) => Some(sign),
        Open::Comment => Some("*/"),
    };
    if let Some(sign) = ends {
        match find_from(b, 0, sign.as_bytes()) {
            Some(j) => {
                parts.push((0, j));
                i = j + sign.len();
                *open = Open::Nothing;
            }
            None => {
                parts.push((0, b.len()));
                return parts;
            }
        }
    }
    while i < b.len() {
        let c = b[i];
        let rest = &b[i..];
        if lang.triple && (rest.starts_with(b"\"\"\"") || rest.starts_with(b"'''")) {
            let sign: &'static str = if c == b'"' { "\"\"\"" } else { "'''" };
            match find_from(b, i + 3, sign.as_bytes()) {
                Some(j) => {
                    parts.push((i + 3, j));
                    i = j + 3;
                }
                None => {
                    parts.push((i + 3, b.len()));
                    *open = Open::Text(sign);
                    break;
                }
            }
        } else if lang.apostrophe && c == b'\'' {
            parts.push((i + 1, b.len()));
            break;
        } else if c == b'"' || c == b'\'' || (lang.backtick && c == b'`') {
            // An ordinary string: to its end, or the line's.
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
        } else if (lang.hash && c == b'#') || (lang.percent && c == b'%') {
            parts.push((i + 1, b.len()));
            break;
        } else if (lang.slashes && rest.starts_with(b"//"))
            || (lang.dashes && rest.starts_with(b"--"))
        {
            parts.push((i + 2, b.len()));
            break;
        } else if lang.slashes && rest.starts_with(b"/*") {
            match find_from(b, i + 2, b"*/") {
                Some(j) => {
                    parts.push((i + 2, j));
                    i = j + 2;
                }
                None => {
                    parts.push((i + 2, b.len()));
                    *open = Open::Comment;
                    break;
                }
            }
        } else {
            i += 1;
        }
    }
    parts
}

/// Where a line of a notebook (`.ipynb`) is: in the code of a code cell,
/// or anywhere else (a text cell, what a cell printed, the file's own
/// structure), which is read as the person's own words.
#[derive(Debug, Default)]
struct Notebook {
    code_cell: bool,
    in_source: bool,
}

impl Notebook {
    /// Take in the next line of the file as it is saved, and say whether
    /// it is a line of a code cell's code.
    fn code(&mut self, raw: &str) -> bool {
        let t = raw.trim();
        if let Some(kind) = t.strip_prefix("\"cell_type\"") {
            self.code_cell = kind.contains("\"code\"");
            self.in_source = false;
            return false;
        }
        if t.starts_with("\"source\"") {
            // `"source": [` opens the cell's lines; anything else has them
            // all on this line.
            self.in_source = t.ends_with('[');
            return false;
        }
        if self.in_source && t.starts_with(']') {
            self.in_source = false;
            return false;
        }
        self.in_source && self.code_cell
    }
}

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
    let lang = lang(&ext);
    let is_notebook = ext == "ipynb";
    let mut out = Vec::new();
    // The line a `.netrc` entry began on (`machine <host>`), when its
    // login and password follow on the next lines.
    let mut machine_at: Option<usize> = None;
    let mut open = Open::Nothing;
    let mut notebook = Notebook::default();
    let mut table = Table::default();
    for (i, line) in text.split('\n').enumerate() {
        if out.len() >= MAX_PER_FILE {
            break;
        }
        // A notebook keeps each line of a cell inside a string of its own.
        let unwrapped;
        let (line, code) = if is_notebook {
            let code = notebook.code(line);
            if !code {
                // A docstring does not run on from one cell into the next.
                open = Open::Nothing;
            }
            unwrapped = notebook_line(line);
            (unwrapped.as_str(), code)
        } else {
            (line, true)
        };
        let prose = match lang {
            Some(lang) if code => Prose::Parts(words_in_code(line, lang, &mut open)),
            _ => Prose::All,
        };
        // Every line is shown to the table, so that it knows its rows.
        let in_a_table = is_text && table.row(line);
        let plain = undecorated(line);
        let mut what = in_line(line, plain, own, is_text, &prose);
        if what.is_none() && in_a_table {
            what = Some(IN_A_TABLE);
        }
        if what.is_none() && is_text {
            let words: Vec<&str> = plain.split_whitespace().collect();
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
const QUOTED: &str = "a password or key written into the file (a quoted value)";
const PASSWORD: &str = "a password written into the file";
const KEY: &str = "a token, key or secret written into the file";
const BETWEEN_TAGS: &str = "a password or key written into the file (between tags)";
const IN_A_TABLE: &str = "a password or key written into the file (in a table)";
const IN_A_COMMAND: &str = "a password written into the file (in a database command)";
const ON_A_COMMAND_LINE: &str = "a password written into the file (on a command line)";

/// What one line looks like it holds, if anything. `plain` is the line
/// without what stands in front of it and around it ([`undecorated`]);
/// `prose` says where on it the person's own words are.
fn in_line(
    line: &str,
    plain: &str,
    own: &[String],
    is_text: bool,
    prose: &Prose,
) -> Option<&'static str> {
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
            return Some(QUOTED);
        }
        // Names given a value with no quotes around the name.
        let bare = given(line, false);
        if unquoted_password(line, &bare, prose) {
            return Some(PASSWORD);
        }
        if url_with_password(line) {
            return Some("an address with a password in it");
        }
        if url_with_secret(line) {
            return Some("an address with a token, key or secret in it");
        }
        if unquoted_key(line, &bare, prose) {
            return Some(KEY);
        }
        if said_in_words(line, prose) {
            return Some(PASSWORD);
        }
        if between_tags(line) {
            return Some(BETWEEN_TAGS);
        }
        if let Some(what) = after_a_space(line) {
            return Some(what);
        }
        if identified_by(line) {
            return Some(IN_A_COMMAND);
        }
        if on_a_command_line(line) {
            return Some(ON_A_COMMAND_LINE);
        }
        if bearer(line) {
            return Some("an access token (after the word Bearer)");
        }
        if pgpass_line(plain) {
            return Some(
                "a line of a password file (.pgpass: host, port, database, user, password)",
            );
        }
        if netrc_line(&plain.split_whitespace().collect::<Vec<_>>()) {
            return Some(NETRC);
        }
    }
    None
}

/// A line without what a script or a note puts in front of it and around
/// it: a comment sign (`#`, `//`, `--`, `;`, `%`, `*`), a list or quote
/// mark (`-`, `*`, `+`, `>`), and the marks that set it as code
/// (`` ` ``). A list mark needs a space after it: `*:*:*:gis:…` is a
/// `.pgpass` line that starts with `*`.
fn undecorated(line: &str) -> &str {
    let mut t = line.trim();
    // A few signs, one inside the other, at most: `# - `…``.
    for _ in 0..4 {
        let before = t;
        for sign in ["#", "//", "/*", "--", ";", "%", ">", "- ", "* ", "+ "] {
            t = t.strip_prefix(sign).unwrap_or(t).trim_start();
        }
        t = t.strip_suffix("*/").unwrap_or(t).trim_end();
        t = t.trim_matches('`').trim();
        if t == before {
            break;
        }
    }
    t
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

/// Words that say what kind of thing a password field holds or how it is
/// kept, not a password: a column's type, a way of hashing or encoding.
const KIND_WORDS: &[&str] = &[
    "argon2",
    "argon2i",
    "argon2id",
    "base32",
    "base64",
    "bcrypt",
    "bigint",
    "binary",
    "boolean",
    "bytea",
    "char",
    "character",
    "int2",
    "int4",
    "int8",
    "integer",
    "latin1",
    "md5",
    "nchar",
    "number",
    "numeric",
    "nvarchar",
    "nvarchar2",
    "pbkdf2",
    "scrypt",
    "sha1",
    "sha2",
    "sha224",
    "sha256",
    "sha3",
    "sha384",
    "sha512",
    "smallint",
    "string",
    "text",
    "utf16",
    "utf8",
    "utf8mb4",
    "varbinary",
    "varchar",
    "varchar2",
];

/// A value that names a kind, not a password: `varchar2`, `nvarchar(64)`,
/// `sha256-hashed`. The word must be all of it, but for a length in
/// brackets or one more plain word: `base64:…` followed by a value is a
/// value.
fn kind_word(v: &str) -> bool {
    let lower = v.to_ascii_lowercase();
    let word = lower
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();
    if !KIND_WORDS.contains(&&lower[..word]) {
        return false;
    }
    let rest = &lower[word..];
    let length = rest
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .is_some_and(|inner| {
            inner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b',' || b == b' ')
        });
    let one_more_word = rest
        .strip_prefix(['-', '_'])
        .is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()));
    rest.is_empty() || length || one_more_word
}

/// A value that is a path: it starts as paths do (`~/`, `./`, `../`,
/// `C:\`, `\\server`), or with `/` and goes through a folder at least
/// (`/home/ana`; `/x9` alone could be a password).
fn path_like(v: &str) -> bool {
    let b = v.as_bytes();
    let drive =
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    let home_or_here = ["~/", "~\\", "./", ".\\", "../", "..\\", "\\\\"]
        .iter()
        .any(|p| v.starts_with(p));
    drive || home_or_here || (v.starts_with('/') && v.matches('/').count() >= 2)
}

/// What a name holds, by how it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// `password`, `db_password`, `PGPASSWORD`, `passwd`, `pwd`,
    /// `passphrase`.
    Password,
    /// `pass`, `db_pass`: short for password, and a word with other
    /// meanings, so its value must look like a password even in quotes.
    Pass,
    /// `token`, `client_secret`, `api_key`: its value must be long and
    /// mixed.
    Key,
}

/// What `pass` is the password of, where it is short for password:
/// `db_pass`, `DB_PASS`, `userpass`. Not `first_pass`, `bypass`, `compass`.
const PASS_OF: &[&str] = &[
    "admin", "auth", "database", "db", "ftp", "gis", "login", "mysql", "ora", "oracle", "pg",
    "portal", "postgres", "root", "sde", "smtp", "sql", "ssh", "user",
];

/// What the name `name` holds, if it holds a password or a key. Letter
/// case, and `-` or `_` around it (`--password`, `__password__`), do not
/// matter.
fn holds(name: &str) -> Option<Holds> {
    let lower = name.to_lowercase();
    let name = lower.trim_matches(['-', '_']);
    if ["password", "passwd", "pwd", "passphrase"]
        .iter()
        .any(|w| name.ends_with(w))
    {
        return Some(Holds::Password);
    }
    if let Some(of) = name.strip_suffix("pass") {
        let of = of.trim_end_matches(['-', '_']);
        let of = of.rsplit(['-', '_']).next().unwrap_or(of);
        if of.is_empty() || PASS_OF.contains(&of) {
            return Some(Holds::Pass);
        }
    }
    [
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
    .any(|w| name.ends_with(w))
    .then_some(Holds::Key)
}

/// A place on a line where a name that holds a password or a key is given
/// a value.
struct Given {
    /// Where the name starts.
    start: usize,
    holds: Holds,
    /// Where the value starts: right after the sign.
    after: usize,
    /// Where it starts when a type stands between the name and its `=`
    /// (`password: str = …`): after that `=`. Only a value in quotes
    /// counts there; a bare one is a number or a variable
    /// (`MAX_PASSWORD: u32 = 256u32`).
    after_type: Option<usize>,
}

/// Every place on a line where a name that holds a password or a key is
/// given a value with `=`, `:` (and so `=>`, `:=`, `==`) or R's `<-`.
/// The name stands right before the sign, but for spaces and for what a
/// note puts around a name to set it off (`**Password**:`,
/// `` `password`: ``). `quoted` also takes a name in quotes or in a
/// subscript: `"password": …`, `cfg["password"] = …`,
/// `os.environ["PGPASSWORD"] = …`.
fn given(line: &str, quoted: bool) -> Vec<Given> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        let arrow = *b == b'<' && bytes.get(i + 1) == Some(&b'-');
        if *b != b'=' && *b != b':' && !arrow {
            continue;
        }
        let mut end = i;
        if arrow && end > 0 && bytes[end - 1] == b'<' {
            // R's `<<-`.
            end -= 1;
        }
        // Back over spaces, and over what closes a name (a few at most).
        let mut closers = 0;
        while end > 0 {
            let c = bytes[end - 1];
            let closes = matches!(c, b'*' | b'`') || (quoted && matches!(c, b'"' | b'\'' | b']'));
            if c != b' ' && c != b'\t' && !(closes && closers < 6) {
                break;
            }
            closers += usize::from(closes);
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (word_char(bytes[start - 1]) || bytes[start - 1] == b'-') {
            start -= 1;
        }
        if start == end {
            continue;
        }
        let Some(holds) = holds(&line[start..end]) else {
            continue;
        };
        let after = i + if arrow { 2 } else { 1 };
        let after_type = if *b == b':' {
            after_a_type(line, after)
        } else {
            None
        };
        out.push(Given {
            start,
            holds,
            after,
            after_type,
        });
    }
    out
}

/// `password: str = "…"`: where the value starts when a type stands
/// between the name and its `=`. `from` is right after the `:`. A type is
/// a few plain words, with brackets (`Optional[str]`, `&'static str`); a
/// comma outside brackets ends a parameter, so what follows is another
/// one's value.
fn after_a_type(line: &str, from: usize) -> Option<usize> {
    let eq = line.as_bytes()[from..]
        .iter()
        .take(60)
        .position(|b| *b == b'=')?;
    let kind = line[from..from + eq].trim();
    if kind.is_empty() || kind.split_whitespace().count() > 4 {
        return None;
    }
    let mut depth = 0;
    for c in kind.chars() {
        match c {
            '[' | '<' => depth += 1,
            ']' | '>' => depth -= 1,
            ',' if depth > 0 => {}
            c if c.is_ascii_alphanumeric() => {}
            '_' | '.' | ' ' | '|' | '&' | '?' | '*' | '\'' => {}
            _ => return None,
        }
    }
    let after = from + eq + 1;
    // `==` and `=>` are not what gives a typed name its value.
    (!matches!(line.as_bytes().get(after), Some(b'=') | Some(b'>'))).then_some(after)
}

/// Whether `value`, found in quotes or between tags, counts as a password
/// or a key for a name that `holds` one.
fn typed_in_quotes(value: &str, holds: Holds) -> bool {
    if value.chars().any(char::is_whitespace)
        || placeholder(value)
        || path_like(value)
        || kind_word(value)
    {
        return false;
    }
    let n = value.chars().count();
    let mixed =
        value.chars().any(|c| c.is_ascii_digit()) && value.chars().any(|c| c.is_alphabetic());
    match holds {
        Holds::Password => n >= 6,
        Holds::Pass => n >= 6 && mixed,
        Holds::Key => n >= 16 && mixed,
    }
}

/// The value in quotes that starts at or after `at`, behind spaces, the
/// rest of the sign (`=>`, `:=`), a note's `**`, and a letter or two that
/// say what kind of string it is (`b"…"`, `N'…'`). `None` when no quoted
/// value starts there.
fn quoted_value(line: &str, mut at: usize) -> Option<&str> {
    let bytes = line.as_bytes();
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t' | b'=' | b'>' | b':' | b'*') {
        at += 1;
    }
    let mut q = at;
    while q < bytes.len() && q - at < 2 && bytes[q].is_ascii_alphabetic() {
        q += 1;
    }
    let quote = bytes
        .get(q)
        .copied()
        .filter(|c| *c == b'"' || *c == b'\'')?;
    let rest = &line[q + 1..];
    let close = rest.find(quote as char)?;
    Some(&rest[..close])
}

/// `name = "value"` (or `:`, `=>`, `<-`), with `name` one that holds a
/// password or a key ([`given`]).
fn password_assignment(line: &str) -> bool {
    given(line, true).iter().any(|g| {
        [Some(g.after), g.after_type]
            .into_iter()
            .flatten()
            .any(|at| quoted_value(line, at).is_some_and(|v| typed_in_quotes(v, g.holds)))
    })
}

fn identifier(v: &str) -> bool {
    let mut chars = v.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `args.password`, `self.pw1`: a field of something.
fn field_of_something(v: &str) -> bool {
    let fields: Vec<&str> = v.split('.').collect();
    fields.len() > 1 && fields.iter().all(|f| identifier(f))
}

/// Whether the text before `at` leaves a string open, counted roughly: an
/// odd number of quotes of one kind.
fn in_a_string(line: &str, at: usize) -> bool {
    let before = &line[..at];
    before.matches('"').count() % 2 == 1 || before.matches('\'').count() % 2 == 1
}

/// The value with no quotes around it that starts at or after `at`,
/// without what a note puts around a value to set it off (`**…**`,
/// `` `…` ``) and without the `**` that closes the name's own emphasis
/// (`**Password:** …`). `None` where a quoted value starts: that is the
/// other rule's.
fn bare_value(line: &str, mut at: usize) -> Option<&str> {
    let bytes = line.as_bytes();
    let blank = |b: u8| b == b' ' || b == b'\t';
    while at < bytes.len() && blank(bytes[at]) {
        at += 1;
    }
    let marks = bytes[at..].iter().take_while(|b| **b == b'*').count();
    if marks > 0 && bytes.get(at + marks).is_some_and(|b| blank(*b)) {
        at += marks;
        while at < bytes.len() && blank(bytes[at]) {
            at += 1;
        }
    }
    let rest = &line[at..];
    if rest.starts_with(['"', '\'']) {
        return None;
    }
    if let Some(code) = rest.strip_prefix('`') {
        // Set as code in a note. (In a shell script the same marks run a
        // command: more than one word, which is no password.)
        return Some(&code[..code.find('`')?]);
    }
    let stop = rest
        .find(|c: char| {
            c.is_whitespace() || matches!(c, ';' | ',' | '&' | '"' | '\'' | ')' | '`' | '|' | '<')
        })
        .unwrap_or(rest.len());
    let mut value = &rest[..stop];
    // `**value**`. One `*` in front and none behind is something else (a
    // YAML alias), and is left on, which makes the value a placeholder.
    let lead = value.bytes().take_while(|b| *b == b'*').count();
    let trail = value.bytes().rev().take_while(|b| *b == b'*').count();
    if lead > 0 && trail >= lead && value.len() > lead + trail {
        value = &value[lead..];
    }
    Some(value.trim_end_matches('*'))
}

/// Whether `value`, found without quotes, counts as a password typed in:
/// six characters or more with a letter and a digit, and not a
/// placeholder, a kind, a path, an address, a call, an index or a field.
fn typed_in_bare(value: &str) -> bool {
    value.chars().count() >= 6
        && has_letter_and_digit(value)
        && !value.chars().any(char::is_whitespace)
        && !placeholder(value)
        && !kind_word(value)
        && !value.contains(['/', '\\', '(', '[', ']', '|', '`'])
        && !value.contains("::")
        && !field_of_something(value)
}

/// Whether a bare word at the name that starts at `start` is a variable,
/// not a value: it is in a line of code, outside its strings, comments and
/// docstrings.
fn a_variable(line: &str, start: usize, value: &str, prose: &Prose) -> bool {
    identifier(value) && !prose.at(start) && !in_a_string(line, start)
}

/// `name = value` or `name: value` with no quotes, `name` one that holds a
/// password: a settings file or a note (`password: …`, `**Password:** …`,
/// ``Password: `…` ``), a shell line (`export PGPASSWORD=…`), a connection
/// string (`PWD=…;`, `Password=…;`, `password=… dbname=…`). The value
/// counts when it is six characters or more with a letter and a digit, and
/// is not code: in a language where a bare word is a variable, only a
/// value inside a string, a comment or a docstring counts.
fn unquoted_password(line: &str, given: &[Given], prose: &Prose) -> bool {
    given.iter().filter(|g| g.holds != Holds::Key).any(|g| {
        bare_value(line, g.after)
            .is_some_and(|v| typed_in_bare(v) && !a_variable(line, g.start, v, prose))
    })
}

/// Words a shell or a batch file puts before a setting.
const SETS: &[&str] = &[
    "declare", "env", "export", "local", "readonly", "set", "setx", "typeset",
];

/// Whether the name at `start` begins a setting, rather than being a word
/// in a sentence: it is first on its line (after a list or comment sign,
/// or a word such as `export`), is a flag (`--token=…`), or comes straight
/// after a bracket, a quote or a sign that stands between settings (`&`,
/// `;`, `,`, `?`).
fn begins_a_setting(line: &str, start: usize) -> bool {
    let bytes = line.as_bytes();
    if bytes[start] == b'-' {
        return true;
    }
    // The whole name, with what it belongs to: `arcgis.api_key`.
    let mut s = start;
    while s > 0 && (word_char(bytes[s - 1]) || bytes[s - 1] == b'.') {
        s -= 1;
    }
    let before = line[..s].trim_end();
    let Some(last) = before.chars().last() else {
        return true;
    };
    if matches!(
        last,
        '"' | '\''
            | '`'
            | '&'
            | '?'
            | ';'
            | ','
            | '{'
            | '('
            | '['
            | '|'
            | '>'
            | '*'
            | '#'
            | '+'
            | '-'
            | '/'
    ) {
        return true;
    }
    let word = before.rsplit(char::is_whitespace).next().unwrap_or("");
    SETS.contains(&word.to_ascii_lowercase().as_str())
        && before[..before.len() - word.len()].trim().is_empty()
}

/// A token or a key that is a path to one: `/run/secrets/x`,
/// `keys/deploy.pem`, `secrets/prod/key-2`. Unlike a password, a key may
/// hold `/` itself (base64), so only what reads as a path is one: it
/// starts as paths do, ends in a file's name, or has no capital in it.
fn path_to_a_key(v: &str) -> bool {
    if !v.contains(['/', '\\']) {
        return false;
    }
    let last = v.rsplit(['/', '\\']).next().unwrap_or(v);
    let file = last.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.bytes().all(|b| b.is_ascii_alphanumeric())
    });
    path_like(v) || file || !v.bytes().any(|b| b.is_ascii_uppercase())
}

/// Whether `value`, found without quotes, counts as a token or a key
/// typed in: sixteen characters or more with a letter and a digit, and not
/// a placeholder, a stand-in that names itself (`access_token_123`), an
/// address or a path.
fn typed_in_bare_key(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    value.chars().count() >= 16
        && has_letter_and_digit(value)
        && !value.chars().any(char::is_whitespace)
        && !placeholder(value)
        && !["token", "apikey", "api_key", "api-key"]
            .iter()
            .any(|w| lower.contains(w))
        && !value.contains("://")
        && !path_to_a_key(value)
}

/// `token: …`, `api_key = …`, `client_secret: …` with no quotes, where a
/// setting begins ([`begins_a_setting`]): a settings file, a shell line
/// (`export ARCGIS_API_KEY=…`), a request's line (`X-Api-Key: …`). The
/// value counts when it is sixteen characters or more with a letter and a
/// digit, and is not a placeholder, a path, an address or code.
fn unquoted_key(line: &str, given: &[Given], prose: &Prose) -> bool {
    given
        .iter()
        .filter(|g| g.holds == Holds::Key && begins_a_setting(line, g.start))
        .any(|g| {
            bare_value(line, g.after).is_some_and(|v| {
                typed_in_bare_key(v)
                    && !v.contains(['(', '[', ']', '{', '}', '`'])
                    && !v.contains("::")
                    && !field_of_something(v)
                    && !a_variable(line, g.start, v, prose)
            })
        })
}

/// A sentence that says it: `the password is …` (also `was`, and with a
/// colon after). What follows counts as an unquoted value does, without
/// the full stop and the marks around it.
fn said_in_words(line: &str, prose: &Prose) -> bool {
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for name in ["password", "passwd", "passphrase", "pwd"] {
        for (i, _) in lower.match_indices(name) {
            // Past the marks that close the name's emphasis, then the verb.
            let mut at = i + name.len();
            while at < bytes.len() && matches!(bytes[at], b'*' | b'`' | b'"' | b'\'') {
                at += 1;
            }
            let Some(verb) = [" is", " was"]
                .iter()
                .find(|v| lower[at..].starts_with(**v))
            else {
                continue;
            };
            at += verb.len();
            if bytes.get(at) == Some(&b':') {
                at += 1;
            }
            if !matches!(bytes.get(at), Some(b' ') | Some(b'\t')) {
                continue;
            }
            let word = line[at..].split_whitespace().next().unwrap_or("");
            let word = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
            // "…" around one word are marks; around several, the first
            // word of a phrase is no value.
            let open = word
                .chars()
                .next()
                .filter(|c| matches!(c, '"' | '\'' | '`'));
            if open.is_some_and(|q| word.len() < 2 || !word.ends_with(q)) {
                continue;
            }
            let value = word.trim_matches(['"', '\'', '`', '*']);
            if typed_in_bare(value) && !a_variable(line, i, value, prose) {
                return true;
            }
        }
    }
    false
}

/// How far a tag's end, and the end of what a password's tag holds, is
/// looked for, in bytes.
const NEAR: usize = 300;

/// `<password>…</password>`: what stands between a tag whose name holds a
/// password or a key and the tag that closes it, counted as a quoted value
/// is.
fn between_tags(line: &str) -> bool {
    let mut rest = line;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        // `db:Password encrypted="false"`: the name, without what it
        // belongs to.
        let name = rest
            .bytes()
            .take_while(|b| word_char(*b) || matches!(b, b':' | b'-' | b'.'))
            .count();
        let name = &rest[..name];
        let Some(holds) = holds(name.rsplit(':').next().unwrap_or(name)) else {
            continue;
        };
        // A tag is short; so is what a password's tag holds.
        let near = |text: &str, sign: u8| text.bytes().take(NEAR).position(|b| b == sign);
        let Some(close) = near(rest, b'>') else {
            continue;
        };
        let tag = &rest[..close];
        let after = &rest[close + 1..];
        if tag.ends_with('/') || tag.contains('<') {
            continue;
        }
        let text = match after.strip_prefix("<![CDATA[") {
            Some(data) => match near(data, b']') {
                Some(end) if data[end..].starts_with("]]></") => &data[..end],
                _ => continue,
            },
            None => match near(after, b'<') {
                Some(end) if after[end..].starts_with("</") => &after[..end],
                _ => continue,
            },
        };
        if typed_in_quotes(text.trim(), holds) {
            return true;
        }
    }
    false
}

/// What a command that makes or changes a database user says.
const DATABASE_USER: &[&str] = &[
    "alter login",
    "alter role",
    "alter user",
    "connect to",
    "create login",
    "create role",
    "create user",
    "grant ",
    "set password",
];

fn database_command(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    DATABASE_USER.iter().any(|w| lower.contains(w))
}

/// A password after its name with only a space between, in quotes: a
/// database command's `PASSWORD '…'` (`CREATE USER … WITH PASSWORD '…'`),
/// a command line's `--password "…"`, a build file's `password "…"`. The
/// first two say plainly that a password follows; anywhere else the value
/// must also have a letter and a digit, since words in quotes follow the
/// word password in ordinary sentences too.
fn after_a_space(line: &str) -> Option<&'static str> {
    let bytes = line.as_bytes();
    let command = database_command(line);
    for (q, quote) in bytes.iter().enumerate() {
        if *quote != b'"' && *quote != b'\'' {
            continue;
        }
        // Back over a letter that says what kind of string it is
        // (`N'…'`, `E'…'`), then the space, then the name.
        let mut end = q;
        if end > 1 && bytes[end - 1].is_ascii_alphabetic() && bytes[end - 2] == b' ' {
            end -= 1;
        }
        if end == 0 || !matches!(bytes[end - 1], b' ' | b'\t') {
            continue;
        }
        while end > 0 && matches!(bytes[end - 1], b' ' | b'\t') {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (word_char(bytes[start - 1]) || bytes[start - 1] == b'-') {
            start -= 1;
        }
        let flag = bytes[start] == b'-';
        // (`pass` only as a flag: in a sentence it is a verb, "pass 'int64'
        // to …".)
        let Some(holds) = holds(&line[start..end])
            .filter(|h| *h == Holds::Password || (flag && *h == Holds::Pass))
        else {
            continue;
        };
        let rest = &line[q + 1..];
        let Some(close) = rest.find(*quote as char) else {
            continue;
        };
        let value = &rest[..close];
        let sure = flag || command;
        if typed_in_quotes(value, holds) && (sure || has_letter_and_digit(value)) {
            return Some(if flag {
                ON_A_COMMAND_LINE
            } else if command {
                IN_A_COMMAND
            } else {
                PASSWORD
            });
        }
    }
    None
}

/// `IDENTIFIED BY …` in a command that makes or changes a database user:
/// in quotes, or bare (as Oracle takes it) when it has a letter and a
/// digit. A value handed in from outside (`:pw`, `&pw`) is not one.
fn identified_by(line: &str) -> bool {
    if !database_command(line) {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    let Some(by) = lower
        .find("identified")
        .and_then(|i| lower[i..].find(" by ").map(|j| i + j + 4))
    else {
        return false;
    };
    let rest = line[by..].trim_start();
    match rest.chars().next() {
        Some(quote @ ('"' | '\'')) => rest[1..]
            .find(quote)
            .is_some_and(|close| typed_in_quotes(&rest[1..1 + close], Holds::Password)),
        _ => {
            let stop = rest
                .find(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | ')' | '"' | '\''))
                .unwrap_or(rest.len());
            let value = &rest[..stop];
            typed_in_bare(value) && !value.starts_with([':', '&', '$', '?', '@'])
        }
    }
}

/// Flags that name the user on a command line.
const USER_FLAGS: &[&str] = &[
    "--login",
    "--uid",
    "--user",
    "--username",
    "-U",
    "-u",
    "-user",
    "-username",
];

/// A password given on a command line, as the words of a command or as
/// the items of a list (`["sqlcmd", "-U", "gis", "-P", "…"]`):
///
/// * after a flag whose name holds a password (`--password …`,
///   `--db-password …`; `--password=…` and `--password "…"` are the other
///   rules');
/// * after `-P` or `-p`, on a line that also names the user (`-U gis`).
///   By themselves the two letters mean too many things (`mkdir -p`,
///   `ssh -p 22`, `rsync -P`), except after `sshpass`, whose `-p` is the
///   password;
/// * as curl's `-u user:password`.
///
/// What follows a flag counts when it is not another flag and reads as a
/// password typed in ([`typed_in_bare`]).
fn on_a_command_line(line: &str) -> bool {
    if !line.contains('-') {
        return false;
    }
    // Each word, with whether it stood in quotes of its own.
    let words: Vec<(bool, &str)> = line
        .split(|c: char| c.is_whitespace() || c == '(' || c == '[')
        .filter(|w| !w.is_empty())
        .map(|w| {
            (
                w.starts_with(['"', '\'', '`']),
                w.trim_matches(['"', '\'', '`', ',', ']', ')', ';']),
            )
        })
        .collect();
    // What follows the flag at `k`, when it is a value: never another
    // flag, and after a flag that is an item of a list (in quotes) only
    // another such item: a bare word there is a variable or another
    // argument.
    let value_of = |k: usize| -> Option<&str> {
        let (quoted, value) = *words.get(k + 1)?;
        ((quoted || !words[k].0) && !value.is_empty() && !value.starts_with('-')).then_some(value)
    };
    // A user's name is no file and no path: `python -u run.py` is not one.
    let names_a_user = |v: &str| {
        let file = v.rsplit_once('.').is_some_and(|(_, ext)| {
            (1..=4).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphabetic())
        });
        !v.contains(['/', ':']) && (!file || v.contains('@'))
    };
    let user = words.iter().enumerate().any(|(k, (_, w))| {
        (USER_FLAGS.contains(w) && value_of(k).is_some_and(names_a_user))
            || ["--user=", "--username="].iter().any(|f| w.starts_with(f))
    });
    let lower = line.to_ascii_lowercase();
    let names = |tools: &[&str]| tools.iter().any(|t| lower.contains(t));
    // Tools that use the two letters for something else: after MySQL's
    // `-p` and a space comes the database (`mysql -u root -p parcels`);
    // PostgreSQL's `-P` sets how psql prints, or asks createuser to prompt.
    let p_is_not_a_password = names(&["mysql", "mariadb"]);
    let capital_p_is_not_a_password = names(&["psql", "createuser", "pg_"]);
    // And one whose `-p` is always the password.
    let sshpass = names(&["sshpass"]);
    let curl = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == "curl");
    for (k, (quoted, word)) in words.iter().enumerate() {
        let Some(value) = value_of(k) else {
            continue;
        };
        let long = word.starts_with('-')
            && !word.contains('=')
            && holds(word).is_some_and(|h| h != Holds::Key);
        let short = (user
            && ((*word == "-P" && !capital_p_is_not_a_password)
                || (*word == "-p" && !p_is_not_a_password)))
            || (sshpass && *word == "-p");
        // A port (`-p 8080:80`, `-p host1:5432`) or a setting
        // (`-P border=2`) is not a password.
        let port = value
            .rsplit_once(':')
            .is_some_and(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if (long && (typed_in_bare(value) || (*quoted && typed_in_quotes(value, Holds::Password))))
            || (short && typed_in_bare(value) && !port && !value.contains('='))
        {
            return true;
        }
        if curl && (*word == "-u" || *word == "--user") {
            if let Some((_, password)) = value.split_once(':') {
                if typed_in_bare(password) {
                    return true;
                }
            }
        }
    }
    false
}

/// A token after the word Bearer, as a request's `Authorization` line has
/// it: twenty characters or more with a letter and a digit.
fn bearer(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.match_indices("bearer ").any(|(i, _)| {
        let rest = line[i + 7..].trim_start();
        let run = rest
            .bytes()
            .take_while(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
            })
            .count();
        let token = &rest[..run];
        run >= 20 && has_letter_and_digit(token) && !placeholder(token)
    })
}

/// A table in a note, as Markdown writes one: a heading row
/// (`| user | password |`), a row of dashes, then the rows. It remembers
/// which columns the heading says hold a password or a key.
#[derive(Debug, Default)]
struct Table {
    /// What each cell of the row above names, when that row was one.
    above: Option<Vec<Option<Holds>>>,
    /// The same for the heading of the table the next rows belong to.
    columns: Option<Vec<Option<Holds>>>,
}

impl Table {
    /// Take in the next line, and say whether it is a row that gives a
    /// password or a key: in a column whose heading names one, or right
    /// after a cell that names one (`| Password | … |`).
    fn row(&mut self, line: &str) -> bool {
        let Some((framed, cells)) = cells(line) else {
            *self = Table::default();
            return false;
        };
        let dashes = |c: &&str| {
            let c = c.trim_matches(':');
            !c.is_empty() && c.bytes().all(|b| b == b'-')
        };
        if cells.iter().all(dashes) {
            // The row of dashes: the row above it is the heading.
            self.columns = self.above.take().filter(|h| h.iter().any(Option::is_some));
            return false;
        }
        let names: Vec<Option<Holds>> = cells.iter().map(|c| names_a_secret(c)).collect();
        let in_its_column = self.columns.as_ref().is_some_and(|columns| {
            cells
                .iter()
                .zip(columns)
                .any(|(cell, holds)| holds.is_some_and(|h| typed_in_a_cell(cell, h)))
        });
        // (Only in a row written as one, between `|`: a shell's
        // `… | grep password | …` is no table.)
        let beside_its_name = framed
            && cells
                .windows(2)
                .zip(&names)
                .any(|(pair, holds)| holds.is_some_and(|h| typed_in_a_cell(pair[1], h)));
        self.above = Some(names);
        in_its_column || beside_its_name
    }
}

/// The cells of a table's row: what stands between `|`, without the
/// marks a note puts around words; and whether the row is written with a
/// `|` in front. `None` for a line that is no row.
fn cells(line: &str) -> Option<(bool, Vec<&str>)> {
    let t = line.trim();
    if !t.contains('|') {
        return None;
    }
    let framed = t.len() > 1 && t.starts_with('|');
    let inner = t.strip_prefix('|').unwrap_or(t);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let cells: Vec<&str> = inner
        .split('|')
        .map(|c| c.trim().trim_matches(['*', '`', '_']).trim())
        .collect();
    (framed || cells.len() > 1).then_some((framed, cells))
}

/// What a cell names, when it is a heading or a label for a password or a
/// key: one to three words, the last of them the name (`Password`,
/// `Admin password`, `API token`).
fn names_a_secret(cell: &str) -> Option<Holds> {
    let words: Vec<&str> = cell.split_whitespace().collect();
    let last = words.last()?.trim_end_matches(':');
    (words.len() <= 3 && last.bytes().all(|b| word_char(b) || b == b'-'))
        .then(|| holds(last))
        .flatten()
}

/// Whether a cell gives a password or a key: one word, counted as an
/// unquoted value is.
fn typed_in_a_cell(cell: &str, holds: Holds) -> bool {
    match holds {
        Holds::Password | Holds::Pass => typed_in_bare(cell),
        Holds::Key => typed_in_bare_key(cell),
    }
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

    /// A script's languages are still told from notes and settings: where a
    /// bare word after `=` is a variable, and where it is a value.
    #[test]
    fn which_files_are_code() {
        for ext in [
            "c", "cc", "cjs", "cpp", "cs", "go", "h", "hpp", "ipynb", "java", "jl", "js", "jsx",
            "kt", "lua", "m", "mjs", "php", "pl", "py", "pyt", "pyw", "r", "rb", "rs", "scala",
            "swift", "ts", "tsx", "vb",
        ] {
            assert!(lang(ext).is_some(), "{ext}");
            // A variable there, as before (in a notebook: in a code cell).
            let line = "conn = connect(user=user, password=pw2026x)";
            let text = match ext {
                "ipynb" => notebook(&[("code", &[line])]),
                _ => format!("{line}\n"),
            };
            assert_eq!(hit_in(&format!("a.{ext}"), &text), None, "{ext}");
        }
        for ext in [
            "md", "txt", "yaml", "ini", "sh", "ps1", "bat", "sql", "json", "xml", "",
        ] {
            assert!(lang(ext).is_none(), "{ext}");
            let text = "host=db user=gis password=pw2026x\n";
            assert!(hit_in(&format!("a.{ext}"), text).is_some(), "{ext}");
        }
    }

    /// A notebook as Jupyter saves it: one line of a cell to a line of the
    /// file, each in quotes.
    fn notebook(cells: &[(&str, &[&str])]) -> String {
        let cells: Vec<String> = cells
            .iter()
            .map(|(kind, lines)| {
                let source: Vec<String> = lines
                    .iter()
                    .map(|l| format!("    {}", serde_json::to_string(&format!("{l}\n")).unwrap()))
                    .collect();
                let more = if *kind == "code" {
                    "   \"execution_count\": 1,\n   \"metadata\": {},\n   \"outputs\": [],\n"
                } else {
                    "   \"metadata\": {},\n"
                };
                format!(
                    "  {{\n   \"cell_type\": \"{kind}\",\n{more}   \"source\": [\n{}\n   ]\n  }}",
                    source.join(",\n")
                )
            })
            .collect();
        format!(
            "{{\n \"cells\": [\n{}\n ],\n \"metadata\": {{}},\n \"nbformat\": 4,\n \"nbformat_minor\": 5\n}}\n",
            cells.join(",\n")
        )
    }

    /// Every finding in `cases` that is missing or on the wrong line, in
    /// words; and a check that no finding repeats a value.
    fn missed(cases: &[(&str, String, usize, &str)], values: &[&str]) -> Vec<String> {
        let mut wrong = Vec::new();
        for (path, text, line, what) in cases {
            let found = find(path, text.as_bytes(), &[]);
            match found.first() {
                Some(f) if f.line == *line && f.what.contains(what) => {}
                other => wrong.push(format!(
                    "{path} should stop at line {line} for {what:?}: {text:?} -> {:?}",
                    other.map(|f| (f.line, f.what.as_str()))
                )),
            }
            // What is said names the file and the line, never the value.
            for f in &found {
                assert_eq!(f.path, *path);
                let said = format!("{f:?} {}", serde_json::to_string(f).unwrap());
                for v in values {
                    assert!(!said.contains(v), "{said}");
                }
            }
        }
        wrong
    }

    /// Everyday ways of writing a password down that the re-check of R-103
    /// sent through a sync and read to the assistant (R-105): each on its
    /// own, with the line the finding must name.
    #[test]
    fn everyday_forms_stop_a_sync() {
        let (m20, m32) = (mixed(20), mixed(32));
        let password = "a password written into the file";
        let quoted = "a password or key written into the file (a quoted value)";
        let cases: Vec<(&str, String, usize, &str)> = vec![
            // In a comment of a script, where the person's own words are.
            ("load.py", format!("import os\n# login gis_reader, password: {PW}\nprint(1)\n"), 2, password),
            ("load.py", format!("# password = {PW}\n"), 1, password),
            ("load.py", format!("# PWD={PW};\n"), 1, password),
            ("load.py", format!("# DRIVER={{SQL Server}};SERVER=db;UID=gis;PWD={PW};\n"), 1, password),
            ("load.py", format!("rows = read()  # the prod password: {PW}\n"), 1, password),
            ("load.R", format!("# password: {PW}\ncon <- 1\n"), 1, password),
            ("app.js", format!("// password: {PW}\n"), 1, password),
            ("app.ts", format!("/*\n * user gis_reader\n * password: {PW}\n */\nexport {{}};\n"), 3, password),
            ("app.cs", format!("int x = 1; /* PWD={PW}; */\n"), 1, password),
            ("tool.vb", format!("' password: {PW}\n"), 1, password),
            ("tool.lua", format!("-- password: {PW}\n"), 1, password),
            ("tool.m", format!("% password: {PW}\n"), 1, password),
            // In a docstring.
            ("load.py", format!("\"\"\"Loads roads.\n\nlogin: gis_reader\npassword: {PW}\n\"\"\"\nprint(1)\n"), 4, password),
            ("load.py", format!("def load():\n    '''Signs in as gis_reader.\n\n    The password is {PW}.\n    '''\n    return 1\n"), 4, password),
            ("load.py", format!("def load():\n    \"\"\"Signs in. password: {PW}\"\"\"\n"), 2, password),
            // A line of a .netrc or a .pgpass file, after a comment sign.
            ("load.py", format!("# machine git.corp.example login ana password {PW}\n"), 1, ".netrc"),
            ("load.py", format!("# machine git.corp.example\n#   login ana\n#   password {PW}\n"), 3, ".netrc"),
            ("load.py", format!("# db.corp.example:5432:gis:gis_reader:{PW}\n"), 1, ".pgpass"),
            ("load.js", format!("// db.corp.example:5432:gis:gis_reader:{PW}\n"), 1, ".pgpass"),
            // Assignments the scan did not follow.
            ("load.py", format!("import os\nos.environ[\"PGPASSWORD\"] = \"{PW}\"\n"), 2, quoted),
            ("load.py", format!("cfg = {{}}\ncfg['password'] = '{PW}'\n"), 2, quoted),
            ("load.py", format!("settings[\"db\"][ \"password\" ] = \"{PW}\"\n"), 1, quoted),
            ("load.py", format!("password: str = \"{PW}\"\n"), 1, quoted),
            ("load.py", format!("def connect(user: str = \"gis\", password: str = \"{PW}\"):\n"), 1, quoted),
            ("app.ts", format!("const password: string = \"{PW}\";\n"), 1, quoted),
            ("load.R", format!("password <- \"{PW}\"\n"), 1, quoted),
            ("load.R", format!("pwd <<- '{PW}'\n"), 1, quoted),
            ("load.py", format!("password = b\"{PW}\"\n"), 1, quoted),
            ("load.sql", format!("ALTER LOGIN gis WITH PASSWORD = N'{PW}';\n"), 1, quoted),
            // Notes, as people write them.
            ("notes.md", format!("**Password:** {PW}\n"), 1, password),
            ("notes.md", format!("**Password**: {PW}\n"), 1, password),
            ("notes.md", format!("Password: **{PW}**\n"), 1, password),
            ("notes.md", format!("**Password: {PW}**\n"), 1, password),
            ("notes.md", format!("Password: `{PW}`\n"), 1, password),
            ("notes.md", format!("- Password: `{PW}`\n"), 1, password),
            ("notes.md", format!("- `password`: `{PW}`\n"), 1, password),
            ("notes.md", format!("* user: gis_reader\n* password: {PW}\n"), 2, password),
            ("notes.md", format!("The password is {PW}.\n"), 1, password),
            ("notes.md", format!("| user | password |\n|---|---|\n| gis_reader | {PW} |\n"), 3, "in a table"),
            ("notes.md", format!("| User | **Password** |\n| :--- | ---: |\n| ana | `{PW}` |\n"), 3, "in a table"),
            ("notes.md", format!("| setting | value |\n|---|---|\n| Password | {PW} |\n"), 3, "in a table"),
            ("notes.md", format!("- machine git.corp.example login ana password {PW}\n"), 1, ".netrc"),
            ("notes.md", format!("`db.corp.example:5432:gis:gis_reader:{PW}`\n"), 1, ".pgpass"),
            ("notes.md", format!("- db.corp.example:5432:gis:gis_reader:{PW}\n"), 1, ".pgpass"),
            // A notebook's text cell, and a comment in one of its code cells.
            ("nb.ipynb", notebook(&[("markdown", &[&format!("password: {PW}")])]), 7, password),
            ("nb.ipynb", notebook(&[("code", &["x = 1", &format!("# password: {PW}")])]), 10, password),
            // Between tags.
            ("conn.xml", format!("<connection><user>gis</user><password>{PW}</password></connection>\n"), 1, "between tags"),
            ("settings.xml", format!("<server>\n  <db:Password><![CDATA[{PW}]]></db:Password>\n</server>\n"), 2, "between tags"),
            // A token, key or secret with no quotes, as a settings file has it.
            ("conf.yaml", format!("portal: https://gis.corp.example/portal\ntoken: {m32}\n"), 2, "a token, key or secret"),
            ("conf.ini", format!("[arcgis]\napi_key = {m32}\n"), 2, "a token, key or secret"),
            ("conf.yaml", format!("client_secret: {m32}\n"), 1, "a token, key or secret"),
            ("run.sh", format!("export ARCGIS_API_KEY={m20}\n"), 1, "a token, key or secret"),
            // `pass` for password.
            ("conf.yaml", format!("user: gis\npass: {PW}\n"), 2, password),
            ("run.sh", format!("DB_PASS={PW}\n"), 1, password),
            ("load.py", format!("db_pass = \"{PW}\"\n"), 1, quoted),
            // A database command.
            ("users.sql", format!("CREATE USER gis WITH PASSWORD '{PW}';\n"), 1, "in a database command"),
            ("users.sql", format!("ALTER ROLE gis WITH ENCRYPTED PASSWORD '{}';\n", "opensesame"), 1, "in a database command"),
            ("users.sql", format!("ALTER USER gis IDENTIFIED BY {PW};\n"), 1, "in a database command"),
            ("users.sql", format!("CREATE USER gis IDENTIFIED BY \"{PW}\";\n"), 1, "in a database command"),
            ("load.py", format!("cur.execute(\"CREATE USER gis WITH PASSWORD '{PW}'\")\n"), 1, "in a database command"),
            // A command line.
            ("run.sh", format!("sqlcmd -S db -U gis -P {PW} -Q 'select 1'\n"), 1, "on a command line"),
            ("cmd.txt", format!("sdetable -o list -u gis -p {PW}\n"), 1, "on a command line"),
            ("run.sh", format!("python publish.py --user gis --password {PW}\n"), 1, "on a command line"),
            ("run.bat", format!("publish.exe --password \"{PW}\" --portal https://gis.corp.example\n"), 1, "on a command line"),
            ("run.sh", format!("publish --password={PW}\n"), 1, password),
            ("load.py", format!("subprocess.run([\"sqlcmd\", \"-U\", \"gis\", \"-P\", \"{PW}\"])\n"), 1, "on a command line"),
            ("load.py", format!("os.system(\"sqlcmd -S db -U gis -P {PW}\")\n"), 1, "on a command line"),
            ("run.sh", format!("curl -u gis:{PW} https://gis.corp.example/geoserver/rest/layers\n"), 1, "on a command line"),
            ("run.sh", format!("sshpass -p '{PW}' sftp ana@files.corp.example\n"), 1, "on a command line"),
            // A token after the word Bearer.
            ("call.sh", format!("curl -H 'Authorization: Bearer {m32}' https://api.example.org\n"), 1, "Bearer"),
            ("call.py", format!("headers = {{'Authorization': 'Bearer {m32}'}}\n"), 1, "Bearer"),
            // As before, however far the sign stands from the name.
            ("load.py", format!("password            = \"{}\"\n", "hunter2-prod"), 1, quoted),
            ("conf.yaml", format!("user:            gis\npassword        : {PW}\n"), 2, password),
            // Borderline, and kept: an example value is still a value.
            ("notes.md", "The default login is admin, password: admin123 (change it).\n".to_string(), 1, password),
            ("list.py", format!("next_page_token = \"{}\"\n", "CAESBggDEAEYAQ1234567890"), 1, quoted),
        ];
        let wrong = missed(&cases, &[PW, &m20, &m32, "opensesame", "admin123"]);
        assert!(
            wrong.is_empty(),
            "{} of {} are not found:\n{}",
            wrong.len(),
            cases.len(),
            wrong.join("\n")
        );
    }

    /// Near misses of each of those forms: ordinary scripts, notes and
    /// settings that name a password without giving one. None may stop a
    /// sync or keep the assistant from a file.
    #[test]
    fn near_misses_of_the_everyday_forms_do_not() {
        let m20 = mixed(20);
        let cases: Vec<(&str, String)> = vec![
            // Comments and docstrings that talk about a password.
            ("a.py", "# ask Ana for the password\n".into()),
            ("a.py", "# password: see the credential store\n".into()),
            ("a.py", "# password = get_password()\n".into()),
            ("a.py", "# password = os.environ[\"PGPASSWORD\"]\n".into()),
            ("a.py", "# pwd = os.getcwd()\n# PWD=$(pwd)\n".into()),
            ("a.py", "# password: str\n".into()),
            ("a.ts", "// password: string;\n".into()),
            ("a.py", "# password: sha256 of what the person typed\n".into()),
            ("a.py", "# the password is base64-encoded in the header\n".into()),
            ("a.py", "# the password is 12 characters or more\n".into()),
            ("a.py", "# the password is kept by Columbus, never here\n".into()),
            ("a.py", "\"\"\"Connect.\n\n    user: the login name\n    password: the database password, read from the environment\n    token: str\n\"\"\"\n".into()),
            ("a.py", "'''The machine needs a login and a password.'''\n".into()),
            ("a.py", "# db.corp.example:5432:gis:gis_reader:<password>\n".into()),
            ("a.py", "# machine git.corp.example login ana password <password>\n".into()),
            // A comment sign inside a string is not a comment, and what
            // follows a real one does not make the code before it words.
            ("a.py", "x = '# not a comment'; password = pw2026x\n".into()),
            ("a.py", "conn = connect(user=user, password=pw2026x)  # as set above\n".into()),
            ("a.js", "const u = \"http://db.corp.example\"; password = pw2026x;\n".into()),
            // Assignments whose value is code, empty or a stand-in.
            ("a.py", "os.environ[\"PGPASSWORD\"] = password\n".into()),
            ("a.py", "os.environ[\"PGPASSWORD\"] = getpass.getpass()\n".into()),
            ("a.py", "cfg['password'] = ''\n".into()),
            ("a.py", "cfg[\"password\"] = \"${DB_PASSWORD}\"\n".into()),
            ("a.py", "cfg['password'] = os.environ['PGPASSWORD']\n".into()),
            ("a.py", "passwords[0] = \"abcdef\"\n".into()),
            ("a.py", "password: str\n".into()),
            ("a.py", "password: str = None\n".into()),
            ("a.py", "password: str = \"\"\n".into()),
            ("a.py", "password=os.environ[\"PGPASSWORD\"]\n".into()),
            ("a.py", "def load(password: str, layer: str = \"roads2026\"):\n".into()),
            ("a.rs", "pub const STATUS_WRONG_PASSWORD: NTSTATUS = 0xC000006A;\n".into()),
            ("a.rs", "pub const ICW_MAX_PASSWORD: u32 = 256u32;\n".into()),
            ("a.py", "MAX_PASSWORD: int = max1024\n".into()),
            ("a.R", "password <- Sys.getenv(\"PGPASSWORD\")\n".into()),
            ("a.R", "password <- rstudioapi::askForPassword(\"Database password\")\n".into()),
            ("a.R", "pwd <- getwd()\n".into()),
            // Notes that name a password without giving one.
            ("notes.md", "Ask Ana for the password.\n".into()),
            ("notes.md", "**Password:** ask Ana\n".into()),
            ("notes.md", "Password: **in the credential store**\n".into()),
            ("notes.md", "Password: `<your password>`\n".into()),
            ("notes.md", "- `password`: `string`, what the person types\n".into()),
            ("notes.md", "The password is 12 characters or more.\n".into()),
            ("notes.md", "The password is sha256-hashed before it is stored.\n".into()),
            ("notes.md", "| user | password |\n|---|---|\n".into()),
            ("notes.md", "| user | password |\n|---|---|\n| gis_reader | <ask Ana> |\n| gis_editor | ******** |\n| gis_admin | in the vault |\n".into()),
            ("notes.md", "| field | type |\n|---|---|\n| password | varchar2 |\n| user_id1 | int4 |\n".into()),
            ("notes.md", "| key | value |\n|---|---|\n| api_key | set in the environment |\n".into()),
            ("notes.md", "| password |\n|---|\n\nparcel4471 | roads2026\n".into()),
            ("notes.md", "cat names1.txt | grep password | wc -l\n".into()),
            // A notebook: text that names a password, and code that reads one.
            ("nb.ipynb", notebook(&[("markdown", &["Ask Ana for the password."]), ("code", &["password = os.environ[\"PGPASSWORD\"]", "conn = connect(user=user, password=pw2026x)"])])),
            // Tags with nothing, or a stand-in, between them.
            ("a.xml", "<password/>\n<password></password>\n".into()),
            ("a.xml", "<password>${DB_PASSWORD}</password>\n".into()),
            ("a.xml", "<password>changeme</password>\n".into()),
            ("a.xml", "<passwordPolicy>min8chars</passwordPolicy>\n".into()),
            ("a.html", "<input type=\"password\" name=\"pwd1\" id=\"pwd-field1\">\n".into()),
            ("a.html", "<label for=\"pwd1\">Password</label>\n".into()),
            // Tokens, keys and secrets that are not there.
            ("conf.yaml", "token: null\n".into()),
            ("conf.yaml", "token: ${ARCGIS_TOKEN}\n".into()),
            ("conf.yaml", "client_secret: <from the portal>\n".into()),
            ("conf.yaml", "token: abc123\n".into()),
            ("conf.yaml", "token_url: https://login.example.org/oauth2/v2.0/token\n".into()),
            ("conf.yaml", "secret: /run/secrets/client-secret-v2\n".into()),
            ("conf.yaml", "private_key: keys/deploy_ed25519_2026.pem\n".into()),
            ("a.py", "api_key = os.environ[\"ARCGIS_API_KEY\"]\n".into()),
            ("a.py", format!("token = {m20}\n")),
            ("a.rs", "    \"token=access_token_123&token_type_hint=access_token\",\n".into()),
            ("notes.md", format!("a word in a sentence, not a setting: token={m20}\n")),
            // `pass` with its other meanings.
            ("conf.yaml", "pass: true\n".into()),
            ("a.py", "    pass  # nothing to do\n".into()),
            ("a.py", "    # columns; pass 'float64' explicitly if that is wanted\n".into()),
            ("notes.md", "To keep the type, pass \"int64x\" as the second value.\n".into()),
            ("conf.yaml", "compass: north2east\n".into()),
            ("a.py", "bypass = \"route66x\"\n".into()),
            ("a.py", "first_pass = \"stage1x\"\n".into()),
            ("a.py", "overpass_url = \"https://overpass-api.de/api/interpreter\"\n".into()),
            // Database commands without a password in them.
            ("a.sql", "ALTER USER gis WITH PASSWORD '<new password>';\n".into()),
            ("a.sql", "ALTER USER gis IDENTIFIED BY :new_pw;\n".into()),
            ("a.sql", "ALTER USER gis PASSWORD EXPIRE;\n".into()),
            ("a.sql", "-- each parcel is identified by 'OBJECTID_1'\n".into()),
            ("a.sql", "COMMENT ON COLUMN users.password IS 'bcrypt2a hash';\n".into()),
            ("a.py", "parser.add_argument(\"--password\", help=\"the password 'for-prod' is asked for\")\n".into()),
            // Command lines where nothing follows the flag, or what
            // follows is not a password.
            ("a.sh", "publish --password\n".into()),
            ("a.sh", "publish --password $DB_PASS1\n".into()),
            ("a.sh", "publish --password \"$DB_PASSWORD\"\n".into()),
            ("a.sh", "publish --password --portal https://gis.corp.example\n".into()),
            ("a.sh", "publish --password-file secrets1.txt\n".into()),
            ("a.sh", "mysql -u root -p\n".into()),
            ("a.sh", "mysql -u root -p parcels2024\n".into()),
            ("a.sh", "psql -U gis -p 5432 -h db1host\n".into()),
            ("a.sh", "ssh -p 2222 ana@host1\n".into()),
            ("a.sh", "sshpass -p \"$SFTP_PASS1\" sftp -P 2222 ana@host1\n".into()),
            ("a.sh", "sshpass -e ssh -p 2222 ana@host1\n".into()),
            ("a.sh", "mkdir -p output2026\n".into()),
            ("a.sh", "cp -u -p file1.txt backup1\n".into()),
            ("a.sh", "rsync -u -P data2024 backup\n".into()),
            ("a.sh", "python -u run.py -p profile1\n".into()),
            ("a.sh", "docker run -u 1000:1000 -p 8080:80 image1\n".into()),
            ("a.sh", "docker run -u ana -p host1:8080 image1\n".into()),
            ("a.sh", "sudo -u postgres createuser -P gis_user1\n".into()),
            ("a.sh", "psql -U gis -d roads -P border=2 -c \"select 1\"\n".into()),
            ("a.py", "# Usage: python load.py -u <user> -p <password>\n".into()),
            ("a.sh", "read -s -p \"Password: \" PW1234\n".into()),
            ("a.sh", "curl -u \"$USER:$PASS\" https://gis.corp.example\n".into()),
            ("a.sh", "curl -u gis https://gis.corp.example/rest1\n".into()),
            ("a.py", "parser.add_argument(\"--password\", type=str2bool, help=\"ask for it\")\n".into()),
            ("a.py", "parser.add_argument(\"-p\", \"--password\", dest=\"pw2026\")\n".into()),
            ("a.py", "@click.option(\"--password\", \"-p\", prompt=True)\n".into()),
            ("a.py", "run([\"sqlcmd\", \"-U\", user, \"-P\", password1])\n".into()),
            ("notes.md", "Use `--password` to give it, or leave it out to be asked.\n".into()),
            // The word Bearer without a token.
            ("a.md", "Authorization: Bearer <token>\n".into()),
            ("a.md", "Authorization: Bearer YOUR_ACCESS_TOKEN_1234567890\n".into()),
            ("a.py", "headers = {\"Authorization\": f\"Bearer {token}\"}\n".into()),
            ("a.md", "the bearer of this letter is 21 years old or more\n".into()),
        ];
        let wrong: Vec<String> = cases
            .iter()
            .filter_map(|(path, text)| {
                hit_in(path, text).map(|(line, what)| {
                    format!("{path} should not stop: {text:?} -> line {line} ({what})")
                })
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "{} of {} stop:\n{}",
            wrong.len(),
            cases.len(),
            wrong.join("\n")
        );
    }

    /// Two stops the re-check called borderline now pass: a value that is a
    /// path, and one that is a column's type. A real value in their place
    /// still stops.
    #[test]
    fn a_path_or_a_column_type_is_not_a_password() {
        for (path, text) in [
            ("run.json", "{\"pwd\": \"/home/ana/project\"}\n"),
            ("a.py", "pwd = \"/home/ana/project\"\n"),
            ("a.py", "pwd = r\"C:\\Users\\ana\\project\"\n"),
            ("run.json", "{\"pwd\": \"C:\\\\Users\\\\ana\\\\project\"}\n"),
            ("a.py", "pwd = '~/project2026'\n"),
            ("schema.yaml", "fields:\n  password: varchar2\n"),
            (
                "schema.yaml",
                "fields:\n  password: nvarchar2\n  pwd: int4\n",
            ),
            (
                "schema.json",
                "{\"password\": \"varchar2\", \"pwd\": \"string\"}\n",
            ),
            ("notes.md", "password: sha256\n"),
        ] {
            assert_eq!(hit_in(path, text), None, "{path} should not stop: {text}");
        }
        for (path, text) in [
            ("run.json", format!("{{\"pwd\": \"/{PW}\"}}\n")),
            ("a.py", format!("pwd = \"{PW}/x\"\n")),
            (
                "schema.yaml",
                format!("fields:\n  password: varchar2{PW}\n"),
            ),
            ("schema.yaml", format!("password: base64:{}\n", mixed(24))),
            ("schema.yaml", "fields:\n  pwd: char8x\n".to_string()),
        ] {
            assert!(hit_in(path, &text).is_some(), "{path} should stop: {text}");
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
