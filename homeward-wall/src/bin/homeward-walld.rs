//! `homeward-walld` — the "Paws & Petals" public wall server.
//!
//! Env vars:
//! - `HOMEWARD_WALL_PORT` — bind port (default `8090`).
//! - `HOMEWARD_WALL_DB` — path to the ingest DB; falls back to
//!   `HOMEWARD_INGEST_DB`, then `$HOME/.local/share/homeward/homeward-ingest.db`.
//! - `HOMEWARD_WALL_POLL_MS` — SSE poll interval override in milliseconds
//!   (default `20000`, i.e. 20 seconds).
//!
//! Opens the ingest DB strictly read-only (see [`homeward_wall::wall_db::open_readonly`])
//! and never writes to it.

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use std::time::Duration;

use homeward_wall::server::{self, AppState, build_router};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Broadcast channel capacity — generous relative to the 20s poll cadence
/// and typical burst sizes, so a momentarily slow subscriber lags rather
/// than blocks the poller.
const BROADCAST_CAPACITY: usize = 256;

fn resolve_db_path() -> PathBuf {
    std::env::var("HOMEWARD_WALL_DB")
        .or_else(|_| std::env::var("HOMEWARD_INGEST_DB"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
            PathBuf::from(home).join(".local/share/homeward/homeward-ingest.db")
        })
}

fn resolve_port() -> u16 {
    std::env::var("HOMEWARD_WALL_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8090)
}

fn resolve_poll_interval() -> Duration {
    std::env::var("HOMEWARD_WALL_POLL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(Duration::from_millis(server::DEFAULT_POLL_MS), Duration::from_millis)
}

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let db_path = resolve_db_path();
    let port = resolve_port();
    let poll_interval = resolve_poll_interval();

    tracing::info!(
        "homeward-walld: db={} port={port} poll_interval={poll_interval:?}",
        db_path.display()
    );

    let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);

    let poller_db = db_path.clone();
    let poller_tx = tx.clone();
    tokio::spawn(async move {
        homeward_wall::stream::run_poller(poller_db, poller_tx, poll_interval).await;
    });

    let state = AppState {
        db_path: Arc::new(db_path),
        broadcaster: tx,
    };

    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind to {addr}: {e}");
            process::exit(1);
        }
    };

    println!("homeward-walld: listening on {addr}");
    if let Err(e) = axum::serve(listener, build_router(state)).await {
        eprintln!("error: server exited: {e}");
        process::exit(1);
    }
}
