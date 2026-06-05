//! The [`Connector`] trait and [`Cursor`] type.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use homeward_schema::{PetRecord, Provenance};
use serde::{Deserialize, Serialize};

use crate::error::ConnectorError;

/// An opaque cursor for incremental polling.
///
/// Connectors use this to track the last-seen watermark so callers can
/// retrieve only records updated after a previous poll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Cursor {
    /// Timestamp-based cursor (ISO 8601).
    Timestamp(DateTime<Utc>),
    /// Opaque string cursor (e.g. a page token or `ETag`).
    Opaque(String),
}

/// The core connector abstraction.
///
/// Implementors are stateless pollers: given an optional [`Cursor`] from the
/// last poll they return the set of [`PetRecord`]s that have changed since
/// then.  Scheduling, dedup, and storage are handled by the caller.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Poll the source for records updated since `since`.
    ///
    /// A `None` cursor requests a full (initial) fetch.
    /// When the source responds 304 Not Modified, returns `Ok(vec![])`.
    ///
    /// # Errors
    /// Returns a [`ConnectorError`] if the network request, rate-limit, or
    /// response parsing fails.
    async fn poll(&self, since: Option<Cursor>) -> Result<Vec<PetRecord>, ConnectorError>;

    /// The provenance metadata for records emitted by this connector.
    fn provenance(&self) -> Provenance;

    /// How often the caller should invoke this connector.
    ///
    /// This is a hint only — the scheduler may use a different cadence.
    fn cadence_hint(&self) -> Duration;
}
