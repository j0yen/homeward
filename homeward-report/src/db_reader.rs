//! Background task that loads [`PetRecord`]s from the ingest SQLite DB.
//!
//! Reads from `HOMEWARD_INGEST_DB` (default: `$HOME/.local/share/homeward/homeward-ingest.db`).
//! On startup does a full load, then every 5 minutes does a delta load
//! (`WHERE last_seen > ?`) and merges new records into the shared vec.
//! If the DB is absent or locked, logs a warning and leaves intake empty.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use homeward_schema::PetRecord;
use rusqlite::{Connection, OpenFlags, params};
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Background DB reader for loading ingest records into `homeward-reportd`.
pub struct IngestDbReader {
    db_path: std::path::PathBuf,
}

impl IngestDbReader {
    /// Create a new reader.
    ///
    /// `HOMEWARD_INGEST_DB` overrides the default path.
    #[must_use]
    pub fn new() -> Self {
        let db_path = std::env::var("HOMEWARD_INGEST_DB")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
                std::path::PathBuf::from(home)
                    .join(".local/share/homeward/homeward-ingest.db")
            });
        Self { db_path }
    }

    /// Run the background load + refresh loop.
    ///
    /// Performs an initial full load, then polls for new records every 5 minutes.
    /// Never panics — all errors are logged and the loop continues.
    ///
    /// # Panics
    /// Does not panic.
    pub async fn run(self, intake: Arc<RwLock<Vec<PetRecord>>>) {
        // Initial load
        match self.load_all() {
            Ok((records, max_seen)) => {
                let n = records.len();
                {
                    let mut w = intake.write().await;
                    *w = records;
                }
                info!("loaded {n} canonical records from ingest DB");
                self.run_delta_loop(intake, max_seen).await;
            }
            Err(e) => {
                warn!("ingest DB unavailable ({e}); intake will be empty");
                // Still run the delta loop — DB may appear later.
                self.run_delta_loop(intake, None).await;
            }
        }
    }

    /// Blocking delta loop (called from async context via `spawn_blocking` or directly).
    async fn run_delta_loop(
        &self,
        intake: Arc<RwLock<Vec<PetRecord>>>,
        initial_max: Option<DateTime<Utc>>,
    ) {
        let mut max_seen = initial_max;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
            match self.load_delta(max_seen) {
                Ok(new_records) if !new_records.is_empty() => {
                    let n = new_records.len();
                    // Update max_seen from new records
                    if let Some(m) = new_records.iter().map(|r| r.last_seen).max() {
                        max_seen = Some(m);
                    }
                    let mut w = intake.write().await;
                    // Append new records, dedup on canonical_id
                    let existing_ids: std::collections::HashSet<ulid::Ulid> =
                        w.iter().map(|r| r.canonical_id).collect();
                    for r in new_records {
                        if !existing_ids.contains(&r.canonical_id) {
                            w.push(r);
                        }
                    }
                    info!("delta load: added {n} new canonical records from ingest DB");
                }
                Ok(_) => {} // No new records
                Err(e) => {
                    warn!("ingest DB delta load failed: {e}");
                }
            }
        }
    }

    /// Full load: SELECT all records (including departed — Socrata data is historical).
    fn load_all(&self) -> Result<(Vec<PetRecord>, Option<DateTime<Utc>>), String> {
        let conn = self.open_conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT canonical_id, species, record_json, availability, last_seen \
                 FROM canonical_records \
                 ORDER BY last_seen ASC",
            )
            .map_err(|e| format!("prepare load_all: {e}"))?;

        let rows = stmt
            .query_map([], deserialize_row)
            .map_err(|e| format!("query load_all: {e}"))?;

        let mut records = Vec::new();
        let mut max_seen: Option<DateTime<Utc>> = None;
        for row in rows {
            match row {
                Ok(record) => {
                    let ts = record.last_seen;
                    if max_seen.map_or(true, |m| ts > m) {
                        max_seen = Some(ts);
                    }
                    records.push(record);
                }
                Err(e) => {
                    warn!("skipping malformed ingest row: {e}");
                }
            }
        }

        Ok((records, max_seen))
    }

    /// Delta load: SELECT records with last_seen > `after`.
    fn load_delta(
        &self,
        after: Option<DateTime<Utc>>,
    ) -> Result<Vec<PetRecord>, String> {
        let conn = self.open_conn()?;

        let after_str = after
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned());

        let mut stmt = conn
            .prepare(
                "SELECT canonical_id, species, record_json, availability, last_seen \
                 FROM canonical_records \
                 WHERE last_seen > ?1 \
                 ORDER BY last_seen ASC",
            )
            .map_err(|e| format!("prepare load_delta: {e}"))?;

        let rows = stmt
            .query_map(params![after_str], deserialize_row)
            .map_err(|e| format!("query load_delta: {e}"))?;

        let mut records = Vec::new();
        for row in rows {
            match row {
                Ok(record) => records.push(record),
                Err(e) => {
                    warn!("skipping malformed delta row: {e}");
                }
            }
        }
        Ok(records)
    }

    /// Open the ingest DB read-only.
    fn open_conn(&self) -> Result<Connection, String> {
        Connection::open_with_flags(
            &self.db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("open {}: {e}", self.db_path.display()))
    }
}

