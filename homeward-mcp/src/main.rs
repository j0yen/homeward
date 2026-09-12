//! `homeward-mcp` -- read-only MCP server over the homeward shelter database.
//!
//! ```text
//! homeward-mcp serve                 # stdio (default, for local MCP clients)
//! homeward-mcp serve --http :8095    # streamable HTTP
//! ```

use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use homeward_mcp::HomewardMcpServer;
use homeward_mcp::http::build_router;

/// Read-only MCP server over the homeward shelter database.
#[derive(Debug, Parser)]
#[command(name = "homeward-mcp", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the server.
    Serve {
        /// Bind address for streamable HTTP, e.g. ":8095" or "0.0.0.0:8095".
        /// When omitted, serves stdio only.
        #[arg(long)]
        http: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve { http: Some(addr) } => run_http(&addr).await,
        Command::Serve { http: None } => run_stdio().await,
    }
}

async fn run_stdio() -> anyhow::Result<()> {
    let server = HomewardMcpServer::from_env();
    let running = server.serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

async fn run_http(addr: &str) -> anyhow::Result<()> {
    let bind_addr = normalize_addr(addr);
    let server = HomewardMcpServer::from_env();
    let router = build_router(server);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(%bind_addr, "homeward-mcp listening");
    axum::serve(listener, router).await?;
    Ok(())
}

/// Accept both "8095" / ":8095" (bind all interfaces) and "host:port" forms.
#[allow(clippy::option_if_let_else)]
fn normalize_addr(addr: &str) -> String {
    if let Some(port) = addr.strip_prefix(':') {
        format!("0.0.0.0:{port}")
    } else if !addr.is_empty() && addr.chars().all(|c| c.is_ascii_digit()) {
        format!("0.0.0.0:{addr}")
    } else {
        addr.to_owned()
    }
}
