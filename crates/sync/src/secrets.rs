//! Before anything is sent: does an outgoing file hold something that
//! looks like a secret? A hit stops the sync before the first byte leaves,
//! and names the file and the line, never the secret itself.
//!
//! The scan itself is `columbus_shared::secrets`: the same one keeps a
//! file with a secret in it from the assistant (`project_files`), so what
//! stops a sync and what the assistant is not given cannot drift apart.

pub use columbus_shared::secrets::{find, Finding};
