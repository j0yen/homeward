//! `homeward ingest backfill` — one-time historical-population backfill
//! against `RescueGroups`.
//!
//! homeward's forward ingest cursor started 2026-06-13; every animal RG
//! held before that date (or in a page the forward cursor otherwise
//! skipped) was never written to the store. This module walks the
//! **complete** RG dogs+cats population page-by-page (reusing
//! [`RescueGroupsConnector::fetch_normalized_page`] — the exact page-based
//! pagination path `Connector::poll` uses, never `offset`), inserts
//! whatever the store doesn't already hold (keyed on RG's own animal id via
//! [`Store::find_by_source_animal_id`]), and enrolls photo-bearing inserts
//! in the embed index.
//!
//! # Idempotency
//!
//! Per the PRD's non-goal ("re-crawling animals the DB already holds"),
//! idempotency here means **skip, not refresh**: an animal already present
//! (by `(source_name, source_animal_id)`) is counted `skipped` and left
//! untouched — the forward ingest cursor owns keeping it fresh.
//!
//! # Resumability
//!
//! Progress (next page to fetch, per species) is persisted via
//! [`Store::save_cursor`] under [`BACKFILL_CURSOR_KEY`] — a storage key
//! distinct from the forward ingest cursor's (`"rescuegroups"`), so the two
//! never collide. Progress is saved after every page, not just at the end,
//! so a run killed mid-way resumes from its last completed page rather than
//! restarting from page 1.

use std::path::{Path, PathBuf};

use homeward_connectors::{ConnectorError, RescueGroupsConnector};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

use crate::enroll::EnrollSink;
use crate::events::{EventKind, EventSink, IngestEvent};
use crate::store::{Store, StoreError};

/// Cursor storage key for backfill progress. Deliberately distinct from the
/// forward ingest cursor's `"rescuegroups"` key (see module docs).
pub const BACKFILL_CURSOR_KEY: &str = "rescuegroups-backfill";

/// Cursor storage key for the location-backfill pass
/// ([`run_location_backfill`]). Deliberately distinct from both
/// [`BACKFILL_CURSOR_KEY`] (the population backfill) and the forward
/// ingest cursor's `"rescuegroups"` key — all three walk the same RG
/// population independently and must never share progress state.
pub const LOCATION_BACKFILL_CURSOR_KEY: &str = "rescuegroups-location-backfill";

/// RG species query segments this command walks, in order.
const SPECIES: [&str; 2] = ["dogs", "cats"];

