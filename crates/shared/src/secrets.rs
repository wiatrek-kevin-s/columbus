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
//!   `password`, `passwd`, `pwd` or `passphrase`, and `pass` or `pw` alone
//!   or as in `db_pass`), or a long mixed value given to a name ending in
//!   `token`, `secret` or `key`:
//!   * in quotes, with `=`, `:`, `=>`, `:=` or R's `<-`, the name plain, in
//!     quotes or in a subscript (`cfg["password"] = "…"`,
//!     `os.environ["PGPASSWORD"] = "…"`), with a type between
//!     (`password: str = "…"`), a letter before the quote (`b"…"`), and
//!     as one of several names given their values at once
//!     (`USER, PASSWORD = "gis", "…"`);
//!   * without quotes, when it is six characters or more with a letter and
//!     a digit (sixteen for a token, secret or key): `password: …` in a
//!     settings file or a note (also as `**Password:** …` and
//!     ``Password: `…` ``), `PWD=…;` or `Password=…;` in a connection
//!     string, `export PGPASSWORD=…`, `token: …`, `api_key = …`;
//!   * in a sentence (`the password is …`), in a row of a table whose
//!     heading names it, and between tags (`<password>…</password>`);
//!   * in a database command (`PASSWORD '…'`, `IDENTIFIED BY …`) and on a
//!     command line (`--password …`, `-P …` beside `-U …`, curl's
//!     `-u user:password`, MySQL's `-pVALUE`);
//! * a password handed to a well-known call by its place, with no name
//!   beside it to say so ([`CALLS`]): `GIS(url, "user", "…")` and
//!   `gis.users.create("user", "…", …)`; arcpy's `SignInToPortal`,
//!   `CreateDatabaseConnection`, `CreateDatabaseConnectionString`,
//!   `CreateArcSDEConnectionFile`, `CreateEnterpriseGeodatabase`,
//!   `CreateDatabaseUser` and `ArcSDESQLExecute`; `HTTPBasicAuth("user",
//!   "…")` and a pair given as the sign-in (`auth=("user", "…")`);
//!   `cx_Oracle.connect("user", "…", dsn)`, `pymssql.connect(server,
//!   "user", "…")`, the `connect` of MySQLdb and PyMySQL; `FTP(host,
//!   "user", "…")` and `login("user", "…")`; PowerShell's
//!   `ConvertTo-SecureString "…" -AsPlainText`; also when the call runs
//!   over several lines, or stands inside another call;
//! * a password or a key to fall back on: the default of a value read
//!   from the environment (`os.environ.get("PGPASSWORD", "…")`,
//!   `os.getenv("PGPASSWORD", default="…")`), a string after such a read
//!   and `or`, `||`, `??` or `?:` (`process.env.DB_PASSWORD || "…"`), a
//!   shell's `${PGPASSWORD:-…}`, a variable set for good on Windows
//!   (`setx PGPASSWORD …`), and a string that a name which holds a
//!   password falls back on (`password = args.password or "…"`);
//! * Oracle's `user/password@database`, on a line that says it is one
//!   (`sqlplus`, `sqlldr`, `expdp`, `impdp`, `exp`, `imp`, `CONNECT`, a
//!   connect string), and a user and password packed after
//!   `Authorization: Basic`;
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
//!
//! A file that is not text (it has a NUL byte near its start) is searched
//! only for the shapes of keys and tokens. Text saved as UTF-16 with its
//! mark in front, as Windows often saves a note, is read as the text it
//! is.

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

/// Text saved as UTF-16 with its mark in front, as Windows often saves a
/// note (and as PowerShell's `>` writes a file): read as the text it is.
/// `None` for anything else, and for bytes that only start like that mark.
fn utf16(bytes: &[u8]) -> Option<String> {
    // Two bytes to a unit; an odd byte at the end is left out.
    let units = |pair: fn([u8; 2]) -> u16| -> Vec<u16> {
        bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| pair(*c))
            .collect()
    };
    let text = match bytes {
        [0xFF, 0xFE, ..] => String::from_utf16_lossy(&units(u16::from_le_bytes)),
        [0xFE, 0xFF, ..] => String::from_utf16_lossy(&units(u16::from_be_bytes)),
        _ => return None,
    };
    (!text.chars().take(8000).any(|c| c == '\0')).then_some(text)
}

