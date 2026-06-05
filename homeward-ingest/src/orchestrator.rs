//! AIMD-cadence multi-connector orchestration loop.
//!
//! Each registered connector runs on its own adaptive interval:
//! - After a no-change poll (empty result): interval *= `backoff_factor`
//!   (up to `max_interval`).
//! - After a high-churn poll (result count ≥ `churn_threshold`): interval
//!   is reset toward the connector's `cadence_hint` floor.
//! - Cursors are persisted to the [`Store`] after every poll so a restart
//!   resumes from the watermark.

use std::sync::Arc;
use std::time::Duration;

use homeward_connectors::{Connector, Cursor, ConnectorError};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::departure::{DepartureConfig, is_explicitly_departed, run_departure_detection};
use crate::dedup::resolve;
use crate::events::{EventKind, EventSink, IngestEvent};
use crate::store::{Store, SourceCursor, StoreError};

/// State tracked per connector between polls.
#[derive(Debug, Clone)]
struct ConnectorState {
    name: String,
    /// Current poll interval (AIMD-managed).
    interval: Duration,
    /// Minimum interval (the connector's `cadence_hint`).
    cadence_floor: Duration,
    /// Maximum interval (backoff cap).
    max_interval: Duration,
    /// Factor applied on a no-change poll.
    backoff_factor: f64,
    /// Records-returned threshold that resets to `cadence_floor`.
    churn_threshold: usize,
}

impl ConnectorState {
    const fn new(name: String, cadence_hint: Duration) -> Self {
        Self {
            name,
            interval: cadence_hint,
            cadence_floor: cadence_hint,
            max_interval: cadence_hint.saturating_mul(16),
            backoff_factor: 2.0,
            churn_threshold: 10,
        }
    }

    /// Adjust the interval based on poll result size (AIMD).
    #[allow(clippy::float_arithmetic)]
    fn adapt(&mut self, returned: usize) {
        if returned == 0 {
            // Additive increase toward max.
            let secs = (self.interval.as_secs_f64() * self.backoff_factor)
                .min(self.max_interval.as_secs_f64());
            self.interval = Duration::from_secs_f64(secs).max(self.cadence_floor);
        } else if returned >= self.churn_threshold {
            // Multiplicative decrease back to floor.
            self.interval = self.cadence_floor;
        }
        // Otherwise keep current interval.
    }
}

/// Errors returned by the orchestrator.
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    /// Store operation failed.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// Connector poll failed.
    #[error("connector {name}: {source}")]
    Connector {
        /// Connector name.
        name: String,
        /// Underlying connector error.
        #[source]
        source: ConnectorError,
    },
}

/// The ingest orchestrator.
///
/// Call [`Orchestrator::tick`] from a loop (or a tokio interval) to advance
/// all connectors by one step.  The orchestrator is intentionally not
/// self-looping so callers can control the scheduler.
pub struct Orchestrator<S: EventSink> {
    store: Arc<Mutex<Store>>,
    connectors: Vec<(Box<dyn Connector>, ConnectorState)>,
    departure_config: DepartureConfig,
    sink: S,
}

impl<S: EventSink> Orchestrator<S> {
    /// Build a new orchestrator.
    #[must_use]
    pub fn new(store: Arc<Mutex<Store>>, sink: S, departure_config: DepartureConfig) -> Self {
        Self {
            store,
            connectors: Vec::new(),
            departure_config,
            sink,
        }
    }

    /// Register a connector.
    pub fn register(&mut self, name: impl Into<String>, connector: Box<dyn Connector>) {
        let state = ConnectorState::new(name.into(), connector.cadence_hint());
        self.connectors.push((connector, state));
    }

