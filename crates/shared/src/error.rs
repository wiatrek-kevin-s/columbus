use std::path::PathBuf;

use chrono::NaiveDate;
use thiserror::Error;

/// Problems reading configuration. These are the operator's to fix.
#[derive(Debug, Error)]
pub enum LoadError {
    #[error("cannot read {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("cannot parse {0}: {1}")]
    Parse(PathBuf, String),
    #[error("{0}")]
    Invalid(String),
}

/// A request the governance layer refused. Messages are written for the
/// agent to relay to a person, so they say what was refused and why, and
/// never include secrets.
#[derive(Debug, Error, PartialEq)]
pub enum Denied {
    #[error("connection type {0:?} is not allowed by organization policy")]
    ConnectionType(String),
    #[error("host {0:?} is not on the organization's allowed host list")]
    Host(String),
    #[error("connection {0:?} expired on {1}")]
    Expired(String, NaiveDate),
    #[error("connection {0:?} is not available in this project")]
    NotInProject(String),
    #[error("no connection named {0:?}")]
    UnknownConnection(String),
    #[error("dataset {0:?} is not in this connection's allowed datasets")]
    Dataset(String),
    #[error("tool {0:?} is not allowed by organization policy")]
    Tool(String),
    #[error("query refused: {0}")]
    Query(String),
    #[error("path refused: {0}")]
    Path(String),
    #[error("approval refused: {0}")]
    Approval(String),
}
