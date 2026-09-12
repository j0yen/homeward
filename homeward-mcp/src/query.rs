//! Read-only ingest-DB loader for `homeward-mcp`.
//!
//! Mirrors the `canonical_records` table contract used by
//! `homeward_report::db_reader::IngestDbReader` (`SELECT canonical_id,
//! species, record_json, availability, last_seen`). That reader's loader
//! (`fn load_all`) is private to its crate, so it can't be called directly
//! from here; the query below is a small, deliberately-attributed mirror
//! of the same schema contract rather than a reinvention of it.
//!
//! Every tool call opens a fresh read-only connection instead of relying on
//! an in-memory cache: AC5 requires a DB-absent tool call to fail cleanly
//! and distinctly from "zero matching rows" (AC3), and a background
//! polling cache (`IngestDbReader`'s own 5-minute-refresh pattern) would
//! leave that distinction racing a stale in-memory snapshot instead of the
//! live file.

use std::path::{Path, PathBuf};

use homeward_schema::PetRecord;
use rusqlite::{Connection, OpenFlags};

/// Resolve the ingest DB path.
///
/// Same env var / default convention as `IngestDbReader::new()` (mirrored
/// here because that resolution is private there): `HOMEWARD_INGEST_DB` if
/// set, else `$HOME/.local/share/homeward/homeward-ingest.db`.
#[must_use]
#[allow(clippy::map_unwrap_or)]
pub fn resolve_db_path() -> PathBuf {
    std::env::var("HOMEWARD_INGEST_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
            PathBuf::from(home).join(".local/share/homeward/homeward-ingest.db")
        })
}

/// Open a fresh read-only connection and drop it immediately.
///
/// A live reachability probe (AC5), distinct from reading the in-memory
/// cache: this always reflects whether the DB file is *currently* openable.
#[must_use]
pub fn db_reachable(db_path: &Path) -> bool {
    open_readonly(db_path).is_ok()
}

fn open_readonly(db_path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open {}: {e}", db_path.display()))
}

/// Load every shelter intake record from the ingest DB, read-only.
///
/// # Errors
/// Returns `Err` with a caller-facing message (never a panic) if the DB
/// file is absent, locked, or the schema doesn't match. The caller (AC5)
/// turns this into a typed tool error rather than crashing the process.
pub fn load_records(db_path: &Path) -> Result<Vec<PetRecord>, String> {
    let conn = open_readonly(db_path)?;

    let mut stmt = conn
        .prepare(
            "SELECT canonical_id, species, record_json, availability, last_seen \
             FROM canonical_records \
             ORDER BY last_seen DESC",
        )
        .map_err(|e| format!("prepare: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            let record_json: String = row.get(2)?;
            serde_json::from_str::<PetRecord>(&record_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .map_err(|e| format!("query: {e}"))?;

    let mut records = Vec::new();
    for row in rows {
        match row {
            Ok(record) => records.push(record),
            Err(e) => tracing::warn!("skipping malformed ingest row: {e}"),
        }
    }
    Ok(records)
}