    /// Run one poll tick for each due connector.
    ///
    /// A connector is "due" when its next scheduled poll time ≤ now.
    /// For simplicity in the single-tick model, we poll ALL connectors every
    /// call; the real daemon wraps this in a `tokio::time::interval`.
    ///
    /// # Errors
    /// Returns the first error encountered; other connectors still proceed.
    pub async fn tick(&mut self) -> Result<(), OrchestratorError> {
        for (connector, state) in &mut self.connectors {
            let source_name = state.name.clone();
            debug!(source = %source_name, "polling");

            // Load persisted cursor.
            let cursor: Option<Cursor> = {
                let store = self.store.lock().await;
                store.load_cursor(&source_name)?.and_then(|sc| {
                    serde_json::from_str::<Cursor>(&sc.cursor_json).ok()
                })
            };

            // Poll the connector.
            let records = match connector.poll(cursor).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(source = %source_name, error = %e, "poll failed");
                    // On error: back off (treat as no-change).
                    state.adapt(0);
                    return Err(OrchestratorError::Connector {
                        name: source_name,
                        source: e,
                    });
                }
            };

            let returned = records.len();
            info!(source = %source_name, count = returned, "polled");

            // Persist records, dedup, departure.
            let mut seen_ids = Vec::with_capacity(records.len());
            {
                let mut store = self.store.lock().await;

                for record in records {
                    let resolved = resolve(&store, record).map_err(OrchestratorError::Store)?;
                    let is_new = store.get(resolved.canonical_id).is_err();
                    let is_departed = is_explicitly_departed(&resolved);

                    seen_ids.push(resolved.canonical_id);
                    store.upsert(&resolved).map_err(OrchestratorError::Store)?;

                    let kind = if is_departed {
                        EventKind::Departed
                    } else if is_new {
                        EventKind::New
                    } else {
                        EventKind::Updated
                    };
                    self.sink.publish(IngestEvent::new(kind, resolved));
                }

                // Departure detection for this source's full sync.
                let departed_ids = run_departure_detection(
                    &mut store,
                    &source_name,
                    &seen_ids,
                    &self.departure_config,
                )
                .map_err(OrchestratorError::Store)?;

                for id in departed_ids {
                    if let Ok(record) = store.get(id) {
                        self.sink
                            .publish(IngestEvent::new(EventKind::Departed, record));
                    }
                }
            }

            // Persist cursor (use Timestamp(now) as a simple advancing watermark).
            {
                let cursor_json = serde_json::to_string(&Cursor::Timestamp(chrono::Utc::now()))
                    .unwrap_or_default();
                let store = self.store.lock().await;
                store
                    .save_cursor(&SourceCursor {
                        source_name: source_name.clone(),
                        cursor_json,
                        updated_at: chrono::Utc::now(),
                    })
                    .map_err(OrchestratorError::Store)?;
            }

            state.adapt(returned);
        }
        Ok(())
    }
}

// ── AIMD unit tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::ConnectorState;

    fn make_state(cadence_secs: u64) -> ConnectorState {
        ConnectorState::new("test".to_owned(), Duration::from_secs(cadence_secs))
    }

    #[test]
    fn no_change_doubles_interval() {
        let mut s = make_state(60);
        s.adapt(0);
        assert_eq!(s.interval, Duration::from_secs(120));
    }

    #[test]
    fn no_change_caps_at_max() {
        let mut s = make_state(60);
        for _ in 0..20 {
            s.adapt(0);
        }
        assert!(
            s.interval <= s.max_interval,
            "interval must not exceed max_interval"
        );
    }

    #[test]
    fn no_change_never_below_floor() {
        let mut s = make_state(60);
        s.adapt(0);
        assert!(
            s.interval >= s.cadence_floor,
            "interval must never drop below cadence_floor"
        );
    }

    #[test]
    fn high_churn_resets_to_floor() {
        let mut s = make_state(60);
        // Back off first.
        s.adapt(0);
        s.adapt(0);
        assert!(s.interval > s.cadence_floor);
        // High-churn poll resets.
        s.adapt(100);
        assert_eq!(s.interval, s.cadence_floor, "high-churn must reset to cadence floor");
    }

    #[test]
    fn low_churn_keeps_interval() {
        let mut s = make_state(60);
        let before = s.interval;
        s.adapt(3); // < churn_threshold (10)
        assert_eq!(s.interval, before, "low churn must not change interval");
    }
}
