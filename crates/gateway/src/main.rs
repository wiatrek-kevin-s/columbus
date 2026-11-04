//! `columbus-mcp`: the governed GIS MCP server.
//!
//! ```text
//! columbus-mcp                      serve MCP over stdio (default)
//! columbus-mcp serve                same
//! columbus-mcp audit verify FILE    check a month's audit log hash chain
//! columbus-mcp secret set ID        store a connection's password (prompted, not echoed)
//! columbus-mcp secret delete ID     remove it
//! columbus-mcp servers list         connected MCP servers and their state
//! columbus-mcp servers review ID    a server's tools, the manifest's settings and any flags
//! columbus-mcp servers approve ID [--accept-flags]   pin the reviewed tool definitions
//! columbus-mcp servers signin ID    sign in to a remote server in the browser (OAuth)
//! columbus-mcp servers signout ID   remove its tokens
//! columbus-mcp registry update [--from DIR|URL]   install the registry index (checked)
//! columbus-mcp pack lint [DIR]      check a context pack
//! columbus-mcp plugins list         engine plugins, their versions, hashes and state
//! columbus-mcp plugins approve ID   approve a plugin's exact contents
//! columbus-mcp --version
//! ```

mod approval_flow;
mod approvals_page;
mod config;
mod context;
mod export_tools;
mod gis_tools;
mod plugin_run;
mod plugins;
mod providers;
mod registry_tools;
mod review_cli;
mod secrets;
mod server;
mod worker;

use anyhow::{bail, Result};
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // stdout carries the MCP protocol; logs go to stderr only.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("COLUMBUS_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["serve"] => serve().await,
        ["audit", "verify", file] => {
            match columbus_shared::audit::verify(std::path::Path::new(file)) {
                Ok(n) => {
                    println!("ok: {n} lines, chain intact");
                    Ok(())
                }
                Err(e) => bail!("audit log check failed: {e:?}"),
            }
        }
        ["secret", "set", id] => secret_set(id),
        ["plugins", "list"] => review_cli::plugins_list().await,
        ["plugins", "approve", id] => review_cli::plugins_approve(id).await,
        ["servers", "list"] => review_cli::list().await,
        ["servers", "review", id] => review_cli::review(id).await.map(|_| ()),
        ["servers", "approve", id] => review_cli::approve(id, false).await,
        ["servers", "approve", id, "--accept-flags"] => review_cli::approve(id, true).await,
        ["servers", "signin", id] => review_cli::signin(id).await,
        ["registry", "update"] => registry_tools::update(None).await,
        ["pack", "lint"] => context::lint_cli(None),
        ["pack", "lint", dir] => context::lint_cli(Some(dir)),
        ["registry", "update", "--from", src] => registry_tools::update(Some(src)).await,
        ["servers", "signout", id] => review_cli::signout(id),
        ["secret", "delete", id] => {
            let store = secrets::SecretStore::from_env();
            if store.delete(id)? {
                println!("deleted the secret for {id}");
            } else {
                println!("no secret was stored for {id}");
            }
            Ok(())
        }
        ["--version"] | ["-V"] => {
            println!("{} {}", server::SERVER_NAME, env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => bail!(
            "usage: columbus-mcp [serve] | audit verify FILE | secret set|delete ID | servers list|review|approve|signin|signout ID | registry update [--from DIR|URL] | plugins list|approve ID | --version"
        ),
    }
}

/// Store a connection's password in the credential store. Run by the
/// person at the keyboard, never by an agent: the gateway exposes no tool
/// for it.
fn secret_set(id: &str) -> Result<()> {
    let paths = config::Paths::from_env();
    let known = config::load_connections(&paths)?;
    if !known.iter().any(|c| c.id == *id) {
        eprintln!(
            "note: no connection {id:?} in {} yet; storing the secret anyway",
            paths.connections.display()
        );
    }
    let store = secrets::SecretStore::from_env();
    let first = rpassword::prompt_password(format!("Password for {id}: "))?;
    let second = rpassword::prompt_password("Again: ")?;
    if first != second {
        bail!("the two entries did not match; nothing stored");
    }
    if first.is_empty() {
        bail!("empty password; nothing stored (integrated sign-in needs no stored secret)");
    }
    store.set(id, &first)?;
    println!("stored the secret for {id} in {}", store.describe());
    Ok(())
}

async fn serve() -> Result<()> {
    let paths = config::Paths::from_env();
    let state = server::State::load(paths)?;
    tracing::info!(session = %state.session, workspace = %state.sandbox.root().display(), "starting");
    let service = server::Gateway::new(state).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
