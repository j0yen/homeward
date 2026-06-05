//! Lightweight change events emitted after each ingest cycle.
//!
//! Consumers (homeward-embed, homeward-match) subscribe to deltas instead of
//! rescanning the full store.

use chrono::{DateTime, Utc};
use homeward_schema::PetRecord;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// A change event produced by the ingest engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEvent {
    /// The canonical id of the affected record.
    pub canonical_id: Ulid,
    /// When the event was produced.
    pub occurred_at: DateTime<Utc>,
    /// The kind of change.
    pub kind: EventKind,
    /// Snapshot of the record at the time of the event.
    pub record: Box<PetRecord>,
}

/// Discriminant for an [`IngestEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A previously-unseen animal entered the store.
    New,
    /// An existing animal's fields were updated.
    Updated,
    /// An animal was marked departed (adopted, returned, etc.).
    Departed,
}

impl IngestEvent {
    /// Create a new [`IngestEvent`].
    #[must_use]
    pub fn new(kind: EventKind, record: PetRecord) -> Self {
        Self {
            canonical_id: record.canonical_id,
            occurred_at: Utc::now(),
            kind,
            record: Box::new(record),
        }
    }
}

/// A callback-style event sink.
///
/// Bus-agnostic: callers may wrap an agorabus publisher, a tokio broadcast
/// channel, or a no-op.  Implementations must be cheaply cloneable.
pub trait EventSink: Send + Sync + 'static {
    /// Publish a single event.  Fire-and-forget; errors are logged, not
    /// propagated to the caller.
    fn publish(&self, event: IngestEvent);
}

/// A no-op [`EventSink`] used in tests and when no bus is configured.
#[derive(Debug, Clone, Default)]
pub struct NullSink;

impl EventSink for NullSink {
    fn publish(&self, _event: IngestEvent) {}
}