/// Look through one file. `own` are strings that must never leave (the
/// linked server's token).
pub fn find(path: &str, bytes: &[u8], own: &[String]) -> Vec<Finding> {
    let (text, is_text) = match utf16(bytes) {
        Some(text) => (std::borrow::Cow::Owned(text), true),
        None => (String::from_utf8_lossy(bytes), is_text(bytes)),
    };
    // The mark some programs put in front of UTF-8 text is not part of
    // the first line.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
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
    let mut calls = Calls::default();
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
        // Every line is shown to the table, so that it knows its rows, and
        // to the calls, which may run over several lines.
        let in_a_table = is_text && table.row(line);
        let in_a_call = if is_text {
            calls.line(line, matches!(prose, Prose::Parts(_)))
        } else {
            None
        };
        let plain = undecorated(line);
        let mut what = in_line(line, plain, own, is_text, &prose);
        if what.is_none() && in_a_table {
            what = Some(IN_A_TABLE);
        }
        if what.is_none() {
            what = in_a_call;
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
const IN_A_CALL: &str = "a password written into the file (handed to a call that signs in)";
const ORACLE: &str = "a password written into the file (an Oracle sign-in, user/password@database)";
const BASIC: &str = "a user and password written into the file (packed after Authorization: Basic)";

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
        let named = given(line, true);
        if password_assignment(line, &named)
            || several_at_once(line)
            || falls_back_on(line, &named)
            || environment_read(line)
        {
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
        if let Some(what) = shell_default(line).or_else(|| set_for_good(line)) {
            return Some(what);
        }
        if made_secure(line) {
            return Some(IN_A_CALL);
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
        if basic_sign_in(line) {
            return Some(BASIC);
        }
        if oracle_sign_in(line) {
            return Some(ORACLE);
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
    /// `pass`, `db_pass`, `pw`, `AGOL_PW`: short for password, and words
    /// with other meanings, so the value must look like a password even
    /// in quotes.
    Pass,
    /// `token`, `client_secret`, `api_key`: its value must be long and
    /// mixed.
    Key,
}

/// What `pass` or `pw` is the password of, where it is short for
/// password: `db_pass`, `DB_PASS`, `userpass`, `AGOL_PW`. Not `first_pass`,
/// `bypass`, `compass`.
const PASS_OF: &[&str] = &[
    "admin", "ags", "agol", "arcgis", "auth", "database", "db", "ftp", "gis", "login", "mysql",
    "ora", "oracle", "pg", "portal", "postgres", "root", "sde", "smtp", "sql", "ssh", "user",
];

/// Units a number is given in. `pw` is also a width, of a page
/// (`pw: 1024px`) or of a pulse (`pw: 10.25ns`), so a number with its unit
/// is no password for it.
const UNITS: &[&str] = &[
    "%", "cm", "deg", "dpi", "em", "ft", "hz", "in", "khz", "km", "m", "mhz", "mm", "ms", "ns",
    "pc", "ppi", "ps", "pt", "px", "rem", "s", "us",
];

/// A number with its unit: `1024px`, `12.5mm`, `300dpi`, `10.25ns`.
fn a_measure(v: &str) -> bool {
    let number = v
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
    number > 0 && UNITS.contains(&&v[number..])
}

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
    for short in ["pass", "pw"] {
        if let Some(of) = name.strip_suffix(short) {
            let of = of.trim_end_matches(['-', '_']);
            let of = of.rsplit(['-', '_']).next().unwrap_or(of);
            if of.is_empty() || PASS_OF.contains(&of) {
                return Some(Holds::Pass);
            }
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
        Holds::Pass => n >= 6 && mixed && !a_measure(value),
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
fn password_assignment(line: &str, given: &[Given]) -> bool {
    given.iter().any(|g| {
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
        bare_value(line, g.after).is_some_and(|v| {
            typed_in_bare(v)
                && !(g.holds == Holds::Pass && a_measure(v))
                && !a_variable(line, g.start, v, prose)
        })
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
/// * as curl's `-u user:password`;
/// * as MySQL's `-pVALUE`, the password right behind the letter, on a
///   `mysql`, `mysqldump` or `mariadb` line that also names the user.
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
    // MySQL's own way: the password right behind `-p`, with no space, on
    // a line that names the user (`-u root`, `-uroot`, `--user=root`).
    if p_is_not_a_password {
        let behind = |w: &str, flag: &str| -> Option<String> {
            let value = w.strip_prefix(flag).filter(|v| !v.is_empty())?;
            Some(value.trim_matches(['"', '\'']).to_string())
        };
        let user_named = user || words.iter().any(|(_, w)| behind(w, "-u").is_some());
        if user_named
            && words
                .iter()
                .any(|(_, w)| behind(w, "-p").is_some_and(|v| typed_in_bare(&v)))
        {
            return true;
        }
    }
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
        Holds::Password => typed_in_bare(cell),
        Holds::Pass => typed_in_bare(cell) && !a_measure(cell),
        Holds::Key => typed_in_bare_key(cell),
    }
}

/// Where the string that opens at `at` closes: the place of its closing
/// quote. `None` when it does not close on this line.
fn string_end(b: &[u8], at: usize) -> Option<usize> {
    let quote = b[at];
    let mut i = at + 1;
    while i < b.len() && b[i] != quote {
        if b[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    (i < b.len()).then_some(i)
}

/// The place after the string that opens at `at`; the line's end when it
/// does not close.
fn after_string(b: &[u8], at: usize) -> usize {
    string_end(b, at).map_or(b.len(), |end| end + 1)
}

/// What stands between the quotes when `text` is one string and nothing
/// else: `"…"`, `'…'`, also with a letter or two before it (`r"…"`). A
/// string put together (`"a" + b`), a variable, a call: `None`.
fn only_a_string(text: &str) -> Option<&str> {
    let text = text.trim();
    let b = text.as_bytes();
    let kind = b
        .iter()
        .take(2)
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if !matches!(b.get(kind), Some(b'"') | Some(b'\'')) {
        return None;
    }
    let end = string_end(b, kind)?;
    (end + 1 == b.len()).then(|| &text[kind + 1..end])
}

/// What a well-known call is handed, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Takes {
    /// A password, as the argument at one of these places (counted from
    /// 0).
    PasswordAt(&'static [usize]),
    /// A name and then its value: a value read from the environment with
    /// its default, or set there.
    NameThenValue,
    /// A name alone: a value read from the environment, for what it falls
    /// back on (`… ?? "…"`). What else the call is handed is no value.
    Name,
}

/// Well-known calls that are handed a password by its place, with no name
/// beside it to say so. A call is known by the end of its name, so
/// `GIS(`, `gis.GIS(` and `arcgis.gis.GIS(` are one; a name with a dot in
/// it here needs both parts (`os.environ.get(`, `gis.users.create(`), and
/// `.login` is a call of that name on anything (`ftp.login(`). The places
/// are the ones each function documents:
///
/// * ArcGIS API for Python: `GIS(url, username, password, …)` and
///   `users.create(username, password, …)`, a new member's first password;
/// * arcpy: `SignInToPortal(portal_url, username, password, …)`,
///   `CreateDatabaseConnection(out_folder_path, out_name,
///   database_platform, instance, account_authentication, username,
///   password, …)`, `CreateDatabaseConnectionString(database_platform,
///   instance, account_authentication, username, password, …)`,
///   `CreateArcSDEConnectionFile(out_folder_path, out_name, server,
///   service, database, account_authentication, username, password, …)`,
///   `CreateEnterpriseGeodatabase(database_platform, instance_name,
///   database_name, account_authentication, database_admin,
///   database_admin_password, sde_schema, gdb_admin_name,
///   gdb_admin_password, …)`, `CreateDatabaseUser(input_database,
///   user_authentication_type, user_name, user_password, …)` (each also
///   by its older name, `…_management`), and `ArcSDESQLExecute(server,
///   instance, database, user, password)`;
/// * requests: `HTTPBasicAuth(username, password)` and its kin, and the
///   `BasicAuth(username, password)` of httpx and aiohttp;
/// * database drivers: cx_Oracle's `connect(user, password, dsn)`,
///   pymssql's `connect(server, user, password, database)`, and the
///   `connect(host, user, password, database)` of MySQLdb and PyMySQL;
/// * Python's own: `FTP(host, user, passwd)`, the `login(user, password)`
///   of its file and mail clients, and `os.environ.get(name, default)`,
///   `os.getenv(name, default)`, `os.environ.setdefault(name, value)` and
///   `os.putenv(name, value)`, when the name holds a password or a key;
/// * .NET's, as PowerShell uses them: `SetEnvironmentVariable(name, value,
///   …)`, and `GetEnvironmentVariable(name, …)` for what it falls back on
///   (`… ?? "…"`; what follows the name there says where to look, and is
///   no value).
///
/// A pair given as the sign-in, `auth=("user", "password")`, is found by
/// its own shape ([`call_at`]).
const CALLS: &[(&str, Takes)] = &[
    ("GIS", Takes::PasswordAt(&[2])),
    ("users.create", Takes::PasswordAt(&[1])),
    ("SignInToPortal", Takes::PasswordAt(&[2])),
    ("CreateDatabaseConnection", Takes::PasswordAt(&[6])),
    (
        "CreateDatabaseConnection_management",
        Takes::PasswordAt(&[6]),
    ),
    ("CreateDatabaseConnectionString", Takes::PasswordAt(&[4])),
    (
        "CreateDatabaseConnectionString_management",
        Takes::PasswordAt(&[4]),
    ),
    ("CreateArcSDEConnectionFile", Takes::PasswordAt(&[7])),
    (
        "CreateArcSDEConnectionFile_management",
        Takes::PasswordAt(&[7]),
    ),
    ("CreateEnterpriseGeodatabase", Takes::PasswordAt(&[5, 8])),
    (
        "CreateEnterpriseGeodatabase_management",
        Takes::PasswordAt(&[5, 8]),
    ),
    ("CreateDatabaseUser", Takes::PasswordAt(&[3])),
    ("CreateDatabaseUser_management", Takes::PasswordAt(&[3])),
    ("ArcSDESQLExecute", Takes::PasswordAt(&[4])),
    ("HTTPBasicAuth", Takes::PasswordAt(&[1])),
    ("HTTPDigestAuth", Takes::PasswordAt(&[1])),
    ("HTTPProxyAuth", Takes::PasswordAt(&[1])),
    ("BasicAuth", Takes::PasswordAt(&[1])),
    ("DigestAuth", Takes::PasswordAt(&[1])),
    ("cx_Oracle.connect", Takes::PasswordAt(&[1])),
    ("cx_Oracle.Connection", Takes::PasswordAt(&[1])),
    ("pymssql.connect", Takes::PasswordAt(&[2])),
    ("MySQLdb.connect", Takes::PasswordAt(&[2])),
    ("pymysql.connect", Takes::PasswordAt(&[2])),
    ("FTP", Takes::PasswordAt(&[2])),
    ("FTP_TLS", Takes::PasswordAt(&[2])),
    (".login", Takes::PasswordAt(&[1])),
    ("environ.get", Takes::NameThenValue),
    ("environ.setdefault", Takes::NameThenValue),
    ("getenv", Takes::NameThenValue),
    ("putenv", Takes::NameThenValue),
    ("SetEnvironmentVariable", Takes::NameThenValue),
    ("GetEnvironmentVariable", Takes::Name),
];

/// What the call named `name` is handed, if it is one of [`CALLS`].
fn well_known(name: &str) -> Option<Takes> {
    let mut parts = name.rsplit('.');
    let last = parts.next()?;
    let before = parts.next();
    CALLS
        .iter()
        .find(|(call, _)| match call.split_once('.') {
            Some(("", function)) => last == function && before.is_some(),
            Some((module, function)) => last == function && before == Some(module),
            None => last == *call,
        })
        .map(|(_, takes)| *takes)
}

/// The well-known call that opens with the bracket at `at`, if one does.
/// Besides the calls of [`CALLS`], a pair given to a name that ends in
/// `auth` (`auth=("user", "password")`, `session.auth = (…)`,
/// `http_auth=(…)`, `"auth": (…)`).
///
/// `hash` is where the line's first `#` stands: after it the rest of the
/// line may be a comment, and so may the next line, so a call that opens
/// there does not go on to the next line. `code` says the line is one of a
/// script: there a blank may stand between a call's name and its bracket
/// (`GIS (url, …)`), which among words is no call.
fn call_at(line: &str, at: usize, hash: Option<usize>, code: bool) -> Option<Call> {
    let b = line.as_bytes();
    let blank = |c: u8| c == b' ' || c == b'\t';
    let name_before = |end: usize| {
        let mut start = end;
        while start > 0 && (word_char(b[start - 1]) || b[start - 1] == b'.') {
            start -= 1;
        }
        &line[start..end]
    };
    let commented = hash.is_some_and(|hash| hash < at);
    let name = name_before(at);
    if !name.is_empty() {
        return well_known(name).map(|takes| Call::new(takes, at + 1, commented));
    }
    let mut end = at;
    while end > 0 && blank(b[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    if code && !commented {
        let name = name_before(end);
        if !name.is_empty() {
            return well_known(name).map(|takes| Call::new(takes, at + 1, true));
        }
    }
    // `auth = (`, perhaps, or `"auth": (` in a dictionary.
    if !matches!(b[end - 1], b'=' | b':') {
        return None;
    }
    end -= 1;
    if end > 0 && matches!(b[end - 1], b'=' | b'!' | b'<' | b'>' | b':') {
        return None;
    }
    while end > 0 && blank(b[end - 1]) {
        end -= 1;
    }
    if end > 0 && matches!(b[end - 1], b'"' | b'\'') {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && word_char(b[start - 1]) {
        start -= 1;
    }
    let name = &b[start..end];
    (name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(b"auth"))
        .then(|| Call::new(Takes::PasswordAt(&[1]), at + 1, commented))
}

/// The next well-known call that opens on `line` at or after `from`, and
/// where its arguments start.
fn next_call(line: &str, from: usize, hash: Option<usize>, code: bool) -> Option<(Call, usize)> {
    let b = line.as_bytes();
    (from..b.len())
        .filter(|&i| b[i] == b'(')
        .find_map(|i| call_at(line, i, hash, code).map(|call| (call, i + 1)))
}

/// How many lines a call may run over before it is given up.
const CALL_LINES: usize = 30;

/// How many well-known calls may be open inside one another.
const CALLS_DEEP: usize = 8;

/// A well-known call whose arguments are being read, perhaps over several
/// lines (as a formatter lays a long call out).
#[derive(Debug)]
struct Call {
    takes: Takes,
    /// Which argument is being read, counted from 0.
    arg: usize,
    /// How many brackets are open inside the call's own.
    depth: usize,
    /// The argument being read has text on a line before this one: it was
    /// judged there, or is more than one string.
    begun: bool,
    /// An argument with a name (`verify_cert=False`) or stars (`*args`)
    /// has come: from there on, places say nothing.
    named: bool,
    /// The user is `anonymous` (or `ftp`, which file servers take for
    /// the same): what follows is an address given out of courtesy, not a
    /// password (`ftp.login("anonymous", "ana@…")`).
    anonymous: bool,
    /// What the first argument names ([`Takes::NameThenValue`]).
    names: Option<Holds>,
    /// How many lines the call has run over.
    lines: usize,
    /// Where the text of the argument being read starts on this line.
    from: usize,
    /// The call opened on the line being read.
    fresh: bool,
    /// The call must close on the line it opens on: it stands after a
    /// `#`, or has a blank before its bracket.
    one_line: bool,
}

impl Call {
    /// A call whose arguments start at `from` on the line being read.
    fn new(takes: Takes, from: usize, one_line: bool) -> Call {
        Call {
            takes,
            arg: 0,
            depth: 0,
            begun: false,
            named: false,
            anonymous: false,
            names: None,
            lines: 0,
            from,
            fresh: true,
            one_line,
        }
    }

    /// Judge the argument whose text on this line is `text`.
    fn argument(&mut self, text: &str) -> Option<&'static str> {
        let text = text.trim();
        if self.begun || text.is_empty() {
            return None;
        }
        // `name = value`: what an argument given by its name is given.
        let name = text.bytes().take_while(|b| word_char(*b)).count();
        let after = text[name..].trim_start();
        let given = (name > 0 && after.starts_with('=') && !after.starts_with("=="))
            .then(|| after[1..].trim_start());
        match self.takes {
            Takes::PasswordAt(places) => {
                if text.starts_with('*') || given.is_some() {
                    self.named = true;
                }
                let value = only_a_string(text)?;
                // Right before the password's place stands the user.
                if places.contains(&(self.arg + 1))
                    && (value.eq_ignore_ascii_case("anonymous") || value == "ftp")
                {
                    self.anonymous = true;
                    return None;
                }
                // (`auth=(token, "x-oauth-basic")`: the words GitHub asks
                // for in the password's place, with the token as the user.)
                (!self.named
                    && !self.anonymous
                    && places.contains(&self.arg)
                    && typed_in_quotes(value, Holds::Password)
                    && value != "x-oauth-basic")
                    .then_some(IN_A_CALL)
            }
            Takes::NameThenValue | Takes::Name => {
                // The default may be given by its name (`default="…"`).
                let text = given.unwrap_or(text);
                if self.arg == 0 {
                    self.names = only_a_string(text).and_then(holds);
                    return None;
                }
                let holds = self
                    .names
                    .filter(|_| self.arg == 1 && self.takes == Takes::NameThenValue)?;
                only_a_string(text)
                    .filter(|value| default_counts(value, holds))
                    .map(|_| QUOTED)
            }
        }
    }
}

/// Whether `value` counts as what a value read from the environment falls
/// back on, for a name that `holds` a password or a key: as a quoted value
/// does, but a key that reads as a path to one (`keys/portal.pem`) is the
/// path, not the key.
fn default_counts(value: &str, holds: Holds) -> bool {
    typed_in_quotes(value, holds) && !(holds == Holds::Key && path_to_a_key(value))
}

/// The string to fall back on that follows a value read from the
/// environment: `… or "…"` (`||`, `??` and `?:` in other languages). One
/// string and nothing put to it: `… or "x" + y` is not one.
fn fallback(rest: &str) -> Option<&str> {
    let rest = rest.trim_start();
    let rest = ["or ", "||", "??", "?:"]
        .iter()
        .find_map(|sign| rest.strip_prefix(sign))?
        .trim_start();
    let b = rest.as_bytes();
    let kind = b
        .iter()
        .take(2)
        .take_while(|c| c.is_ascii_alphabetic())
        .count();
    if !matches!(b.get(kind), Some(b'"') | Some(b'\'')) {
        return None;
    }
    let end = string_end(b, kind)?;
    let after = rest[end + 1..].trim_start();
    (after.is_empty() || after.starts_with([')', ']', '}', ',', ';', '#']))
        .then(|| &rest[kind + 1..end])
}

/// The well-known calls of a file, line by line: a call may open on one
/// line and be handed its password on another, and one call may stand
/// inside another (`GIS(url, "ana", os.environ.get("AGOL_PW", "…"))`).
#[derive(Debug, Default)]
struct Calls {
    /// The calls being read, the innermost last.
    open: Vec<Call>,
}

impl Calls {
    /// Take in the next line, and say what it holds if it hands a
    /// well-known call its password. `code` says the line is one of a
    /// script ([`call_at`]).
    fn line(&mut self, line: &str, code: bool) -> Option<&'static str> {
        // A call does not run on over an empty line, or without end.
        if line.trim().is_empty() || self.open.iter().any(|c| c.lines >= CALL_LINES) {
            self.open.clear();
        }
        for call in &mut self.open {
            call.from = 0;
        }
        let b = line.as_bytes();
        let hash = line.find('#');
        let mut found = None;
        // Where the line's code ends (a comment may follow), and whether
        // it ends inside a string.
        let (mut end, mut in_a_string) = (b.len(), false);
        let mut i = 0;
        while i < b.len() {
            if self.open.is_empty() {
                // Outside a call, the next one counts wherever it stands:
                // in a comment or in a string too.
                let Some((call, after)) = next_call(line, i, hash, code) else {
                    break;
                };
                self.open.push(call);
                i = after;
                continue;
            }
            match b[i] {
                b'"' | b'\'' => {
                    match string_end(b, i) {
                        Some(close) => i = close + 1,
                        None => {
                            in_a_string = true;
                            i = b.len();
                        }
                    }
                    continue;
                }
                b'#' => {
                    end = i;
                    break;
                }
                // A well-known call inside this one is read by itself.
                b'(' if self.open.len() < CALLS_DEEP => {
                    if let Some(call) = call_at(line, i, hash, code) {
                        self.open.push(call);
                        i += 1;
                        continue;
                    }
                }
                _ => {}
            }
            let Some(call) = self.open.last_mut() else {
                break;
            };
            match b[i] {
                b'(' | b'[' | b'{' => call.depth += 1,
                b')' | b']' | b'}' if call.depth > 0 => call.depth -= 1,
                b')' | b']' | b'}' => {
                    found = found.or(call.argument(&line[call.from..i]));
                    // A value read from the environment, and what stands
                    // in for it when it is not there.
                    let read = match call.takes {
                        Takes::NameThenValue => call.arg == 0,
                        Takes::Name => true,
                        Takes::PasswordAt(_) => false,
                    };
                    if let (true, Some(holds)) = (read, call.names) {
                        if fallback(&line[i + 1..]).is_some_and(|v| default_counts(v, holds)) {
                            found = found.or(Some(QUOTED));
                        }
                    }
                    self.open.pop();
                }
                b',' if call.depth == 0 => {
                    found = found.or(call.argument(&line[call.from..i]));
                    call.arg += 1;
                    call.begun = false;
                    call.from = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        // The line ends inside these calls. A string alone on its line is
        // judged where it stands; what follows it belongs to it.
        for call in &mut self.open {
            let rest = &line[call.from.min(end)..end];
            if !rest.trim().is_empty() {
                found = found.or(call.argument(rest));
                call.begun = true;
            }
            call.lines += 1;
        }
        // A call that opened on this line goes on to the next only as a
        // call laid out over several lines does: the line ends with a
        // bracket that opens or with a comma. Anything else is a bracket
        // left open among words, and what follows are not its arguments.
        let laid_out = !in_a_string && line[..end].trim_end().ends_with(['(', '[', '{', ',']);
        if let Some(at) = self
            .open
            .iter()
            .position(|c| c.fresh && (c.one_line || !laid_out))
        {
            // Their brackets stay open inside the call around them.
            let brackets: usize = self.open[at..].iter().map(|c| 1 + c.depth).sum();
            self.open.truncate(at);
            if let Some(around) = self.open.last_mut() {
                around.depth += brackets;
            }
        }
        for call in &mut self.open {
            call.fresh = false;
        }
        found
    }
}

/// A string to fall back on, given to a name that holds a password or a
/// key for when what it is given first is not there: `password =
/// args.password or "…"`, `const password = opts.password || "…"`. The
/// string counts as a quoted value does. Looked for in what the name is
/// given, up to where that ends (a comma, a bracket that closes, a
/// comment) and not far ([`NEAR`]); not inside a call's arguments, a
/// subscript or a list there, where it is something else's fallback
/// (`passwd = getpass.getpass("… %s" % (host or "localhost"))`).
fn falls_back_on(line: &str, given: &[Given]) -> bool {
    let b = line.as_bytes();
    given.iter().any(|g| {
        let stop = b.len().min(g.after + NEAR);
        // How many brackets are open, and at which of them a call's
        // arguments, a subscript or a list began.
        let (mut depth, mut inside) = (0usize, None);
        let mut i = g.after;
        while i < stop {
            match b[i] {
                b'"' | b'\'' => {
                    i = after_string(b, i);
                    continue;
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    // A bracket that only groups: `(a or "…")`.
                    let groups = b[i] == b'('
                        && (i == 0
                            || !(word_char(b[i - 1])
                                || matches!(b[i - 1], b')' | b']' | b'"' | b'\'')));
                    if !groups && inside.is_none() {
                        inside = Some(depth);
                    }
                }
                b')' | b']' | b'}' if depth == 0 => return false,
                b')' | b']' | b'}' => {
                    if inside == Some(depth) {
                        inside = None;
                    }
                    depth -= 1;
                }
                b',' | b';' | b'#' if depth == 0 => return false,
                // `or` as a word of its own; `||`, `??`, `?:`.
                b'o' if i > 0 && word_char(b[i - 1]) => {}
                b'o' | b'|' | b'?'
                    if inside.is_none()
                        && fallback(&line[i..])
                            .is_some_and(|value| default_counts(value, g.holds)) =>
                {
                    return true;
                }
                _ => {}
            }
            i += 1;
        }
        false
    })
}

/// A value read from the environment by its name, with a string to fall
/// back on: `process.env.DB_PASSWORD || "…"` (also `process.env["…"]`,
/// `import.meta.env.…`, Ruby's `ENV["…"]`, PHP's `$_ENV['…']`,
/// PowerShell's `$env:…`). The name must hold a password or a key, and
/// the string counts as a quoted value does.
fn environment_read(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.match_indices("env").any(|(at, _)| {
        let rest = &line[at + 3..];
        let (name, after) = if let Some(rest) = rest.strip_prefix(['.', ':']) {
            let name = rest.bytes().take_while(|c| word_char(*c)).count();
            rest.split_at(name)
        } else if let Some(rest) = rest.strip_prefix('[') {
            let b = rest.as_bytes();
            if !matches!(b.first(), Some(b'"') | Some(b'\'')) {
                return false;
            }
            let Some(end) = string_end(b, 0) else {
                return false;
            };
            let Some(after) = rest[end + 1..].strip_prefix(']') else {
                return false;
            };
            (&rest[1..end], after)
        } else {
            return false;
        };
        holds(name)
            .is_some_and(|holds| fallback(after).is_some_and(|value| default_counts(value, holds)))
    })
}

/// A value read from the environment as a shell does, with what to use
/// when it is not there: `${PGPASSWORD:-…}` (also `:=`), in a script, a
/// `.env` file or a compose file. The name must hold a password or a key,
/// and the value counts as an unquoted one does.
fn shell_default(line: &str) -> Option<&'static str> {
    line.match_indices("${").find_map(|(at, _)| {
        let rest = &line[at + 2..];
        let name = rest.bytes().take_while(|c| word_char(*c)).count();
        let holds = holds(&rest[..name])?;
        let value = rest[name..]
            .strip_prefix(":-")
            .or_else(|| rest[name..].strip_prefix(":="))?;
        let end = value.bytes().take(NEAR).position(|c| c == b'}')?;
        let value = value[..end].trim_matches(['"', '\'']);
        match holds {
            Holds::Key => typed_in_bare_key(value).then_some(KEY),
            Holds::Pass if a_measure(value) => None,
            _ => typed_in_bare(value).then_some(PASSWORD),
        }
    })
}

/// Windows' `setx NAME value`, which sets a variable for good: the value
/// with no quotes, for a name that holds a password or a key (in quotes it
/// is the other rule's, [`after_a_space`]). It counts as an unquoted value
/// does.
fn set_for_good(line: &str) -> Option<&'static str> {
    let mut words = line.split_whitespace();
    while let Some(word) = words.next() {
        if !word.eq_ignore_ascii_case("setx") {
            continue;
        }
        let (name, value) = (words.next()?, words.next()?);
        return match holds(name)? {
            Holds::Key => typed_in_bare_key(value).then_some(KEY),
            Holds::Pass if a_measure(value) => None,
            _ => typed_in_bare(value).then_some(PASSWORD),
        };
    }
    None
}

/// PowerShell's `ConvertTo-SecureString "…" -AsPlainText`: what is made a
/// secret there is a secret written out. The string handed to it: the
/// first thing after its name and its flags (`-String "…"`), or what is
/// piped into it (`"…" | ConvertTo-SecureString -AsPlainText -Force`). A
/// variable there, or a file read there, is not one. The string counts as
/// a quoted password does.
fn made_secure(line: &str) -> bool {
    const NAME: &str = "convertto-securestring";
    let lower = line.to_ascii_lowercase();
    if !lower.contains("-asplaintext") {
        return false;
    }
    lower.match_indices(NAME).any(|(at, _)| {
        // Past the flags, to what it is handed.
        let mut rest = line[at + NAME.len()..].trim_start();
        while rest.starts_with('-') {
            let flag = rest.find(char::is_whitespace).unwrap_or(rest.len());
            rest = rest[flag..].trim_start();
        }
        let handed = rest
            .chars()
            .next()
            .filter(|quote| matches!(quote, '"' | '\''))
            .and_then(|quote| rest[1..].split_once(quote))
            .map(|(value, _)| value);
        // What is piped in: a string that stands by itself before the
        // `|`, not the last word of another command (`Get-Content "…" |`).
        let piped = line[..at]
            .trim_end()
            .strip_suffix('|')
            .map(str::trim_end)
            .and_then(|before| {
                let quote = before.chars().last().filter(|q| matches!(q, '"' | '\''))?;
                let inner = &before[..before.len() - 1];
                let open = inner.rfind(quote)?;
                let alone = inner[..open].trim_end();
                (alone.is_empty() || alone.ends_with(['=', '(', '{', ';', '|', ',']))
                    .then(|| &inner[open + 1..])
            });
        [handed, piped]
            .into_iter()
            .flatten()
            .any(|value| typed_in_quotes(value, Holds::Password))
    })
}

/// The parts of `text` between its commas, leaving alone the commas inside
/// strings and brackets: up to a comment, or to a bracket that closes what
/// was not opened here.
fn between_commas(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut parts = Vec::new();
    let (mut start, mut depth, mut i) = (0, 0usize, 0);
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => {
                i = after_string(b, i);
                continue;
            }
            b'#' => break,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => break,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&text[start..i.min(b.len())]);
    parts
}

/// Several names given their values at once, one of them a password:
/// `USER, PASSWORD = "gis", "…"` (also in brackets, and with Go's `:=`).
/// The value in the same place as the name counts, as a quoted value
/// does.
fn several_at_once(line: &str) -> bool {
    let b = line.as_bytes();
    // The first `=` that gives values: not part of `==`, `<=`, `>=`, `!=`.
    let Some(eq) = (0..b.len()).find(|&i| {
        b[i] == b'='
            && b.get(i + 1) != Some(&b'=')
            && (i == 0 || !matches!(b[i - 1], b'=' | b'!' | b'<' | b'>'))
    }) else {
        return false;
    };
    let left = line[..eq].trim_end_matches(':').trim();
    if !left.contains(',') {
        return false;
    }
    let left = ["const ", "let ", "var ", "my ", "local "]
        .iter()
        .find_map(|word| left.strip_prefix(word))
        .unwrap_or(left);
    let left = left
        .trim()
        .trim_start_matches(['(', '['])
        .trim_end_matches([')', ']']);
    let names: Vec<&str> = left.split(',').map(str::trim).collect();
    let plain = |n: &&str| !n.is_empty() && n.bytes().all(|c| word_char(c) || c == b'.');
    if names.len() < 2 || !names.iter().all(plain) {
        return false;
    }
    let right = line[eq + 1..].trim_start();
    let right = right.strip_prefix(['(', '[']).unwrap_or(right);
    names
        .iter()
        .zip(between_commas(right))
        .any(|(name, value)| {
            holds(name.rsplit('.').next().unwrap_or(name)).is_some_and(|holds| {
                only_a_string(value).is_some_and(|value| typed_in_quotes(value, holds))
            })
        })
}

/// What a run of base64 holds, when that is plain text: a sign-in packed
/// as a request's Basic sign-in packs it. `None` for anything else.
fn unpacked(run: &str) -> Option<String> {
    let mut out = Vec::new();
    let (mut bits, mut have) = (0u32, 0);
    for c in run.bytes() {
        let six = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(six);
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push((bits >> have) as u8);
            bits &= (1 << have) - 1;
        }
    }
    (out.iter().all(|c| (0x20..0x7f).contains(c)))
        .then(|| String::from_utf8(out).ok())
        .flatten()
}

/// `Authorization: Basic …`: a user and a password packed into one value
/// (base64 of `user:password`). The line must say `Authorization`, and
/// what is packed must read as `user:password` with a password of six
/// characters or more that is not a stand-in. The pair the standard
/// itself gives as its example (`aladdin:opensesame`, RFC 7617), which
/// texts about this sign-in repeat, is not one.
fn basic_sign_in(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if !lower.contains("authorization") {
        return false;
    }
    lower.match_indices("basic ").any(|(i, _)| {
        let rest = line[i + 6..].trim_start();
        let run = rest
            .bytes()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'-' | b'_'))
            .count();
        run >= 8
            && unpacked(&rest[..run]).is_some_and(|pair| {
                !pair.eq_ignore_ascii_case("aladdin:opensesame")
                    && pair.split_once(':').is_some_and(|(_, password)| {
                        password.chars().count() >= 6
                            && !password.contains(' ')
                            && !placeholder(password)
                    })
            })
    })
}

/// Oracle's own programs, which take a sign-in as `user/password@database`.
const ORACLE_PROGRAMS: &[&str] = &["sqlplus", "sqlldr", "expdp", "impdp", "exp", "imp"];

/// Those of them whose names are no plain words: a line that names one
/// may hold an Oracle sign-in anywhere. (`exp` and `imp` are plain words
/// too, as in `np.exp(…)`: they count only right before the sign-in.)
const ORACLE_WORDS: &[&str] = &["sqlplus", "sqlldr", "expdp", "impdp"];

/// Other signs that a line may hold an Oracle sign-in: Python's Oracle
/// drivers, a call that connects, a name that says it holds what one
/// connects with, the `userid=` of Oracle's programs. (A name with `dsn`
/// at its start or end, and GDAL's `OCI:` right before the sign-in, are
/// two more: see [`oracle_sign_in`].)
const ORACLE_SIGNS: &[&str] = &[
    "cx_oracle",
    "oracledb",
    "connect(",
    "connect_string",
    "connection_string",
    "conn_str",
    "connstr",
    "userid=",
];

/// The password in a word that has Oracle's shape, `user/password`, with
/// or without `@database` behind it (and with `userid=` before it, as
/// Oracle's programs take it): when the user is a plain name and the
/// password counts as an unquoted value does.
fn oracle_word(word: &str) -> bool {
    let word = word
        .trim_start_matches(['"', '\'', '(', '['])
        .trim_end_matches(['"', '\'', ',', ';', ')', ']']);
    let word = match word.get(..7) {
        Some(start) if start.eq_ignore_ascii_case("userid=") => &word[7..],
        _ => word,
    };
    let Some((user, rest)) = word.split_once('/') else {
        return false;
    };
    let named = user.bytes().next().is_some_and(|c| c.is_ascii_alphabetic())
        && user
            .bytes()
            .all(|c| word_char(c) || matches!(c, b'$' | b'#' | b'.'));
    named && typed_in_bare(rest.split('@').next().unwrap_or(rest))
}

/// The program a word of a command runs, in small letters: without the
/// folders before it, what a list puts around it, and `.exe`. `sqlplus`
/// for `C:\oracle\bin\sqlplus.exe` and for the `["sqlplus",` of a list;
/// `np.exp` for `np.exp`, which is not `exp`.
fn program(word: &str) -> String {
    let word = word.trim_end_matches(['"', '\'', ',', ';', ')', ']']);
    let name = word
        .bytes()
        .rev()
        .take_while(|c| word_char(*c) || *c == b'.')
        .count();
    let name = word[word.len() - name..].to_ascii_lowercase();
    match name.strip_suffix(".exe") {
        Some(plain) => plain.to_string(),
        None => name,
    }
}

/// Oracle's `user/password@database`, on a line that says it is one: a
/// `sqlplus`, `sqlldr`, `expdp`, `impdp`, `exp` or `imp` line, SQL*Plus's
/// `CONNECT`, or a connect string ([`ORACLE_SIGNS`]). The shape alone is
/// too common (`actions/checkout@v4`, `data/2024@v1`). Right after one of
/// those programs' names (or `CONNECT` at the start of a line),
/// `user/password` counts with no database behind it too: there it is the
/// sign-in, not a path. The password counts as an unquoted value does.
fn oracle_sign_in(line: &str) -> bool {
    if !line.contains('/') {
        return false;
    }
    // Right after the program's name, past its flags. `exp`, `imp` and
    // `connect` are plain words too: after them the sign-in must name its
    // database, end the line, or stand before a setting (`file=…`).
    let words: Vec<&str> = line.split_whitespace().collect();
    let after_a_program = words.iter().enumerate().any(|(k, word)| {
        let program = program(word);
        let connect = k == 0 && matches!(program.as_str(), "connect" | "conn");
        if !connect && !ORACLE_PROGRAMS.contains(&program.as_str()) {
            return false;
        }
        let Some(at) = (k + 1..words.len()).find(|n| !words[*n].starts_with('-')) else {
            return false;
        };
        let certain = !(connect || matches!(program.as_str(), "exp" | "imp"))
            || words[at].contains('@')
            || words.get(at + 1).is_none_or(|after| after.contains('='));
        certain && oracle_word(words[at])
    });
    if after_a_program {
        return true;
    }
    // Anywhere on a line that says so, with the database behind it.
    if !line.contains('@') {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    let says_so = ORACLE_SIGNS.iter().any(|sign| lower.contains(sign))
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| {
                ORACLE_WORDS.contains(&word) || word.starts_with("dsn") || word.ends_with("dsn")
            });
    let b = line.as_bytes();
    line.match_indices('/').any(|(slash, _)| {
        // The user: a name right before the slash, where a word starts
        // (not a part of a path or of an address).
        let name = b[..slash]
            .iter()
            .rev()
            .take_while(|c| word_char(**c) || matches!(**c, b'$' | b'#' | b'.'))
            .count();
        let start = slash - name;
        if name == 0
            || !b[start].is_ascii_alphabetic()
            || (start > 0 && matches!(b[start - 1], b'/' | b'\\' | b'@' | b'-'))
            || !(says_so || lower[..start].ends_with("oci:"))
        {
            return false;
        }
        // The password: up to the `@`, with a database behind it.
        let rest = &line[slash + 1..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '@' | '/' | '"' | '\'' | ',' | ')' | ';')
            })
            .unwrap_or(rest.len());
        let database = rest[end..].strip_prefix('@').and_then(|d| d.bytes().next());
        database.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'"' | b'('))
            && typed_in_bare(&rest[..end])
    })
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
    const PW: &str = concat!("Wint3r", "2026");
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
        let values = [PW, &m20, &m32, "opensesame", "admin123"];
        let mut wrong = missed(&cases, &values);
        // The same with the line ends Windows writes.
        let windows: Vec<(&str, String, usize, &str)> = cases
            .iter()
            .map(|(path, text, line, what)| (*path, text.replace('\n', "\r\n"), *line, *what))
            .collect();
        wrong.extend(missed(&windows, &values));
        assert!(
            wrong.is_empty(),
            "{} of {} are not found:\n{}",
            wrong.len(),
            2 * cases.len(),
            wrong.join("\n")
        );
    }

    /// A note as Windows saves it is read like any other: UTF-16 with its
    /// mark in front (what PowerShell's `>` writes), or UTF-8 with one.
    #[test]
    fn text_as_windows_saves_it_is_searched_too() {
        let text = format!("user: gis\r\npassword: {PW}\r\n");
        let little: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let big: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let marked: Vec<u8> = [0xEF, 0xBB, 0xBF].into_iter().chain(text.bytes()).collect();
        for bytes in [&little, &big, &marked] {
            let found = find("notes.txt", bytes, &[]);
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!((found[0].line, found[0].what.as_str()), (2, PASSWORD));
            assert!(!format!("{found:?}").contains(PW));
        }
        // A setting on the very first line, behind the mark.
        let first = format!("{}token: {}\r\nportal: gis\r\n", '\u{feff}', mixed(32));
        let found = find("conf.yaml", first.as_bytes(), &[]);
        assert_eq!(
            found.iter().map(|f| f.line).collect::<Vec<_>>(),
            [1],
            "{found:?}"
        );
        // The linked server's own token, in a file saved that way.
        let own = vec!["zq7k-FAKE-token-0001".to_string()];
        let note: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "pasted zq7k-FAKE-token-0001 here\r\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(find("notes.txt", &note, &own).len(), 1);
        // The same text that only names a password stays quiet, and bytes
        // that merely start like the mark are not text.
        let quiet: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "Ask Ana for the password.\r\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(find("notes.txt", &quiet, &[]), []);
        for bytes in [
            vec![0xFF, 0xFE],
            vec![0xFF, 0xFE, 0x41],
            vec![0xFF, 0xFE, 0, 0, 1, 2, 3, 4],
            vec![0xFE, 0xFF, 0, 0, 0xD8, 0x00],
        ] {
            assert_eq!(find("x.bin", &bytes, &[]), [], "{bytes:?}");
        }
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
        // As they are, and with the line ends Windows writes.
        let wrong: Vec<String> = cases
            .iter()
            .flat_map(|(path, text)| [(*path, text.clone()), (*path, text.replace('\n', "\r\n"))])
            .filter_map(|(path, text)| {
                hit_in(path, &text).map(|(line, what)| {
                    format!("{path} should not stop: {text:?} -> line {line} ({what})")
                })
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "{} of {} stop:\n{}",
            wrong.len(),
            2 * cases.len(),
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

    /// A user and a password packed as a request's Basic sign-in packs
    /// them (base64 of `user:password`), for the tests.
    fn basic(user: &str, password: &str) -> String {
        const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let text = format!("{user}:{password}");
        let mut out = String::new();
        for three in text.as_bytes().chunks(3) {
            let n = (u32::from(three[0]) << 16)
                | (u32::from(*three.get(1).unwrap_or(&0)) << 8)
                | u32::from(*three.get(2).unwrap_or(&0));
            for k in 0..4 {
                if k <= three.len() {
                    out.push(LETTERS[((n >> (18 - 6 * k)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// A password handed to a well-known call by its place, with no name
    /// beside it, and the other forms the first round of R-105 left out:
    /// each on its own, with the line the finding must name.
    #[test]
    fn well_known_calls_and_forms_stop_a_sync() {
        let m32 = mixed(32);
        let packed = basic("gis", PW);
        assert_eq!(basic("user", "pass"), "dXNlcjpwYXNz");
        let call = "handed to a call that signs in";
        let quoted = "a password or key written into the file (a quoted value)";
        let password = "a password written into the file";
        let oracle = "an Oracle sign-in";
        let cases: Vec<(&str, String, usize, &str)> = vec![
            // ArcGIS: the third string of GIS, by any of its names.
            ("sign_in.py", format!("from arcgis.gis import GIS\ngis = GIS(\"https://www.arcgis.com\", \"ana\", \"{PW}\")\n"), 2, call),
            ("sign_in.py", format!("portal = arcgis.gis.GIS(url, \"ana\", \"{PW}\")\n"), 1, call),
            ("sign_in.py", format!("portal = gis.GIS(url, 'ana', '{PW}', verify_cert=False)\n"), 1, call),
            // Over several lines, as a formatter lays a call out: with a
            // comma after the last argument, and without.
            ("sign_in.py", format!("g = arcgis.gis.GIS(\n    \"https://gis.corp.example/portal\",\n    \"ana\",\n    \"{PW}\",\n    verify_cert=False,\n)\n"), 4, call),
            ("sign_in.py", format!("g = GIS(\n    url,  # the county's portal\n    'ana',\n    '{PW}'\n)\nprint(g)\n"), 4, call),
            // In a note and in a notebook's code.
            ("notes.md", format!("Sign in first: `GIS(\"https://www.arcgis.com\", \"ana\", \"{PW}\")`.\n"), 1, call),
            ("nb.ipynb", notebook(&[("code", &["from arcgis.gis import GIS", &format!("gis = GIS(url, \"ana\", \"{PW}\")")])]), 10, call),
            // arcpy: the portal, a connection file, a geodatabase.
            ("a.py", format!("arcpy.SignInToPortal(\"https://gis.corp.example/portal\", \"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("arcpy.SignInToPortal(arcpy.GetActivePortalURL(), \"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("arcpy.management.CreateDatabaseConnection(out, \"prod.sde\", \"SQL_SERVER\", \"db1\", \"DATABASE_AUTH\", \"gis\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("arcpy.CreateDatabaseConnection_management(r\"C:\\conn\", \"prod.sde\", \"ORACLE\", \"db1/orcl\", \"DATABASE_AUTH\", \"gis\", \"{PW}\", \"SAVE_USERNAME\")\n"), 1, call),
            ("a.py", format!("arcpy.management.CreateDatabaseConnection(\n    out_folder,\n    \"prod.sde\",\n    \"POSTGRESQL\",\n    \"db1.corp.example\",\n    \"DATABASE_AUTH\",\n    \"gis\",\n    \"{PW}\",\n    \"SAVE_USERNAME\",\n    \"roads\",\n)\n"), 8, call),
            ("a.py", format!("egdb = arcpy.ArcSDESQLExecute(\"db1\", \"sde:oracle11g:orcl\", \"\", \"gis\", \"{PW}\")\n"), 1, call),
            // arcpy's other tools that are handed a password by its place.
            ("a.py", format!("cs = arcpy.management.CreateDatabaseConnectionString(\"SQL_SERVER\", \"db1\", \"DATABASE_AUTH\", \"gis\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("cs = arcpy.CreateDatabaseConnectionString_management(\"ORACLE\", \"db1/orcl\", \"DATABASE_AUTH\", \"gis\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("arcpy.management.CreateEnterpriseGeodatabase(\"SQL_Server\", \"db1\", \"roads\", \"DATABASE_AUTH\", \"sa\", \"{PW}\", \"SDE_SCHEMA\", \"sde\", sde_pw, \"\", keycodes)\n"), 1, call),
            ("a.py", format!("arcpy.CreateEnterpriseGeodatabase_management(\"SQL_Server\", \"db1\", \"roads\", \"DATABASE_AUTH\", \"sa\", sa_pw, \"SDE_SCHEMA\", \"sde\", \"{PW}\", \"\", keycodes)\n"), 1, call),
            ("a.py", format!("arcpy.management.CreateDatabaseUser(sde, \"DATABASE_USER\", \"editor1\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("arcpy.CreateDatabaseUser_management(sde, \"DATABASE_USER\", \"editor1\", \"{PW}\", \"editors\")\n"), 1, call),
            ("a.py", format!("arcpy.CreateArcSDEConnectionFile_management(out, \"prod.sde\", \"db1\", \"5151\", \"\", \"DATABASE_AUTH\", \"gis\", \"{PW}\")\n"), 1, call),
            // A new member's first password (ArcGIS API for Python).
            ("a.py", format!("gis.users.create(\"ana2\", \"{PW}\", \"Ana\", \"Gis\", \"ana@county.example\")\n"), 1, call),
            // A blank between a call's name and its bracket, in a script.
            ("a.py", format!("gis = GIS (url, \"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("arcpy.SignInToPortal (url, \"ana\", \"{PW}\")\n"), 1, call),
            // Web clients: a pair given as the sign-in.
            ("a.py", format!("r = requests.get(url, auth=(\"ana\", \"{PW}\"))\n"), 1, call),
            ("a.py", format!("session.auth = (\"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("options = {{\"timeout\": 30, \"auth\": (\"ana\", \"{PW}\")}}\n"), 1, call),
            ("a.py", format!("AUTH = (\n    \"ana\",\n    \"{PW}\",\n)\n"), 3, call),
            ("a.py", format!("es = Elasticsearch(hosts, http_auth=('ana', '{PW}'))\n"), 1, call),
            ("a.py", format!("auth = HTTPBasicAuth(\"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("r = requests.get(url, auth=requests.auth.HTTPDigestAuth('ana', '{PW}'))\n"), 1, call),
            ("a.py", format!("client = httpx.Client(auth=httpx.BasicAuth(\"ana\", \"{PW}\"))\n"), 1, call),
            ("a.py", format!("auth = aiohttp.BasicAuth(\"ana\", \"{PW}\")\n"), 1, call),
            // A default given to a value read from the environment.
            ("a.py", format!("password = os.environ.get(\"PGPASSWORD\", \"{PW}\")\n"), 1, quoted),
            ("a.py", format!("secret = os.getenv(\"AGOL_PASSWORD\", \"{PW}\")\n"), 1, quoted),
            ("a.py", format!("os.environ.setdefault(\"PGPASSWORD\", \"{PW}\")\n"), 1, quoted),
            ("a.py", format!("os.putenv('PGPASSWORD', '{PW}')\n"), 1, quoted),
            ("a.py", format!("key = os.environ.get(\"ARCGIS_API_KEY\", \"{m32}\")\n"), 1, quoted),
            ("a.py", format!("key = os.getenv(\n    \"MAPBOX_ACCESS_TOKEN\",\n    \"{m32}\",\n)\n"), 3, quoted),
            // The default given by its name, and a value to fall back on.
            ("a.py", format!("password = os.getenv(\"PGPASSWORD\", default=\"{PW}\")\n"), 1, quoted),
            ("a.py", format!("password = os.environ.get('PGPASSWORD', default='{PW}')\n"), 1, quoted),
            ("a.r", format!("pw <- Sys.getenv(\"PGPASSWORD\", unset = \"{PW}\")\n"), 1, quoted),
            ("a.py", format!("password = os.environ.get(\"PGPASSWORD\") or \"{PW}\"\n"), 1, quoted),
            ("a.py", format!("pw = os.getenv(\"AGOL_PW\") or '{PW}'\n"), 1, quoted),
            ("a.py", format!("conn = connect(password=os.getenv(\"PGPASSWORD\") or \"{PW}\", host=h)\n"), 1, quoted),
            ("a.php", format!("$pw = getenv('DB_PASSWORD') ?: '{PW}';\n"), 1, quoted),
            ("a.cs", format!("var p = Environment.GetEnvironmentVariable(\"PGPASSWORD\") ?? \"{PW}\";\n"), 1, quoted),
            // Read from the environment as other languages do.
            ("a.js", format!("const p = process.env.DB_PASSWORD || \"{PW}\";\n"), 1, quoted),
            ("a.js", format!("const k = process.env[\"ARCGIS_API_KEY\"] ?? \"{m32}\";\n"), 1, quoted),
            ("a.ts", format!("const k = import.meta.env.VITE_ARCGIS_API_KEY || \"{m32}\";\n"), 1, quoted),
            ("a.ps1", format!("$p = $env:PGPASSWORD ?? \"{PW}\"\n"), 1, quoted),
            ("a.rb", format!("db = ENV[\"DB_PASSWORD\"] || \"{PW}\"\n"), 1, quoted),
            ("run.sh", format!("export PGPASSWORD=\"${{PGPASSWORD:-{PW}}}\"\n"), 1, password),
            ("run.sh", format!("PGPASSWORD=${{PGPASSWORD:-{PW}}} psql -h db1 roads\n"), 1, password),
            ("compose.yml", format!("      POSTGRES_PASSWORD: ${{POSTGRES_PASSWORD:-{PW}}}\n"), 1, password),
            ("run.sh", format!(": \"${{ARCGIS_API_KEY:={m32}}}\"\n"), 1, "a token, key or secret"),
            // Windows' own: a variable set for good, and what PowerShell
            // makes a secret.
            ("setup.bat", format!("setx PGPASSWORD {PW}\n"), 1, password),
            ("setup.bat", format!("SETX ARCGIS_API_KEY {m32} /M\n"), 1, "a token, key or secret"),
            ("setup.ps1", format!("[Environment]::SetEnvironmentVariable(\"PGPASSWORD\", \"{PW}\", \"User\")\n"), 1, quoted),
            ("a.ps1", format!("$p = [Environment]::GetEnvironmentVariable(\"PGPASSWORD\", \"User\") ?? \"{PW}\"\n"), 1, quoted),
            ("a.ps1", format!("$secure = ConvertTo-SecureString \"{PW}\" -AsPlainText -Force\n"), 1, call),
            ("a.ps1", format!("$secure = ConvertTo-SecureString -String '{PW}' -AsPlainText -Force\n"), 1, call),
            ("a.ps1", format!("$secure = convertto-securestring -AsPlainText -Force -String \"{PW}\"\n"), 1, call),
            ("a.ps1", format!("$cred = New-Object System.Management.Automation.PSCredential (\"siteadmin\", (ConvertTo-SecureString \"{PW}\" -AsPlainText -Force))\n"), 1, call),
            ("a.ps1", format!("$secure = \"{PW}\" | ConvertTo-SecureString -AsPlainText -Force\n"), 1, call),
            // A string to fall back on, given to a name that holds a
            // password.
            ("a.py", format!("password = args.password or \"{PW}\"\n"), 1, quoted),
            ("tool.py", format!("password = arcpy.GetParameterAsText(1) or \"{PW}\"\n"), 1, quoted),
            ("a.js", format!("const password = opts.password || \"{PW}\";\n"), 1, quoted),
            ("a.py", format!("conn = connect(user=u, password=p or \"{PW}\", host=h)\n"), 1, quoted),
            ("a.py", format!("settings = {{\"password\": (stored or \"{PW}\")}}\n"), 1, quoted),
            // Read from the environment inside another call.
            ("a.py", format!("gis = GIS(url, \"ana\", os.environ.get(\"AGOL_PW\", \"{PW}\"))\n"), 1, quoted),
            ("a.py", format!("auth = HTTPBasicAuth(\"ana\", os.getenv(\"PORTAL_PASSWORD\", \"{PW}\"))\n"), 1, quoted),
            ("a.py", format!("r = requests.get(url, auth=(\"ana\", os.environ.get(\"PORTAL_PASSWORD\", \"{PW}\")))\n"), 1, quoted),
            ("a.py", format!("gis = GIS(url, \"ana\", os.environ.get(\n    \"AGOL_PW\", \"{PW}\"))\n"), 2, quoted),
            // Oracle: by place, and as user/password@database.
            ("a.py", format!("conn = cx_Oracle.connect(\"gis\", \"{PW}\", \"db1.corp.example/orcl\")\n"), 1, call),
            ("a.py", format!("conn = cx_Oracle.Connection(\"gis\", \"{PW}\", dsn)\n"), 1, call),
            // SQL Server and MySQL, by place.
            ("a.py", format!("conn = pymssql.connect(\"db1\", \"gis\", \"{PW}\", \"roads\")\n"), 1, call),
            ("a.py", format!("conn = MySQLdb.connect(\"db1\", \"gis\", \"{PW}\", \"roads\")\n"), 1, call),
            ("a.py", format!("conn = pymysql.connect(\"db1\", \"gis\", \"{PW}\", \"roads\")\n"), 1, call),
            // A file server and a mail server: `login(user, password)`.
            ("a.py", format!("ftp.login(\"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("server.login(\"gis@county.example\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("ftp = FTP(\"ftp.county.example\", \"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("ftp = ftplib.FTP_TLS(\"ftp.county.example\", \"ana\", \"{PW}\")\n"), 1, call),
            ("a.py", format!("conn = oracledb.connect(\"gis/{PW}@db1.corp.example:1521/orcl\")\n"), 1, oracle),
            ("a.py", format!("conn = cx_Oracle.connect('gis/{PW}@orcl')\n"), 1, oracle),
            ("a.py", format!("dsn = \"gis/{PW}@//db1.corp.example:1521/orcl\"\n"), 1, oracle),
            ("load.sh", format!("sqlplus gis/{PW}@orcl @load.sql\n"), 1, oracle),
            ("load.sh", format!("sqlplus -s gis/{PW}@//db1:1521/orcl <<EOF\n"), 1, oracle),
            ("load.sh", format!("sqlplus \"gis/{PW}@(DESCRIPTION=(ADDRESS=(HOST=db1)(PORT=1521))(CONNECT_DATA=(SID=orcl)))\"\n"), 1, oracle),
            // With no database behind it, right after the program's name.
            ("load.sh", format!("sqlplus -s gis/{PW} @load.sql\n"), 1, oracle),
            ("load.bat", format!("C:\\oracle\\bin\\sqlplus.exe gis/{PW}\n"), 1, oracle),
            ("dump.sh", format!("exp gis/{PW} file=gis.dmp\n"), 1, oracle),
            ("dump.sh", format!("imp userid=gis/{PW} file=gis.dmp\n"), 1, oracle),
            ("load.sql", format!("connect gis/{PW}\n"), 1, oracle),
            ("load.bat", format!("sqlldr userid=gis/{PW}@orcl control=roads.ctl\n"), 1, oracle),
            ("load.par", format!("userid=gis/{PW}@orcl\n"), 1, oracle),
            ("dump.sh", format!("expdp gis/{PW}@orcl schemas=gis dumpfile=gis.dmp\n"), 1, oracle),
            ("dump.sh", format!("impdp gis/{PW}@orcl schemas=gis dumpfile=gis.dmp\n"), 1, oracle),
            ("dump.sh", format!("exp gis/{PW}@orcl file=gis.dmp\n"), 1, oracle),
            ("dump.sh", format!("imp gis/{PW}@orcl file=gis.dmp full=y\n"), 1, oracle),
            ("load.sql", format!("CONNECT gis/{PW}@orcl\n"), 1, oracle),
            ("load.sql", format!("conn gis/{PW}@orcl\n"), 1, oracle),
            ("run.sh", format!("ogr2ogr -f GeoJSON roads.json OCI:gis/{PW}@orcl:ROADS\n"), 1, oracle),
            // A sign-in packed into one value.
            ("call.sh", format!("curl -H \"Authorization: Basic {packed}\" https://gis.corp.example/rest\n"), 1, "Authorization: Basic"),
            ("call.py", format!("headers = {{\"Authorization\": \"Basic {packed}\"}}\n"), 1, "Authorization: Basic"),
            ("call.py", format!("headers = {{'authorization': 'basic {}'}}\n", basic("admin", "geoserver")), 1, "Authorization: Basic"),
            // Several names given their values at once.
            ("a.py", format!("USER, PASSWORD = \"gis\", \"{PW}\"\n"), 1, quoted),
            ("a.py", format!("user, pwd = (\"gis\", \"{PW}\")\n"), 1, quoted),
            ("a.py", format!("host, user, password, db = 'db1', 'gis', '{PW}', 'roads'\n"), 1, quoted),
            ("a.go", format!("user, password := \"gis\", \"{PW}\"\n"), 1, quoted),
            // `pw`, as `pass`.
            ("a.py", format!("pw = \"{PW}\"\n"), 1, quoted),
            ("a.py", format!("AGOL_PW = '{PW}'\n"), 1, quoted),
            ("notes.md", format!("user: gis\npw: {PW}\n"), 2, password),
            ("conf.yaml", format!("db_pw: {PW}\n"), 1, password),
            ("run.sh", format!("plink -pw {PW} ana@files.corp.example\n"), 1, "on a command line"),
            // MySQL's -p with the password right behind it.
            ("run.sh", format!("mysql -u root -p{PW} gis < load.sql\n"), 1, "on a command line"),
            ("run.sh", format!("mysqldump -uroot -p{PW} gis > gis.sql\n"), 1, "on a command line"),
            ("run.sh", format!("mariadb --user=root -p'{PW}' gis\n"), 1, "on a command line"),
        ];
        let values = [PW, m32.as_str(), packed.as_str(), "geoserver"];
        let mut wrong = missed(&cases, &values);
        let windows: Vec<(&str, String, usize, &str)> = cases
            .iter()
            .map(|(path, text, line, what)| (*path, text.replace('\n', "\r\n"), *line, *what))
            .collect();
        wrong.extend(missed(&windows, &values));
        assert!(
            wrong.is_empty(),
            "{} of {} are not found:\n{}",
            wrong.len(),
            2 * cases.len(),
            wrong.join("\n")
        );
        // Oracle's `exp` and `imp` as items of a list: found before the
        // two words stopped counting anywhere on a line, and still.
        for text in [
            format!("subprocess.run([\"exp\", \"gis/{PW}@orcl\", \"file=gis.dmp\"])\n"),
            format!("os.system(\"imp gis/{PW}@orcl file=gis.dmp full=y\")\n"),
            format!("subprocess.run([\"sqlplus\", \"gis/{PW}@orcl\", \"@load.sql\"])\n"),
            format!("DSN = \"gis/{PW}@db1/orcl\"\n"),
            format!("oraDsn = \"gis/{PW}@db1/orcl\"\n"),
        ] {
            let got = hit_in("a.py", &text);
            assert!(
                got.as_ref().is_some_and(|(_, w)| w.contains(oracle)),
                "{text} -> {got:?}"
            );
        }
        // An address with a user and a password in it was stopped before
        // R-105, and still is.
        for text in [
            format!("url = \"postgresql://ana:{PW}@db1/roads\"\n"),
            format!("engine = create_engine('oracle+oracledb://gis:{PW}@db1:1521/?service_name=orcl')\n"),
        ] {
            let got = hit_in("a.py", &text);
            assert!(
                got.as_ref().is_some_and(|(_, w)| w.contains("an address with a password")),
                "{text} -> {got:?}"
            );
        }
    }

    /// Near misses of those calls and forms: a variable, a call or a value
    /// read from the environment in the password's place, a call with too
    /// few arguments, a stand-in, and ordinary text of the same shape.
    #[test]
    fn near_misses_of_the_well_known_calls_do_not() {
        let cases: Vec<(&str, String)> = vec![
            // GIS without a password written into it.
            ("a.py", "gis = GIS(url, username, password)\n".into()),
            ("a.py", "gis = GIS(\"https://www.arcgis.com\", \"ana\")\n".into()),
            ("a.py", "gis = GIS(\"pro\")\n".into()),
            ("a.py", "gis = GIS(profile=\"work\")\n".into()),
            ("a.py", "gis = GIS(url, \"ana\", getpass.getpass())\n".into()),
            ("a.py", "gis = GIS(url, \"ana\", os.environ[\"AGOL_PW\"])\n".into()),
            ("a.py", "gis = GIS()\n".into()),
            ("a.py", "gis = GIS(\"https://www.arcgis.com\", \"username\", \"password\")\n".into()),
            ("a.py", "gis = GIS(\"https://www.arcgis.com\", \"ana\", verify_cert=False)\n".into()),
            ("a.py", "gis = GIS(url, \"ana\", \"\")\n".into()),
            ("a.py", "gis = GIS(url, \"ana\", f\"{pw}\")\n".into()),
            ("a.py", "gis = GIS(url, \"ana\", pw + \"suffix1\")\n".into()),
            ("a.py", "gis = GIS(*args)\ngis = GIS(**settings)\n".into()),
            ("a.py", "gis = GIS(\n    url,\n    user,\n    password,\n)\nlayer = \"roads2026\"\n".into()),
            ("a.py", "gis = GIS(\n    \"https://www.arcgis.com\",\n    \"ana\",\n    profile=\"county2026\",\n)\n".into()),
            ("a.py", "class GIS(Base):\n    def __init__(self, url, username=None, password=None):\n        pass\n".into()),
            ("a.py", "layer = QGIS(\"roads\", \"north\", \"zone2026\")\nweb = WebGIS(\"a\", \"b\", \"zone2026\")\n".into()),
            ("notes.md", "The GIS(url, username, password) call signs in; ArcGIS (2026, \"release1\", \"build2026\") is not it.\n".into()),
            ("notes.md", "Open GIS(\n\nand then, later, \"roads2026\",\n\"north2026\",\n\"zone2026\"\n".into()),
            // arcpy calls without one.
            ("a.py", "arcpy.SignInToPortal(arcpy.GetActivePortalURL(), user, pw)\n".into()),
            ("a.py", "arcpy.SignInToPortal(\"https://gis.corp.example/portal\")\n".into()),
            ("a.py", "arcpy.management.CreateDatabaseConnection(out, \"prod.sde\", \"SQL_SERVER\", \"db1\", \"OPERATING_SYSTEM_AUTH\")\n".into()),
            ("a.py", "arcpy.management.CreateDatabaseConnection(out, \"prod.sde\", \"SQL_SERVER\", \"db1\", \"DATABASE_AUTH\", user, pw)\n".into()),
            ("a.py", "arcpy.management.CreateDatabaseConnection(out, \"prod.sde\", \"SQL_SERVER\", \"db1\", \"DATABASE_AUTH\", \"gis\", \"#\", \"SAVE_USERNAME\")\n".into()),
            ("a.py", "arcpy.management.CreateDatabaseConnection(out, \"roads2026.sde\", \"POSTGRESQL\", \"db1.corp.example\", \"DATABASE_AUTH\", \"gis2026\")\n".into()),
            ("a.py", "egdb = arcpy.ArcSDESQLExecute(r\"C:\\connections\\prod2026.sde\")\n".into()),
            ("a.py", "egdb = arcpy.ArcSDESQLExecute(\"db1\", \"5151\", \"roads2026\", user, pw)\n".into()),
            // Web clients.
            ("a.py", "r = requests.get(url, auth=(user, pw))\n".into()),
            ("a.py", "options = {\"timeout\": 30, \"auth\": (user, pw)}\n".into()),
            ("a.py", "auth = (\n    os.environ[\"AGOL_USER\"],\n    os.environ[\"AGOL_PW\"],\n)\nlayer = \"roads2026\"\n".into()),
            ("a.py", "def fetch(url, auth: tuple = (\"\", \"\")):\n".into()),
            ("a.py", "r = requests.get(url, auth=(\"ana\", password))\n".into()),
            ("a.py", "r = requests.get(url, auth=(\"x-access-token\", token))\n".into()),
            ("a.py", "r = requests.get(url, auth=(token, \"x-oauth-basic\"))\n".into()),
            ("a.py", "r = requests.get(url, auth=(\"username\", \"password\"))\n".into()),
            ("a.py", "auth = HTTPBasicAuth(user, pw)\n".into()),
            ("a.py", "auth = HTTPBasicAuth(\"ana\", os.environ[\"AGOL_PW\"])\n".into()),
            ("a.py", "client = httpx.Client(auth=httpx.BasicAuth(user, pw))\n".into()),
            ("a.py", "auth = (\"roads2026\",)\nauthors = (\"ana\", \"benito2026\")\n".into()),
            // The environment, read without a default that is a secret.
            ("a.py", "password = os.environ.get(\"PGPASSWORD\")\n".into()),
            ("a.py", "password = os.environ.get(\"PGPASSWORD\", \"\")\n".into()),
            ("a.py", "password = os.getenv(\"PGPASSWORD\", None)\n".into()),
            ("a.py", "host = os.environ.get(\"PGHOST\", \"db1.corp.example\")\n".into()),
            ("a.py", "password = os.environ.get(\"PGPASSWORD\", default_pw)\n".into()),
            ("a.py", "key = os.environ.get(\"ARCGIS_API_KEY\", \"<your key>\")\n".into()),
            ("a.py", "os.environ.setdefault(\"PGPASSFILE\", \"/home/ana/.pgpass\")\n".into()),
            ("a.py", "os.environ.setdefault(\"PGPASSWORD\", password)\n".into()),
            ("a.py", "sort = os.environ.get(\"SORT_KEY\", \"name\")\n".into()),
            ("a.py", "layer = settings.get(\"password_field\", \"PWD_HASH2\")\n".into()),
            // Oracle without a password in it.
            ("a.py", "conn = oracledb.connect(user=user, password=pw, dsn=dsn)\n".into()),
            ("a.py", "conn = cx_Oracle.connect(user, pw, dsn)\n".into()),
            ("a.py", "conn = cx_Oracle.connect(\"gis\", pw, \"db1.corp.example/orcl\")\n".into()),
            ("a.py", "conn = oracledb.connect(user=\"gis\", password=pw, dsn=\"db1.corp.example:1521/orcl\")\n".into()),
            ("a.sh", "sqlplus /nolog\n".into()),
            ("a.sh", "sqlplus ana@roads\n".into()),
            ("a.sh", "sqlplus -s gis@orcl @scripts/load2026.sql\n".into()),
            ("a.sh", "sqlplus gis/$ORA_PW@orcl @load.sql\n".into()),
            ("a.sh", "sqlplus / as sysdba\n".into()),
            ("a.sh", "expdp gis@orcl directory=dp2026 dumpfile=roads2026.dmp\n".into()),
            ("a.sh", "expdp gis@orcl dumpfile=exports/roads2026.dmp logfile=logs/run2026\n".into()),
            ("a.sh", "sqlldr gis@orcl control=ctl/roads2026.ctl log=logs/run2026\n".into()),
            ("a.sh", "exp file=dumps/roads2026.dmp owner=gis\n".into()),
            ("a.sh", "sqlplus -s /nolog @scripts/load2026.sql\n".into()),
            ("a.sql", "connect / as sysdba\nconnect gis@orcl\n".into()),
            ("a.md", "To connect roads2026/north2026 lanes, see the plan.\n".into()),
            ("a.md", "Connect roads/trails2026 layers before the north2026 run.\n".into()),
            ("a.md", "exp x/y2z3w4 is the ratio we plot; imp a/b2c3d4 was dropped.\n".into()),
            ("a.sh", "imp userid=gis/${ORA_PW}@orcl file=gis2026.dmp\n".into()),
            ("notes.md", "The layer is \"ana/roads@2024\" in the old naming.\n".into()),
            ("notes.md", "Copied to data/2024@v1 and to exports/roads2026@final.\n".into()),
            ("a.yml", "    uses: actions/checkout@v4\n    uses: county/deploy2026@main\n".into()),
            ("a.md", "docker pull ghcr.io/county/roads2026@sha256:0a1b2c\n".into()),
            // Addresses without a password.
            ("a.py", "url = \"postgresql://ana@db1/roads\"\n".into()),
            ("a.py", "url = f\"postgresql://{user}:{pw}@db1/roads\"\n".into()),
            ("a.py", "url = \"https://example.com:8443/arcgis\"\n".into()),
            // The word Basic without a sign-in behind it.
            ("a.md", "Authorization: Basic <credentials>\n".into()),
            ("a.md", format!("Authorization: Basic {}\n", basic("username", "password"))),
            ("a.md", format!("Authorization: Basic {}\n", basic("user", "pass"))),
            ("a.md", format!("Authorization: Basic {}\n", basic("aladdin", "opensesame"))),
            ("a.md", format!("Authorization: Basic {}\n", basic("Aladdin", "open sesame"))),
            ("a.py", "headers = {\"Authorization\": \"Basic \" + b64encode(pair).decode()}\n".into()),
            ("a.py", "headers = {\"Authorization\": f\"Basic {packed}\"}\n".into()),
            ("a.md", "WWW-Authenticate: Basic realm=\"arcgis2026\"\nBasic authorization1234 is off here.\n".into()),
            ("a.md", format!("The word Basic {} alone says nothing.\n", basic("gis", PW))),
            // Several names at once, none of them given a secret.
            ("a.py", "x, y = \"12.5\", \"48.1\"\n".into()),
            ("a.py", "user, password = \"gis\", getpass.getpass()\n".into()),
            ("a.py", "user, password = line.split(\",\")\n".into()),
            ("a.py", "user, password = creds\n".into()),
            ("a.py", "USER, PASSWORD = \"gis\", \"changeme\"\n".into()),
            ("a.py", "name, layer = \"roads2026\", \"north2026\"\n".into()),
            ("a.py", "if user == \"gis\", password:\n".into()),
            // `pw` with its other meanings.
            ("a.py", "pw = get_pw()\n".into()),
            ("a.py", "pw = args.pw\n".into()),
            ("a.py", "pw = \"1024px\"\npw: 12.5mm\n".into()),
            ("conf.yaml", "pw: 1024px\npw: 0.5\n".into()),
            ("a.py", "kpw = \"roads2026\"\nmpw: north2026\n".into()),
            // MySQL without a password on the line.
            ("a.sh", "mysql -uroot -p$MYSQL_PW gis\n".into()),
            ("a.sh", "mysql -u root -p\"$DB_PASS1\" gis\n".into()),
            ("a.sh", "mysql -u root -P 3306 gis2026\n".into()),
            ("a.sh", "find . -name \"*.sql\" -print0 | xargs -0 mysql -u root\n".into()),
            ("a.sh", "tar -xpzf roads2026.tgz\npython -profile1x run.py\n".into()),
            ("a.sh", "mysql -u root -p gis2026\n".into()),
            ("a.sh", "mysqldump -u root -p --databases gis2026 > out2026.sql\n".into()),
            ("a.sh", "mysqldump -u root gis | ssh -p2222 backup@host \"cat > gis2026.sql\"\n".into()),
            // A blank before the bracket, where it is no call.
            ("a.py", "gis = GIS (url, username, password)\n".into()),
            ("notes.md", "the three GIS (\"ArcGIS\", \"QGIS\", \"MapInfo2026\") packages\n".into()),
            ("a.py", "# the three GIS (\"ArcGIS\", \"QGIS\", \"MapInfo2026\") packages\n".into()),
            // A bracket left open after a call's name, in words: what
            // follows are not its arguments.
            ("a.py", "# our GIS(the county's\nprint \"roads\", \"north\", \"zone2026\"\n".into()),
            ("a.py", "# Sign in to the GIS (the county portal,\n# not ArcGIS Online) before running.\nprint \"roads\", \"north\", \"zone2026\"\n".into()),
            ("notes.md", "We use GIS(ArcGIS Online\nfor \"roads\", \"north\", \"zone2026\"\n".into()),
            ("a.py", "# gis = GIS(url,\n#           user, pw)\nprint \"roads\", \"north\", \"zone2026\"\n".into()),
            // arcpy's other tools without a password written into them.
            ("a.py", "arcpy.management.CreateDatabaseUser(sde, \"OPERATING_SYSTEM_USER\", \"COUNTY\\\\editor1\")\n".into()),
            ("a.py", "arcpy.management.CreateDatabaseUser(sde, \"DATABASE_USER\", \"editor1\", pw, \"editors2026\")\n".into()),
            ("a.py", "arcpy.management.CreateEnterpriseGeodatabase(\"SQL_Server\", \"db1\", \"roads2026\", \"OPERATING_SYSTEM_AUTH\", \"\", \"\", \"SDE_SCHEMA\", \"sde\", sde_pw, \"\", keycodes)\n".into()),
            ("a.py", "arcpy.management.CreateDatabaseConnectionString(\"SQL_SERVER\", \"db1.corp.example\", \"OPERATING_SYSTEM_AUTH\")\n".into()),
            ("a.py", "arcpy.management.CreateDatabaseConnectionString(\"SQL_SERVER\", \"db1\", \"DATABASE_AUTH\", \"gis\", pw, \"roads2026\")\n".into()),
            ("a.py", "gis.users.create(row[\"username\"], row[\"password\"], row[\"first\"], row[\"last\"], row[\"email\"])\n".into()),
            ("a.py", "gis.users.create(username=name, password=first_password, firstname=\"Ana2026\", lastname=\"Gis2026\", email=mail)\n".into()),
            ("a.py", "gis.groups.create(\"Roads2026\", \"roads,north2026\")\n".into()),
            // Other clients without one.
            ("a.py", "conn = pymssql.connect(\"db1\", \"gis\", pw, \"roads2026\")\n".into()),
            ("a.py", "conn = pymssql.connect(server=\"db1\", user=\"gis\", password=pw, database=\"roads2026\")\n".into()),
            ("a.py", "conn = MySQLdb.connect(\"db1.corp.example\", \"gis2026\")\n".into()),
            ("a.py", "ftp.login()\nftp.login(user, pw)\nftp.login(\"ana\", getpass.getpass())\n".into()),
            ("a.py", "ftp.login(\"anonymous\", \"ana@county.gov\")\n".into()),
            ("a.py", "ftp = FTP(\"ftp.county.example\", \"anonymous\", \"ana@county.gov\")\n".into()),
            ("a.py", "ftp = FTP(\"ftp.county.example\")\nftp.cwd(\"roads2026\")\n".into()),
            // The environment: a name that holds no secret, a path to a key.
            ("a.py", "key = os.environ.get(\"S3_KEY\", \"exports/2026/roads_north_v2.parquet\")\n".into()),
            ("a.py", "field = os.getenv(\"JOIN_KEY\", \"parcel_id_2026_north\")\n".into()),
            ("a.py", "mk = os.getenv(\"MONKEY\", \"capuchin-2026-north-x1\")\n".into()),
            ("a.py", "key_path = os.getenv(\"PRIVATE_KEY\", \"keys/portal2026.pem\")\n".into()),
            ("a.py", "password = os.getenv(\"PGPASSWORD\", default=None)\n".into()),
            ("a.py", "host = os.getenv(\"PGHOST\", default=\"db1.corp.example\")\n".into()),
            ("a.py", "password = os.getenv(\"PGPASSWORD\") or default_pw\n".into()),
            ("a.py", "password = os.environ.get(\"PGPASSWORD\") or getpass.getpass()\n".into()),
            ("a.py", "password = os.environ.get(\"PGPASSWORD\") or \"\"\n".into()),
            ("a.py", "host = os.environ.get(\"PGHOST\") or \"db1.corp.example\"\n".into()),
            ("a.py", "gis = GIS(url, \"ana\", os.environ.get(\"AGOL_PW\"))\n".into()),
            ("a.py", "gis = GIS(url, \"ana\", os.environ.get(\"AGOL_PW\", \"\"))\n".into()),
            // Windows' own, without a password written out.
            ("setup.bat", "setx PGHOST db1x2026\nsetx PGPASSWORD %NEW_PW1%\nsetx /?\nsetx PGPASSWORD\n".into()),
            ("setup.bat", "echo setx sets it for good; PGPASSWORD roads2026 is not how\n".into()),
            ("a.ps1", "$t = [Environment]::GetEnvironmentVariable(\"PGPASSWORD\", \"Machine\")\n".into()),
            ("a.ps1", "$t = [Environment]::GetEnvironmentVariable(\"PGPASSWORD\", \"Process\")\n".into()),
            ("a.ps1", "[Environment]::SetEnvironmentVariable(\"PGPASSWORD\", $plain, \"User\")\n".into()),
            ("a.ps1", "[Environment]::SetEnvironmentVariable(\"PGHOST\", \"db1.corp.example\", \"Machine\")\n".into()),
            ("a.ps1", "$secure = ConvertTo-SecureString $env:PORTAL_PW -AsPlainText -Force\n".into()),
            ("a.ps1", "$secure = ConvertTo-SecureString $plain -AsPlainText -Force\n".into()),
            ("a.ps1", "$secure = ConvertTo-SecureString \"$plain\" -AsPlainText -Force\n".into()),
            ("a.ps1", "$secure = ConvertTo-SecureString (Get-Content \"pw2026.txt\") -AsPlainText -Force\n".into()),
            ("a.ps1", "$secure = Get-Content \"pw2026.txt\" | ConvertTo-SecureString -AsPlainText -Force\n".into()),
            ("a.ps1", "$secure = Get-Content \"pw2026.txt\" | ConvertTo-SecureString\n".into()),
            ("a.ps1", "$secure = Read-Host \"Password for roads2026\" -AsSecureString\n".into()),
            ("a.ps1", "# ConvertTo-SecureString wants -AsPlainText for \"roads2026\" too\n".into()),
            ("a.py", "ftp.login(\"ftp\", \"ana@county.gov\")\n".into()),
            // A fallback that is no password written out.
            ("a.py", "password = args.password or getpass.getpass()\n".into()),
            ("a.py", "password = args.password or \"\"\n".into()),
            ("a.py", "password = args.password or \"changeme\"\n".into()),
            ("a.py", "pwd = os.getcwd() or \"C:/work2026\"\n".into()),
            ("a.py", "title = heading or \"Roads2026x\"\n".into()),
            ("a.py", "token = header.get(\"X-Token\") or \"anonymous\"\n".into()),
            ("a.py", "if password == expected or \"x1y2z3w4\" in names:\n".into()),
            ("a.py", "user = args.user or \"gis2026\"; password = args.password\n".into()),
            ("a.py", "password = floor or 1\npassword = color or \"\"\n".into()),
            ("a.js", "const password = opts.password || prompt(\"Password for roads2026:\");\n".into()),
            // (Python's own imaplib has this line: the fallback is the
            // host's, inside a call's arguments.)
            ("a.py", "PASSWD = getpass.getpass(\"IMAP password for %s on %s: \" % (USER, host or \"localhost\"))\n".into()),
            ("a.py", "password = ask(prompts[kind or \"default2026\"])\n".into()),
            // The environment read elsewhere, without a secret to fall
            // back on.
            ("a.js", "const host = process.env.DB_HOST || \"db1.corp.example\";\n".into()),
            ("a.js", "const p = process.env.DB_PASSWORD;\nconst q = process.env.DB_PASSWORD || \"\";\n".into()),
            ("a.js", "const p = process.env.DB_PASSWORD || fail(\"DB_PASSWORD is not set, see notes2026\");\n".into()),
            ("a.js", "const mode = process.env.NODE_ENV || \"development2026\";\n".into()),
            ("a.js", "const port = env.PORT ?? \"8080x1\";\n".into()),
            ("a.ps1", "$p = $env:PGPASSWORD\n$h = $env:PGHOST ?? \"db1.corp.example\"\n".into()),
            ("a.cs", "var p = Environment.GetEnvironmentVariable(\"PGPASSWORD\") ?? throw new Exception(\"unset2026\");\n".into()),
            ("run.sh", "export PGPASSWORD=\"${PGPASSWORD:-}\"\n".into()),
            ("run.sh", "export PGPASSWORD=\"${PGPASSWORD:?PGPASSWORD is not set, see notes2026}\"\n".into()),
            ("run.sh", "export PGPASSWORD=\"${PGPASSWORD:-$DEFAULT_PW1}\"\n".into()),
            ("run.sh", "export PGHOST=\"${PGHOST:-db1.corp.example}\" PGPORT=\"${PGPORT:-5432}\" PGDATABASE=\"${PGDATABASE:-roads2026}\"\n".into()),
            ("compose.yml", "      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}\n      POSTGRES_PASSWORD_FILE: ${POSTGRES_PASSWORD_FILE:-/run/secrets/db2026}\n".into()),
            ("ci.yml", "          PGPASSWORD: ${{ secrets.PGPASSWORD }}\n".into()),
            ("a.js", "const s = `${password}` + `${token.length}x2026abc`;\n".into()),
            // Oracle's shape where it is something else: the words `exp`
            // and `imp`, letters inside a word, a template, a variable.
            ("a.py", "y = np.exp(x/sigma2@w)\n".into()),
            ("a.py", "imp.reload(module); path = \"roads2026/north2026@v1\"\n".into()),
            ("a.py", "import imp, os  # see roads2026/north2026@v1\n".into()),
            ("a.md", "Copied the roadsnorth layer to exports/roads2026@final.\n".into()),
            ("a.bat", "sqlplus %ORA_USER%/%ORA_PW%@%ORA_DB% @load.sql\n".into()),
            ("a.py", "cmd = f\"sqlplus {user}/{pw}@{dsn} @load.sql\"\n".into()),
            ("a.py", "cmd = \"sqlplus {}/{}@{} @load.sql\".format(user, pw, dsn)\n".into()),
            ("a.py", "cmd = \"sqlplus %s/%s@%s\" % (user, pw, dsn)\n".into()),
            ("a.py", "conn = cx_Oracle.connect(user + \"/\" + pw + \"@\" + dsn)\n".into()),
            ("a.md", "sqlplus username/password@database\n".into()),
            ("a.py", "cx_Oracle.init_oracle_client(lib_dir=\"oracle/instantclient19\")\n".into()),
            // `pw` as a pulse's width.
            ("conf.yaml", "pw: 10.25ns\npw: 1500us\n".into()),
        ];
        let wrong: Vec<String> = cases
            .iter()
            .flat_map(|(path, text)| [(*path, text.clone()), (*path, text.replace('\n', "\r\n"))])
            .filter_map(|(path, text)| {
                hit_in(path, &text).map(|(line, what)| {
                    format!("{path} should not stop: {text:?} -> line {line} ({what})")
                })
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "{} of {} stop:\n{}",
            wrong.len(),
            2 * cases.len(),
            wrong.join("\n")
        );
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
