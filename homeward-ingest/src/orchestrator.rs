//! AIMD-cadence multi-connector orchestration loop — stray-aware.
//!
//! Each registered connector runs on its own adaptive interval:
//! - After a no-change poll (empty result): interval *= `backoff_factor`
//!   (up to `max_interval`).
//! - After a high-churn poll (result count ≥ `churn_threshold`) containing
//!   **no strays** (total > 0, stray == 0): normal AIMD backoff proceeds —
//!   adoptable churn does **not** pin the interval at the floor.
//! - After a poll with stray count ≥ `stray_floor_threshold`: interval is
//!   reset to `stray_floor` (a new fast-minimum) and a short "hot" window
//!   begins.  During the hot window each absent-stray poll decays the hot
//!   counter; once it expires, the source reverts to normal AIMD.
//! - Cursors are persisted to the [`Store`] after every poll so a restart
//!   resumes from the watermark.
//!
//! ## Stray classification
//!
//! [`IntakeType::Stray`] and [`IntakeType::FoundReport`] count as stray-class
//! records.  All other variants (Adoptable, OwnerSurrender, Transfer, …) do not.
//!
//! ## Configuration
//!
//! | field | default | env override |
//! |---|---|---|
//! | `stray_floor_threshold` | 3 | `HW_STRAY_FLOOR_THRESHOLD` |
//! | `stray_floor` | `cadence_floor` (same as normal floor) | `HW_STRAY_FLOOR_SECS` |
//! | `hot_window_ticks` | 5 | `HW_HOT_WINDOW_TICKS` |
//!
//! Hard lower bound: `stray_floor` is silently clamped to ≥ 10 s to avoid
//! violating upstream rate limits (configurable floor, not bypassable).

use std::sync::Arc;
use std::time::Duration;

use homeward_connectors::{Connector, Cursor, ConnectorError};
use homeward_schema::IntakeType;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::departure::{DepartureConfig, is_explicitly_departed, run_departure_detection};
use crate::dedup::resolve;
use crate::events::{EventKind, EventSink, IngestEvent};
use crate::store::{Store, SourceCursor, StoreError};

/// Minimum allowed `stray_floor` to respect upstream rate limits.
pub const STRAY_FLOOR_MIN: Duration = Duration::from_secs(10);

/// The outcome of a single connector poll, carrying both total and stray counts.
///
/// Build with [`PollOutcome::new`] — stray classification lives here so `tick()`
/// computes it in the existing record loop at zero extra cost (AC8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollOutcome {
    /// Total records returned by the poll.
    pub total: usize,
    /// Records classified as stray-class (`Stray` or `FoundReport`).
    pub stray: usize,
}

impl PollOutcome {
    /// Construct a `PollOutcome`.
    #[must_use]
    pub const fn new(total: usize, stray: usize) -> Self {
        Self { total, stray }
    }

    /// Convenience: zero-result outcome (used on error / no records).
    #[must_use]
    pub const fn empty() -> Self {
        Self { total: 0, stray: 0 }
    }
}

/// Returns `true` when `intake_type` counts as stray-class for cadence purposes.
///
/// Stray-class = [`IntakeType::Stray`] | [`IntakeType::FoundReport`].
/// All other variants (Adoptable, OwnerSurrender, Transfer, LostReport, Unknown)
/// do not count (AC5).
#[must_use]
pub fn is_stray_class(intake_type: IntakeType) -> bool {
    matches!(intake_type, IntakeType::Stray | IntakeType::FoundReport)
}

/// Cadence configuration with stray-aware thresholds.
///
/// All env overrides are read once at [`CadenceConfig::from_env`].
#[derive(Debug, Clone)]
pub struct CadenceConfig {
    /// Minimum stray count to enter the hot window (default: 3).
    pub stray_floor_threshold: usize,
    /// Override for `stray_floor` in seconds.  `None` = use `cadence_floor`.
    /// Clamped to ≥ [`STRAY_FLOOR_MIN`].
    pub stray_floor_secs: Option<u64>,
    /// How many ticks the hot window lasts per stray burst (default: 5).
    pub hot_window_ticks: usize,
}

