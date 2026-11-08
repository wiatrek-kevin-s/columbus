//! One approval flow for every tool that needs a person's yes.
//!
//! 1. An approval id the person already granted, or a standing "allow for
//!    this session", is spent on the exact request. "Allow for this
//!    session" grants what the person was shown when asked (the exact
//!    request, or the tool's scopes), fixed when the approval is made.
//! 2. Otherwise the person is asked in their own MCP client (elicitation).
//! 3. If the client cannot ask, the request is shown on Columbus's local
//!    approvals page and the call waits briefly for the decision.
//! 4. If no decision arrives in time, the agent gets `approval_required`
//!    with the approval's id, never the page's address.

use std::time::{Duration, Instant};

use chrono::Local;
use columbus_shared::approvals::{Decision, Offer, Risk, State};
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
    /// What "allow for this session" covers, when wider than this exact
    /// request: every one of these scopes must stand for a later call to go
    /// ahead without asking, and choosing it grants exactly these (fixed
    /// when the person is asked). None: this exact request.
    pub standing: Option<&'a [Value]>,
    /// What that wider grant covers, in the person's words; shown with the
    /// choice and added to the summary.
    pub always_covers: Option<String>,
    /// A script that may be allowed for the session (REQ-17, R-15: policy
    /// `scripts.session_approval`, the engine or a governed level-A
    /// environment): "Allow this script for the session" is offered, and
    /// grants exactly `standing` (its code hash and environment), so the
    /// same code with other inputs runs without asking in this session.
    pub script_session: bool,
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
        } else if self.state.policy().approvals.page {
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
        // The request was checked against the settings in force now; if they
        // change while the person decides, a yes is not used.
        let generation = st.generation();
        let settled = |granted: String| -> Sought {
            st.refresh();
            // A change seen but not yet settled counts too: it is about to apply.
            if st.generation() != generation || st.problem().is_some() || st.unsettled() {
                // Nor does "allow for this session" outlive the settings it
                // was given under.
                st.approvals.unstand(&granted);
                Sought::Declined("Columbus's settings changed while the person was deciding, so this approval was not used. Call the tool again: the request is checked against the new settings and the person is asked again.".into())
            } else {
                Sought::Granted(granted)
            }
        };
        if let Some(id) = ask.approval_id {
            st.approvals
                .consume(id, ask.tool, ask.request, self.now())?;
            return Ok(Sought::Granted(id.to_string()));
        }
        // Scripts stand only as the session approval of their code, and
        // never without scopes to bind it to.
        let script_session = ask.script_session
            && ask.risk == Risk::Script
            && ask.standing.is_some_and(|s| !s.is_empty());
        let may_stand =
            !ask.every_time && (script_session || !matches!(ask.risk, Risk::Script | Risk::Edit));
        if may_stand {
            let stands = match ask.standing {
                Some(scopes) => st.approvals.standing_covers(ask.tool, scopes),
                None => st.approvals.standing_allows(ask.tool, ask.request),
            };
            if stands {
                return Ok(Sought::Granted("standing".into()));
            }
        }
        // A workflow run has no person to ask: it stops here, and says why.
        if st.unattended {
            return Ok(Sought::Declined(format!(
                "{} a workflow run never waits for a person, and this needs their approval: {}",
                crate::workflows::NEEDS_YOU,
                ask.summary
            )));
        }
        // Scripts are approved one run at a time (unless allowed for the
        // session, above) and edits one change set at a time, wherever the
        // person is asked.
        let offer_always = may_stand;
        let covers = ask.always_covers.clone().filter(|_| offer_always);
        // The prompt in the person's client says what it covers too.
        let prompt = match &covers {
            Some(c) => format!("{}\n\n{c}", ask.summary),
            None => ask.summary.clone(),
        };
        let ap = st.approvals.request_offering(
            ask.tool,
            ask.request,
            ask.risk,
            // The page and the app show what it covers beside the choice.
            ask.summary.clone(),
            ask.detail.clone(),
            Offer {
                always: offer_always,
                covers: covers.clone(),
                scopes: ask.standing.map(<[Value]>::to_vec).unwrap_or_default(),
            },
            self.now(),
        )?;
        let declined = || format!("the person declined: {}", ask.summary);

        let always_choice = offer_always.then(|| match &covers {
            Some(c) if script_session => {
                format!("Allow once, or allow this script for the session: {c}")
            }
            Some(c) => format!("Allow once, or allow for this session: {c}"),
            None => "Allow once, or allow this exact request again for the rest of the session"
                .to_string(),
        });
        match crate::providers::ask_person_with(
            ctx,
            &format!("Columbus: allow {}?", ask.label),
            &prompt,
            ask.code,
            always_choice.as_deref(),
            Duration::from_secs(600),
        )
        .await
        {
            Asked::Allowed { always } => {
                let d = if always && offer_always {
                    Decision::AlwaysThisSession
                } else {
                    Decision::AllowOnce
                };
                st.approvals.decide_via(&ap.id, d, self.now(), "client")?;
                st.approvals
                    .consume(&ap.id, ask.tool, ask.request, self.now())?;
                return Ok(settled(ap.id));
            }
            Asked::Declined => {
                let _ = st
                    .approvals
                    .decide_via(&ap.id, Decision::Decline, self.now(), "client");
                return Ok(Sought::Declined(declined()));
            }
            Asked::Unsupported => {}
        }

        let next = if !st.policy().approvals.page {
            "This client cannot show approval prompts and organization policy turns the Columbus approvals page off, so the person cannot approve this here.".to_string()
        } else {
            match st.page.show().await {
                Err(e) => format!(
                    "This client cannot show approval prompts and Columbus could not open its approvals page: {e}. Tell the person what is waiting."
                ),
                Ok(_) => {
                    let deadline = Instant::now() + Duration::from_secs(st.policy().approvals.wait_seconds.min(300));
                    loop {
                        match st.approvals.status(&ap.id, self.now()).map(|a| a.state) {
                            Some(State::Approved) => {
                                st.approvals.consume(&ap.id, ask.tool, ask.request, self.now())?;
                                return Ok(settled(ap.id));
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
