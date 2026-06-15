//! In-memory log of MatchAlerts delivered for each report.
//! Thread-safe via Mutex; keyed by report_id.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single alert entry stored in the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertEntry {
    /// The candidate's canonical ID from the shelter intake.
    pub candidate_id: String,
    /// Similarity score (0–1) from the match engine.
    pub score: f32,
    /// Coarse shelter area (city/state, never a street address).
    pub shelter_area: Option<String>,
    /// Link to the source shelter listing.
    pub source_url: Option<String>,
    /// When the stray's hold window closes (if applicable).
    pub reclaimable_until: Option<DateTime<Utc>>,
    /// When this alert was delivered.
    pub alerted_at: DateTime<Utc>,
}

/// Thread-safe in-memory log of MatchAlerts.
#[derive(Debug, Default)]
pub struct AlertLog {
    inner: Mutex<HashMap<String, Vec<AlertEntry>>>,
}

impl AlertLog {
    /// Create a new, empty alert log.
    pub fn new() -> Self { Self::default() }

    /// Append an entry for `report_id`.
    pub fn push(&self, report_id: &str, entry: AlertEntry) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(report_id.to_owned()).or_default().push(entry);
    }

    /// Return all entries for `report_id` (empty vec if none).
    pub fn for_report(&self, report_id: &str) -> Vec<AlertEntry> {
        let map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        map.get(report_id).cloned().unwrap_or_default()
    }
}
