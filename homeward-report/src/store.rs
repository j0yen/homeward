//! In-memory report store (phase-1 implementation — swap for persistent
//! storage in a later PRD).
//!
//! Invariants enforced:
//! - `insert` rejects duplicate `report_id`.
//! - `delete` removes the record immediately (CCPA AC4).
//! - `expire_stale` transitions expired records to [`LostStatus::Expired`]
//!   and removes them from the active index.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use homeward_schema::{LostReport, LostStatus};
use thiserror::Error;

/// Errors that can occur when submitting a new report.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubmitError {
    /// A report with this ID already exists.
    #[error("duplicate report_id: {0}")]
    Duplicate(String),
}

/// Errors from store update operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreUpdateError {
    /// No record found for the given report_id.
    #[error("report not found: {0}")]
    NotFound(String),
}

/// In-memory store for [`LostReport`] records.
///
/// Thread-safety: not provided here — the caller (service or test) is
/// responsible for wrapping in a Mutex/RwLock if needed.
#[derive(Debug, Default)]
pub struct ReportStore {
    records: HashMap<String, LostReport>,
}

impl ReportStore {
    /// Create a new, empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new report.
    ///
    /// # Errors
    /// Returns [`SubmitError::Duplicate`] if `report_id` already exists.
    pub fn insert(&mut self, report: LostReport) -> Result<(), SubmitError> {
        if self.records.contains_key(&report.report_id) {
            return Err(SubmitError::Duplicate(report.report_id.clone()));
        }
        self.records.insert(report.report_id.clone(), report);
        Ok(())
    }

    /// Get a report by ID.
    #[must_use]
    pub fn get(&self, report_id: &str) -> Option<&LostReport> {
        self.records.get(report_id)
    }

    /// Update the status of an existing report.
    ///
    /// # Errors
    /// Returns [`StoreUpdateError::NotFound`] if the report_id does not exist.
    pub fn update_status(
        &mut self,
        report_id: &str,
        status: LostStatus,
        _now: DateTime<Utc>,
    ) -> Result<(), StoreUpdateError> {
        let rec = self
            .records
            .get_mut(report_id)
            .ok_or_else(|| StoreUpdateError::NotFound(report_id.to_owned()))?;
        rec.status = status;
        Ok(())
    }

    /// Delete a report and purge all stored data immediately (CCPA AC4).
    ///
    /// # Errors
    /// Returns [`StoreUpdateError::NotFound`] if the report_id does not exist.
    pub fn delete(&mut self, report_id: &str) -> Result<(), StoreUpdateError> {
        self.records
            .remove(report_id)
            .ok_or_else(|| StoreUpdateError::NotFound(report_id.to_owned()))?;
        Ok(())
    }

    /// Expire all records whose `expires` timestamp is at or before `now`.
    ///
    /// Returns the list of expired report IDs.
    pub fn expire_stale(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let expired: Vec<String> = self
            .records
            .values()
            .filter(|r| r.expires <= now && r.status == LostStatus::Active)
            .map(|r| r.report_id.clone())
            .collect();
        for id in &expired {
            self.records.remove(id);
        }
        expired
    }

    /// Iterate over all active reports.
    pub fn active_reports(&self) -> impl Iterator<Item = &LostReport> {
        self.records
            .values()
            .filter(|r| r.status == LostStatus::Active)
    }

    /// Number of records in the store.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Returns true if the store is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_common::make_report;

    #[test]
    fn insert_and_get() {
        let mut store = ReportStore::new();
        let r = make_report("r1");
        store.insert(r.clone()).expect("first insert succeeds");
        let found = store.get("r1").expect("record present");
        assert_eq!(found.report_id, "r1");
    }

    #[test]
    fn duplicate_insert_rejected() {
        let mut store = ReportStore::new();
        store.insert(make_report("r1")).unwrap();
        let err = store.insert(make_report("r1")).unwrap_err();
        assert!(matches!(err, SubmitError::Duplicate(_)));
    }

    #[test]
    fn delete_removes_record() {
        let mut store = ReportStore::new();
        store.insert(make_report("r1")).unwrap();
        store.delete("r1").expect("delete ok");
        assert!(store.get("r1").is_none());
    }

    #[test]
    fn delete_missing_returns_err() {
        let mut store = ReportStore::new();
        let err = store.delete("no-such").unwrap_err();
        assert!(matches!(err, StoreUpdateError::NotFound(_)));
    }

    #[test]
    fn expire_stale_removes_active_past_ttl() {
        use chrono::Duration;
        let now = Utc::now();
        let mut store = ReportStore::new();
        let mut r = make_report("r1");
        r.expires = now - Duration::seconds(1); // already expired
        store.insert(r).unwrap();
        let expired = store.expire_stale(now);
        assert_eq!(expired, vec!["r1"]);
        assert!(store.get("r1").is_none());
    }

    #[test]
    fn expire_stale_leaves_future_reports() {
        use chrono::Duration;
        let now = Utc::now();
        let mut store = ReportStore::new();
        let mut r = make_report("r2");
        r.expires = now + Duration::days(10); // not yet expired
        store.insert(r).unwrap();
        let expired = store.expire_stale(now);
        assert!(expired.is_empty());
        assert!(store.get("r2").is_some());
    }
}
