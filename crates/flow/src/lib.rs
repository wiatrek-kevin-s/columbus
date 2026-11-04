//! Data labels and cross-provider flow rules (design change 1, Rule 3).
//!
//! Reading from a governed connection adds that connection's label to the
//! session (`connection:roads-prod`), and to every output file made while
//! it is active. While any label is active, a call that can send data out
//! (`egress`) or write on another provider needs a person's approval,
//! whatever that provider's manifest says, and organization flow rules can
//! forbid it outright.
//!
//! Labels last for the session and are recorded in provenance, so a file
//! remembers which connections it came from even after it moves.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use columbus_shared::policy::{FlowAction, FlowRule};
use serde::Serialize;

pub fn connection_label(connection_id: &str) -> String {
    format!("connection:{connection_id}")
}

/// What the flow rules say about one outbound call.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum FlowDecision {
    /// No labelled data in play, or the call cannot send data out.
    Allow,
    /// Labelled data may leave, with a person's approval.
    Approve {
        labels: Vec<String>,
        /// Index of the organization rule that applied, if one did.
        rule: Option<usize>,
    },
    /// An organization rule forbids this flow.
    Deny { labels: Vec<String>, rule: usize },
}

impl FlowDecision {
    pub fn label(&self) -> &'static str {
        match self {
            FlowDecision::Allow => "allow",
            FlowDecision::Approve { .. } => "approve",
            FlowDecision::Deny { .. } => "deny",
        }
    }
}

fn matches(pattern: &str, value: &str) -> Option<u8> {
    if pattern.eq_ignore_ascii_case(value) {
        return Some(2);
    }
    let (pk, pv) = pattern.split_once(':')?;
    let (vk, _) = value.split_once(':')?;
    (pv == "*" && pk.eq_ignore_ascii_case(vk)).then_some(1)
}

/// Decide an outbound call to `server` carrying data with `labels`.
/// `outbound` is true for tools with `egress: true` or risk write/destructive.
pub fn evaluate(
    rules: &[FlowRule],
    labels: &BTreeSet<String>,
    server: &str,
    outbound: bool,
) -> FlowDecision {
    if !outbound {
        return FlowDecision::Allow;
    }
    evaluate_to(rules, labels, &format!("server:{server}"))
}

/// Decide data with `labels` leaving for `dest`, a destination such as
/// `server:drive` or `folder:district-share` (an export folder's id).
pub fn evaluate_to(rules: &[FlowRule], labels: &BTreeSet<String>, dest: &str) -> FlowDecision {
    if labels.is_empty() {
        return FlowDecision::Allow;
    }
    let all: Vec<String> = labels.iter().cloned().collect();
    let mut applied: Option<usize> = None;
    for label in labels {
        // Most specific rule for this label; deny wins a tie.
        let mut best: Option<(u8, FlowAction, usize)> = None;
        for (i, r) in rules.iter().enumerate() {
            let from = r.from.iter().filter_map(|p| matches(p, label)).max();
            let to = r.to.iter().filter_map(|p| matches(p, dest)).max();
            if let (Some(f), Some(t)) = (from, to) {
                let score = f + t;
                let better = match best {
                    None => true,
                    Some((s, a, _)) => {
                        score > s
                            || (score == s && r.action == FlowAction::Deny && a != FlowAction::Deny)
                    }
                };
                if better {
                    best = Some((score, r.action, i));
                }
            }
        }
        match best {
            Some((_, FlowAction::Deny, i)) => {
                return FlowDecision::Deny {
                    labels: all,
                    rule: i,
                }
            }
            Some((_, FlowAction::Approve, i)) => applied = applied.or(Some(i)),
            None => {}
        }
    }
    FlowDecision::Approve {
        labels: all,
        rule: applied,
    }
}

/// Labels for one session: those active on the session, and those carried
/// by each workspace file made while labels were active.
#[derive(Default)]
pub struct FlowState {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    session: BTreeSet<String>,
    files: BTreeMap<PathBuf, BTreeSet<String>>,
}

