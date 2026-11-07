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
//! columbus-mcp registry query JSON                one registry search or entry, as JSON (the app's Registry page)
//! columbus-mcp pack lint [DIR]      check a context pack
//! columbus-mcp plugins list         engine plugins, their versions, hashes and state
//! columbus-mcp plugins approve ID   approve a plugin's exact contents
//! columbus-mcp tools list           the person's own tools, installed and waiting
//! columbus-mcp tools show ID        a waiting tool's manifest and code
//! columbus-mcp tools install ID     install a waiting tool (confirmed on the terminal)
//! columbus-mcp tools decline ID     delete a waiting tool
//! columbus-mcp tools remove ID      remove an installed tool
//! columbus-mcp --version
//! ```

mod approval_flow;
mod approvals_page;
mod companion;
mod config;
mod context;
mod edit_tools;
mod export_tools;
mod fetch_tools;
mod gis_tools;
mod notes;
mod personal_tools;
mod plugin_run;
mod plugins;
mod project_db;
mod propose_tool;
mod providers;
mod raster_tools;
mod registry_cli;
mod registry_tools;
mod review_cli;
mod safefs;
mod script_tools;
mod secrets;
mod server;
mod snapshot_tools;
mod worker;
mod workflows;

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
        ["workflow", "run", id] => std::process::exit(workflows::run_cli(id, false).await?),
        ["workflow", "run", id, "--scheduled"] => {
            std::process::exit(workflows::run_cli(id, true).await?)
        }
        // The scheduler entry the app writes names the engine it chose.
        ["workflow", "run", id, "--scheduled", "--python", py] => {
            std::process::exit(workflows::run_cli_with(id, true, Some(py)).await?)
        }
        ["workflow", "validate", file] => workflows::validate_cli(std::path::Path::new(file)),
        ["workflow", "list"] => {
            workflows::list_cli();
            Ok(())
        }
        ["tools", "list"] => review_cli::tools_list(),
        ["tools", "show", id] => review_cli::tools_show(id),
        ["tools", "install", id] => review_cli::tools_install(id),
        ["tools", "decline", id] => review_cli::tools_decline(id),
        ["tools", "remove", id] => review_cli::tools_remove(id),
        ["servers", "list"] => review_cli::list().await,
        ["servers", "review", id] => review_cli::review(id).await.map(|_| ()),
        ["servers", "approve", id] => review_cli::approve(id, false).await,
        ["servers", "approve", id, "--accept-flags"] => review_cli::approve(id, true).await,
        ["servers", "signin", id] => review_cli::signin(id).await,
        ["registry", "update"] => registry_tools::update(None).await,
        ["registry", "query", request] => registry_cli::query(request).await,
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
            "usage: columbus-mcp [serve] | audit verify FILE | secret set|delete ID | servers list|review|approve|signin|signout ID | registry update [--from DIR|URL]|query JSON | plugins list|approve ID | tools list|show|install|decline|remove ID | workflow run ID [--scheduled]|validate FILE|list | --version"
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
    // Other processes of the same user (a confined server or script among
    // them) must not read this one's memory or environment through /proc.
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let paths = config::Paths::from_env();
    let mut state = server::State::load(paths)?;
    // A session for a project: only the connections it lists (it can only
    // narrow what the person has), and its instructions for the assistant.
    if let Some(id) = std::env::var("COLUMBUS_PROJECT")
        .ok()
        .filter(|v| !v.is_empty())
    {
        let p = columbus_shared::projects::load(&id).map_err(anyhow::Error::msg)?;
        if !p.connections.is_empty() {
            state.scope.connections = p.connections.clone();
        }
        // Its data groups: then its chats see only their datasets.
        if !p.data_groups.is_empty() {
            state.scope.data_groups =
                context::project_groups(&state, &p.data_groups).map_err(anyhow::Error::msg)?;
        }
        state.project = Some(p);
    }
    tracing::info!(session = %state.session, workspace = %state.sandbox.root().display(), "starting");
    let gateway = server::Gateway::new(state);
    gateway.start_companion().await;
    let service = gateway.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
