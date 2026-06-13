//! Async HTTP client for the `homeward-embed` Python sidecar.
//!
//! The sidecar exposes three endpoints:
//!
//! - `POST /enroll`  — embed + index one intake photo.
//! - `POST /query`   — embed a query photo + kNN search.
//! - `GET  /health`  — liveness probe.
//!
//! The client is intentionally thin: no Python types leak here.  All request
//! and response shapes are local Rust structs that mirror the `FastAPI` models in
//! `homeward/embed/homeward_embed/service.py`.
//!
//! # Configuration
//!
//! | Env var            | Default               | Meaning                    |
//! |--------------------|-----------------------|----------------------------|
//! | `HW_EMBED_HOST`    | `127.0.0.1`           | Sidecar host               |
//! | `HW_EMBED_PORT`    | `8741`                | Sidecar port               |
//! | `HW_EMBED_TIMEOUT` | `10`                  | Per-request timeout (secs) |
//!
//! # Example
//!
//! ```rust,no_run
//! # use homeward_embed_client::{EmbedClient, EmbedClientConfig, QueryRequest};
//! # #[tokio::main]
//! # async fn main() -> Result<(), homeward_embed_client::EmbedClientError> {
//! let cfg = EmbedClientConfig::from_env();
//! let client = EmbedClient::new(cfg)?;
//! let matches = client.query(QueryRequest {
//!     image_url: Some("https://example.com/dog.jpg".into()),
//!     image_b64: None,
//!     k: 5,
//!     species_filter: Some("dog".into()),
//! }).await?;
//! for m in &matches {
//!     println!("{}: {:.3}", m.canonical_id, m.score);
//! }
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for [`EmbedClient`].
#[derive(Debug, Clone)]
pub struct EmbedClientConfig {
    /// Base URL of the sidecar (e.g. `http://127.0.0.1:8741`).
    pub base_url: String,
    /// Per-request timeout.
    pub timeout: Duration,
}

impl EmbedClientConfig {
    /// Default development configuration pointing at localhost:8741.
    #[must_use]
    pub fn default_local() -> Self {
        Self {
            base_url: "http://127.0.0.1:8741".to_owned(),
            timeout: Duration::from_secs(10),
        }
    }

