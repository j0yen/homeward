//! Polite HTTP core shared by all connectors.
//!
//! Features:
//! - Conditional requests (`If-None-Match` / `If-Modified-Since`).
//! - Identifying `User-Agent` header.
//! - `Retry-After` handling and per-host exponential backoff on 429/5xx.
//! - robots.txt checking (cached per host).
//! - Never downloads raw image bytes (callers store URLs only).

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use reqwest::{
    Client, Response, StatusCode,
    header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, USER_AGENT},
};
use robots_txt::{Robots, matcher::SimpleMatcher};
use tokio::sync::Mutex;
use tracing::warn;
use url::Url;

use crate::error::ConnectorError;

/// The User-Agent string sent with every request.
///
/// Identifies the project and contact URL per politeness requirements.
pub const HOMEWARD_USER_AGENT: &str =
    "homeward-connectors/0.1 (+https://github.com/j0yen/homeward; shelter-pet aggregation)";

/// How long to cache robots.txt per host.
const ROBOTS_TTL: Duration = Duration::from_secs(3600);

/// Maximum number of retry attempts on 429/5xx.
const MAX_RETRIES: u32 = 4;

/// Base delay for exponential backoff.
const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Cached robots.txt entry.
struct RobotsEntry {
    /// Raw text of robots.txt for this host.
    text: String,
    /// When this entry was cached.
    cached_at: Instant,
}

/// Per-host rate-limit state.
struct HostRateState {
    /// When we are allowed to send the next request to this host.
    next_allowed: Instant,
}

/// The polite HTTP client wrapper.
///
/// Wrap a [`reqwest::Client`] with conditional-request, robots.txt, and
/// per-host rate-limit logic.
pub struct PoliteClient {
    inner: Client,
    robots_cache: Arc<Mutex<HashMap<String, RobotsEntry>>>,
    rate_state: Arc<Mutex<HashMap<String, HostRateState>>>,
    /// Minimum interval between requests to the same host.
    min_interval: Duration,
}

