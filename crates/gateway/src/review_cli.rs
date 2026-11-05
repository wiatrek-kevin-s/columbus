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
        secrets,
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

// ---------- engine plugins ----------

fn plugins(paths: &Paths) -> Result<crate::plugins::Plugins> {
    let (policy, _) = config::load_policy(paths)?;
    let scratch = paths.workspaces.join("review");
    std::fs::create_dir_all(&scratch)?;
    let host = paths
        .worker_script
        .parent()
        .map(|d| d.join("columbus_plugin_host.py"))
        .unwrap_or_default();
    Ok(crate::plugins::Plugins::load(
        &policy,
        &paths.plugins,
        &paths.personal_plugins,
        paths.python.clone(),
        host,
        scratch,
        &Default::default(),
    ))
}

pub async fn plugins_list() -> Result<()> {
    let paths = Paths::from_env();
    let ps = plugins(&paths)?;
    for n in &ps.notes {
        println!("note: {n}");
    }
    if ps.all().is_empty() {
        println!("No plugins in {}.", paths.plugins.display());
    }
    for p in ps.all() {
        let status = ps.status(p);
        println!(
            "{:<28} {:<8} {} {}  {}",
            p.id(),
            p.manifest.version,
            if p.personal { "personal" } else { "org" },
            p.hash.get(..19).unwrap_or(&p.hash),
            crate::plugins::describe_status(p.id(), &status)
        );
    }
    Ok(())
}

pub async fn plugins_approve(id: &str) -> Result<()> {
    let paths = Paths::from_env();
    let ps = plugins(&paths)?;
    let p = ps
        .get(id)
        .ok_or_else(|| anyhow!("no plugin with id {id:?}"))?;
    println!(
        "{} ({id}) {} from {}",
        p.manifest.display_name(),
        p.manifest.version,
        p.dir.display()
    );
    println!(
        "runtime {:?}, entry {}, contents {}",
        p.manifest.runtime, p.manifest.entry, p.hash
    );
    for (name, t) in &p.manifest.tools {
        println!("\n  {name}  [risk {:?}]  {}", t.risk, t.summary);
        for (pn, ps) in &t.params {
            println!(
                "    param  {pn}: {}",
                serde_json::to_string(ps).unwrap_or_default()
            );
        }
        for (on, o) in &t.outputs {
            println!("    output {on}: {}", o.kind());
        }
    }
    // Start it once so a worker that does not match plugin.yaml is caught now.
    if let Err(e) = ps.worker_unchecked(p).await {
        bail!("the plugin did not start or does not match its plugin.yaml: {e}");
    }
    let confirm = rpassword::prompt_password(format!(
        "\nApprove {id} at {}? Type the plugin id to confirm: ",
        p.hash
    ))
    .context("approval needs an interactive terminal")?;
    if confirm.trim() != id {
        bail!("not confirmed; nothing changed");
    }
    let root = p
        .dir
        .parent()
        .ok_or_else(|| anyhow!("plugin folder has no parent"))?;
    let mut approvals = crate::plugins::read_approvals(root);
    approvals.insert(id.to_string(), p.hash.clone());
    std::fs::write(
        root.join(crate::plugins::APPROVALS_FILE),
        serde_yaml::to_string(&approvals)?,
    )?;
    println!("Approved {id}.");
    Ok(())
}

/// `columbus-mcp tools list`: the person's own tools, installed and waiting.
pub fn tools_list() -> Result<()> {
    use columbus_shared::tools;
    let paths = Paths::from_env();
    let (policy, _) = config::load_policy(&paths)?;
    println!("Personal tools in {}", paths.tools.display());
    if !policy.allow_personal_tools {
        println!("note: organization policy turns personal tools off (allow_personal_tools); none of these are used.");
    }
    for t in tools::list_installed(&paths.tools) {
        println!(
            "installed  {:<32} {:<8} {}  {}",
            t.tool,
            t.version,
            t.state.as_deref().unwrap_or(""),
            t.problem.as_deref().unwrap_or(&t.title)
        );
    }
    for t in tools::list_proposals(&paths.tools) {
        println!(
            "waiting    {:<32} {:<8} {}  {}",
            t.tool,
            t.version,
            t.hash,
            t.problem.as_deref().unwrap_or(&t.title)
        );
    }
    Ok(())
}

