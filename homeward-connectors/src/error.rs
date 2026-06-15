//! Error types for the connector framework.

use thiserror::Error;

/// Errors that can arise during a connector poll.
#[derive(Debug, Error)]
pub enum ConnectorError {
    /// HTTP transport error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// The source returned an unexpected response status.
    #[error("unexpected HTTP status {status}: {body}")]
    UnexpectedStatus {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated).
        body: String,
    },

    /// JSON deserialization failed.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// A required field was missing from the source response.
    #[error("missing required field: {field} in record {context}")]
    MissingField {
        /// Name of the missing field.
        field: &'static str,
        /// Context string (e.g. resource ID) for diagnosis.
        context: String,
    },

    /// Species value from source could not be mapped.
    #[error("unknown species: {0:?}")]
    UnknownSpecies(String),

    /// Rate-limited after all retries exhausted.
    #[error("rate-limited: all retries exhausted for {host}")]
    RateLimitExhausted {
        /// The host that rate-limited us.
        host: String,
    },

    /// The source's robots.txt disallows our path.
    #[error("robots.txt disallows path {path} for {user_agent}")]
    RobotsDisallowed {
        /// The disallowed path.
        path: String,
        /// The User-Agent used.
        user_agent: String,
    },

    /// Connector name not found in the registry.
    #[error("unknown connector: {0:?}")]
    UnknownConnector(String),

    /// URL parsing error.
    #[error("URL error: {0}")]
    Url(#[from] url::ParseError),

    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// The source family named by this config entry has no built connector yet.
    ///
    /// The inner `String` is the source name (not the family name) to give the
    /// operator a clear pointer to the offending catalog entry.
    #[error("connector family not yet built for source {0:?}")]
    FamilyNotBuilt(String),
}
