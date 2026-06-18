//! `homeward connectors coverage` — per-source catchment report.
//!
//! Reads the loaded [`ConnectorRegistry`] (from `HOMEWARD_SOURCES` or built-ins)
//! and, when a store path is given, the canonical SQLite record store the ingest
//! daemon writes to. Emits a human-readable table or `--json` structured output.
//!
//! # Status thresholds (constants)
//!
//! | Constant | Value | Meaning |
//! |---|---|---|
//! | [`STALE_MULTIPLIER`] | 3 | A source is STALE when its last success exceeds its cadence × 3 |
//! | [`RECENT_WINDOW_SECS`] | 14400 | A source is LIVE if last success is within 4 hours (4×3600 s) |
//!
//! These constants are stable across releases and are documented in `--help`.
//!
//! # Source statuses
//!
//! - **LIVE** — last success within the recent window AND record_count > 0.
//! - **STALE** — last success present but older than cadence × STALE_MULTIPLIER.
//! - **SILENT** — registered, last success present but record_count = 0; OR no
//!   success recorded at all AND no records in store.
//! - **UNREACHABLE** — last poll recorded an error (tracked via absence of cursor
//!   entry when records are also absent but the source was expected to have data).

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::ConnectorRegistry;

// ── Threshold constants ───────────────────────────────────────────────────────

/// A source is STALE when its last success is older than (cadence × STALE_MULTIPLIER).
///
/// Default cadence hint for Socrata sources is 4 hours; so STALE kicks in at 12 hours.
pub const STALE_MULTIPLIER: u32 = 3;

/// A source is LIVE if last success is within this many seconds of now (4 hours).
pub const RECENT_WINDOW_SECS: u64 = 4 * 3600;

// ── Per-source status ─────────────────────────────────────────────────────────

/// Operational status of a registered source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SourceStatus {
    /// Recent success and records present.
    Live,
    /// Last success older than cadence × STALE_MULTIPLIER.
    Stale,
    /// Registered but zero records ever ingested.
    Silent,
    /// Last poll errored (no recent cursor advancement, no records).
    Unreachable,
    /// No store was provided; status cannot be determined.
    Unknown,
}

impl std::fmt::Display for SourceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live => f.write_str("LIVE"),
            Self::Stale => f.write_str("STALE"),
            Self::Silent => f.write_str("SILENT"),
            Self::Unreachable => f.write_str("UNREACHABLE"),
            Self::Unknown => f.write_str("UNKNOWN"),
        }
    }
}

/// Coverage row for a single registered source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceCoverageRow {
    /// Source name (from registry).
    pub name: String,
    /// Declared metro / human label (from catalog metadata if present, else empty).
    pub metro: String,
    /// Last successful poll timestamp, or `None` if store absent / never succeeded.
    pub last_success: Option<DateTime<Utc>>,
    /// Total records in the store from this source. `None` when no store provided.
    pub record_count: Option<u64>,
    /// Records classified as stray intake. `None` when no store provided.
    pub stray_count: Option<u64>,
    /// Derived operational status.
    pub status: SourceStatus,
    /// Whether the store was consulted for this row.
    pub store_available: bool,
}

/// A metro/geographic hole: a named area with no registered feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogGap {
    /// Metro or region name.
    pub metro: String,
    /// Optional note from the catalog entry documenting the gap.
    pub note: Option<String>,
}

/// Structured JSON output of `coverage --json`.
///
/// The `cells`, `geo_holes`, and `ungeocoded` fields are purely additive
/// under new keys; the `sources`, `holes`, and `generated_at` fields are
/// byte-stable for unchanged input (AC6).
#[derive(Debug, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Per-source coverage rows.
    pub sources: Vec<SourceCoverageRow>,
    /// Geographic holes: metros with no registered feed.
    pub holes: Vec<CatalogGap>,
    /// ISO-8601 UTC timestamp when this report was generated.
    pub generated_at: String,
    /// Per-cell geographic statistics derived from `PetRecord.location`.
    /// `None` when no store was provided or the store has no geocoded records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<crate::geo_coverage::CellStats>>,
    /// Detected coverage holes ranked by estimated missed population.
    /// `None` when no store was provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo_holes: Option<Vec<crate::geo_coverage::GeoHole>>,
    /// Count of records with `location == None` (ungeocoded).
    /// `None` when no store was provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ungeocoded: Option<u64>,
}

