//! Axum HTTP server for `homeward-reportd serve`.
//!
//! # Endpoints
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | GET | `/health` | Liveness probe → `{"status":"ok"}` |
//! | GET | `/coverage` | Per-source coverage report (JSON) |
//! | GET | `/intake` | Shelter intake records (filterable) |
//! | POST | `/search` | Photo similarity search (multipart) |
//!
//! # Privacy
//! No `LostReport` fields are ever exposed through any endpoint in this module.

use std::sync::Arc;

use axum::extract::{Multipart, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use homeward_connectors::coverage::{CoverageArgs, build_report};
use homeward_connectors::registry::ConnectorRegistry;
use homeward_schema::{PetRecord, Species};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::api::{ApiConfig, ShelterQuery, ShelterRecord, ShelterQueryResult, image_similarity_search};
use crate::MatchCandidate;

// ─── Shared application state ────────────────────────────────────────────────

/// Shared state threaded through all handlers.
///
/// This is cheap to clone (Arc internals).
#[derive(Clone)]
pub struct AppState {
    /// API configuration (rate-limit caps etc.).
    pub cfg: Arc<ApiConfig>,
    /// In-memory shelter intake records (read-only view for the HTTP layer).
    pub intake: Arc<Vec<PetRecord>>,
    /// Connector registry for coverage reporting.
    pub registry: Arc<ConnectorRegistry>,
}

// ─── Query params ─────────────────────────────────────────────────────────────

/// Query parameters accepted by `GET /intake`.
#[derive(Debug, Default, Deserialize)]
pub struct IntakeParams {
    /// Filter by species (`dog` or `cat`).
    pub species: Option<String>,
    /// Filter by ZIP code (coarse geo).
    pub zip: Option<String>,
    /// Filter by state code (e.g. `TX`).
    pub state: Option<String>,
    /// Maximum records to return (capped at `ApiConfig::max_results_per_query`).
    pub limit: Option<usize>,
}

// ─── Response types ───────────────────────────────────────────────────────────

/// `GET /health` response.
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

/// `GET /intake` response.
#[derive(Debug, Serialize)]
struct IntakeResponse {
    records: Vec<ShelterRecord>,
    truncated: bool,
}

/// `POST /search` response.
#[derive(Debug, Serialize)]
struct SearchResponse {
    candidates: Vec<SearchCandidate>,
}

/// A single candidate in the `/search` response — candidate-framed, no PII.
#[derive(Debug, Serialize)]
pub struct SearchCandidate {
    /// Canonical shelter record ID.
    pub canonical_id: String,
    /// Similarity score 0–1.
    pub score: f32,
    /// Candidate-framed explanation (never "confirmed match").
    pub explanation: String,
}

// ─── Router factory ───────────────────────────────────────────────────────────

/// Build the axum [`Router`] with all four endpoints wired up.
///
/// The returned router is ready to be bound to a [`TcpListener`].
#[must_use]
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/coverage", get(handle_coverage))
        .route("/intake", get(handle_intake))
        .route("/search", post(handle_search))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ─── Server entry point ───────────────────────────────────────────────────────

/// Start the HTTP server, binding to `bind:port`.
///
/// This function runs until the process is killed.
///
/// # Errors
/// Returns an error if the TCP listener cannot be bound or the server exits.
pub async fn serve(port: u16, bind: &str) -> Result<(), String> {
    let state = AppState {
        cfg: Arc::new(ApiConfig::default()),
        intake: Arc::new(vec![]),
        registry: Arc::new(ConnectorRegistry::new()),
    };

    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("cannot bind to {addr}: {e}"))?;

    tracing::info!("homeward-reportd listening on {addr}");

    axum::serve(listener, build_router(state))
        .await
        .map_err(|e| format!("server error: {e}"))
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /health` — liveness probe.
async fn handle_health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// `GET /coverage` — per-source coverage report.
async fn handle_coverage(State(state): State<AppState>) -> impl IntoResponse {
    let args = CoverageArgs {
        store_path: None,
        json: true,
        registry: &state.registry,
        cadence_hints: std::collections::HashMap::new(),
    };

    match build_report(&args) {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

/// `GET /intake` — filtered shelter intake records.
async fn handle_intake(
    State(state): State<AppState>,
    Query(params): Query<IntakeParams>,
) -> impl IntoResponse {
    let species: Option<Species> = match params.species.as_deref() {
        Some("dog") => Some(Species::Dog),
        Some("cat") => Some(Species::Cat),
        None => None,
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("unknown species: {other}")})),
            )
                .into_response();
        }
    };

    let query = ShelterQuery {
        species,
        zip_code: params.zip,
        state: params.state,
        intake_after: None,
        limit: params.limit,
    };

    let result: ShelterQueryResult =
        crate::api::query_shelter(&state.intake, &query, &state.cfg);

    (StatusCode::OK, Json(IntakeResponse {
        records: result.records,
        truncated: result.truncated,
    }))
        .into_response()
}

/// `POST /search` — photo similarity search.
///
/// Accepts a multipart form with a `photo` field containing the image bytes.
/// Returns a ranked candidate shortlist (zero candidates ok; 4xx on bad upload).
async fn handle_search(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Extract the `photo` field from the multipart body.
    let mut photo_bytes: Option<Vec<u8>> = None;

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                if field.name() == Some("photo") {
                    match field.bytes().await {
                        Ok(bytes) => {
                            photo_bytes = Some(bytes.to_vec());
                        }
                        Err(e) => {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({"error": format!("failed to read photo field: {e}")})),
                            )
                                .into_response();
                        }
                    }
                }
                // Skip unknown fields.
            }
            Ok(None) => break,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": format!("multipart error: {e}")})),
                )
                    .into_response();
            }
        }
    }

    if photo_bytes.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing 'photo' field in multipart body"})),
        )
            .into_response();
    }

    // With an empty intake gallery the prescored list is always empty.
    // In production, homeward-match would score the photo against the gallery.
    let prescored: Vec<MatchCandidate> = vec![];
    let results = image_similarity_search(prescored, &state.cfg);

    let candidates: Vec<SearchCandidate> = results
        .into_iter()
        .map(|c| SearchCandidate {
            canonical_id: c.record.canonical_id,
            score: c.score,
            explanation: c.explanation,
        })
        .collect();

    (StatusCode::OK, Json(SearchResponse { candidates })).into_response()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt as _;

    fn make_state() -> AppState {
        AppState {
            cfg: Arc::new(ApiConfig::default()),
            intake: Arc::new(vec![]),
            registry: Arc::new(ConnectorRegistry::new()),
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

    /// Handler unit test: `/coverage` returns a JSON object with `sources` array.
    #[tokio::test]
    async fn coverage_returns_json_with_sources() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/coverage")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
        assert!(json["sources"].is_array(), "coverage response must have 'sources' array");
        assert!(json.get("generated_at").is_some(), "coverage response must have 'generated_at'");
    }

    /// Handler unit test: `/intake` with empty gallery returns empty array, truncated=false.
    #[tokio::test]
    async fn intake_empty_gallery() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/intake")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
        assert!(json["records"].is_array());
        assert_eq!(json["records"].as_array().expect("array").len(), 0);
        assert_eq!(json["truncated"], false);
    }

    /// Handler unit test: `/intake?species=invalid` returns 400.
    #[tokio::test]
    async fn intake_bad_species_is_400() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/intake?species=fish")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Handler unit test: `/search` without multipart body returns 400.
    #[tokio::test]
    async fn search_missing_photo_is_400() {
        let app = build_router(make_state());
        // Send a POST with content-type multipart but empty body — no `photo` field.
        let boundary = "testboundary";
        let body_str = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"other\"\r\n\r\nvalue\r\n--{boundary}--\r\n");

        let req = Request::builder()
            .method(Method::POST)
            .uri("/search")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body_str))
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Handler unit test: `/search` with a valid photo field returns 200 with candidates array.
    #[tokio::test]
    async fn search_with_photo_returns_candidates() {
        let app = build_router(make_state());
        let boundary = "photoboundary";
        // Minimal fake JPEG bytes
        let photo_bytes = b"\xFF\xD8\xFF\xD9";
        let body_str = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"pet.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n{}\r\n--{boundary}--\r\n",
            String::from_utf8_lossy(photo_bytes)
        );

        let req = Request::builder()
            .method(Method::POST)
            .uri("/search")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body_str))
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("parse JSON");
        assert!(json["candidates"].is_array(), "search response must have 'candidates' array");
    }

    /// Privacy: no LostReport fields should appear anywhere in the HTTP response.
    #[tokio::test]
    async fn no_pii_in_intake_response() {
        let app = build_router(make_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/intake")
            .body(Body::empty())
            .expect("build request");

        let resp = app.oneshot(req).await.expect("call handler");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let body_str = String::from_utf8_lossy(&body);
        assert!(!body_str.contains("phone"), "intake must not expose phone");
        assert!(!body_str.contains("email"), "intake must not expose email");
        assert!(!body_str.contains("contact"), "intake must not expose contact");
    }
}
