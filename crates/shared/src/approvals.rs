//! Approvals (design section 3, "Approvals").
//!
//! An approval is bound to a hash of the exact request, is single-use, and
//! expires after ten minutes. Only the person's UI can decide one: this type
//! has no method an agent-facing tool can call to approve, and the gateway
//! never exposes `decide` as a tool.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::canon::request_hash;
use crate::error::Denied;

pub const APPROVAL_TTL_MINUTES: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Edit,
    Script,
    Export,
    Download,
    LongJob,
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Decline,
    AllowOnce,
    /// Allow this exact request again for the rest of the session.
    AlwaysThisSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Pending,
    Approved,
    Declined,
    Used,
    Expired,
}

/// What the person sees, and what the agent gets back as `approval_required`.
#[derive(Debug, Clone, Serialize)]
pub struct Approval {
    pub id: String,
    pub tool: String,
    pub risk: Risk,
    /// Plain summary, e.g. "Apply 37 attribute edits to ROADS.ROADWAY_INVENTORY on version QC_1103".
    pub summary: String,
    /// The exact request, shown on demand.
    pub request: Value,
    pub hash: String,
    pub created: DateTime<Utc>,
    pub expires: DateTime<Utc>,
    pub state: State,
}

#[derive(Default)]
pub struct ApprovalStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    approvals: HashMap<String, Approval>,
    /// Request hashes the person said to always allow this session.
    standing: HashMap<String, String>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a pending approval for a request.
    pub fn request(
        &self,
        tool: &str,
        args: &Value,
        risk: Risk,
        summary: String,
        now: DateTime<Utc>,
    ) -> Approval {
        let hash = request_hash(tool, args);
        let approval = Approval {
            id: format!("ap_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            tool: tool.into(),
            risk,
            summary,
            request: args.clone(),
            hash,
            created: now,
            expires: now + Duration::minutes(APPROVAL_TTL_MINUTES),
            state: State::Pending,
        };
        let mut inner = self.inner.lock().expect("approval store poisoned");
        inner
            .approvals
            .insert(approval.id.clone(), approval.clone());
        approval
    }

    /// True when the person already said "always for this session" to this exact request.
    pub fn standing_allows(&self, tool: &str, args: &Value) -> bool {
        let hash = request_hash(tool, args);
        self.inner
            .lock()
            .expect("approval store poisoned")
            .standing
            .get(&hash)
            .is_some_and(|t| t == tool)
    }

    /// Record the person's decision. Called only from the person's UI
    /// (in-client elicitation, the app, the tray or the local approvals page).
    pub fn decide(
        &self,
        id: &str,
        decision: Decision,
        now: DateTime<Utc>,
    ) -> Result<Approval, Denied> {
        let mut inner = self.inner.lock().expect("approval store poisoned");
        let a = inner
            .approvals
            .get_mut(id)
            .ok_or_else(|| Denied::Approval(format!("no approval {id}")))?;
        if now > a.expires {
            a.state = State::Expired;
            return Err(Denied::Approval(format!("approval {id} expired")));
        }
        if a.state != State::Pending {
            return Err(Denied::Approval(format!(
                "approval {id} was already decided"
            )));
        }
        a.state = match decision {
            Decision::Decline => State::Declined,
            Decision::AllowOnce | Decision::AlwaysThisSession => State::Approved,
        };
        let out = a.clone();
        if decision == Decision::AlwaysThisSession {
            inner.standing.insert(out.hash.clone(), out.tool.clone());
        }
        Ok(out)
    }

    /// Current state, for `approval_status`.
    pub fn status(&self, id: &str, now: DateTime<Utc>) -> Option<Approval> {
        let mut inner = self.inner.lock().expect("approval store poisoned");
        let a = inner.approvals.get_mut(id)?;
        if a.state == State::Pending && now > a.expires {
            a.state = State::Expired;
        }
        Some(a.clone())
    }

    /// Spend an approval on the request it was granted for. Fails unless the
    /// approval is approved, unexpired, unused and the request matches exactly.
    pub fn consume(
        &self,
        id: &str,
        tool: &str,
        args: &Value,
        now: DateTime<Utc>,
    ) -> Result<(), Denied> {
        let hash = request_hash(tool, args);
        let mut inner = self.inner.lock().expect("approval store poisoned");
        let a = inner
            .approvals
            .get_mut(id)
            .ok_or_else(|| Denied::Approval(format!("no approval {id}")))?;
        if a.hash != hash || a.tool != tool {
            return Err(Denied::Approval(
                "this approval was granted for a different request".into(),
            ));
        }
        if now > a.expires {
            a.state = State::Expired;
            return Err(Denied::Approval(format!("approval {id} expired")));
        }
        match a.state {
            State::Approved => {
                a.state = State::Used;
                Ok(())
            }
            State::Pending => Err(Denied::Approval(format!(
                "approval {id} is still waiting for a person"
            ))),
            State::Declined => Err(Denied::Approval(format!("approval {id} was declined"))),
            State::Used => Err(Denied::Approval(format!("approval {id} was already used"))),
            State::Expired => Err(Denied::Approval(format!("approval {id} expired"))),
        }
    }

    pub fn pending(&self, now: DateTime<Utc>) -> Vec<Approval> {
        let inner = self.inner.lock().expect("approval store poisoned");
        let mut v: Vec<_> = inner
            .approvals
            .values()
            .filter(|a| a.state == State::Pending && now <= a.expires)
            .cloned()
            .collect();
        v.sort_by_key(|a| a.created);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (ApprovalStore, Approval, Value, DateTime<Utc>) {
        let s = ApprovalStore::new();
        let args = json!({"connection": "roads-prod", "edits": 37});
        let now = Utc::now();
        let a = s.request(
            "apply_edits",
            &args,
            Risk::Edit,
            "Apply 37 edits".into(),
            now,
        );
        (s, a, args, now)
    }

    #[test]
    fn approve_once_then_single_use() {
        let (s, a, args, now) = setup();
        assert!(
            s.consume(&a.id, "apply_edits", &args, now).is_err(),
            "pending"
        );
        s.decide(&a.id, Decision::AllowOnce, now).unwrap();
        s.consume(&a.id, "apply_edits", &args, now).unwrap();
        assert!(
            s.consume(&a.id, "apply_edits", &args, now).is_err(),
            "reused"
        );
    }

    #[test]
    fn bound_to_exact_request() {
        let (s, a, _, now) = setup();
        s.decide(&a.id, Decision::AllowOnce, now).unwrap();
        let other = json!({"connection": "roads-prod", "edits": 38});
        assert!(s.consume(&a.id, "apply_edits", &other, now).is_err());
        let reordered = json!({"edits": 37, "connection": "roads-prod"});
        assert!(
            s.consume(&a.id, "apply_edits", &reordered, now).is_ok(),
            "key order must not matter"
        );
    }

    #[test]
    fn expires_after_ten_minutes() {
        let (s, a, args, now) = setup();
        let late = now + Duration::minutes(11);
        assert!(s.decide(&a.id, Decision::AllowOnce, late).is_err());
        assert_eq!(s.status(&a.id, late).unwrap().state, State::Expired);
        assert!(s.consume(&a.id, "apply_edits", &args, late).is_err());
    }

    #[test]
    fn declined_cannot_be_used_or_redecided() {
        let (s, a, args, now) = setup();
        s.decide(&a.id, Decision::Decline, now).unwrap();
        assert!(s.consume(&a.id, "apply_edits", &args, now).is_err());
        assert!(s.decide(&a.id, Decision::AllowOnce, now).is_err());
    }

    #[test]
    fn always_this_session_covers_only_the_same_request() {
        let (s, a, args, now) = setup();
        s.decide(&a.id, Decision::AlwaysThisSession, now).unwrap();
        assert!(s.standing_allows("apply_edits", &args));
        assert!(!s.standing_allows(
            "apply_edits",
            &json!({"connection": "roads-prod", "edits": 1})
        ));
        assert!(!s.standing_allows("run_python", &args));
    }
}
