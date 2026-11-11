//! Files that may hold data are held, not sent (final review of part 3,
//! R-103).
//!
//! What the assistant writes into a project's files may quote data it has
//! read. Columbus's server records, beside the folder and never in it
//! (`files-labels.json`), which sources a chat had read when it wrote each
//! file: the file's data labels (`connection:roads-prod`). Sync reads that
//! record:
//!
//! * a file that carries a label is not sent. It is listed, with the
//!   sources it may hold data from;
//! * it goes only when the person chose **Send it anyway** for that file as
//!   it is now. The choice is remembered for exactly that content
//!   ([`State::released`]): once the file changes, it is held again;
//! * where the organization's flow rules forbid that source's data going
//!   to the Git server, there is no such choice;
//! * a record that is there and cannot be read holds every file: nothing
//!   that might be labelled is sent on a guess.
//!
//! A copy sync itself makes of a labelled file (the person's version of a
//! file changed on both sides) carries the labels with it
//! ([`State::carried`]). Files that arrive from the server carry none.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use columbus_flow::FlowDecision;
use columbus_shared::policy::FlowRule;
use serde::Serialize;

use crate::engine::What;
use crate::state::State;

/// A change here that was not sent because the file may hold data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Held {
    pub path: String,
    pub what: What,
    /// What the file holds now ([`crate::hash::content_hash`]): Send it
    /// anyway is for exactly this.
    pub hash: String,
    /// The sources it may hold data from, by name (`roads-prod`).
    pub sources: Vec<String>,
    /// Whether the person may choose to send it anyway. Not where the
    /// organization's rules forbid it, or the record cannot be read.
    pub choice: bool,
    /// Why it is held, in plain words.
    pub why: String,
}

/// A Git server as the organization's flow rules name it: `git:<host>`
/// (`git:github.com`, `git:*`).
pub fn destination(host: &str) -> String {
    format!("git:{}", host.to_lowercase())
}

/// A label as a person reads it: `connection:roads-prod` is the source
/// `roads-prod`.
pub fn source_name(label: &str) -> String {
    label
        .strip_prefix("connection:")
        .unwrap_or(label)
        .to_string()
}

/// Whether the organization's flow rules forbid data with `label` going to
/// the Git server at `host`. A Git server is an outside service: a rule
/// for `git:<host>`, `git:*` or `*` applies, and so does one written for
/// every connected server (`server:*`), as it does for the AI review.
pub fn denied(rules: &[FlowRule], label: &str, host: &str) -> bool {
    let one: BTreeSet<String> = [label.to_string()].into();
    let host = host.to_lowercase();
    [destination(&host), format!("server:{host}")]
        .iter()
        .any(|dest| {
            matches!(
                columbus_flow::evaluate_to(rules, &one, dest),
                FlowDecision::Deny { .. }
            )
        })
}

/// The data labels of a folder's files, and what the organization says
/// about sending them to the linked server.
#[derive(Debug, Clone, Default)]
pub struct DataLabels {
    /// What each labelled path carries.
    by_path: BTreeMap<String, BTreeSet<String>>,
    /// The organization's flow rules, and the server's host they are asked
    /// about.
    rules: Vec<FlowRule>,
    host: String,
    /// Why the record cannot be read, if so.
    unreadable: Option<String>,
}

impl DataLabels {
    /// No file carries a label (a folder that is not a project's).
    pub fn none() -> DataLabels {
        DataLabels::default()
    }

    /// These labels, under these rules, for the server at `host`.
    pub fn new(
        by_path: BTreeMap<String, BTreeSet<String>>,
        rules: &[FlowRule],
        host: &str,
    ) -> DataLabels {
        DataLabels {
            by_path,
            rules: rules.to_vec(),
            host: host.to_string(),
            unreadable: None,
        }
    }

    /// Read the record kept beside a project's files (`project` is the
    /// project's own folder, `projects/<id>`, which holds `files/` and
    /// `files-labels.json`), through the same reader as the server that
    /// writes it. A record that does not read holds every file.
    pub fn load(project: &Path, rules: &[FlowRule], host: &str) -> DataLabels {
        match columbus_shared::project_files::load_labels(project) {
            Ok(by_path) => DataLabels::new(by_path, rules, host),
            Err(_) => DataLabels {
                unreadable: Some(format!(
                    "Columbus cannot read its record of which of this project's files may hold data ({}, beside the files folder), so it sends none of them. Remove that record to send them; your assistant's files are then treated as holding no data.",
                    columbus_shared::project_files::LABELS_FILE
                )),
                rules: rules.to_vec(),
                host: host.to_string(),
                ..DataLabels::default()
            },
        }
    }

