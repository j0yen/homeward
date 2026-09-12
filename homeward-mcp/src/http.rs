//! Streamable-HTTP transport wiring.
//!
//! Mirrors `mcphost`'s own `http.rs` (same `rmcp` version, same
//! streamable-HTTP service construction) -- that is this fleet's one
//! proven wiring for this SDK, and AC6 requires the same tool calls to
//! succeed over HTTP as over stdio, which this crate's own
//! `ServerHandler` impl (`src/server.rs`) already guarantees regardless of
//! transport.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::server::HomewardMcpServer;

async fn healthz() -> &'static str {
    "ok"
}

/// Build the axum router: `GET /healthz` plus the `POST /mcp`
/// streamable-HTTP MCP endpoint.
pub fn build_router(server: HomewardMcpServer) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .disable_allowed_hosts();

    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    Router::new()
        .route("/healthz", get(healthz))
        .route_service("/mcp", service)
}