/// `columbus-mcp tools show ID`: a waiting tool's manifest and code in full.
pub fn tools_show(id: &str) -> Result<()> {
    let paths = Paths::from_env();
    let t = columbus_shared::tools::read_proposal(&paths.tools, id).map_err(|e| anyhow!(e))?;
    println!(
        "{} ({}) version {}, contents {}",
        t.title, t.tool, t.version, t.hash
    );
    if t.replaces.is_some() {
        println!("It replaces the installed tool of the same id.");
    }
    println!(
        "\n----- plugin.yaml -----\n{}",
        t.manifest_text.unwrap_or_default()
    );
    println!("----- tool.py -----\n{}", t.code.unwrap_or_default());
    Ok(())
}

/// `columbus-mcp tools install ID`: install a waiting tool, confirmed on the
/// terminal itself (not stdin), as plugins are approved, so an agent that
/// can run commands cannot install one for the person.
pub fn tools_install(id: &str) -> Result<()> {
    use columbus_shared::tools;
    let paths = Paths::from_env();
    let (policy, _) = config::load_policy(&paths)?;
    if !policy.allow_personal_tools {
        bail!("organization policy turns personal tools off (allow_personal_tools)");
    }
    let t = tools::read_proposal(&paths.tools, id).map_err(|e| anyhow!(e))?;
    println!(
        "{} ({}) version {}, contents {}\n{}\nRead it in full with `columbus-mcp tools show {id}`.",
        t.title, t.tool, t.version, t.hash, t.description
    );
    let confirm = rpassword::prompt_password(format!(
        "\nInstall {id} at {}? Your assistant may then run it on your data without asking. Type the tool id to confirm: ",
        t.hash
    ))
    .context("installing needs an interactive terminal")?;
    if confirm.trim() != id {
        bail!("not confirmed; nothing changed");
    }
    tools::install(&paths.tools, id, &t.hash, &tools::now()).map_err(|e| anyhow!(e))?;
    println!(
        "Installed {}. Running sessions can use it from their next call.",
        t.tool
    );
    Ok(())
}

pub fn tools_decline(id: &str) -> Result<()> {
    let paths = Paths::from_env();
    columbus_shared::tools::decline(&paths.tools, id).map_err(|e| anyhow!(e))?;
    println!("Declined {id}.");
    Ok(())
}

pub fn tools_remove(id: &str) -> Result<()> {
    let paths = Paths::from_env();
    columbus_shared::tools::remove(&paths.tools, id).map_err(|e| anyhow!(e))?;
    println!("Removed {id}. Running sessions stop using it from their next call.");
    Ok(())
}

/// `columbus-mcp servers signin ID`: sign in to a remote server in the
/// browser (design change 1, section 8). Tokens go to the credential store;
/// nothing is printed but the outcome.
pub async fn signin(id: &str) -> Result<()> {
    use columbus_broker::manifest::{Auth, Transport};
    let paths = Paths::from_env();
    let b = broker(&paths)?;
    let p = b
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("no server manifest with id {id:?}"))?;
    let m = &p.loaded.manifest;
    let (scopes, client_id) = match &m.auth {
        Auth::Oauth { scopes, client_id } => (scopes.clone(), client_id.clone()),
        _ => anyhow::bail!("{id} does not use OAuth sign-in (its manifest's auth is not oauth)"),
    };
    let Transport::Http { url } = &m.transport else {
        anyhow::bail!("{id} is not a remote server");
    };
    let store: Arc<dyn columbus_broker::oauth::SecretStore> = Arc::new(SecretStore::from_env());
    println!(
        "Signing in to {} ({url}) with scopes: {}",
        m.display_name(),
        if scopes.is_empty() {
            "the server's defaults".to_string()
        } else {
            scopes.join(", ")
        }
    );
    let done = columbus_broker::oauth::sign_in(
        url,
        id,
        &scopes,
        client_id.as_deref(),
        store,
        |auth_url| {
            if std::env::var("COLUMBUS_SIGNIN_BROWSER").as_deref() == Ok("print") {
                // Tests: the harness follows the link itself.
                println!("open: {auth_url}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                return Ok(());
            }
            println!("Your browser is opening the sign-in page. Waiting up to 5 minutes...");
            crate::approvals_page::open_browser(auth_url).map_err(|e| {
                format!("could not open the browser ({e}); open this address yourself:\n{auth_url}")
            })
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;
    println!(
        "Signed in to {}. Granted scopes: {}. Tokens are in the credential store.",
        m.display_name(),
        if done.scopes.is_empty() {
            "(not reported)".to_string()
        } else {
            done.scopes.join(", ")
        }
    );
    Ok(())
}

/// `columbus-mcp servers signout ID`: forget a server's tokens.
pub fn signout(id: &str) -> Result<()> {
    let store = SecretStore::from_env();
    columbus_broker::oauth::sign_out(&store, id).map_err(|e| anyhow::anyhow!(e))?;
    println!("Signed out of {id}: its tokens were removed from the credential store.");
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
