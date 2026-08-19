//! Axum HTTP server for `homeward-walld`.
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/` | The wall UI (embedded, single-file, no build step) |
//! | GET | `/health` | Liveness probe → `{"status":"ok"}` |
//! | GET | `/api/buddies` | Cursor-paginated photo-bearing records |
//! | GET | `/api/stream` | Server-Sent Events of newly ingested buddies |
//! | GET | `/api/stats` | Aggregate counts |
//!
//! Axum 0.7 is pinned across this workspace, so any path-param routes use
//! `:id` syntax — this crate has no path params today, but new routes must
//! keep that in mind rather than the 0.8 `{id}` form.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::wall_db::{self, Buddy, BuddyPage, WallStats};

/// The single-file wall UI served at `GET /`.
const INDEX_HTML: &str = include_str!("../static/index.html");

/// Default poll interval for new-buddy detection (20 seconds per spec).
/// Overridable via `HOMEWARD_WALL_POLL_MS` in the `homeward-walld` binary.
pub const DEFAULT_POLL_MS: u64 = 20_000;

/// SSE keep-alive comment interval (25 seconds per spec).
pub const KEEPALIVE_SECS: u64 = 25;

/// Shared application state threaded through all handlers.
///
/// Cheap to clone — `db_path` is behind an `Arc` and `broadcaster` is a
/// `tokio::sync::broadcast::Sender`, itself cheap to clone.
#[derive(Clone)]
pub struct AppState {
    /// Path to the read-only ingest DB.
    pub db_path: Arc<PathBuf>,
    /// Broadcast channel fed by [`crate::stream::run_poller`]; each `/api/stream`
    /// connection subscribes its own receiver.
    pub broadcaster: broadcast::Sender<Buddy>,
}

/// Query parameters accepted by `GET /api/buddies`.
#[derive(Debug, Default, Deserialize)]
pub struct BuddiesParams {
    /// Cursor: return buddies with `canonical_id` strictly less than this.
    pub before: Option<String>,
    /// Page size, clamped to `[1, `[`wall_db::MAX_PAGE_LIMIT`]`]`; defaults to
    /// [`wall_db::DEFAULT_PAGE_LIMIT`].
    pub limit: Option<i64>,
}

/// Build the axum [`Router`] with all endpoints wired up.
#[must_use]
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/health", get(handle_health))
        .route("/api/buddies", get(handle_buddies))
        .route("/api/stream", get(handle_stream))
        .route("/api/stats", get(handle_stats))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// `GET /` — embedded single-page wall UI.
async fn serve_index() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

/// `GET /health` — liveness probe.
async fn handle_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// `GET /api/buddies?before=<canonical_id>&limit=<n>` — cursor-paginated
/// photo-bearing records, newest first.
///
/// Degrades to an empty page (still `200 OK`) if the DB query fails, so a
/// transient lock or a not-yet-created DB does not break the page — it just
/// shows nothing yet.
async fn handle_buddies(
    State(state): State<AppState>,
    Query(params): Query<BuddiesParams>,
) -> impl IntoResponse {
    let db_path = Arc::clone(&state.db_path);
    let before = params.before;
    let limit = params.limit.unwrap_or(wall_db::DEFAULT_PAGE_LIMIT);

    let result = tokio::task::spawn_blocking(move || {
        let conn = wall_db::open_readonly(&db_path)?;
        wall_db::buddies_page(&conn, before.as_deref(), limit)
    })
    .await;

    match result {
        Ok(Ok(page)) => (StatusCode::OK, Json(page)).into_response(),
        Ok(Err(e)) => {
            tracing::warn!("/api/buddies query failed: {e}");
            (
                StatusCode::OK,
                Json(BuddyPage {
                    items: vec![],
                    next_cursor: None,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("/api/buddies blocking task panicked: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}

/// `GET /api/stats` — aggregate counts.
async fn handle_stats(State(state): State<AppState>) -> impl IntoResponse {
    let db_path = Arc::clone(&state.db_path);
    let result = tokio::task::spawn_blocking(move || {
        let conn = wall_db::open_readonly(&db_path)?;
        wall_db::stats(&conn)
    })
    .await;

    match result {
        Ok(Ok(stats)) => (StatusCode::OK, Json(stats)).into_response(),
        Ok(Err(e)) => {
            tracing::warn!("/api/stats query failed: {e}");
            (
                StatusCode::OK,
                Json(WallStats {
                    total: 0,
                    photo_bearing: 0,
                    by_species: wall_db::SpeciesCounts { dog: 0, cat: 0 },
                    newest_created_at: None,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("/api/stats blocking task panicked: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}

/// `GET /api/stream` — Server-Sent Events of newly ingested photo-bearing
/// buddies, fed by [`crate::stream::run_poller`]. Sends a keep-alive comment
/// every [`KEEPALIVE_SECS`] seconds so idle proxies do not time the
/// connection out.
async fn handle_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(buddy) => {
            let json = serde_json::to_string(&buddy).unwrap_or_else(|e| {
                tracing::warn!("failed to serialize buddy for SSE: {e}");
                "{}".to_owned()
            });
            Some(Ok(Event::default().event("buddy").data(json)))
        }
        // A slow/disconnected-then-reconnected client missed some messages.
        // Drop the gap silently — the client's next /api/buddies page load
        // (on reconnect) will catch it up; we don't replay history over SSE.
        Err(_lagged) => None,
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(KEEPALIVE_SECS))
            .text("keep-alive"),
    )
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt as _;

    fn make_state() -> AppState {
        let (tx, _rx) = broadcast::channel(16);
        AppState {
            db_path: Arc::new(PathBuf::from("/nonexistent/for/tests.db")),
            broadcaster: tx,
        }
    }

    /// Handler unit test: `/health` returns `{"status":"ok"}` and HTTP 200.
    #[tokio::test]
    async fn health_returns_ok() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/health")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
        assert_eq!(json["status"], "ok");
    }

    /// Handler unit test: `GET /` returns HTTP 200 text/html containing the page title.
    #[tokio::test]
    async fn index_route_returns_html() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/html"));

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body_str = String::from_utf8_lossy(&body);
        assert!(body_str.contains("Paws"), "index page must mention Paws & Petals");
    }

    /// Handler unit test: `/api/buddies` degrades to an empty page (still 200)
    /// when the underlying DB does not exist.
    #[tokio::test]
    async fn buddies_missing_db_degrades_to_empty_page() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/buddies")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
        assert!(json["items"].is_array());
        assert_eq!(json["items"].as_array().expect("array").len(), 0);
    }

    /// Handler unit test: `/api/stats` degrades gracefully (still 200) when
    /// the underlying DB does not exist.
    #[tokio::test]
    async fn stats_missing_db_degrades_gracefully() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/stats")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
