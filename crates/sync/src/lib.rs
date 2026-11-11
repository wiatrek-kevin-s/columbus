//! Sync a folder of files with a repository on a Git server, through the
//! server's web API (build brief 3, D-20).
//!
//! No Git program and no Git library is used: Columbus talks to GitHub,
//! GitHub Enterprise Server, Gitea or Forgejo over HTTPS with a token the
//! person gave. So it works on a computer without Git, and nothing from a
//! repository can ever run there: no hooks, no Git configuration, no
//! submodules. The folder is a plain folder, not a Git clone.
//!
//! * [`check`] compares the folder, the server and what was last synced.
//!   It only reads.
//! * [`sync`] gets the server's changes, keeps both versions of a file
//!   changed on both sides (never merging, never overwriting), then sends
//!   what only the person changed as one commit. It never forces (R-24).
//! * [`resolve`] settles a file that needs a look, as the person chose.
//!
//! What never syncs is decided in [`paths`] (refused paths, the ignore
//! list, files over 5 MB) and [`secrets`] (a sync stops before sending
//! anything that looks like a token or a key). The token is given to
//! [`remote::Server`] by the caller, is sent only to that server's host,
//! and is never written, logged or put in an error by this crate.

use std::path::Path;

pub mod engine;
mod fsx;
pub mod hash;
pub mod host;
pub mod paths;
mod plan;
pub mod remote;
mod scan;
pub mod secrets;
pub mod settings;
pub mod state;
pub mod trash;

pub use engine::{
    check, preview, resolve, sync, BothItem, Changes, Choice, Item, NotSent, Options, Preview,
    Removed, Report, Side, What,
};
pub use host::{Change, Commit, EntryKind, Host, SendError, Tip, TreeEntry};
pub use secrets::Finding;
pub use state::{Conflict, ConflictKind, Link, Record, State, Synced};
pub use trash::{Keep, Kept};

/// Something that does not sync, and why.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Skipped {
    /// The path (a folder's ends with `/`).
    pub path: String,
    /// Why, finishing the sentence "<path> does not sync: …".
    pub why: String,
    /// Whether it is the server's (else the folder's).
    pub on_server: bool,
}

/// Why something could not be done, in words for the person. None of these
/// ever holds a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A file on this computer could not be read or written.
    Io(String),
    /// A rule of Columbus's says no.
    Refused(String),
    /// The server did not accept the token.
    Token(String),
    /// The token may not do this.
    Permission(String),
    /// The repository, branch or server address was not found.
    NotFound(String),
    /// The server asks to wait.
    RateLimited(String),
    /// The server is older than Columbus can sync with (A-9).
    TooOld(String),
    /// The server answered with a redirect to another host, which Columbus
    /// does not follow (the token goes to the linked host only).
    Redirected(String),
    /// The server could not be reached, or did not answer in time, or its
    /// certificate could not be checked.
    Network(String),
    /// The server answered something else.
    Server(String),
    /// The repository kept changing while a sync tried to send.
    Busy(String),
}

impl Error {
    pub(crate) fn io(path: &Path, e: std::io::Error) -> Error {
        Error::Io(format!("{}: {e}", path.display()))
    }

    fn text(&self) -> &str {
        match self {
            Error::Io(s)
            | Error::Refused(s)
            | Error::Token(s)
            | Error::Permission(s)
            | Error::NotFound(s)
            | Error::RateLimited(s)
            | Error::TooOld(s)
            | Error::Redirected(s)
            | Error::Network(s)
            | Error::Server(s)
            | Error::Busy(s) => s,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for Error {}
