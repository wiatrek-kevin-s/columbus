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
/// Most requests that may wait for a person at once in one session, so an
/// agent cannot bury one harmful request among many harmless ones.
pub const MAX_PENDING: usize = 20;
/// Decided and expired approvals are kept this long for the recent list.
const KEEP_MINUTES: i64 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Edit,
    /// Changes to the person's own notes (never data, never the
    /// organization's pack).
    Notes,
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
    /// Structured facts for the approval card: files and row counts,
    /// data labels, destination, the flow rule that applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    /// Where the person decided: `client` (in-client prompt) or `page`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_via: Option<String>,
    /// Whether "allow for this session" may be chosen (never for scripts
    /// and edits, nor where policy asks every time).
    pub offer_always: bool,
    /// What "allow for this session" would cover, in the person's words,
    /// when it is wider than this exact request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub always_covers: Option<String>,
    /// The scopes "allow for this session" grants, fixed when the person is
    /// asked (so a decision covers what they were shown, nothing later).
    #[serde(skip)]
    scopes: Vec<String>,
}

/// How an approval may be granted beyond this one call.
#[derive(Debug, Clone, Default)]
pub struct Offer {
    pub always: bool,
    /// Shown to the person: what "allow for this session" covers.
    pub covers: Option<String>,
    /// Granted, all of them, by "allow for this session"; empty: this exact
    /// request only.
    pub scopes: Vec<Value>,
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
    ) -> Result<Approval, Denied> {
        self.request_with_detail(tool, args, risk, summary, None, now)
    }

    /// Open a pending approval with structured detail for the card. An
    /// identical request already waiting is returned instead of a second
    /// one, and no more than [`MAX_PENDING`] may wait at once.
    pub fn request_with_detail(
        &self,
        tool: &str,
        args: &Value,
        risk: Risk,
        summary: String,
        detail: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<Approval, Denied> {
        let offer = Offer {
            always: !matches!(risk, Risk::Script | Risk::Edit),
            ..Offer::default()
        };
        self.request_offering(tool, args, risk, summary, detail, offer, now)
    }

    /// As [`request_with_detail`](Self::request_with_detail), saying what
    /// "allow for this session" may cover.
    #[allow(clippy::too_many_arguments)]
    pub fn request_offering(
        &self,
        tool: &str,
        args: &Value,
        risk: Risk,
        summary: String,
        detail: Option<Value>,
        offer: Offer,
        now: DateTime<Utc>,
    ) -> Result<Approval, Denied> {
        let hash = request_hash(tool, args);
        let scopes: Vec<String> = offer.scopes.iter().map(|v| request_hash(tool, v)).collect();
        {
            let mut inner = self.inner.lock().expect("approval store poisoned");
            inner.approvals.retain(|_, a| {
                a.state == State::Pending || now - a.created < Duration::minutes(KEEP_MINUTES)
            });
            for a in inner.approvals.values_mut() {
                if a.state == State::Pending && now > a.expires {
                    a.state = State::Expired;
                }
            }
            if let Some(same) = inner.approvals.values().find(|a| {
                a.state == State::Pending
                    && a.hash == hash
                    && a.tool == tool
                    && a.scopes == scopes
                    && a.offer_always == offer.always
            }) {
                return Ok(same.clone());
            }
            let waiting = inner
                .approvals
                .values()
                .filter(|a| a.state == State::Pending)
                .count();
            if waiting >= MAX_PENDING {
                return Err(Denied::Approval(format!(
                    "{waiting} requests are already waiting for the person; wait for them to decide before asking for more"
                )));
            }
        }
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
            detail,
            decided_at: None,
            decided_via: None,
            offer_always: offer.always,
            always_covers: offer.covers.filter(|_| offer.always),
            scopes,
        };
        let mut inner = self.inner.lock().expect("approval store poisoned");
        inner
            .approvals
            .insert(approval.id.clone(), approval.clone());
        Ok(approval)
    }

    /// Forget every "allow for this session" and every approval not yet
    /// used (they expire): the settings they were given under changed, so
    /// the person is asked again. Returns how many were withdrawn.
    pub fn revoke_all(&self) -> usize {
        let mut inner = self.inner.lock().expect("approval store poisoned");
        let mut n = inner.standing.len();
        inner.standing.clear();
        for a in inner.approvals.values_mut() {
            if matches!(a.state, State::Pending | State::Approved) {
                a.state = State::Expired;
                n += 1;
            }
        }
        n
    }

    /// True when "allow for this session" covers every one of `scopes`
    /// (none: never).
    pub fn standing_covers(&self, tool: &str, scopes: &[Value]) -> bool {
        !scopes.is_empty() && scopes.iter().all(|s| self.standing_allows(tool, s))
    }

    /// Take back what one approval's "allow for this session" granted (its
    /// decision came under settings that have since changed).
    pub fn unstand(&self, id: &str) {
        let mut inner = self.inner.lock().expect("approval store poisoned");
        let Some(a) = inner.approvals.get(id).cloned() else {
            return;
        };
        inner.standing.remove(&a.hash);
        for h in &a.scopes {
            inner.standing.remove(h);
        }
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
        self.decide_via(id, decision, now, "client")
    }

    /// As [`decide`](Self::decide), recording where the person decided.
    pub fn decide_via(
        &self,
        id: &str,
        decision: Decision,
        now: DateTime<Utc>,
        via: &str,
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
        let decision = if decision == Decision::AlwaysThisSession && !a.offer_always {
            Decision::AllowOnce
        } else {
            decision
        };
        a.state = match decision {
            Decision::Decline => State::Declined,
            Decision::AllowOnce | Decision::AlwaysThisSession => State::Approved,
        };
        a.decided_at = Some(now);
        a.decided_via = Some(via.to_string());
        let out = a.clone();
        if decision == Decision::AlwaysThisSession {
            inner.standing.insert(out.hash.clone(), out.tool.clone());
            for h in &out.scopes {
                inner.standing.insert(h.clone(), out.tool.clone());
            }
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

    /// Approvals no longer pending, newest first, for the approvals page.
    pub fn recent(&self, limit: usize, now: DateTime<Utc>) -> Vec<Approval> {
        let mut inner = self.inner.lock().expect("approval store poisoned");
        for a in inner.approvals.values_mut() {
            if a.state == State::Pending && now > a.expires {
                a.state = State::Expired;
            }
        }
        let mut v: Vec<_> = inner
            .approvals
            .values()
            .filter(|a| a.state != State::Pending)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            b.decided_at
                .unwrap_or(b.expires)
                .cmp(&a.decided_at.unwrap_or(a.expires))
        });
        v.truncate(limit);
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
        let a = s
            .request(
                "apply_edits",
                &args,
                Risk::Edit,
                "Apply 37 edits".into(),
                now,
            )
            .unwrap();
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
    fn identical_requests_merge_and_the_queue_is_capped() {
        let s = ApprovalStore::new();
        let now = Utc::now();
        let a = s
            .request("t", &json!({"x": 1}), Risk::Export, "x".into(), now)
            .unwrap();
        let b = s
            .request("t", &json!({"x": 1}), Risk::Export, "x".into(), now)
            .unwrap();
        assert_eq!(a.id, b.id, "the same request waiting twice is one approval");
        for i in 2..=MAX_PENDING {
            s.request("t", &json!({"x": i}), Risk::Export, "x".into(), now)
                .unwrap();
        }
        assert!(s
            .request("t", &json!({"x": 999}), Risk::Export, "x".into(), now)
            .is_err());
        // Once the person decides one, there is room again.
        s.decide(&a.id, Decision::Decline, now).unwrap();
        assert!(s
            .request("t", &json!({"x": 999}), Risk::Export, "x".into(), now)
            .is_ok());
    }

    #[test]
    fn decisions_record_where_and_appear_in_recent() {
        let (s, a, _, now) = setup();
        assert!(s.recent(10, now).is_empty());
        s.decide_via(&a.id, Decision::Decline, now, "page").unwrap();
        let r = s.recent(10, now);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].decided_via.as_deref(), Some("page"));
        assert!(s.pending(now).is_empty());
    }

    #[test]
    fn always_this_session_covers_only_the_same_request() {
        let s = ApprovalStore::new();
        let args = json!({"connection": "roads-prod", "edits": 37});
        let now = Utc::now();
        let a = s
            .request("apply_edits", &args, Risk::Export, "Send".into(), now)
            .unwrap();
        s.decide(&a.id, Decision::AlwaysThisSession, now).unwrap();
        assert!(s.standing_allows("apply_edits", &args));
        assert!(!s.standing_allows(
            "apply_edits",
            &json!({"connection": "roads-prod", "edits": 1})
        ));
        assert!(!s.standing_allows("run_python", &args));
    }

    #[test]
    fn edits_and_scripts_are_never_allowed_for_the_session() {
        let (s, a, args, now) = setup();
        assert!(!a.offer_always);
        s.decide(&a.id, Decision::AlwaysThisSession, now).unwrap();
        assert!(!s.standing_allows("apply_edits", &args), "counted as once");
        s.consume(&a.id, "apply_edits", &args, now).unwrap();
    }

    #[test]
    fn allow_for_the_session_grants_the_scopes_shown_when_asked() {
        let s = ApprovalStore::new();
        let now = Utc::now();
        let base = json!({"scope": "notes"});
        let flow = json!({"scope": "notes", "into": "parcels", "from": "connection:roads"});
        let other = json!({"scope": "notes", "into": "parcels", "from": "connection:hr"});
        let offer = |scopes: Vec<Value>| Offer {
            always: true,
            covers: Some("covers".into()),
            scopes,
        };
        let a = s
            .request_offering(
                "n",
                &json!({"x": 1}),
                Risk::Notes,
                "s".into(),
                None,
                offer(vec![base.clone()]),
                now,
            )
            .unwrap();
        assert_eq!(a.always_covers.as_deref(), Some("covers"));
        // Before the person decides, nothing stands.
        assert!(!s.standing_covers("n", std::slice::from_ref(&base)));
        s.decide(&a.id, Decision::AlwaysThisSession, now).unwrap();
        assert!(s.standing_covers("n", std::slice::from_ref(&base)));
        // A flow not shown is not covered, alone or with the base.
        assert!(!s.standing_covers("n", &[base.clone(), flow.clone()]));
        assert!(!s.standing_covers("n", &[]), "no scopes: never");
        let b = s
            .request_offering(
                "n",
                &json!({"x": 2}),
                Risk::Notes,
                "s".into(),
                None,
                offer(vec![base.clone(), flow.clone()]),
                now,
            )
            .unwrap();
        s.decide(&b.id, Decision::AlwaysThisSession, now).unwrap();
        assert!(s.standing_covers("n", &[base.clone(), flow.clone()]));
        assert!(!s.standing_covers("n", &[base.clone(), other]));
        assert!(
            !s.standing_covers("m", std::slice::from_ref(&base)),
            "bound to the tool"
        );
        // Taken back (settings changed while the person decided).
        s.unstand(&b.id);
        assert!(!s.standing_covers("n", &[base.clone(), flow]));
        // All of what it granted, the base scope too.
        assert!(!s.standing_covers("n", &[base]));
        // Allow once grants nothing for later.
        let c = s
            .request_offering(
                "n",
                &json!({"x": 3}),
                Risk::Notes,
                "s".into(),
                None,
                offer(vec![json!({"scope": "z"})]),
                now,
            )
            .unwrap();
        s.decide(&c.id, Decision::AllowOnce, now).unwrap();
        assert!(!s.standing_covers("n", &[json!({"scope": "z"})]));
    }

    #[test]
    fn not_offered_means_once() {
        let s = ApprovalStore::new();
        let now = Utc::now();
        let a = s
            .request_offering(
                "n",
                &json!({"x": 1}),
                Risk::Notes,
                "s".into(),
                None,
                Offer {
                    always: false,
                    covers: Some("covers".into()),
                    scopes: vec![json!({"scope": "notes"})],
                },
                now,
            )
            .unwrap();
        assert!(a.always_covers.is_none());
        s.decide(&a.id, Decision::AlwaysThisSession, now).unwrap();
        assert!(!s.standing_covers("n", &[json!({"scope": "notes"})]));
        assert!(!s.standing_allows("n", &json!({"x": 1})));
    }

    #[test]
    fn a_waiting_request_is_reused_only_with_the_same_offer() {
        let s = ApprovalStore::new();
        let now = Utc::now();
        let args = json!({"x": 1});
        let offer = |scopes: Vec<Value>| Offer {
            always: true,
            covers: None,
            scopes,
        };
        let a = s
            .request_offering(
                "n",
                &args,
                Risk::Notes,
                "s".into(),
                None,
                offer(vec![json!({"a": 1})]),
                now,
            )
            .unwrap();
        let same = s
            .request_offering(
                "n",
                &args,
                Risk::Notes,
                "s".into(),
                None,
                offer(vec![json!({"a": 1})]),
                now,
            )
            .unwrap();
        assert_eq!(a.id, same.id);
        let other = s
            .request_offering(
                "n",
                &args,
                Risk::Notes,
                "s".into(),
                None,
                offer(vec![json!({"a": 2})]),
                now,
            )
            .unwrap();
        assert_ne!(a.id, other.id);
    }
}