impl PoliteClient {
    /// Build a new `PoliteClient` with the given minimum per-host interval.
    ///
    /// # Errors
    /// Returns [`ConnectorError`] if the underlying [`reqwest::Client`] cannot
    /// be constructed.
    pub fn new(min_interval: Duration) -> Result<Self, ConnectorError> {
        let inner = Client::builder()
            .user_agent(HOMEWARD_USER_AGENT)
            .gzip(true)
            .deflate(true)
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            inner,
            robots_cache: Arc::new(Mutex::new(HashMap::new())),
            rate_state: Arc::new(Mutex::new(HashMap::new())),
            min_interval,
        })
    }

    /// Build from an existing [`reqwest::Client`] (useful for tests with a mock).
    #[must_use]
    pub fn from_client(client: Client, min_interval: Duration) -> Self {
        Self {
            inner: client,
            robots_cache: Arc::new(Mutex::new(HashMap::new())),
            rate_state: Arc::new(Mutex::new(HashMap::new())),
            min_interval,
        }
    }

    /// Check robots.txt for the given URL.
    ///
    /// Fetches and caches robots.txt for the host, then checks whether our
    /// User-Agent is allowed to access `url`.
    ///
    /// # Errors
    /// Returns [`ConnectorError::RobotsDisallowed`] if disallowed.
    pub async fn check_robots(&self, url: &Url) -> Result<(), ConnectorError> {
        let host = url
            .host_str()
            .map(|h| h.to_owned())
            .unwrap_or_default();
        let robots_url = format!(
            "{}://{}/robots.txt",
            url.scheme(),
            url.authority()
        );

        let robots_text = {
            let mut cache = self.robots_cache.lock().await;
            if let Some(entry) = cache.get(&host) {
                if entry.cached_at.elapsed() < ROBOTS_TTL {
                    Some(entry.text.clone())
                } else {
                    cache.remove(&host);
                    None
                }
            } else {
                None
            }
        };

        let robots_text = if let Some(text) = robots_text {
            text
        } else {
            // Fetch robots.txt — failures are treated as "allow all".
            match self.inner.get(&robots_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let text = resp.text().await.unwrap_or_default();
                    let mut cache = self.robots_cache.lock().await;
                    cache.insert(
                        host.clone(),
                        RobotsEntry {
                            text: text.clone(),
                            cached_at: Instant::now(),
                        },
                    );
                    text
                }
                Ok(_) | Err(_) => {
                    // robots.txt absent or unreachable → allow all
                    return Ok(());
                }
            }
        };

        let robots = Robots::from_str_lossy(&robots_text);
        let path = url.path();
        let section = robots.choose_section(HOMEWARD_USER_AGENT);
        let matcher = SimpleMatcher::new(&section.rules);
        if !matcher.check_path(path) {
            return Err(ConnectorError::RobotsDisallowed {
                path: path.to_owned(),
                user_agent: HOMEWARD_USER_AGENT.to_owned(),
            });
        }
        Ok(())
    }

    /// Wait until the per-host rate limit allows the next request.
    async fn wait_rate_limit(&self, host: &str) {
        let now = Instant::now();
        let next_allowed = {
            let state = self.rate_state.lock().await;
            state
                .get(host)
                .map(|s| s.next_allowed)
                .unwrap_or(now)
        };
        if next_allowed > now {
            let wait = next_allowed - now;
            tokio::time::sleep(wait).await;
        }
    }

    /// Update the per-host rate state after a successful request.
    async fn update_rate_state(&self, host: &str, extra_delay: Option<Duration>) {
        let mut state = self.rate_state.lock().await;
        let delay = extra_delay.unwrap_or(self.min_interval);
        state.insert(
            host.to_owned(),
            HostRateState {
                next_allowed: Instant::now() + delay,
            },
        );
    }

    /// Perform a GET with optional conditional headers, rate limiting, and retry.
    ///
    /// - `etag`: If provided, sends `If-None-Match`.
    /// - `last_modified`: If provided, sends `If-Modified-Since`.
    /// - A 304 response is returned as-is (caller should treat as no-work).
    ///
    /// # Errors
    /// Returns [`ConnectorError`] on network failure, robots disallow, or
    /// exhausted retries.
    pub async fn get(
        &self,
        url: &Url,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<Response, ConnectorError> {
        self.check_robots(url).await?;

        let host = url.host_str().unwrap_or("").to_owned();
        let mut attempt = 0u32;

        loop {
            self.wait_rate_limit(&host).await;

            let mut req = self.inner.get(url.as_str());
            req = req.header(USER_AGENT, HOMEWARD_USER_AGENT);
            if let Some(et) = etag {
                req = req.header(IF_NONE_MATCH, et);
            }
            if let Some(lm) = last_modified {
                req = req.header(IF_MODIFIED_SINCE, lm);
            }

            let resp = req.send().await?;
            let status = resp.status();

            match status {
                // Success or Not Modified — return immediately.
                s if s.is_success() || s == StatusCode::NOT_MODIFIED => {
                    self.update_rate_state(&host, None).await;
                    return Ok(resp);
                }

                // Rate-limited or server error — back off and retry.
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
                    if attempt >= MAX_RETRIES {
                        return Err(ConnectorError::RateLimitExhausted { host });
                    }
                    let retry_after = resp
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let backoff = retry_after.unwrap_or_else(|| {
                        BACKOFF_BASE * 2u32.saturating_pow(attempt)
                    });
                    warn!(
                        attempt,
                        host,
                        ?backoff,
                        "rate-limited, backing off"
                    );
                    self.update_rate_state(&host, Some(backoff)).await;
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                    continue;
                }

                // 5xx errors — retry with backoff.
                s if s.is_server_error() => {
                    if attempt >= MAX_RETRIES {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ConnectorError::UnexpectedStatus {
                            status: status.as_u16(),
                            body: body.chars().take(256).collect(),
                        });
                    }
                    let backoff = BACKOFF_BASE * 2u32.saturating_pow(attempt);
                    warn!(attempt, host, ?backoff, "server error, backing off");
                    tokio::time::sleep(backoff).await;
                    attempt += 1;
                    continue;
                }

                // Any other error.
                _ => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(ConnectorError::UnexpectedStatus {
                        status: status.as_u16(),
                        body: body.chars().take(256).collect(),
                    });
                }
            }
        }
    }

    /// Extract ETag from a response, if present.
    #[must_use]
    pub fn extract_etag(resp: &Response) -> Option<String> {
        resp.headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned())
    }

    /// Extract Last-Modified from a response, if present.
    #[must_use]
    pub fn extract_last_modified(resp: &Response) -> Option<String> {
        resp.headers()
            .get(LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned())
    }

    /// Return a reference to the inner [`reqwest::Client`] for connectors that
    /// need to build custom request builders.
    #[must_use]
    pub fn inner(&self) -> &Client {
        &self.inner
    }
}

impl std::fmt::Debug for PoliteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoliteClient")
            .field("min_interval", &self.min_interval)
            .finish_non_exhaustive()
    }
}
