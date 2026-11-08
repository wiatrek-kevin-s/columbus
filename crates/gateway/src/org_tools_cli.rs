//! Tools the organization installs for everyone on a computer, from a
//! shared file (`.columbus`) someone made with Share in the Columbus app:
//!
//! - `columbus-mcp tools org-install FILE` checks each tool in the file as
//!   Columbus checks any proposed tool (exactly what Columbus builds from a
//!   proposal, its code within Columbus's rules), says who signed the file,
//!   and puts the tool in the organization's tools folder
//!   (`COLUMBUS_ORG_TOOLS`, or `tools` in ProgramData or
//!   /Library/Application Support/Columbus), approved: the administrator
//!   running it is the reviewer. Writing there needs an administrator.
//! - `columbus-mcp tools org-remove ID` takes one away.
//! - `columbus-mcp tools org-list` lists them.
//!
//! Every session then offers them as tools "installed by your
//! organization", confined like the person's own (level A: no network,
//! writing only their own folder), needing no approval to run. An
//! organization tool's id wins over a personal tool's. Policy can turn
//! them all off (`allow_org_tools: false`) or deny one (`deny_tools`).
//! Each install and removal is audited.

use anyhow::{anyhow, bail};
use columbus_shared::audit::AuditRecord;
use columbus_shared::share;
use columbus_shared::tools as store;
use serde_json::json;

use crate::server::State;

fn record(state: &State, tool: &str, decision: &str, reason: String, request: Option<String>) {
    let _ = state.audit.append(&AuditRecord {
        user: state.user.clone(),
        client: "columbus-mcp tools".into(),
        session: Some(state.session.clone()),
        tool: tool.into(),
        decision: decision.into(),
        reason: Some(reason),
        request,
        ..Default::default()
    });
}

pub async fn install(file: &str) -> anyhow::Result<()> {
    let state = State::load(crate::config::Paths::from_env())?;
    let workspace = state.sandbox.root().to_path_buf();
    let out = run(&state, file).await;
    let _ = std::fs::remove_dir(&workspace);
    match out {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
            Ok(())
        }
        Err(e) => {
            record(
                &state,
                "org_install_tool",
                "denied",
                format!("{file}: {e}"),
                None,
            );
            Err(anyhow!(e))
        }
    }
}

async fn run(state: &State, file: &str) -> Result<Vec<String>, String> {
    let bytes = share::read_bytes(std::path::Path::new(file))?;
    let sha = columbus_shared::canon::sha256_hex(&bytes);
    let shared = share::parse_bytes(&bytes)?;
    let policy = state.policy().clone();
    let signer = share::signer(
        &shared,
        &share::trusted(&share::trust_file()),
        &policy.share_signers,
    );
    let signed = crate::share_cli::signer_text(&signer);
    if signer["status"] == "invalid" {
        return Err(
            "its signature doesn't match it: the file was changed after it was signed".into(),
        );
    }
    if policy.require_signed_shares && signer["status"] != "trusted" {
        return Err("this computer's policy takes only shared files signed by someone trusted (require_signed_shares), and this one isn't".into());
    }
    if shared.tools.is_empty() {
        return Err("it holds no tools".into());
    }
    let root = store::org_tools_dir();
    let mut lines = vec![format!(
        "{} shared by {} on {}, {signed}.",
        if shared.kind == "workflow" {
            "A workflow"
        } else {
            "A tool"
        },
        shared.shared_by,
        shared.shared_at
    )];
    // Every tool checked before any is installed.
    let mut ready = Vec::new();
    for t in &shared.tools {
        let (args, built) = crate::propose_tool::rebuild(&t.id, &t.manifest, &t.code)
            .map_err(|e| format!("{}: {e}", t.id))?;
        let tool = format!("{}.{}", built.id, store::TOOL);
        if !policy.tool_allowed(&tool) {
            return Err(format!(
                "organization policy does not allow the tool {tool}"
            ));
        }
        let warnings = crate::share_cli::lint_tool(state, &args, &t.id).await?;
        for w in &warnings {
            lines.push(format!("  {}: note: {w}", t.id));
        }
        ready.push(built);
    }
    for b in ready {
        // Recorded first: nothing is installed for everyone without its line.
        state
            .audit
            .append(&AuditRecord {
                user: state.user.clone(),
                client: "columbus-mcp tools".into(),
                session: Some(state.session.clone()),
                tool: "org_install_tool".into(),
                decision: "allowed".into(),
                reason: Some(format!(
                    "installing {} for everyone (hash {}) from a file shared by {}, {signed}",
                    b.id, b.hash, shared.shared_by
                )),
                request: Some(sha.clone()),
                ..Default::default()
            })
            .map_err(|e| format!("refused: the audit log could not be written ({e})"))?;
        let info = json!({
            "installed": chrono::Local::now().to_rfc3339(),
            "by": state.user,
            "from": shared.shared_by,
            "shared_at": shared.shared_at,
            "signed": signed,
            "file_sha256": sha,
        });
        let hash = store::place(
            &root,
            &b.id,
            b.manifest_text.as_bytes(),
            b.entry.as_bytes(),
            info,
        )
        .map_err(|e| {
            format!(
                "{}: {e} (installing for everyone needs an administrator: run this as one)",
                b.id
            )
        })?;
        let _ = hash;
        lines.push(format!(
            "Installed {} ({}) for everyone, in {}.",
            b.id,
            b.manifest.display_name(),
            root.display()
        ));
    }
    if !policy.allow_org_tools {
        lines.push("Note: organization tools are off until this computer's policy turns them on (allow_org_tools: true).".into());
    }
    Ok(lines)
}

pub fn remove(id: &str) -> anyhow::Result<()> {
    let state = State::load(crate::config::Paths::from_env())?;
    let root = store::org_tools_dir();
    match store::remove(&root, id) {
        Ok(()) => {
            record(
                &state,
                "org_remove_tool",
                "allowed",
                format!("removed {id} for everyone"),
                None,
            );
            println!("Removed {id} for everyone.");
            Ok(())
        }
        Err(e) => {
            record(
                &state,
                "org_remove_tool",
                "denied",
                format!("{id}: {e}"),
                None,
            );
            bail!(e)
        }
    }
}

pub fn list() -> anyhow::Result<()> {
    let root = store::org_tools_dir();
    println!("Organization tools in {}", root.display());
    let all = store::list_installed(&root);
    if all.is_empty() {
        println!("  none");
    }
    for t in all {
        println!(
            "  {} ({}) version {}: {}",
            t.id,
            t.title,
            t.version,
            t.state.as_deref().unwrap_or("not approved")
        );
    }
    Ok(())
}