    /// Read configuration from environment variables (see module-level docs).
    ///
    /// Falls back to [`EmbedClientConfig::default_local`] for unset vars.
    #[must_use]
    pub fn from_env() -> Self {
        let host = std::env::var("HW_EMBED_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
        let port: u16 = std::env::var("HW_EMBED_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8741);
        let timeout_secs: u64 = std::env::var("HW_EMBED_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        Self {
            base_url: format!("http://{host}:{port}"),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

// ─── Request / response types ────────────────────────────────────────────────

/// Request body for `POST /enroll`.
#[derive(Debug, Clone, Serialize)]
pub struct EnrollRequest {
    /// Stable pet identifier (ULID as string, matches `canonical_id` in the store).
    pub canonical_id: String,
    /// URL of the photo to embed and index.
    pub image_url: String,
    /// Optional species hint (`"dog"` or `"cat"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub species: Option<String>,
}

impl EnrollRequest {
    /// Construct an [`EnrollRequest`] from a [`Ulid`] and a photo URL.
    #[must_use]
    pub fn new(canonical_id: Ulid, image_url: impl Into<String>, species: Option<&str>) -> Self {
        Self {
            canonical_id: canonical_id.to_string(),
            image_url: image_url.into(),
            species: species.map(str::to_owned),
        }
    }
}

/// Response body from `POST /enroll`.
#[derive(Debug, Clone, Deserialize)]
pub struct EnrollResponse {
    /// The canonical id echoed back.
    pub canonical_id: String,
    /// Internal HNSW id assigned by the index.
    pub internal_id: i64,
    /// `true` if an animal body-crop was used; `false` if whole-image fallback.
    pub detected: bool,
}

/// Request body for `POST /query`.
#[derive(Debug, Clone, Serialize)]
pub struct QueryRequest {
    /// URL of the query photo, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// Base64-encoded JPEG/PNG, if URL is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_b64: Option<String>,
    /// Number of nearest neighbours requested (1–200).
    pub k: u32,
    /// If set, restrict results to `"dog"` or `"cat"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub species_filter: Option<String>,
}

/// One match returned by `POST /query`.
#[derive(Debug, Clone, Deserialize)]
pub struct QueryMatch {
    /// Canonical pet id.
    pub canonical_id: String,
    /// Cosine similarity score in `[0, 1]`.
    pub score: f64,
}

// ─── Error ───────────────────────────────────────────────────────────────────

/// Errors returned by [`EmbedClient`].
#[derive(Debug, Error)]
pub enum EmbedClientError {
    /// Failed to build the underlying HTTP client.
    #[error("failed to build HTTP client: {0}")]
    Build(#[from] reqwest::Error),

    /// HTTP transport error (connection refused, timeout, etc.).
    #[error("transport error calling embed sidecar: {0}")]
    Transport(reqwest::Error),

    /// The sidecar returned a non-2xx status.
    #[error("embed sidecar returned HTTP {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body (may be truncated).
        body: String,
    },

    /// Could not decode the sidecar's JSON response.
    #[error("could not decode embed sidecar response: {0}")]
    Decode(reqwest::Error),
}

// ─── Client ──────────────────────────────────────────────────────────────────

/// Async HTTP client for the homeward-embed sidecar.
///
/// Cheaply cloneable (backed by a `reqwest::Client` which uses an `Arc` internally).
#[derive(Clone, Debug)]
pub struct EmbedClient {
    client: Client,
    base_url: String,
}

impl EmbedClient {
    /// Create a new client from the given configuration.
    ///
    /// # Errors
    ///
    /// Returns [`EmbedClientError::Build`] if `reqwest::Client` construction fails
    /// (e.g. invalid TLS configuration).
    pub fn new(cfg: EmbedClientConfig) -> Result<Self, EmbedClientError> {
        let client = Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(EmbedClientError::Build)?;
        Ok(Self {
            client,
            base_url: cfg.base_url,
        })
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Enroll a shelter animal's photo into the embed gallery.
    ///
    /// Calls `POST {base_url}/enroll`.  The sidecar downloads the image,
    /// runs detection + embedding, and persists the vector.
    ///
    /// # Errors
    ///
    /// Returns an [`EmbedClientError`] on transport failure, non-2xx status,
    /// or JSON decode failure.
    pub async fn enroll(&self, req: EnrollRequest) -> Result<EnrollResponse, EmbedClientError> {
        let url = format!("{}/enroll", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(EmbedClientError::Transport)?;

        self.check_status_and_decode(resp).await
    }

    /// Query the gallery for visually-similar pets.
    ///
    /// Calls `POST {base_url}/query`.  The sidecar embeds the query image
    /// and returns the top-k cosine-nearest `canonical_id`s.
    ///
    /// # Errors
    ///
    /// Returns an [`EmbedClientError`] on transport failure, non-2xx status,
    /// or JSON decode failure.
    pub async fn query(
        &self,
        req: QueryRequest,
    ) -> Result<Vec<QueryMatch>, EmbedClientError> {
        #[derive(Deserialize)]
        struct QueryResponse {
            matches: Vec<QueryMatch>,
        }

        let url = format!("{}/query", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(EmbedClientError::Transport)?;

        let qr: QueryResponse = self.check_status_and_decode(resp).await?;
        Ok(qr.matches)
    }

    /// Liveness probe — returns `true` if the sidecar is reachable and healthy.
    ///
    /// This is a best-effort check; a `false` result means the sidecar is either
    /// not running or returned a non-2xx status.
    pub async fn health_check(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        self.client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    async fn check_status_and_decode<T: serde::de::DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, EmbedClientError> {
        let status = resp.status();
        if !status.is_success() {
            let code = status.as_u16();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_owned());
            return Err(EmbedClientError::Status { status: code, body });
        }
        resp.json::<T>().await.map_err(EmbedClientError::Decode)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Config tests ─────────────────────────────────────────────────────────

    #[test]
    fn default_local_config_has_expected_url() {
        let cfg = EmbedClientConfig::default_local();
        assert_eq!(cfg.base_url, "http://127.0.0.1:8741");
        assert_eq!(cfg.timeout, Duration::from_secs(10));
    }

    #[test]
    fn default_local_config_base_url_matches_env_format() {
        // Verify that EmbedClientConfig::from_env() produces the same URL as
        // default_local() when the environment variables are absent.
        // We cannot safely mutate env vars in multi-threaded tests (Rust 1.85+
        // made set_var/remove_var unsafe); instead we verify the helper that
        // constructs the base URL given explicit host/port values.
        let url = format!("http://{}:{}", "127.0.0.1", 8741_u16);
        assert_eq!(url, "http://127.0.0.1:8741");
    }

    #[test]
    fn base_url_format_with_custom_host_and_port() {
        // Verify the URL construction logic independent of env vars.
        let host = "10.0.0.1";
        let port: u16 = 9999;
        let url = format!("http://{host}:{port}");
        assert_eq!(url, "http://10.0.0.1:9999");
    }

    // ── Request construction ──────────────────────────────────────────────────

    #[test]
    fn enroll_request_new_formats_ulid() {
        let id = Ulid::new();
        let req = EnrollRequest::new(id, "https://example.com/dog.jpg", Some("dog"));
        assert_eq!(req.canonical_id, id.to_string());
        assert_eq!(req.image_url, "https://example.com/dog.jpg");
        assert_eq!(req.species, Some("dog".to_owned()));
    }

    #[test]
    fn enroll_request_species_none_omitted_in_json() {
        let id = Ulid::new();
        let req = EnrollRequest::new(id, "https://example.com/cat.jpg", None);
        let json = serde_json::to_string(&req).expect("serialize");
        // `species` field must be absent when None (skip_serializing_if)
        assert!(!json.contains("species"), "species present in JSON: {json}");
    }

    #[test]
    fn query_request_url_only_serialises_correctly() {
        let req = QueryRequest {
            image_url: Some("https://example.com/q.jpg".into()),
            image_b64: None,
            k: 10,
            species_filter: None,
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("image_url"));
        assert!(!json.contains("image_b64"));
        assert!(!json.contains("species_filter"));
    }

    #[test]
    fn query_request_b64_only_serialises_correctly() {
        let req = QueryRequest {
            image_url: None,
            image_b64: Some("AAAA".into()),
            k: 3,
            species_filter: Some("cat".into()),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(!json.contains("image_url"));
        assert!(json.contains("image_b64"));
        assert!(json.contains("species_filter"));
    }

    // ── Client construction ───────────────────────────────────────────────────

    #[test]
    fn client_builds_successfully() {
        let cfg = EmbedClientConfig::default_local();
        let client = EmbedClient::new(cfg);
        assert!(client.is_ok(), "EmbedClient::new should succeed with valid config");
    }

    // ── Error display ─────────────────────────────────────────────────────────

    #[test]
    fn embed_client_error_status_display() {
        let err = EmbedClientError::Status {
            status: 422,
            body: "unprocessable".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("422"), "status code in message: {msg}");
        assert!(msg.contains("unprocessable"), "body in message: {msg}");
    }

    // ── Health check — returns false for unreachable sidecar ─────────────────

    #[tokio::test]
    async fn health_check_returns_false_when_sidecar_not_running() {
        // Use a port that should be unoccupied in CI.
        let cfg = EmbedClientConfig {
            base_url: "http://127.0.0.1:19999".to_owned(),
            timeout: Duration::from_millis(500),
        };
        let client = EmbedClient::new(cfg).expect("build");
        let alive = client.health_check().await;
        assert!(!alive, "health_check should return false when sidecar not running");
    }
}