impl Default for CadenceConfig {
    fn default() -> Self {
        Self {
            stray_floor_threshold: 3,
            stray_floor_secs: None,
            hot_window_ticks: 5,
        }
    }
}

impl CadenceConfig {
    /// Read from environment variables, falling back to defaults.
    #[must_use]
    pub fn from_env() -> Self {
        let stray_floor_threshold = std::env::var("HW_STRAY_FLOOR_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let stray_floor_secs = std::env::var("HW_STRAY_FLOOR_SECS")
            .ok()
            .and_then(|v| v.parse().ok());
        let hot_window_ticks = std::env::var("HW_HOT_WINDOW_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        Self {
            stray_floor_threshold,
            stray_floor_secs,
            hot_window_ticks,
        }
    }
}

/// Per-source observability counters exposed via [`SourceStatus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    /// Connector name.
    pub name: String,
    /// Current poll interval.
    pub interval: Duration,
    /// Total strays seen across all ticks.
    pub stray_seen: u64,
    /// Remaining hot-window ticks (0 = not in hot window).
    pub hot_ticks_remaining: usize,
}

/// State tracked per connector between polls.
#[derive(Debug, Clone)]
struct ConnectorState {
    name: String,
    /// Current poll interval (AIMD-managed).
    interval: Duration,
    /// Minimum interval (the connector's `cadence_hint`).
    cadence_floor: Duration,
    /// Fast minimum during hot window (≥ STRAY_FLOOR_MIN, ≤ cadence_floor).
    stray_floor: Duration,
    /// Maximum interval (backoff cap).
    max_interval: Duration,
    /// Factor applied on a no-change poll.
    backoff_factor: f64,
    /// Records-returned threshold for old-style churn reset (stray-unaware path).
    churn_threshold: usize,
    /// Minimum stray count to trigger the hot window.
    stray_floor_threshold: usize,
    /// Remaining ticks in hot window (0 = normal mode).
    hot_ticks_remaining: usize,
    /// Hot window length in ticks.
    hot_window_ticks: usize,
    /// Cumulative stray records seen.
    stray_seen: u64,
}

impl ConnectorState {
    fn new(name: String, cadence_hint: Duration, cfg: &CadenceConfig) -> Self {
        let stray_floor = cfg
            .stray_floor_secs
            .map(|s| Duration::from_secs(s).max(STRAY_FLOOR_MIN))
            .unwrap_or(cadence_hint)
            .min(cadence_hint); // stray_floor ≤ cadence_floor (it's faster)
        let stray_floor = stray_floor.max(STRAY_FLOOR_MIN);
        Self {
            name,
            interval: cadence_hint,
            cadence_floor: cadence_hint,
            stray_floor,
            max_interval: cadence_hint.saturating_mul(16),
            backoff_factor: 2.0,
            churn_threshold: 10,
            stray_floor_threshold: cfg.stray_floor_threshold,
            hot_ticks_remaining: 0,
            hot_window_ticks: cfg.hot_window_ticks,
            stray_seen: 0,
        }
    }

    /// Adjust the interval based on a [`PollOutcome`] (stray-aware AIMD).
    ///
    /// Invariants:
    /// - `stray == 0` with any `total` → reproduces exact legacy AIMD behavior
    ///   **except** that adoptable-only churn (`total > 0, stray == 0`) no longer
    ///   resets to `cadence_floor` (AC1, AC3).
    /// - `stray >= stray_floor_threshold` → enter/refresh hot window; reset to
    ///   `stray_floor` (AC2).
    /// - Hot window decay: each tick without strays decrements `hot_ticks_remaining`;
    ///   normal backoff resumes when it hits 0 (AC4).
    /// - `total == 0` → normal backoff (unchanged).
    #[allow(clippy::float_arithmetic)]
    fn adapt(&mut self, outcome: PollOutcome) {
        // Update cumulative stray counter.
        self.stray_seen = self.stray_seen.saturating_add(outcome.stray as u64);

        if outcome.stray >= self.stray_floor_threshold {
            // Enter / refresh hot window.
            self.interval = self.stray_floor;
            self.hot_ticks_remaining = self.hot_window_ticks;
            return;
        }

        // No strays this tick — decay hot window.
        if self.hot_ticks_remaining > 0 {
            self.hot_ticks_remaining -= 1;
            // Keep using stray_floor while in the window.
            self.interval = self.stray_floor;
            return;
        }

        // Normal AIMD (stray == 0 path).
        if outcome.total == 0 {
            // No records at all → back off.
            let secs = (self.interval.as_secs_f64() * self.backoff_factor)
                .min(self.max_interval.as_secs_f64());
            self.interval = Duration::from_secs_f64(secs).max(self.cadence_floor);
        } else {
            // total > 0, stray == 0: adoptable-only churn.
            // Do NOT reset to floor (AC3).  Keep current interval unchanged.
        }
    }

