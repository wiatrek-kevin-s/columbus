//! The broker: Columbus as an MCP client to other MCP servers, under
//! Columbus manifests (design change 1).

pub mod manifest;
pub mod registry;
pub mod review;

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("manifest {0}: {1}")]
    Manifest(PathBuf, String),
    #[error("cannot read {0}: {1}")]
    Io(PathBuf, String),
}