impl FlowState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&self, label: String) {
        self.inner
            .lock()
            .expect("flow state poisoned")
            .session
            .insert(label);
    }

    pub fn session_labels(&self) -> BTreeSet<String> {
        self.inner
            .lock()
            .expect("flow state poisoned")
            .session
            .clone()
    }

    /// Record that a file was produced while `labels` were active.
    pub fn record_file(&self, path: &Path, labels: &BTreeSet<String>) {
        if labels.is_empty() {
            return;
        }
        self.inner
            .lock()
            .expect("flow state poisoned")
            .files
            .entry(path.to_path_buf())
            .or_default()
            .extend(labels.iter().cloned());
    }

    pub fn file_labels(&self, path: &Path) -> BTreeSet<String> {
        self.inner
            .lock()
            .expect("flow state poisoned")
            .files
            .get(path)
            .cloned()
            .unwrap_or_default()
    }

    /// Labels in play for a call: the session's plus those of any files it passes.
    pub fn labels_for(&self, files: &[PathBuf]) -> BTreeSet<String> {
        let inner = self.inner.lock().expect("flow state poisoned");
        let mut out = inner.session.clone();
        for f in files {
            if let Some(l) = inner.files.get(f) {
                out.extend(l.iter().cloned());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<FlowRule> {
        serde_yaml_like(
            r#"[
              {"from": ["connection:roads-prod", "connection:assets-prod"], "to": ["server:*"], "action": "deny"},
              {"from": ["connection:roads-prod"], "to": ["server:sharepoint"], "action": "approve"}
            ]"#,
        )
    }

    fn serde_yaml_like(json: &str) -> Vec<FlowRule> {
        serde_json::from_str(json).unwrap()
    }

    fn set(labels: &[&str]) -> BTreeSet<String> {
        labels.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_labelled_or_not_outbound_is_allowed() {
        assert_eq!(
            evaluate(&rules(), &set(&[]), "drive", true),
            FlowDecision::Allow
        );
        assert_eq!(
            evaluate(&rules(), &set(&["connection:roads-prod"]), "drive", false),
            FlowDecision::Allow
        );
    }

    #[test]
    fn the_change_documents_example() {
        // Any server: denied for roads-prod and assets-prod.
        assert!(matches!(
            evaluate(&rules(), &set(&["connection:roads-prod"]), "drive", true),
            FlowDecision::Deny { rule: 0, .. }
        ));
        // SharePoint: the more specific approve rule wins for roads-prod...
        assert!(matches!(
            evaluate(
                &rules(),
                &set(&["connection:roads-prod"]),
                "sharepoint",
                true
            ),
            FlowDecision::Approve { rule: Some(1), .. }
        ));
        // ...but assets-prod data is still denied there.
        assert!(matches!(
            evaluate(
                &rules(),
                &set(&["connection:roads-prod", "connection:assets-prod"]),
                "sharepoint",
                true
            ),
            FlowDecision::Deny { rule: 0, .. }
        ));
    }

    #[test]
    fn labelled_data_without_a_rule_needs_approval() {
        assert_eq!(
            evaluate(&[], &set(&["connection:gis"]), "drive", true),
            FlowDecision::Approve {
                labels: vec!["connection:gis".into()],
                rule: None
            }
        );
    }

    #[test]
    fn deny_wins_a_tie() {
        let r = serde_yaml_like(
            r#"[{"from": ["connection:*"], "to": ["server:drive"], "action": "approve"},
                {"from": ["connection:*"], "to": ["server:drive"], "action": "deny"}]"#,
        );
        assert!(matches!(
            evaluate(&r, &set(&["connection:x"]), "drive", true),
            FlowDecision::Deny { rule: 1, .. }
        ));
    }

    #[test]
    fn files_carry_their_labels() {
        let f = FlowState::new();
        let p = PathBuf::from("/ws/results/a.csv");
        f.record_file(&p, &set(&["connection:roads-prod"]));
        assert_eq!(f.labels_for(&[]), set(&[]));
        assert_eq!(
            f.labels_for(std::slice::from_ref(&p)),
            set(&["connection:roads-prod"])
        );
        f.add("connection:gis".into());
        assert_eq!(
            f.labels_for(&[p]),
            set(&["connection:gis", "connection:roads-prod"])
        );
    }
}
