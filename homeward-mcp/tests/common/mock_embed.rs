//! A minimal mock of the `homeward-embed` sidecar's `/query` + `/health`
//! endpoints, for `match_photo` acceptance tests that need a controllable
//! embed response without depending on the real Python DINOv2 service.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// A running mock embed sidecar. Dropping this stops accepting new
/// connections once the owning test ends (the spawned task is aborted).
pub struct MockEmbedServer {
    pub addr: SocketAddr,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for MockEmbedServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl MockEmbedServer {
    /// Start a mock sidecar whose `/query` always returns `matches`
    /// (canonical_id, score) in the given order, regardless of the request
    /// body.
    pub async fn start(matches: Vec<(String, f64)>) -> Self {
        let state = Arc::new(matches);
        let app = Router::new()
            .route("/query", post(query_handler))
            .route("/health", get(|| async { "ok" }))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock embed sidecar");
        let addr = listener.local_addr().expect("mock embed local addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, handle }
    }

    /// Reserve a `127.0.0.1:PORT` address and immediately release it, so
    /// nothing is listening there -- used to simulate a stopped/unreachable
    /// embed sidecar (AC4) without depending on a fixed "surely unused"
    /// port number.
    pub async fn closed_port() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port to reserve");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);
        addr
    }
}

async fn query_handler(
    State(matches): State<Arc<Vec<(String, f64)>>>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let items: Vec<Value> = matches
        .iter()
        .map(|(id, score)| json!({ "canonical_id": id, "score": score }))
        .collect();
    Json(json!({ "matches": items }))
}
