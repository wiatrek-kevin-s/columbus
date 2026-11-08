//! `propose_environment`: the assistant asks for a new Python environment
//! (build brief Part 5 "Creating one", REQ-14). It only files a proposal:
//! nothing is downloaded or created until the person approves it at the
//! keyboard (the Columbus app, or `columbus-mcp env approve <id>`), and no
//! tool the assistant has can approve it.

use chrono::Local;
use columbus_shared::env_create::{self as create, Proposal};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::server::{client_name, Gateway, Output};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProposeEnvironmentArgs {
    /// Its id: lowercase letters, digits and -, at most 40 (e.g. "whitebox").
    pub id: String,
    /// "3.N" or "3.N.M", e.g. "3.12". Omit for the engine's own version.
    /// Columbus uses a Python already on this computer and never downloads one.
    #[serde(default)]
    pub python_version: Option<String>,
    /// Every package as an exact pin, name==version (e.g. "whitebox==2.3.6").
    /// Ranges, wildcards, URLs, paths and VCS links are refused. Each must
    /// publish a wheel for this system: nothing is built from source.
    pub packages: Vec<String>,
    /// What it is for, in the person's words (shown to them when they decide).
    pub reason: String,
}

#[tool_router(router = env_router, vis = "pub(crate)")]
impl Gateway {
    #[tool(
        name = "propose_environment",
        description = "Ask the person for a new Python environment for scripts, with packages the engine lacks (for example whitebox==2.3.6). Give an id, the Python version (omit for the engine's), every package pinned exactly as name==version, and the reason. This only files a request: the person approves it in the Columbus app or the terminal, and only then does Columbus create it from the organization's package index (wheels only, no source builds) and register it governed. You cannot approve it. Then run_python can use it by its id. Needs environments_allowed in whoami.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn propose_environment(
        &self,
        Parameters(args): Parameters<ProposeEnvironmentArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<Json<Output<Value>>, String> {
        let request = json!({
            "id": args.id,
            "python_version": args.python_version,
            "packages": args.packages,
        });
        let rec = self.gate(&ctx, "propose_environment", &request)?;
        let settings = self.state.settings();
        let policy = &settings.policy.environments;
        let p = Proposal {
            id: args.id.trim().to_string(),
            python_version: args.python_version.map(|v| v.trim().to_string()),
            packages: args.packages.iter().map(|s| s.trim().to_string()).collect(),
            reason: args.reason.trim().to_string(),
            proposed: Local::now().to_rfc3339(),
            session: self.state.session.clone(),
            client: client_name(&ctx),
        };
        let fingerprint = match create::write_proposal(
            &self.state.paths.environments,
            &self.state.paths.envs,
            policy,
            &p,
        ) {
            Ok(f) => f,
            Err(e) => {
                return Err(self.deny(&ctx, rec, format!("the environment was not proposed: {e}")))
            }
        };
        self.audit(
            &ctx,
            columbus_shared::audit::AuditRecord {
                decision: "allow".into(),
                ..rec
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Json(Output {
            result: json!({
                "status": "waiting_for_person",
                "id": p.id,
                "fingerprint": fingerprint,
                "index_hosts": policy.index_hosts,
                "next": format!(
                    "Tell the person the environment {id} is waiting for them: they approve it with `columbus-mcp env approve {id}` in a terminal (or in the Columbus app, where it offers this). Nothing is downloaded before that. Once whoami lists {id} as ready, pass it to run_python.",
                    id = p.id
                ),
            }),
            checks: vec![],
            provenance: None,
        }))
    }
}