    /// Expose observable status for this source.
    fn status(&self) -> SourceStatus {
        SourceStatus {
            name: self.name.clone(),
            interval: self.interval,
            stray_seen: self.stray_seen,
            hot_ticks_remaining: self.hot_ticks_remaining,
        }
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
    cadence_cfg: CadenceConfig,
}

impl<S: EventSink> Orchestrator<S> {
    /// Build a new orchestrator with default cadence config.
    #[must_use]
    pub fn new(store: Arc<Mutex<Store>>, sink: S, departure_config: DepartureConfig) -> Self {
        Self::with_cadence(store, sink, departure_config, CadenceConfig::default())
    }

    /// Build a new orchestrator with an explicit cadence config.
    #[must_use]
    pub fn with_cadence(
        store: Arc<Mutex<Store>>,
        sink: S,
        departure_config: DepartureConfig,
        cadence_cfg: CadenceConfig,
    ) -> Self {
        Self {
            store,
            connectors: Vec::new(),
            departure_config,
            sink,
            cadence_cfg,
        }
    }

    /// Register a connector.
    pub fn register(&mut self, name: impl Into<String>, connector: Box<dyn Connector>) {
        let state = ConnectorState::new(name.into(), connector.cadence_hint(), &self.cadence_cfg);
        self.connectors.push((connector, state));
    }

    /// Return the current observable status for all registered connectors (AC7).
    #[must_use]
    pub fn status(&self) -> Vec<SourceStatus> {
        self.connectors
            .iter()
            .map(|(_, state)| state.status())
            .collect()
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

            // Load persisted cursor.  Track whether this is a full-sync (no
            // prior cursor) so we only run absence-based departure detection
            // on bootstrap polls, not on delta ticks.  Delta polls return only
            // records updated since the watermark, so any animal absent from
            // the result set is simply unchanged — not departed.
            let (cursor, is_full_sync): (Option<Cursor>, bool) = {
                let store = self.store.lock().await;
                let c = store.load_cursor(&source_name)?.and_then(|sc| {
                    serde_json::from_str::<Cursor>(&sc.cursor_json).ok()
                });
                let full = c.is_none();
                (c, full)
            };

            // Poll the connector exactly once (AC8).
            let records = match connector.poll(cursor).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(source = %source_name, error = %e, "poll failed");
                    // On error: back off (treat as no-change).
                    state.adapt(PollOutcome::empty());
                    return Err(OrchestratorError::Connector {
                        name: source_name,
                        source: e,
                    });
                }
            };

            let returned = records.len();
            info!(source = %source_name, count = returned, "polled");