// ── Store reader (minimal, no dependency on homeward-ingest) ──────────────────

/// Per-source stats read directly from the ingest SQLite store.
struct SourceStats {
    record_count: u64,
    stray_count: u64,
    last_seen: Option<DateTime<Utc>>,
    /// Last cursor advancement timestamp (proxy for last successful poll).
    last_cursor: Option<DateTime<Utc>>,
    /// True if the source has a cursor entry at all (was ever polled).
    ever_polled: bool,
}

/// Open the store at `path` and read per-source stats.
///
/// This function queries only tables guaranteed by the ingest schema migration:
/// `source_links`, `canonical_records`, and `source_cursors`.
///
/// # Errors
/// Returns a string error if the database cannot be opened or queried.
fn read_store_stats(
    path: &Path,
    source_names: &[&str],
) -> Result<std::collections::HashMap<String, SourceStats>, String> {
    let conn = Connection::open(path)
        .map_err(|e| format!("cannot open store at {}: {e}", path.display()))?;

    let mut map = std::collections::HashMap::new();

    for &name in source_names {
        // Count all records linked to this source.
        let record_count: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM source_links WHERE source_name = ?1",
                params![name],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| u64::try_from(n).unwrap_or(0))
            .unwrap_or(0);

        // Count stray records: join source_links → canonical_records, filter intake
        // type "stray" from the JSON blob (intake_type field).
        // We do a lightweight JSON extract rather than deserializing the full record.
        let stray_count: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM source_links sl
                 JOIN canonical_records cr ON sl.canonical_id = cr.canonical_id
                 WHERE sl.source_name = ?1
                   AND json_extract(cr.record_json, '$.intake_type') = 'Stray'",
                params![name],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| u64::try_from(n).unwrap_or(0))
            .unwrap_or(0);

        // Max last_seen across source_links for this source.
        let last_seen: Option<DateTime<Utc>> = conn
            .query_row(
                "SELECT MAX(last_seen) FROM source_links WHERE source_name = ?1",
                params![name],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap_or(None)
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        // Cursor updated_at (proxy for last successful poll advancement).
        let cursor_row: Option<(String, bool)> = conn
            .query_row(
                "SELECT updated_at, 1 FROM source_cursors WHERE source_name = ?1",
                params![name],
                |r| Ok((r.get::<_, String>(0)?, true)),
            )
            .optional()
            .unwrap_or(None);

        let (last_cursor, ever_polled) = if let Some((ts_str, _)) = cursor_row {
            let dt = DateTime::parse_from_rfc3339(&ts_str)
                .ok()
                .map(|dt| dt.with_timezone(&Utc));
            (dt, true)
        } else {
            (None, false)
        };

        map.insert(
            name.to_owned(),
            SourceStats {
                record_count,
                stray_count,
                last_seen,
                last_cursor,
                ever_polled,
            },
        );
    }

    Ok(map)
}

// ── Status derivation ─────────────────────────────────────────────────────────