impl Default for IngestDbReader {
    fn default() -> Self {
        Self::new()
    }
}

/// Deserialize a row from canonical_records into a [`PetRecord`].
fn deserialize_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PetRecord> {
    // Columns: canonical_id, species, record_json, availability, last_seen
    let record_json: String = row.get(2)?;
    serde_json::from_str::<PetRecord>(&record_json)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))
}

// ─── Coverage DB query ────────────────────────────────────────────────────────

/// Status for a source based on `last_seen` freshness.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DbSourceStatus {
    /// `last_seen` within 6 hours.
    Live,
    /// `last_seen` within 48 hours.
    Stale,
    /// `last_seen` older than 48 hours (or no rows).
    Silent,
}

/// One coverage row returned by [`query_coverage`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct DbCoverageItem {
    /// Source name.
    pub source: String,
    /// Operational status.
    pub status: DbSourceStatus,
    /// Total records from this source.
    pub count: i64,
    /// ISO-8601 UTC timestamp of the newest `last_seen` for this source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_seen: Option<String>,
}

/// Query `source_links` for per-source coverage stats.
///
/// Returns empty vec on error (logs warning).
///
/// # Errors
/// Returns `Err(String)` if the DB cannot be opened or queried.
pub fn query_coverage(db_path: &std::path::Path) -> Result<Vec<DbCoverageItem>, String> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open {}: {e}", db_path.display()))?;

    let mut stmt = conn
        .prepare(
            "SELECT source_name, COUNT(*) as cnt, MAX(last_seen) as newest \
             FROM source_links \
             GROUP BY source_name \
             ORDER BY source_name",
        )
        .map_err(|e| format!("prepare coverage: {e}"))?;

    let now = Utc::now();
    let rows = stmt
        .query_map([], |row| {
            let source: String = row.get(0)?;
            let count: i64 = row.get(1)?;
            let newest: Option<String> = row.get(2)?;
            Ok((source, count, newest))
        })
        .map_err(|e| format!("query coverage: {e}"))?;

    let mut items = Vec::new();
    for row in rows {
        match row {
            Ok((source, count, newest_str)) => {
                let status = match &newest_str {
                    None => DbSourceStatus::Silent,
                    Some(s) => match DateTime::parse_from_rfc3339(s) {
                        Err(_) => DbSourceStatus::Silent,
                        Ok(newest) => {
                            let age = now.signed_duration_since(newest.with_timezone(&Utc));
                            if age.num_hours() < 6 {
                                DbSourceStatus::Live
                            } else if age.num_hours() < 48 {
                                DbSourceStatus::Stale
                            } else {
                                DbSourceStatus::Silent
                            }
                        }
                    },
                };
                items.push(DbCoverageItem {
                    source,
                    status,
                    count,
                    newest_seen: newest_str,
                });
            }
            Err(e) => {
                warn!("skipping malformed coverage row: {e}");
            }
        }
    }
    Ok(items)
}
