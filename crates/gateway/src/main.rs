//! `columbus-mcp`: the governed GIS MCP server.
//!
//! ```text
//! columbus-mcp                      serve MCP over stdio (default)
//! columbus-mcp serve                same
//! columbus-mcp audit verify FILE    check a month's audit log hash chain
//! columbus-mcp --version
//! ```

mod config;
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
        ["--version"] | ["-V"] => {
            println!("{} {}", server::SERVER_NAME, env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => bail!("usage: columbus-mcp [serve] | audit verify FILE | --version"),
    }
}

async fn serve() -> Result<()> {
    let paths = config::Paths::from_env();
    let state = server::State::load(paths)?;
    tracing::info!(session = %state.session, workspace = %state.sandbox.root().display(), "starting");
    let service = server::Gateway::new(state).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