/// Errors from a backfill run.
#[derive(Debug, Error)]
pub enum BackfillError {
    /// Store (sqlite/json) error.
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    /// `RescueGroups` connector error (transport, rate-limit-exhausted, parse).
    #[error("connector error: {0}")]
    Connector(#[from] ConnectorError),
    /// Persisted progress failed to (de)serialize.
    #[error("backfill progress (de)serialization error: {0}")]
    Progress(#[from] serde_json::Error),
}

// ─── Progress (persisted) ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct SpeciesProgress {
    /// 1-based page to fetch next.
    next_page: u64,
    /// `true` once this species' population has been fully walked.
    done: bool,
}

impl Default for SpeciesProgress {
    fn default() -> Self {
        Self { next_page: 1, done: false }
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
struct BackfillProgress {
    dogs: SpeciesProgress,
    cats: SpeciesProgress,
}

impl BackfillProgress {
    fn get(&self, species: &str) -> SpeciesProgress {
        if species == "dogs" { self.dogs } else { self.cats }
    }

    fn set(&mut self, species: &str, progress: SpeciesProgress) {
        if species == "dogs" {
            self.dogs = progress;
        } else {
            self.cats = progress;
        }
    }
}

fn load_progress_keyed(store: &Store, key: &str) -> Result<BackfillProgress, BackfillError> {
    match store.load_cursor(key)? {
        Some(cursor) => Ok(serde_json::from_str(&cursor.cursor_json)?),
        None => Ok(BackfillProgress::default()),
    }
}

fn save_progress_keyed(store: &Store, key: &str, progress: &BackfillProgress) -> Result<(), BackfillError> {
    let cursor_json = serde_json::to_string(progress)?;
    store.save_cursor(&crate::store::SourceCursor {
        source_name: key.to_owned(),
        cursor_json,
        updated_at: chrono::Utc::now(),
    })?;
    Ok(())
}

fn load_progress(store: &Store) -> Result<BackfillProgress, BackfillError> {
    load_progress_keyed(store, BACKFILL_CURSOR_KEY)
}

fn save_progress(store: &Store, progress: &BackfillProgress) -> Result<(), BackfillError> {
    save_progress_keyed(store, BACKFILL_CURSOR_KEY, progress)
}

// ─── Report ──────────────────────────────────────────────────────────────────

/// Per-species fetch/insert/skip/fail counts for the completion report.
#[derive(Debug, Default, Clone, Copy)]
pub struct SourceCounts {
    /// Records fetched from RG this run (across all pages).
    pub fetched: u64,
    /// New animals written to the store.
    pub inserted: u64,
    /// Animals already present — left untouched (idempotency = skip, not
    /// refresh).
    pub skipped: u64,
    /// Records that failed to write (fetched successfully, store error).
    pub failed: u64,
}

/// Completion report for a [`run_backfill`] call.
#[derive(Debug, Default)]
pub struct BackfillReport {
    /// Dogs counts.
    pub dogs: SourceCounts,
    /// Cats counts.
    pub cats: SourceCounts,
    /// Total rows in the store after this run.
    pub db_total: u64,
    /// RG-reported total for dogs (`meta.count` from the first page seen).
    pub rg_total_dogs: u64,
    /// RG-reported total for cats.
    pub rg_total_cats: u64,
    /// Newly-inserted, photo-bearing animals handed to the enroll sink.
    pub enroll_candidates: u64,
    /// Length of `id_map.json` read back as a **list** (never a dict — see
    /// module docs / the homeward-ops 2026-09-07 miscount trap), when an
    /// audit path was configured and the file was readable.
    pub enrollment_audit: Option<usize>,
}

impl BackfillReport {
    fn counts_mut(&mut self, species: &str) -> &mut SourceCounts {
        if species == "dogs" { &mut self.dogs } else { &mut self.cats }
    }

    /// RG-reported dogs+cats total (coverage denominator).
    #[must_use]
    pub const fn rg_total(&self) -> u64 {
        self.rg_total_dogs + self.rg_total_cats
    }

    /// Render the completion report as the multi-line human-readable text
    /// `homeward-ingestd backfill` prints (AC6).
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "backfill complete\n\
             dogs:  fetched={} inserted={} skipped={} failed={}\n\
             cats:  fetched={} inserted={} skipped={} failed={}\n\
             db total: {}   rg total: {} (dogs {} + cats {})\n\
             enroll candidates: {}   id_map.json audit: {}",
            self.dogs.fetched, self.dogs.inserted, self.dogs.skipped, self.dogs.failed,
            self.cats.fetched, self.cats.inserted, self.cats.skipped, self.cats.failed,
            self.db_total, self.rg_total(), self.rg_total_dogs, self.rg_total_cats,
            self.enroll_candidates,
            self.enrollment_audit.map_or_else(|| "n/a".to_owned(), |n| n.to_string()),
        )
    }
}

// ─── Dry-run plan ────────────────────────────────────────────────────────────

/// `--dry-run` plan: current coverage + a page-count estimate, computed
/// without writing anything (AC7).
#[derive(Debug, Default)]
pub struct DryRunPlan {
    /// Current row count in the store (read via a read-only connection —
    /// never opens the store for write, so the DB file's mtime is
    /// untouched).
    pub current_coverage: u64,
    /// RG-reported dogs total.
    pub dogs_total: u64,
    /// Estimated dogs pages at RG's page size.
    pub dogs_pages: u64,
    /// RG-reported cats total.
    pub cats_total: u64,
    /// Estimated cats pages at RG's page size.
    pub cats_pages: u64,
}

/// Compute a [`DryRunPlan`] without mutating the store.
///
/// Reads the current coverage via a **read-only** sqlite connection (never
/// `Store::open`, which would run schema migration / set `journal_mode=WAL`
/// on a fresh file) and fetches only page 1 of each species to read RG's
/// reported totals.
///
/// # Errors
/// Propagates [`BackfillError`] on sqlite or connector failures.
pub async fn dry_run_plan(
    db_path: &Path,
    connector: &RescueGroupsConnector,
) -> Result<DryRunPlan, BackfillError> {
    let current_coverage = read_only_count(db_path).unwrap_or(0);

    let dogs = connector.fetch_normalized_page("dogs", 1).await?;
    let cats = connector.fetch_normalized_page("cats", 1).await?;

    Ok(DryRunPlan {
        current_coverage,
        dogs_total: dogs.total,
        dogs_pages: dogs.pages_total,
        cats_total: cats.total,
        cats_pages: cats.pages_total,
    })
}

/// Read the current row count via a read-only connection. Returns `0` (not
/// an error) if the database file does not exist yet — an empty/missing DB
/// has zero coverage, which is a valid dry-run answer, not a failure.
fn read_only_count(db_path: &Path) -> Result<u64, BackfillError> {
    if !db_path.exists() {
        return Ok(0);
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(StoreError::from)?;
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM canonical_records", [], |r| r.get(0))
        .map_err(StoreError::from)?;
    Ok(u64::try_from(n).unwrap_or(0))
}

// ─── Enrollment audit ────────────────────────────────────────────────────────

/// Read `id_map.json` as a **list** and return its length.
///
/// `id_map.json` (the embed sidecar's on-disk index, see
/// `homeward_embed.index.EmbedIndex`) is a JSON array — `[[canonical_id,
/// species], ...]`, one entry per enrolled photo. The known miscount trap
/// (homeward-ops 2026-09-07) is treating it as a dict and counting keys;
/// this parses it explicitly as `Vec<serde_json::Value>` so that mistake
/// cannot silently recur here.
///
/// Returns `None` (not an error — honest degradation, matching the rest of
/// the enrollment path) if the file is absent or unparseable.
#[must_use]
pub fn read_id_map_len(id_map_path: &Path) -> Option<usize> {
    let text = std::fs::read_to_string(id_map_path).ok()?;
    let list: Vec<serde_json::Value> = serde_json::from_str(&text).ok()?;
    Some(list.len())
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for [`run_backfill`].
#[derive(Debug, Default, Clone)]
pub struct BackfillConfig {
    /// Path to the embed sidecar's `id_map.json`, for the post-run
    /// enrollment audit (AC4). `None` skips the audit (`enrollment_audit`
    /// stays `None` in the report).
    pub id_map_path: Option<PathBuf>,
}

// ─── Run ─────────────────────────────────────────────────────────────────────

/// Walk the complete RG dogs+cats population, inserting whatever the store
/// doesn't already hold and enrolling photo-bearing inserts.
///
/// # Errors
/// Propagates [`BackfillError`] on sqlite, connector, or progress
/// (de)serialization failure. On error, progress already persisted for
/// completed pages is left in place — re-running resumes from there.
pub async fn run_backfill(
    store: &mut Store,
    connector: &RescueGroupsConnector,
    cfg: &BackfillConfig,
    enroll_sink: Option<&EnrollSink>,
) -> Result<BackfillReport, BackfillError> {
    let mut progress = load_progress(store)?;
    let mut report = BackfillReport::default();

    for species in SPECIES {
        loop {
            let mut sp = progress.get(species);
            if sp.done {
                break;
            }

            let page = connector.fetch_normalized_page(species, sp.next_page).await?;
            let is_last = page.is_last_page();

            if species == "dogs" {
                report.rg_total_dogs = page.total;
            } else {
                report.rg_total_cats = page.total;
            }

            for record in page.records {
                let counts = report.counts_mut(species);
                counts.fetched += 1;

                let already_present = match record.source_animal_id.as_deref() {
                    Some(sid) => store.find_by_source_animal_id(&record.source.name, sid)?.is_some(),
                    None => false,
                };

                if already_present {
                    report.counts_mut(species).skipped += 1;
                    continue;
                }

                let mut record = record;
                record.canonical_id = Ulid::new();
                let has_photos = !record.photos.is_empty();

                match store.upsert(&record) {
                    Ok(()) => {
                        report.counts_mut(species).inserted += 1;
                        if has_photos {
                            if let Some(sink) = enroll_sink {
                                report.enroll_candidates += 1;
                                sink.publish(IngestEvent::new(EventKind::New, record));
                            }
                        }
                    }
                    Err(_e) => {
                        report.counts_mut(species).failed += 1;
                    }
                }
            }

            sp.next_page += 1;
            sp.done = is_last;
            progress.set(species, sp);
            save_progress(store, &progress)?;

            if is_last {
                break;
            }
        }
    }

    report.db_total = store.count()?;

    // Enrollment is deliberately async/fire-and-forget (see `enroll` module
    // docs) so the poll hot path never blocks on a slow sidecar — meaning
    // the enroll queue may not have drained the instant this loop finishes.
    // Give it a short grace window before reading `id_map.json` back so the
    // audit reflects this run's enrollments rather than racing them. This is
    // best-effort, not a synchronous flush: `EnrollWorker` exposes no join
    // handle to wait on deterministically.
    if report.enroll_candidates > 0 && enroll_sink.is_some() {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    report.enrollment_audit = cfg
        .id_map_path
        .as_deref()
        .and_then(read_id_map_len);

    Ok(report)
}


// ─── Location backfill (PRD-homeward-ingest-location-backfill-missing) ──────
//
// `run_backfill` above only ever INSERTS animals the store doesn't already
// hold; it never revisits the ~185k rows already ingested before the
// connector's `location: None,` bug was fixed (AC1). This pass re-walks the
// same RG population — reusing the identical page-based `fetch_normalized_page`
// path, never `offset` — and for every already-present record whose stored
// `location` is still null, copies over the location the (now-fixed)
// connector mapped for that animal this run. Idempotency here is
// "update-if-missing, never clobber": a record that already carries a
// location (whether from a prior location-backfill run or a fresh forward
// poll) is left untouched, matching AC2's "some source records may
// genuinely lack a location upstream — 100% is not required, but 0% must
// not persist" framing.

/// Per-species counts for [`LocationBackfillReport`].
#[derive(Debug, Default, Clone, Copy)]
pub struct LocationSourceCounts {
    /// Records fetched from RG this run (across all pages).
    pub scanned: u64,
    /// Already-present records whose `location` was null and is now set.
    pub updated: u64,
    /// Already-present records whose `location` was already non-null —
    /// left untouched.
    pub already_had_location: u64,
    /// Records RG itself has no usable location for this run (the mapping
    /// legitimately returned `None`) — left null, not an error.
    pub source_missing_location: u64,
    /// Fetched records with no matching row in the store at all (never
    /// ingested — out of scope for this pass; the population backfill
    /// above owns that gap).
    pub not_in_store: u64,
}

/// Completion report for [`run_location_backfill`].
#[derive(Debug, Default)]
pub struct LocationBackfillReport {
    /// Dogs counts.
    pub dogs: LocationSourceCounts,
    /// Cats counts.
    pub cats: LocationSourceCounts,
    /// `canonical_records` rows with non-null `location` after this run
    /// (via [`Store::count_with_location`]).
    pub db_with_location_after: u64,
    /// Same count before this run started — the delta is this run's own
    /// contribution, distinct from location data that arrived via ordinary
    /// forward polling in between.
    pub db_with_location_before: u64,
}

impl LocationBackfillReport {
    fn counts_mut(&mut self, species: &str) -> &mut LocationSourceCounts {
        if species == "dogs" { &mut self.dogs } else { &mut self.cats }
    }

    /// Render the completion report as human-readable text.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "location-backfill complete\n\
             dogs:  scanned={} updated={} already_had_location={} source_missing_location={} not_in_store={}\n\
             cats:  scanned={} updated={} already_had_location={} source_missing_location={} not_in_store={}\n\
             db rows with location: {} -> {}",
            self.dogs.scanned, self.dogs.updated, self.dogs.already_had_location,
            self.dogs.source_missing_location, self.dogs.not_in_store,
            self.cats.scanned, self.cats.updated, self.cats.already_had_location,
            self.cats.source_missing_location, self.cats.not_in_store,
            self.db_with_location_before, self.db_with_location_after,
        )
    }
}

/// Walk the complete RG dogs+cats population and backfill `location` on
/// already-ingested records that don't have one yet.
///
/// # Errors
/// Propagates [`BackfillError`] on sqlite, connector, or progress
/// (de)serialization failure. On error, progress already persisted for
/// completed pages is left in place — re-running resumes from there.
pub async fn run_location_backfill(
    store: &mut Store,
    connector: &RescueGroupsConnector,
) -> Result<LocationBackfillReport, BackfillError> {
    let mut progress = load_progress_keyed(store, LOCATION_BACKFILL_CURSOR_KEY)?;
    let mut report = LocationBackfillReport::default();
    report.db_with_location_before = store.count_with_location()?;

    for species in SPECIES {
        loop {
            let mut sp = progress.get(species);
            if sp.done {
                break;
            }

            let page = connector.fetch_normalized_page(species, sp.next_page).await?;
            let is_last = page.is_last_page();

            for record in page.records {
                report.counts_mut(species).scanned += 1;

                let Some(sid) = record.source_animal_id.as_deref() else { continue };
                let Some(existing_id) = store.find_by_source_animal_id(&record.source.name, sid)? else {
                    report.counts_mut(species).not_in_store += 1;
                    continue;
                };

                let mut existing = store.get(existing_id)?;
                if existing.location.is_some() {
                    report.counts_mut(species).already_had_location += 1;
                    continue;
                }

                if let Some(loc) = record.location {
                    existing.location = Some(loc);
                    store.upsert(&existing)?;
                    report.counts_mut(species).updated += 1;
                } else {
                    report.counts_mut(species).source_missing_location += 1;
                }
            }

            sp.next_page += 1;
            sp.done = is_last;
            progress.set(species, sp);
            save_progress_keyed(store, LOCATION_BACKFILL_CURSOR_KEY, &progress)?;

            if is_last {
                break;
            }
        }
    }

    report.db_with_location_after = store.count_with_location()?;
    Ok(report)
}
