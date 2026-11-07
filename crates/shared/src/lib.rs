//! Shared governance code for Columbus: the parts the gateway enforces and
//! the desktop app displays. Nothing in this crate talks to an agent; it
//! decides what an agent's request is allowed to do.

pub mod approvals;
pub mod audit;
pub mod canon;
pub mod datagroups;
pub mod error;
pub mod paths;
pub mod policy;
pub mod projects;
pub mod proof;
pub mod sandbox;
pub mod sqlcheck;
pub mod tools;
pub mod workflows;

pub use error::{Denied, LoadError};