    /// The labels `path` carries: the record's, and those that came with a
    /// copy sync made.
    pub(crate) fn of(&self, state: &State, path: &str) -> BTreeSet<String> {
        let mut out = self.by_path.get(path).cloned().unwrap_or_default();
        if let Some(carried) = state.carried.get(path) {
            out.extend(carried.iter().cloned());
        }
        out
    }

    /// The sources a file that is about to be sent may hold data from, by
    /// name: empty for a file that carries no label.
    pub(crate) fn sources(&self, state: &State, path: &str) -> Vec<String> {
        self.of(state, path)
            .iter()
            .map(|l| source_name(l))
            .collect()
    }

    /// Why the change to `path` (which holds `hash` now) is not sent, if it
    /// is held.
    pub(crate) fn hold(&self, state: &State, path: &str, hash: &str, what: What) -> Option<Held> {
        let held = |sources: Vec<String>, choice: bool, why: String| Held {
            path: path.to_string(),
            what,
            hash: hash.to_string(),
            sources,
            choice,
            why,
        };
        if let Some(why) = &self.unreadable {
            return Some(held(Vec::new(), false, why.clone()));
        }
        let labels = self.of(state, path);
        if labels.is_empty() {
            return None;
        }
        let sources: Vec<String> = labels.iter().map(|l| source_name(l)).collect();
        let from = sources.join(", ");
        let forbidden: Vec<String> = labels
            .iter()
            .filter(|l| denied(&self.rules, l, &self.host))
            .map(|l| source_name(l))
            .collect();
        if !forbidden.is_empty() {
            return Some(held(
                sources,
                false,
                format!(
                    "it may hold data read from {from}, and your organization's rules do not let data from {} go to this server",
                    forbidden.join(", ")
                ),
            ));
        }
        if state.released.get(path).map(String::as_str) == Some(hash) {
            return None;
        }
        Some(held(
            sources,
            true,
            format!(
                "it may hold data read from {from} (your assistant wrote it in a chat that had read that source)"
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use columbus_shared::policy::FlowAction;

    fn rule(from: &str, to: &str, action: FlowAction) -> FlowRule {
        FlowRule {
            from: vec![from.into()],
            to: vec![to.into()],
            action,
        }
    }

    #[test]
    fn what_the_flow_rules_forbid() {
        let label = "connection:roads-prod";
        assert!(!denied(&[], label, "github.com"), "no rule: no denial");
        for to in ["git:github.com", "git:GitHub.com", "git:*", "server:*", "*"] {
            let rules = [rule("connection:roads-prod", to, FlowAction::Deny)];
            assert!(denied(&rules, label, "github.com"), "{to}");
            assert!(denied(&rules, label, "GitHub.com"), "{to}");
        }
        // Another host, another source, another kind of destination.
        let rules = [rule(
            "connection:roads-prod",
            "git:git.example.org",
            FlowAction::Deny,
        )];
        assert!(!denied(&rules, label, "github.com"));
        let rules = [rule("connection:hr", "git:*", FlowAction::Deny)];
        assert!(!denied(&rules, label, "github.com"));
        let rules = [rule("connection:*", "folder:*", FlowAction::Deny)];
        assert!(!denied(&rules, label, "github.com"));
        // A rule that asks for approval is not a denial: the person's own
        // choice at the keyboard is the approval.
        let rules = [rule("connection:*", "git:*", FlowAction::Approve)];
        assert!(!denied(&rules, label, "github.com"));
        // The more exact rule wins, as everywhere.
        let rules = [
            rule("connection:*", "git:*", FlowAction::Deny),
            rule(
                "connection:roads-prod",
                "git:github.com",
                FlowAction::Approve,
            ),
        ];
        assert!(!denied(&rules, label, "github.com"));
        assert!(denied(&rules, "connection:hr", "github.com"));
    }

    #[test]
    fn a_label_is_named_by_its_source() {
        assert_eq!(source_name("connection:roads-prod"), "roads-prod");
        assert_eq!(source_name("server:drive"), "server:drive");
        assert_eq!(destination("Git.Example.org"), "git:git.example.org");
    }
}
