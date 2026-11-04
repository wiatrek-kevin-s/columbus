//! `columbus-mcp servers …`: the person's side of the server lifecycle
//! (design change 1, section 4). Review lists what a server offers next to
//! the manifest's settings and any flags; approve pins the definitions.
//! Approving asks for confirmation on the terminal itself (not stdin), so
//! an agent that can run commands cannot approve on the person's behalf.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use columbus_broker::registry::{describe_status, Broker, Status};
use columbus_broker::review::{self, ToolState};

use crate::config::{self, Paths};
use crate::secrets::SecretStore;

fn broker(paths: &Paths) -> Result<Broker> {
    let (policy, _) = config::load_policy(paths)?;
    let scratch = paths.workspaces.join("review");
    std::fs::create_dir_all(&scratch)?;
    let secrets = Arc::new(SecretStore::from_env());
    Ok(Broker::load(
        &policy,
        &paths.servers,
        &paths.personal_servers,
        &scratch,
        Arc::new(move |n: &str| secrets.get(n).map_err(|e| e.to_string())),
        crate::server::NATIVE_NAMESPACES,
    )?)
}

pub async fn list() -> Result<()> {
    let paths = Paths::from_env();
    let b = broker(&paths)?;
    for n in &b.notes {
        println!("note: {n}");
    }
    if b.providers().is_empty() {
        println!(
            "No server manifests in {} or {}.",
            paths.servers.display(),
            paths.personal_servers.display()
        );
    }
    for p in b.providers() {
        let status = Broker::status_of(&b.connected(p).await);
        println!(
            "{:<16} {:?}  {}",
            p.id(),
            p.loaded.source,
            describe_status(p.id(), &status)
        );
    }
    Ok(())
}

pub async fn review(id: &str) -> Result<bool> {
    let paths = Paths::from_env();
    let b = broker(&paths)?;
    let p = b
        .get(id)
        .ok_or_else(|| anyhow!("no manifest with id {id:?}"))?;
    let tools = match b.reviewed(p).await {
        Ok(t) => t,
        Err(s) => bail!(describe_status(id, &s)),
    };
    let m = &p.loaded.manifest;
    println!(
        "{} ({}), trust {:?}, manifest {}",
        m.display_name(),
        id,
        m.trust,
        p.loaded.path.display()
    );
    let mut flagged = false;
    for t in &tools {
        let rule = m.rule_for(&t.name);
        let setting = match &rule {
            Some(r) => format!(
                "risk {:?}{}{}, approval {}",
                r.effective_risk(),
                if r.egress { ", egress" } else { "" },
                if r.output == columbus_broker::manifest::OutputMode::ToWorkspace {
                    ", saves to workspace"
                } else {
                    ""
                },
                if r.needs_approval() { "yes" } else { "no" },
            ),
            None => "not allowed by the manifest".into(),
        };
        println!(
            "\n  {}  [{:?}]  {}",
            t.name,
            t.state,
            &t.hash[..19.min(t.hash.len())]
        );
        println!(
            "    server says: {}",
            t.definition
                .description
                .as_deref()
                .unwrap_or("(no description)")
        );
        if let Some(a) = &t.definition.annotations {
            println!(
                "    server hints (not trusted): {}",
                serde_json::to_string(a).unwrap_or_default()
            );
        }
        println!("    Columbus manifest: {setting}");
        for f in &t.flags {
            flagged = true;
            println!("    FLAG {:?}: {}", f.kind, f.detail);
        }
    }
    let removed = b
        .connected(p)
        .await
        .map(|c| c.review.removed.clone())
        .unwrap_or_default();
    if !removed.is_empty() {
        println!(
            "\n  Approved earlier but no longer listed: {}",
            removed.join(", ")
        );
    }
    Ok(flagged)
}

pub async fn approve(id: &str, accept_flags: bool) -> Result<()> {
    let flagged = review(id).await?;
    if flagged && !accept_flags {
        bail!("some tools are flagged; read the flags above, then rerun with --accept-flags to approve anyway");
    }
    let paths = Paths::from_env();
    let b = broker(&paths)?;
    let p = b
        .get(id)
        .ok_or_else(|| anyhow!("no manifest with id {id:?}"))?;
    let c = match b.connected(p).await {
        Ok(c) => c,
        Err(s @ Status::Blocked { .. }) | Err(s) => bail!(describe_status(id, &s)),
    };
    let confirm = rpassword::prompt_password(format!(
        "\nApprove these {} tool definitions for {id}? Type the server id to confirm: ",
        c.review.tools.len()
    ))
    .context("approval needs an interactive terminal")?;
    if confirm.trim() != id {
        bail!("not confirmed; nothing changed");
    }
    let pinned = review::pin(&c.review, c.server_version.clone());
    write_pinned(&p.loaded.path, &pinned)?;
    let enabled = c
        .review
        .tools
        .iter()
        .filter(|t| {
            t.state != ToolState::NotAllowed && p.loaded.manifest.rule_for(&t.name).is_some()
        })
        .count();
    println!(
        "Pinned {} tool definitions in {}; {enabled} are enabled by the manifest.",
        pinned.tools.len(),
        p.loaded.path.display()
    );
    Ok(())
}

/// Replace the top-level `pinned:` block in a manifest, keeping the rest of
/// the file (comments included) as it was.
fn write_pinned(path: &Path, pinned: &columbus_broker::manifest::Pinned) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let block = serde_yaml::to_string(&serde_yaml::Mapping::from_iter([(
        serde_yaml::Value::from("pinned"),
        serde_yaml::to_value(pinned)?,
    )]))?;
    let mut out = String::new();
    let mut skipping = false;
    let mut replaced = false;
    for line in text.lines() {
        let top_level =
            !line.starts_with(' ') && !line.starts_with('\t') && !line.trim().is_empty();
        if skipping {
            if top_level {
                skipping = false;
            } else {
                continue;
            }
        }
        if top_level && line.starts_with("pinned:") {
            out.push_str(&block);
            skipping = true;
            replaced = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced {
        out.push_str(&block);
    }
    // Check the result still parses before writing it.
    columbus_broker::manifest::ServerManifest::parse(&out, path)?;
    std::fs::write(path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_block_is_replaced_and_comments_kept() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("notes.yaml");
        std::fs::write(
            &p,
            "# reviewed by GIS team\nid: notes\ntransport: { type: stdio, command: x }\npinned:\n  tools_hash: old\n  tools:\n    a: old\ntools:\n  search: { risk: read }  # keep me\n",
        )
        .unwrap();
        let pinned = columbus_broker::manifest::Pinned {
            version: Some("1.0".into()),
            tools_hash: Some("sha256:new".into()),
            tools: [("search".to_string(), "sha256:s".to_string())].into(),
        };
        write_pinned(&p, &pinned).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# reviewed by GIS team") && text.contains("# keep me"));
        assert!(!text.contains("old"));
        let m = columbus_broker::manifest::ServerManifest::parse(&text, &p).unwrap();
        assert_eq!(m.pinned.as_ref().unwrap().tools["search"], "sha256:s");
        assert!(m.rule_for("search").is_some());
    }
}
