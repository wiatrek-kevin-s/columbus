//! What sync needs from a Git server, whichever kind it is: read a branch's
//! newest commit and its list of files, read a file, send changes as one
//! commit on top of what was read (never forcing), and list recent commits.
//!
//! The servers Columbus talks to implement it over their web APIs
//! ([`crate::remote::Remote`]); the tests have one in memory.

use std::future::Future;

use crate::Error;

/// A branch's newest commit, or that the repository has no commit yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tip {
    Empty,
    At(String),
}

impl Tip {
    pub fn commit(&self) -> Option<&str> {
        match self {
            Tip::Empty => None,
            Tip::At(c) => Some(c),
        }
    }
}

/// What a path in a commit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A file; `exec` when the repository marks it as a program.
    File { exec: bool },
    /// A symbolic link. Never synced.
    Link,
    /// Another repository at a commit (a submodule). Never synced.
    Submodule,
}

/// One path of a commit's list of files (folders are not listed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    /// The id of what it holds (a Git blob id; for a submodule, its commit).
    pub blob: String,
    /// Its size in bytes, when the server says.
    pub size: Option<u64>,
    pub kind: EntryKind,
}

/// One change to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A file the server does not have.
    Create {
        path: String,
        bytes: Vec<u8>,
        exec: bool,
    },
    /// A file the server has as `old` (its blob id), replaced.
    Update {
        path: String,
        bytes: Vec<u8>,
        old: String,
        exec: bool,
    },
    /// A file the server has as `old`, removed.
    Delete { path: String, old: String },
}

impl Change {
    pub fn path(&self) -> &str {
        match self {
            Change::Create { path, .. }
            | Change::Update { path, .. }
            | Change::Delete { path, .. } => path,
        }
    }

    /// The bytes sent, if any.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Change::Create { bytes, .. } | Change::Update { bytes, .. } => Some(bytes),
            Change::Delete { .. } => None,
        }
    }
}

/// A commit, for History.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Commit {
    pub id: String,
    /// Who made it (a name, as the server gives it).
    pub who: String,
    /// When (as the server gives it, ISO 8601).
    pub when: String,
    /// Its message.
    pub note: String,
}

/// Why changes were not sent.
#[derive(Debug)]
pub enum SendError {
    /// The branch, or one of the files, is no longer what was read. Nothing
    /// was overwritten: read again and repeat (R-24).
    Moved,
    Failed(Error),
}

impl From<Error> for SendError {
    fn from(e: Error) -> Self {
        SendError::Failed(e)
    }
}

/// One branch of one repository on a server.
pub trait Host: Sync {
    /// The branch's newest commit.
    fn tip(&self) -> impl Future<Output = Result<Tip, Error>> + Send;

    /// Every file of a commit (all folders, no folder entries).
    fn tree(&self, commit: &str) -> impl Future<Output = Result<Vec<TreeEntry>, Error>> + Send;

    /// The bytes of a file, by its id.
    fn blob(&self, id: &str) -> impl Future<Output = Result<Vec<u8>, Error>> + Send;

    /// Send `changes` as one commit on top of `tip`, with `message`, and
    /// give the new commit's id. Never forces: if the branch (or a file the
    /// changes touch) is no longer as it was at `tip`, nothing changes and
    /// the answer is [`SendError::Moved`].
    fn send(
        &self,
        tip: &Tip,
        changes: &[Change],
        message: &str,
    ) -> impl Future<Output = Result<String, SendError>> + Send;

    /// The branch's newest commits, newest first, at most `limit`.
    fn history(&self, limit: usize) -> impl Future<Output = Result<Vec<Commit>, Error>> + Send;
}