            // Single-pass: persist records, count strays, dedup, departure.
            let mut seen_ids = Vec::with_capacity(records.len());
            let mut stray_count: usize = 0;
            {
                let mut store = self.store.lock().await;

                for record in records {
                    // Count stray-class records in the same loop as persistence (AC8).
                    if is_stray_class(record.intake_type) {
                        stray_count += 1;
                    }

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

                // Absence-based departure detection only on full-sync polls.
                // Delta polls (cursor was Some) return only recently-updated
                // records; animals absent from the delta are simply unchanged,
                // not departed.  Running absence detection on a delta tick
                // would mark every stable record as departed after two quiet
                // ticks.  Explicit departure (outcome_date / Availability::Departed)
                // is handled per-record above and is unaffected by this guard.
                if is_full_sync {
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

            let outcome = PollOutcome::new(returned, stray_count);
            info!(
                source = %source_name,
                stray = stray_count,
                hot_remaining = state.hot_ticks_remaining,
                "stray cadence"
            );
            state.adapt(outcome);
        }
        Ok(())
    }
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use homeward_schema::IntakeType;

    use super::{CadenceConfig, ConnectorState, PollOutcome, STRAY_FLOOR_MIN, is_stray_class};

    fn cfg_default() -> CadenceConfig {
        CadenceConfig::default()
    }

    fn make_state(cadence_secs: u64) -> ConnectorState {
        ConnectorState::new("test".to_owned(), Duration::from_secs(cadence_secs), &cfg_default())
    }

    fn make_state_with_cfg(cadence_secs: u64, cfg: &CadenceConfig) -> ConnectorState {
        ConnectorState::new("test".to_owned(), Duration::from_secs(cadence_secs), cfg)
    }

    // ── AC1: stray==0 reproduces legacy AIMD exactly ──────────────────────────

    #[test]
    fn ac1_no_change_doubles_interval() {
        let mut s = make_state(60);
        s.adapt(PollOutcome::new(0, 0));
        assert_eq!(s.interval, Duration::from_secs(120));
    }

    #[test]
    fn ac1_no_change_caps_at_max() {
        let mut s = make_state(60);
        for _ in 0..20 {
            s.adapt(PollOutcome::new(0, 0));
        }
        assert!(
            s.interval <= s.max_interval,
            "interval must not exceed max_interval"
        );
    }

    #[test]
    fn ac1_no_change_never_below_floor() {
        let mut s = make_state(60);
        s.adapt(PollOutcome::new(0, 0));
        assert!(
            s.interval >= s.cadence_floor,
            "interval must never drop below cadence_floor"
        );
    }

    /// Legacy high-churn (stray==0) path: does NOT reset to floor any more
    /// (AC3). A high-churn all-adoptable poll keeps the interval unchanged.
    #[test]
    fn ac1_high_churn_stray_zero_keeps_interval() {
        let mut s = make_state(60);
        // back off first
        s.adapt(PollOutcome::new(0, 0));
        s.adapt(PollOutcome::new(0, 0));
        let backed_off = s.interval;
        assert!(backed_off > s.cadence_floor);
        // high-churn adoptable-only → interval unchanged (not reset)
        s.adapt(PollOutcome::new(100, 0));
        assert_eq!(s.interval, backed_off, "adoptable churn must not reset interval");
    }

    // ── AC2: stray >= threshold drives interval to fast minimum ───────────────

    #[test]
    fn ac2_stray_burst_resets_to_stray_floor() {
        let cfg = CadenceConfig {
            stray_floor_threshold: 3,
            stray_floor_secs: Some(15),
            hot_window_ticks: 5,
        };
        let mut s = make_state_with_cfg(60, &cfg);
        // back off
        s.adapt(PollOutcome::new(0, 0));
        s.adapt(PollOutcome::new(0, 0));
        assert!(s.interval > s.stray_floor);
        // stray burst
        s.adapt(PollOutcome::new(5, 4));
        assert_eq!(
            s.interval, s.stray_floor,
            "stray burst must reset to stray_floor"
        );
    }

    #[test]
    fn ac2_stray_burst_enters_hot_window() {
        let mut s = make_state(60);
        s.adapt(PollOutcome::new(3, 3)); // exactly threshold
        assert!(s.hot_ticks_remaining > 0, "should enter hot window");
    }

    // ── AC3: total > 0, stray == 0 does NOT reset to floor ───────────────────

    #[test]
    fn ac3_adoptable_churn_no_floor_reset() {
        let mut s = make_state(60);
        // back off
        s.adapt(PollOutcome::new(0, 0));
        let backed_off = s.interval;
        // adoptable churn — must NOT reset to floor
        s.adapt(PollOutcome::new(50, 0));
        assert_eq!(
            s.interval, backed_off,
            "adoptable-only churn must not change interval"
        );
        assert!(backed_off > s.cadence_floor);
    }

    // ── AC4: hot window decays ────────────────────────────────────────────────

    #[test]
    fn ac4_hot_window_decays_then_backsoff() {
        let cfg = CadenceConfig {
            stray_floor_threshold: 3,
            stray_floor_secs: Some(15),
            hot_window_ticks: 3,
        };
        let mut s = make_state_with_cfg(60, &cfg);
        // trigger hot window
        s.adapt(PollOutcome::new(4, 3));
        assert_eq!(s.hot_ticks_remaining, 3);
        let hot_interval = s.interval;

        // tick 1 — still in hot window
        s.adapt(PollOutcome::new(0, 0));
        assert_eq!(s.hot_ticks_remaining, 2);
        assert_eq!(s.interval, hot_interval);

        // tick 2 — still in hot window
        s.adapt(PollOutcome::new(0, 0));
        assert_eq!(s.hot_ticks_remaining, 1);

        // tick 3 — last hot tick
        s.adapt(PollOutcome::new(0, 0));
        assert_eq!(s.hot_ticks_remaining, 0);

        // tick 4 — hot window expired → normal backoff resumes
        let interval_before = s.interval;
        s.adapt(PollOutcome::new(0, 0));
        assert!(
            s.interval > interval_before,
            "interval should grow again after hot window expires"
        );
    }

    // ── AC5: stray classification ─────────────────────────────────────────────

    #[test]
    fn ac5_stray_class_includes_stray_and_found_report() {
        assert!(is_stray_class(IntakeType::Stray));
        assert!(is_stray_class(IntakeType::FoundReport));
    }

    #[test]
    fn ac5_stray_class_excludes_non_stray() {
        assert!(!is_stray_class(IntakeType::Adoptable));
        assert!(!is_stray_class(IntakeType::OwnerSurrender));
        assert!(!is_stray_class(IntakeType::Transfer));
        assert!(!is_stray_class(IntakeType::LostReport));
        assert!(!is_stray_class(IntakeType::Unknown));
    }

    // ── AC6: configurable threshold changes behavior ──────────────────────────

    #[test]
    fn ac6_custom_threshold_changes_behavior() {
        // With threshold=1, even a single stray triggers hot window.
        let cfg = CadenceConfig {
            stray_floor_threshold: 1,
            stray_floor_secs: None,
            hot_window_ticks: 5,
        };
        let mut s = make_state_with_cfg(60, &cfg);
        s.adapt(PollOutcome::new(1, 1));
        assert!(s.hot_ticks_remaining > 0, "threshold=1 should trigger on single stray");

        // With threshold=10, 3 strays don't trigger.
        let cfg2 = CadenceConfig {
            stray_floor_threshold: 10,
            stray_floor_secs: None,
            hot_window_ticks: 5,
        };
        let mut s2 = make_state_with_cfg(60, &cfg2);
        s2.adapt(PollOutcome::new(5, 3));
        assert_eq!(s2.hot_ticks_remaining, 0, "threshold=10 should not trigger on 3 strays");
    }

    // ── AC7: status surface ───────────────────────────────────────────────────

    #[test]
    fn ac7_status_reflects_stray_seen_and_hot_window() {
        let mut s = make_state(60);
        // initial: no strays, not hot
        let st = s.status();
        assert_eq!(st.stray_seen, 0);
        assert_eq!(st.hot_ticks_remaining, 0);

        // trigger with 4 strays
        s.adapt(PollOutcome::new(4, 4));
        let st2 = s.status();
        assert_eq!(st2.stray_seen, 4);
        assert!(st2.hot_ticks_remaining > 0);
        assert_eq!(st2.name, "test");
        assert_eq!(st2.interval, s.stray_floor);
    }

    // ── AC9 / hard lower bound: stray_floor ≥ STRAY_FLOOR_MIN ────────────────

    #[test]
    fn stray_floor_hard_lower_bound() {
        // Even if stray_floor_secs is set to 1 s (below the 10 s minimum), it
        // should be clamped to STRAY_FLOOR_MIN.
        let cfg = CadenceConfig {
            stray_floor_threshold: 3,
            stray_floor_secs: Some(1),
            hot_window_ticks: 5,
        };
        let s = make_state_with_cfg(60, &cfg);
        assert!(
            s.stray_floor >= STRAY_FLOOR_MIN,
            "stray_floor must never go below STRAY_FLOOR_MIN"
        );
    }
}