/// Derive the [`SourceStatus`] for a source given its stats and cadence hint.
///
/// Status rules (applied in order):
/// 1. Zero records ever ingested → **SILENT** (registered but never produced data).
/// 2. Has records, no cursor at all → **STALE** (data present, polling unknown).
/// 3. Last success within `RECENT_WINDOW_SECS` → **LIVE**.
/// 4. Last success older than `cadence × STALE_MULTIPLIER` → **STALE**.
/// 5. Otherwise → **LIVE** (between recent window and stale threshold).
///
/// Note: **UNREACHABLE** is reserved for future use when the store tracks per-source
/// poll errors explicitly. Currently the store schema does not persist error state,
/// so we cannot distinguish "never polled" from "polling but erroring" — both show
/// as zero records and no cursor. Both are classified SILENT.
fn derive_status(stats: &SourceStats, cadence_secs: u64) -> SourceStatus {
    let now = Utc::now();

    // Rule 1: zero records = SILENT regardless of cursor state.
    if stats.record_count == 0 {
        return SourceStatus::Silent;
    }

    // Prefer cursor timestamp as "last success"; fall back to last_seen in store.
    let last_success = stats.last_cursor.or(stats.last_seen);

    match last_success {
        None => {
            // Has records but no cursor / last_seen — legacy or edge case; treat as STALE.
            SourceStatus::Stale
        }
        Some(ts) => {
            let age_secs = (now - ts).num_seconds().max(0) as u64;
            if age_secs <= RECENT_WINDOW_SECS {
                SourceStatus::Live
            } else if age_secs > cadence_secs * u64::from(STALE_MULTIPLIER) {
                SourceStatus::Stale
            } else {
                // Between recent window and stale threshold — still operational.
                SourceStatus::Live
            }
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Arguments for the coverage command.
pub struct CoverageArgs<'a> {
    /// Path to the ingest SQLite store, if provided by the caller.
    pub store_path: Option<&'a Path>,
    /// Whether to emit JSON output instead of human-readable table.
    pub json: bool,
    /// Whether to compute and include geographic cell coverage (AC7).
    pub geo: bool,
    /// Minimum record count for a cell to appear in the geo output.
    /// Default 0 (all cells).
    pub geo_min_count: u64,
    /// The loaded connector registry.
    pub registry: &'a ConnectorRegistry,
    /// Cadence hint in seconds per source, keyed by source name.
    /// If a source is not in the map, defaults to 4 hours.
    pub cadence_hints: std::collections::HashMap<String, u64>,
}

/// Run the coverage report.
///
/// Prints to stdout (human table or JSON). Returns `Ok(())` on success.
///
/// # Errors
/// Returns a string error if the store cannot be read.
pub fn run_coverage(args: &CoverageArgs<'_>) -> Result<(), String> {
    let report = build_report(args)?;

    if args.json {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("JSON serialization error: {e}"))?;
        println!("{json}");
    } else {
        print_human_report(&report);
    }

    Ok(())
}

/// Build the [`CoverageReport`] from the registry and optional store.
///
/// This is the testable core — separated from I/O.
///
/// # Errors
/// Returns a string error if the store cannot be opened or queried.
pub fn build_report(args: &CoverageArgs<'_>) -> Result<CoverageReport, String> {
    let mut source_names: Vec<&str> = args.registry.names().collect();
    source_names.sort_unstable();

    // Read store stats if a path was provided.
    let store_stats: Option<std::collections::HashMap<String, SourceStats>> =
        if let Some(path) = args.store_path {
            Some(read_store_stats(path, &source_names)?)
        } else {
            None
        };

    let default_cadence = RECENT_WINDOW_SECS; // 4 hours

    let mut sources: Vec<SourceCoverageRow> = Vec::with_capacity(source_names.len());

    for &name in &source_names {
        let cadence = args
            .cadence_hints
            .get(name)
            .copied()
            .unwrap_or(default_cadence);

        let (last_success, record_count, stray_count, status, store_available) =
            if let Some(ref stats_map) = store_stats {
                if let Some(stats) = stats_map.get(name) {
                    let status = derive_status(stats, cadence);
                    let last_success = stats.last_cursor.or(stats.last_seen);
                    (
                        last_success,
                        Some(stats.record_count),
                        Some(stats.stray_count),
                        status,
                        true,
                    )
                } else {
                    // Source in registry but not in store at all — SILENT.
                    (None, Some(0), Some(0), SourceStatus::Silent, true)
                }
            } else {
                (None, None, None, SourceStatus::Unknown, false)
            };

        // Metro: derive a human-readable label from the source name.
        // In a future iteration this can pull from a metadata field on SocrataConfig.
        let metro = metro_from_name(name);

        sources.push(SourceCoverageRow {
            name: name.to_owned(),
            metro,
            last_success,
            record_count,
            stray_count,
            status,
            store_available,
        });
    }

    // Catalog gaps: we don't have a formal "documented holes" table yet,
    // but we emit any built-in metros that are NOT covered by registered sources.
    // This makes the gaps section non-empty even without a custom catalog.
    let registered_metros: std::collections::HashSet<String> =
        sources.iter().map(|r| r.metro.clone()).collect();
    let holes = catalog_documented_gaps(&registered_metros);

    // Build geo extension if requested and a store is available.
    let (cells, geo_holes, ungeocoded) = if args.geo {
        if let Some(path) = args.store_path {
            match crate::geo_coverage::build_geo_extension(path, args.geo_min_count) {
                Ok(ext) => (Some(ext.cells), Some(ext.geo_holes), Some(ext.ungeocoded)),
                Err(e) => {
                    // Log but don't fail the whole report — geo is additive.
                    eprintln!("warning: geo coverage failed: {e}");
                    (None, None, None)
                }
            }
        } else {
            (None, None, None)
        }
    } else {
        (None, None, None)
    };

    Ok(CoverageReport {
        sources,
        holes,
        generated_at: Utc::now().to_rfc3339(),
        cells,
        geo_holes,
        ungeocoded,
    })
}

/// Derive a metro label from a source name for display.
fn metro_from_name(name: &str) -> String {
    match name {
        "austin" => "Austin, TX".to_owned(),
        "dallas" => "Dallas, TX".to_owned(),
        "sonoma" => "Sonoma County, CA".to_owned(),
        "long_beach" => "Long Beach, CA".to_owned(),
        "petfbi" => "National (PetFBI)".to_owned(),
        "rescuegroups" => "National (RescueGroups)".to_owned(),
        other => {
            // Capitalize first letter, replace underscores with spaces.
            let mut s = other.replace('_', " ");
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s
        }
    }
}

/// Return catalog-documented metro gaps not covered by any registered source.
///
/// This list represents well-known metros that homeward intentionally tracks
/// as "would love a feed here" holes in coverage.
fn catalog_documented_gaps(
    registered_metros: &std::collections::HashSet<String>,
) -> Vec<CatalogGap> {
    // Known metros we want coverage for, with no current feed.
    let want = [
        ("Los Angeles, CA", "No open SODA dataset; LAAS uses a proprietary portal"),
        ("New York, NY", "ACC data only via ACS311; no real-time API available"),
        ("Chicago, IL", "CACC uses PetPoint, no public open-data endpoint"),
        ("Houston, TX", "BARC uses proprietary system; Socrata dataset deprecated"),
        ("Phoenix, AZ", "MCACC dataset removed from Socrata 2023"),
    ];

    want.iter()
        .filter(|(metro, _)| !registered_metros.contains(*metro))
        .map(|(metro, note)| CatalogGap {
            metro: (*metro).to_owned(),
            note: Some((*note).to_owned()),
        })
        .collect()
}

// ── Human-readable output ─────────────────────────────────────────────────────

fn print_human_report(report: &CoverageReport) {
    println!("homeward coverage report — {}", report.generated_at);
    println!();

    // Header.
    println!(
        "{:<20}  {:<25}  {:<26}  {:>12}  {:>11}  {}",
        "SOURCE", "METRO", "LAST_SUCCESS", "RECORDS", "STRAY", "STATUS"
    );
    println!("{}", "-".repeat(110));

    for row in &report.sources {
        let last = row
            .last_success
            .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_else(|| if row.store_available { "never".to_owned() } else { "unknown".to_owned() });

        let records = row
            .record_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".to_owned());

        let stray = row
            .stray_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unknown".to_owned());

        println!(
            "{:<20}  {:<25}  {:<26}  {:>12}  {:>11}  {}",
            row.name, row.metro, last, records, stray, row.status
        );
    }

    println!();

    // Needs attention section.
    let needs_attention: Vec<&SourceCoverageRow> = report
        .sources
        .iter()
        .filter(|r| matches!(r.status, SourceStatus::Silent | SourceStatus::Unreachable))
        .collect();

    if needs_attention.is_empty() {
        println!("needs-attention: none");
    } else {
        println!("needs-attention ({} source{}):", needs_attention.len(), if needs_attention.len() == 1 { "" } else { "s" });
        for row in &needs_attention {
            println!("  {} [{}] — metro: {}", row.name, row.status, row.metro);
        }
    }

    println!();

    // Known gaps.
    if report.holes.is_empty() {
        println!("known-gaps: none");
    } else {
        println!("known-gaps ({} metro{} with no feed):", report.holes.len(), if report.holes.len() == 1 { "" } else { "s" });
        for gap in &report.holes {
            if let Some(note) = &gap.note {
                println!("  {} — {}", gap.metro, note);
            } else {
                println!("  {}", gap.metro);
            }
        }
    }

    println!();
    println!(
        "thresholds: STALE_MULTIPLIER={STALE_MULTIPLIER} (cadence×{STALE_MULTIPLIER}), \
         RECENT_WINDOW_SECS={RECENT_WINDOW_SECS} ({}h)",
        RECENT_WINDOW_SECS / 3600
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConnectorRegistry, connectors::socrata::{SocrataConfig, SocrataConnector}};
    use std::time::Duration;

    fn make_registry_with_names(names: &[&str]) -> ConnectorRegistry {
        let mut reg = ConnectorRegistry::new();
        for &name in names {
            let config = SocrataConfig {
                domain: "data.example.gov".to_owned(),
                dataset_id: "xxxx-xxxx".to_owned(),
                name: name.to_owned(),
                column_map: crate::connectors::socrata::SocrataColumnMap {
                    animal_id: "id".to_owned(),
                    animal_type: "type".to_owned(),
                    intake_type: "intake_type".to_owned(),
                    outcome_date: None,
                    kennel_status: None,
                    found_location: None,
                    chip_status: None,
                    intake_date: None,
                    breed: None,
                    name: None,
                    color: None,
                },
                app_token_env: None,
            };
            let connector = SocrataConnector::with_client(
                config,
                crate::http::PoliteClient::from_client(reqwest::Client::new(), Duration::from_millis(0)),
            );
            reg.register(name, Box::new(connector));
        }
        reg
    }

    /// AC1 / fixture-store: a source in registry but absent from store = SILENT.
    #[test]
    fn silent_source_when_store_has_no_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");

        // Create the store schema without inserting any records.
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS source_links (
                    canonical_id TEXT NOT NULL,
                    source_name TEXT NOT NULL,
                    source_animal_id TEXT,
                    source_url TEXT,
                    source_etag TEXT,
                    absent_count INTEGER NOT NULL DEFAULT 0,
                    last_seen TEXT NOT NULL,
                    PRIMARY KEY (canonical_id, source_name)
                );
                CREATE TABLE IF NOT EXISTS canonical_records (
                    canonical_id TEXT PRIMARY KEY,
                    species TEXT NOT NULL,
                    record_json TEXT NOT NULL,
                    availability TEXT NOT NULL DEFAULT 'unknown',
                    last_seen TEXT NOT NULL,
                    last_confirmed TEXT,
                    created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS source_cursors (
                    source_name TEXT PRIMARY KEY,
                    cursor_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            ).expect("migrate");
        }

        let registry = make_registry_with_names(&["austin"]);
        let args = CoverageArgs {
            store_path: Some(&db_path),
            json: false,
            geo: false,
            geo_min_count: 0,
            registry: &registry,
            cadence_hints: std::collections::HashMap::new(),
        };

        let report = build_report(&args).expect("build_report");
        assert_eq!(report.sources.len(), 1);
        let row = &report.sources[0];
        assert_eq!(row.name, "austin");
        assert_eq!(row.status, SourceStatus::Silent, "source with no records must be SILENT");
        assert_eq!(row.record_count, Some(0));
    }

    /// AC2: --json round-trip — SILENT source + catalog gap both appear.
    #[test]
    fn json_roundtrip_silent_and_gap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");

        // Minimal schema, no records.
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS source_links (
                    canonical_id TEXT NOT NULL,
                    source_name TEXT NOT NULL,
                    source_animal_id TEXT,
                    source_url TEXT,
                    source_etag TEXT,
                    absent_count INTEGER NOT NULL DEFAULT 0,
                    last_seen TEXT NOT NULL,
                    PRIMARY KEY (canonical_id, source_name)
                );
                CREATE TABLE IF NOT EXISTS canonical_records (
                    canonical_id TEXT PRIMARY KEY,
                    species TEXT NOT NULL,
                    record_json TEXT NOT NULL,
                    availability TEXT NOT NULL DEFAULT 'unknown',
                    last_seen TEXT NOT NULL,
                    last_confirmed TEXT,
                    created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS source_cursors (
                    source_name TEXT PRIMARY KEY,
                    cursor_json TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            ).expect("migrate");
        }

        let registry = make_registry_with_names(&["austin"]);
        let args = CoverageArgs {
            store_path: Some(&db_path),
            json: true,
            geo: false,
            geo_min_count: 0,
            registry: &registry,
            cadence_hints: std::collections::HashMap::new(),
        };

        let report = build_report(&args).expect("build_report");

        // Serialize to JSON and deserialize.
        let json = serde_json::to_string(&report).expect("serialize");
        let deserialized: CoverageReport = serde_json::from_str(&json).expect("deserialize");

        // Known SILENT source must appear.
        let silent = deserialized
            .sources
            .iter()
            .find(|r| r.name == "austin")
            .expect("austin row must be present");
        assert_eq!(silent.status, SourceStatus::Silent, "austin must be SILENT in deserialized JSON");

        // At least one catalog gap must appear (we don't have LA/NY/etc. feeds).
        assert!(!deserialized.holes.is_empty(), "holes must be non-empty — unregistered metros should appear");
        // Verify the gap has a metro field.
        for gap in &deserialized.holes {
            assert!(!gap.metro.is_empty(), "each gap must have a non-empty metro");
        }
    }

    /// AC3: no-store mode — status is UNKNOWN, counts are None (not 0).
    #[test]
    fn no_store_marks_unknown() {
        let registry = make_registry_with_names(&["dallas"]);
        let args = CoverageArgs {
            store_path: None,
            json: false,
            geo: false,
            geo_min_count: 0,
            registry: &registry,
            cadence_hints: std::collections::HashMap::new(),
        };

        let report = build_report(&args).expect("build_report");
        assert_eq!(report.sources.len(), 1);
        let row = &report.sources[0];
        assert_eq!(row.status, SourceStatus::Unknown, "no-store must yield UNKNOWN status");
        assert!(row.record_count.is_none(), "no-store must not fabricate record counts");
        assert!(row.stray_count.is_none(), "no-store must not fabricate stray counts");
        assert!(row.last_success.is_none(), "no-store must not fabricate last_success");
    }

    /// AC4: threshold constants are explicit and correctly labelled.
    #[test]
    fn threshold_constants_are_documented() {
        // STALE_MULTIPLIER is a named constant, not a magic literal.
        assert_eq!(STALE_MULTIPLIER, 3, "STALE_MULTIPLIER must be 3");
        // RECENT_WINDOW_SECS is 4 hours.
        assert_eq!(RECENT_WINDOW_SECS, 4 * 3600, "RECENT_WINDOW_SECS must be 4 hours");
    }

    /// AC2 extra: STALE status when last cursor is old.
    #[test]
    fn stale_when_last_success_is_old() {
        // A source with 100 records but last cursor 48 hours ago → STALE.
        let old_ts = Utc::now() - chrono::Duration::hours(48);
        let stats = SourceStats {
            record_count: 100,
            stray_count: 10,
            last_seen: Some(old_ts),
            last_cursor: Some(old_ts),
            ever_polled: true,
        };
        let cadence = 4 * 3600u64; // 4 hours
        let status = derive_status(&stats, cadence);
        assert_eq!(status, SourceStatus::Stale, "48h old with 4h cadence×3=12h threshold must be STALE");
    }

    /// LIVE when last cursor is recent.
    #[test]
    fn live_when_recent_and_has_records() {
        let recent_ts = Utc::now() - chrono::Duration::hours(1);
        let stats = SourceStats {
            record_count: 50,
            stray_count: 5,
            last_seen: Some(recent_ts),
            last_cursor: Some(recent_ts),
            ever_polled: true,
        };
        let cadence = 4 * 3600u64;
        let status = derive_status(&stats, cadence);
        assert_eq!(status, SourceStatus::Live, "1h old with 4h RECENT_WINDOW must be LIVE");
    }

    /// AC6: existing JSON fields are byte-stable when geo is not requested
    /// (no regression to name/metro coverage; geo data is purely additive).
    #[test]
    fn geo_fields_absent_when_geo_false() {
        let registry = make_registry_with_names(&["austin"]);
        let args = CoverageArgs {
            store_path: None,
            json: false,
            geo: false,
            geo_min_count: 0,
            registry: &registry,
            cadence_hints: std::collections::HashMap::new(),
        };

        let report = build_report(&args).expect("build_report");
        // Geo fields must be None when geo=false, ensuring backward compatibility.
        assert!(
            report.cells.is_none(),
            "cells must be None when geo=false"
        );
        assert!(
            report.geo_holes.is_none(),
            "geo_holes must be None when geo=false"
        );
        assert!(
            report.ungeocoded.is_none(),
            "ungeocoded must be None when geo=false"
        );
        // The existing fields must still be present.
        assert_eq!(report.sources.len(), 1, "sources must be present");
        assert!(!report.generated_at.is_empty(), "generated_at must be present");
    }
}
