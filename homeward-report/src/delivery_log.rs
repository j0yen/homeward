//! Append-only delivery ledger for match-alert delivery records.
//!
//! Records every delivery attempt with `{alert_id, report_id, deliverer,
//! outcome, ts}`. The file is opened with `O_APPEND` so concurrent writes
//! from different processes are safe on POSIX filesystems.
//!
//! The ledger also provides an in-memory mode for tests (`DeliveryLedger::in_memory()`).

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ─── Record ────────────────────────────────────────────────────────────────────

/// A single delivery-attempt record written to the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryRecord {
    /// Stable alert ID derived from `(report_id, candidate_id)`.
    pub alert_id: String,
    /// The report this alert is for.
    pub report_id: String,
    /// Which deliverer was used (e.g., `"DryRun"`, `"RelayEmail"`).
    pub deliverer: String,
    /// Outcome label (e.g., `"DryRun"`, `"Sent"`, `"Suppressed"`, `"Failed"`).
    pub outcome: String,
    /// When the attempt was made.
    pub ts: DateTime<Utc>,
}

// ─── Ledger backing ────────────────────────────────────────────────────────────

enum LedgerBacking {
    /// Persistent file at the given path (O_APPEND).
    File(PathBuf),
    /// In-memory for tests.
    Memory(Vec<DeliveryRecord>),
}

// ─── Ledger ───────────────────────────────────────────────────────────────────

/// An append-only delivery ledger.
///
/// Use [`DeliveryLedger::open`] for persistent storage or
/// [`DeliveryLedger::in_memory`] for tests.
pub struct DeliveryLedger {
    backing: LedgerBacking,
}

impl DeliveryLedger {
    /// Open (or create) a persistent ledger at `path`.
    ///
    /// The file is written with `O_APPEND` to allow concurrent writers.
    ///
    /// # Errors
    /// Returns an `io::Error` if the path cannot be created or opened.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path: PathBuf = path.into();
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Pre-open to validate the path is writable
        let _ = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            backing: LedgerBacking::File(path),
        })
    }

    /// Create an in-memory ledger for testing.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            backing: LedgerBacking::Memory(Vec::new()),
        }
    }

    /// Default path: `$HOMEWARD_DATA_DIR/delivery.log` or
    /// `~/.local/share/homeward/delivery.log`.
    #[must_use]
    pub fn default_path() -> PathBuf {
        if let Ok(dir) = std::env::var("HOMEWARD_DATA_DIR") {
            return PathBuf::from(dir).join("delivery.log");
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("homeward")
            .join("delivery.log")
    }

    /// Append a [`DeliveryRecord`] to the ledger.
    ///
    /// Each record is written as a single newline-terminated JSON line
    /// (NDJSON/JSON-Lines format). The `O_APPEND` flag on file backing ensures
    /// atomic line writes on POSIX for lines under PIPE_BUF (~4 KiB).
    ///
    /// # Panics
    /// Panics if JSON serialisation fails (which would indicate a bug in
    /// `DeliveryRecord`'s `Serialize` impl).
    pub fn append(&mut self, record: DeliveryRecord) {
        match &mut self.backing {
            LedgerBacking::Memory(v) => {
                v.push(record);
            }
            LedgerBacking::File(path) => {
                let mut line = serde_json::to_string(&record)
                    .expect("DeliveryRecord must be serializable");
                line.push('\n');
                // O_APPEND: each write atomically appends on POSIX.
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
    }

    /// Returns true if the ledger already has a non-suppressed entry for `alert_id`.
    ///
    /// Used by deliverers to enforce at-most-once delivery (AC4).
    #[must_use]
    pub fn has_delivered(&self, alert_id: &str) -> bool {
        match &self.backing {
            LedgerBacking::Memory(v) => v
                .iter()
                .any(|r| r.alert_id == alert_id && r.outcome != "Suppressed"),
            LedgerBacking::File(path) => {
                // Re-read the file on each check so persistent ledgers are
                // correctly consulted. For high-throughput use, callers should
                // maintain an in-process seen-set.
                if let Ok(content) = std::fs::read_to_string(path) {
                    return content.lines().any(|line| {
                        serde_json::from_str::<DeliveryRecord>(line)
                            .map(|r| r.alert_id == alert_id && r.outcome != "Suppressed")
                            .unwrap_or(false)
                    });
                }
                false
            }
        }
    }

    /// Return all records currently in the ledger.
    ///
    /// For in-memory ledgers this is the full slice. For file-backed ledgers
    /// the file is re-read and parsed (use sparingly — intended for `alerts-log`).
    #[must_use]
    pub fn records(&self) -> Vec<DeliveryRecord> {
        match &self.backing {
            LedgerBacking::Memory(v) => v.clone(),
            LedgerBacking::File(path) => {
                let Ok(content) = std::fs::read_to_string(path) else {
                    return vec![];
                };
                content
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(alert_id: &str, outcome: &str) -> DeliveryRecord {
        DeliveryRecord {
            alert_id: alert_id.to_owned(),
            report_id: "r-test".to_owned(),
            deliverer: "DryRun".to_owned(),
            outcome: outcome.to_owned(),
            ts: Utc::now(),
        }
    }

    #[test]
    fn in_memory_append_and_query() {
        let mut ledger = DeliveryLedger::in_memory();
        assert!(!ledger.has_delivered("a1"));

        ledger.append(make_record("a1", "DryRun"));
        assert!(ledger.has_delivered("a1"));

        let records = ledger.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].alert_id, "a1");
    }

    #[test]
    fn suppressed_does_not_count_as_delivered() {
        let mut ledger = DeliveryLedger::in_memory();
        ledger.append(make_record("a2", "Suppressed"));
        // Suppressed entry should NOT trigger has_delivered
        assert!(
            !ledger.has_delivered("a2"),
            "Suppressed entry must not block re-delivery"
        );
    }

    #[test]
    fn file_backed_append_and_query() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("delivery.log");

        let mut ledger = DeliveryLedger::open(&path).expect("open ledger");
        assert!(!ledger.has_delivered("a3"));
        ledger.append(make_record("a3", "Sent"));
        assert!(ledger.has_delivered("a3"));

        // Second ledger instance pointing at same file
        let ledger2 = DeliveryLedger::open(&path).expect("open ledger 2");
        assert!(
            ledger2.has_delivered("a3"),
            "second instance should see entry from first"
        );
        let records = ledger2.records();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn file_backed_multiple_appends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("delivery.log");

        let mut ledger = DeliveryLedger::open(&path).expect("open ledger");
        ledger.append(make_record("a4", "DryRun"));
        ledger.append(make_record("a5", "DryRun"));
        ledger.append(make_record("a4", "Suppressed"));

        let records = ledger.records();
        assert_eq!(records.len(), 3);
        assert!(ledger.has_delivered("a4"));
        assert!(ledger.has_delivered("a5"));
    }
}
