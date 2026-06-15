//! Webhook delivery sink for owner match-alert notifications.
//!
//! When a [`crate::alerts::MatchAlert`] is produced for a [`LostReport`] that
//! has `notify_url` set, [`WebhookSink::fire`] fires a best-effort HTTP POST
//! to that URL carrying the alert as JSON.
//!
//! Delivery contract (AC2/AC3):
//! - Fire-and-forget: the result is never awaited by the match-watch loop.
//! - Non-2xx responses and network errors are logged at WARN; no retry.
//! - A failed webhook delivery does NOT block the match-watch loop.

use reqwest::Client;
use tracing::warn;

use crate::alerts::MatchAlert;

/// Sink that POSTs [`MatchAlert`] payloads to owner-supplied webhook URLs.
///
/// Cheaply cloneable (wraps an `Arc<reqwest::Client>` internally).
#[derive(Debug, Clone)]
pub struct WebhookSink {
    client: Client,
}

impl WebhookSink {
    /// Create a new [`WebhookSink`] with a shared `reqwest::Client`.
    ///
    /// # Errors
    /// Returns an error if the `reqwest::Client` cannot be built
    /// (e.g. the TLS backend is unavailable).
    pub fn new() -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }

    /// Spawn a best-effort HTTP POST for `alert` to `url`.
    ///
    /// The request is spawned as a detached tokio task; this function returns
    /// immediately and never blocks the caller. Errors (network, non-2xx) are
    /// logged at WARN and silently swallowed.
    pub fn fire(&self, url: &str, alert: &MatchAlert) {
        let client = self.client.clone();
        // Clone what we need before moving into the task.
        let url = url.to_owned();
        let alert = alert.clone();

        tokio::spawn(async move {
            match client.post(&url).json(&alert).send().await {
                Err(e) => {
                    warn!(
                        report_id = %alert.report_id,
                        candidate_id = %alert.candidate_id,
                        webhook_url = %url,
                        error = %e,
                        "webhook: delivery failed (network error)",
                    );
                }
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        warn!(
                            report_id = %alert.report_id,
                            candidate_id = %alert.candidate_id,
                            webhook_url = %url,
                            http_status = %status,
                            "webhook: non-2xx response (best-effort; no retry)",
                        );
                    }
                }
            }
        });
    }
}

/// Validate that `raw_url` is a valid `http://` or `https://` URL.
///
/// Returns the canonical URL string on success, or an error message on failure.
///
/// # Errors
/// Returns a `String` describing the validation error.
pub fn validate_notify_url(raw_url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(raw_url)
        .map_err(|e| format!("notify_url is not a valid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        other => Err(format!(
            "notify_url scheme must be http or https, got: {other}"
        )),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_accepts_http() {
        assert!(validate_notify_url("http://example.com/hook").is_ok());
    }

    #[test]
    fn validate_accepts_https() {
        assert!(validate_notify_url("https://owner.example.com/webhook").is_ok());
    }

    #[test]
    fn validate_rejects_non_url() {
        let err = validate_notify_url("not-a-url").unwrap_err();
        assert!(err.contains("not a valid URL"), "unexpected: {err}");
    }

    #[test]
    fn validate_rejects_ftp_scheme() {
        let err = validate_notify_url("ftp://files.example.com/x").unwrap_err();
        assert!(err.contains("scheme must be http or https"), "unexpected: {err}");
    }

    #[test]
    fn validate_rejects_empty_string() {
        assert!(validate_notify_url("").is_err());
    }
}
