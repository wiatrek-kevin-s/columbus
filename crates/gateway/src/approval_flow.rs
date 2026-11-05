//! One approval flow for every tool that needs a person's yes.
//!
//! 1. An approval id the person already granted, or a standing "allow for
//!    this session", is spent on the exact request.
//! 2. Otherwise the person is asked in their own MCP client (elicitation).
//! 3. If the client cannot ask, the request is shown on Columbus's local
//!    approvals page and the call waits briefly for the decision.
//! 4. If no decision arrives in time, the agent gets `approval_required`
//!    with the approval's id, never the page's address.

use std::time::{Duration, Instant};

use chrono::Local;
use columbus_shared::approvals::{Decision, Risk, State};
use columbus_shared::Denied;
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use serde_json::Value;

use crate::providers::Asked;
use crate::server::{ApprovalNeeded, Gateway};

pub(crate) enum Sought {
    /// Go ahead; the id (or `standing`) goes in the audit line.
    Granted(String),
    /// The person said no, or the approval expired.
    Declined(String),
    /// Still waiting for the person.
    Pending(ApprovalNeeded),
}

pub(crate) struct Ask<'a> {
    /// The tool the approval is bound to (with `request`).
    pub tool: &'a str,
    /// The name shown to the person, e.g. `drive.create_file`.
    pub label: &'a str,
    pub request: &'a Value,
    pub risk: Risk,
    pub summary: String,
    pub detail: Option<Value>,
    pub approval_id: Option<&'a str>,
    /// Ask on every call: no standing "allow for this session" covers it,
    /// and the person is only offered "allow once" (scripts).
    pub every_time: bool,
    /// Code written by the AI that the person must read before deciding (a
    /// script), sent to the client as its own part; the page shows it from
    /// `detail`.
    pub code: Option<&'a str>,
}

impl Gateway {
    /// How this client's person will be asked, for `whoami`.
    pub(crate) fn approval_channel(&self, ctx: &RequestContext<RoleServer>) -> String {
        use rmcp::service::ElicitationMode;
        if ctx
            .peer
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
        {
            "in your AI client, as a prompt".into()
        } else if self.state.policy.approvals.page {
            "on the Columbus approvals page, opened in the browser".into()
        } else {
            "nowhere: this client cannot show prompts and policy turns the approvals page off"
                .into()
        }
    }

    pub(crate) async fn seek_approval(
        &self,
        ctx: &RequestContext<RoleServer>,
        ask: Ask<'_>,
    ) -> Result<Sought, Denied> {
        let st = &self.state;
        if let Some(id) = ask.approval_id {
            st.approvals
                .consume(id, ask.tool, ask.request, self.now())?;
            return Ok(Sought::Granted(id.to_string()));
        }
        if !ask.every_time && st.approvals.standing_allows(ask.tool, ask.request) {
            return Ok(Sought::Granted("standing".into()));
        }
        let ap = st.approvals.request_with_detail(
            ask.tool,
            ask.request,
            ask.risk,
            ask.summary.clone(),
            ask.detail.clone(),
            self.now(),
        )?;
        let declined = || format!("the person declined: {}", ask.summary);

        match crate::providers::ask_person_with(
            ctx,
            &format!("Columbus: allow {}?", ask.label),
            &ask.summary,
            ask.code,
            !ask.every_time,
            Duration::from_secs(600),
        )
        .await
        {
            Asked::Allowed { always } => {
                let d = if always && !ask.every_time {
                    Decision::AlwaysThisSession
                } else {
                    Decision::AllowOnce
                };
                st.approvals.decide_via(&ap.id, d, self.now(), "client")?;
                st.approvals
                    .consume(&ap.id, ask.tool, ask.request, self.now())?;
                return Ok(Sought::Granted(ap.id));
            }
            Asked::Declined => {
                let _ = st
                    .approvals
                    .decide_via(&ap.id, Decision::Decline, self.now(), "client");
                return Ok(Sought::Declined(declined()));
            }
            Asked::Unsupported => {}
        }

        let next = if !st.policy.approvals.page {
            "This client cannot show approval prompts and organization policy turns the Columbus approvals page off, so the person cannot approve this here.".to_string()
        } else {
            match st.page.show().await {
                Err(e) => format!(
                    "This client cannot show approval prompts and Columbus could not open its approvals page: {e}. Tell the person what is waiting."
                ),
                Ok(_) => {
                    let deadline = Instant::now() + Duration::from_secs(st.policy.approvals.wait_seconds.min(300));
                    loop {
                        match st.approvals.status(&ap.id, self.now()).map(|a| a.state) {
                            Some(State::Approved) => {
                                st.approvals.consume(&ap.id, ask.tool, ask.request, self.now())?;
                                return Ok(Sought::Granted(ap.id));
                            }
                            Some(State::Declined) => return Ok(Sought::Declined(declined())),
                            Some(State::Pending) if Instant::now() < deadline => {
                                tokio::time::sleep(Duration::from_millis(200)).await
                            }
                            Some(State::Pending) => break,
                            _ => return Ok(Sought::Declined(format!("the approval expired: {}", ask.summary))),
                        }
                    }
                    "Columbus has put this request on its approvals page in the person's browser. Tell them what is waiting and that they can decide there. Once they have, call the tool again with the same arguments plus approval_id; approval_status shows whether they have decided.".to_string()
                }
            }
        };
        Ok(Sought::Pending(ApprovalNeeded {
            id: ap.id.clone(),
            summary: ask.summary,
            expires: ap.expires.with_timezone(&Local).to_rfc3339(),
            next,
        }))
    }
}
