//! Read-only SQLite access to the homeward ingest DB.
//!
//! Every query here filters to photo-bearing records
//! (`json_array_length(record_json, '$.photos') > 0`) — the wall only ever
//! shows animals with at least one photo. Connections are opened via
//! [`open_readonly`], which never grants write access.

use std::path::Path;
use std::time::Duration;

use homeward_schema::{Availability, PetRecord, Species};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use thiserror::Error;

/// Errors from wall DB operations.
#[derive(Debug, Error)]
pub enum WallDbError {
    /// Underlying sqlite error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// JSON deserialization error on a `record_json` blob.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// A photo-bearing record surfaced to the wall UI.
#[derive(Debug, Clone, Serialize)]
pub struct Buddy {
    /// ULID string primary key (also embedded in `record`).
    pub canonical_id: String,
    /// Species, duplicated here for convenient client-side filtering.
    pub species: Species,
    /// Current availability, duplicated here for convenient client-side filtering.
    pub availability: Availability,
    /// The primary photo URL (first `is_primary` photo, else the first photo).
    pub primary_photo_url: String,
    /// The full parsed record, for the popover.
    pub record: PetRecord,
}

/// One page of buddies plus a cursor for the next page.
#[derive(Debug, Clone, Serialize)]
pub struct BuddyPage {
    /// Buddies in this page, newest first.
    pub items: Vec<Buddy>,
    /// Pass as `?before=` to fetch the next page; `None` when exhausted.
    pub next_cursor: Option<String>,
}

/// Photo-bearing / total counts broken out by species.
#[derive(Debug, Clone, Serialize)]
pub struct SpeciesCounts {
    /// Dogs.
    pub dog: i64,
    /// Cats.
    pub cat: i64,
}

/// Aggregate stats for `GET /api/stats`.
#[derive(Debug, Clone, Serialize)]
pub struct WallStats {
    /// Total records of any kind (photo or no photo).
    pub total: i64,
    /// Records with at least one photo — what the wall actually shows.
    pub photo_bearing: i64,
    /// Total records broken out by species.
    pub by_species: SpeciesCounts,
    /// The newest `created_at` timestamp in the store (RFC 3339), if any rows exist.
    pub newest_created_at: Option<String>,
}

/// Default page size for `GET /api/buddies` when `limit` is omitted.
pub const DEFAULT_PAGE_LIMIT: i64 = 100;
/// Hard cap on page size regardless of the requested `limit`.
pub const MAX_PAGE_LIMIT: i64 = 200;

/// Open the ingest DB read-only, tolerating a live WAL writer on the same file.
///
/// Uses a `file:` URI with `mode=ro&immutable=0` — `immutable=0` tells SQLite
/// the file may still be changing underneath us (the ingest daemon keeps
/// writing), so it must consult the `-wal`/`-shm` files rather than assuming
/// a static snapshot. A five-second busy-timeout absorbs the rare case where
/// a writer holds a lock at the moment we read.
///
/// # Errors
/// Returns [`WallDbError::Sqlite`] if the DB cannot be opened or the
/// busy-timeout cannot be set.
pub fn open_readonly(path: &Path) -> Result<Connection, WallDbError> {
    let uri = format!("file:{}?mode=ro&immutable=0", path.display());
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

/// Pick the photo to feature: the first photo marked `is_primary`, else the
/// first photo in the list. Callers only reach here for photo-bearing
/// records, so an empty result means malformed data — return an empty string
/// rather than fabricating a URL.
fn primary_photo_url(record: &PetRecord) -> String {
    record
        .photos
        .iter()
        .find(|p| p.is_primary)
        .or_else(|| record.photos.first())
        .map(|p| p.url.clone())
        .unwrap_or_default()
}

/// Parse a `(canonical_id, record_json)` row into a [`Buddy`].
///
/// Returns `None` (rather than an error) on malformed JSON — one bad row
/// must not take down a whole page of otherwise-good buddies.
fn parse_buddy(canonical_id: String, record_json: &str) -> Option<Buddy> {
    let record: PetRecord = serde_json::from_str(record_json)
        .map_err(|e| tracing::warn!("skipping malformed wall row {canonical_id}: {e}"))
        .ok()?;
    Some(Buddy {
        canonical_id,
        species: record.species,
        availability: record.availability,
        primary_photo_url: primary_photo_url(&record),
        record,
    })
}

const PHOTO_FILTER: &str = "json_array_length(record_json, '$.photos') > 0";

/// Fetch one cursor-paginated page of photo-bearing buddies, newest first.
///
/// ULIDs sort lexicographically in creation order, so `ORDER BY canonical_id
/// DESC` is "newest first" with no separate timestamp column needed. Pass
/// the last item's `canonical_id` back in as `before` to fetch the next page.
///
/// `limit` is clamped to `[1, `[`MAX_PAGE_LIMIT`]`]`.
///
/// # Errors
/// Returns [`WallDbError::Sqlite`] on a query failure.
pub fn buddies_page(
    conn: &Connection,
    before: Option<&str>,
    limit: i64,
) -> Result<BuddyPage, WallDbError> {
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);
    let fetch = limit + 1; // one extra row to detect "has more"

    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(cursor) = before {
        let sql = format!(
            "SELECT canonical_id, record_json FROM canonical_records \
             WHERE {PHOTO_FILTER} AND canonical_id < ?1 \
             ORDER BY canonical_id DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mapped = stmt.query_map(params![cursor, fetch], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in mapped {
            rows.push(row?);
        }
    } else {
        let sql = format!(
            "SELECT canonical_id, record_json FROM canonical_records \
             WHERE {PHOTO_FILTER} \
             ORDER BY canonical_id DESC LIMIT ?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mapped = stmt.query_map(params![fetch], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in mapped {
            rows.push(row?);
        }
    }

    let fetched = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    let has_more = fetched > limit;
    let limit_usize = usize::try_from(limit).unwrap_or(0);
    rows.truncate(limit_usize);

    let next_cursor = if has_more {
        rows.last().map(|(id, _)| id.clone())
    } else {
        None
    };

    let items = rows
        .into_iter()
        .filter_map(|(id, json)| parse_buddy(id, &json))
        .collect();

    Ok(BuddyPage { items, next_cursor })
}

/// Fetch all photo-bearing buddies with `canonical_id` strictly greater than
/// `watermark`, oldest first — the shape the SSE poller wants so it can
/// broadcast in arrival order and advance the watermark to the last item.
///
/// `watermark = None` means "everything" (used once, at cold start, to seed
/// an initial watermark — see [`crate::stream::run_poller`]).
///
/// # Errors
/// Returns [`WallDbError::Sqlite`] on a query failure.
pub fn buddies_since(conn: &Connection, watermark: Option<&str>) -> Result<Vec<Buddy>, WallDbError> {
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(mark) = watermark {
        let sql = format!(
            "SELECT canonical_id, record_json FROM canonical_records \
             WHERE {PHOTO_FILTER} AND canonical_id > ?1 \
             ORDER BY canonical_id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mapped = stmt.query_map(params![mark], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in mapped {
            rows.push(row?);
        }
    } else {
        let sql = format!(
            "SELECT canonical_id, record_json FROM canonical_records \
             WHERE {PHOTO_FILTER} \
             ORDER BY canonical_id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in mapped {
            rows.push(row?);
        }
    }

    Ok(rows
        .into_iter()
        .filter_map(|(id, json)| parse_buddy(id, &json))
        .collect())
}

/// The highest `canonical_id` among photo-bearing records, or `None` if there
/// are none yet. Used to seed the SSE poller's watermark at startup so it
/// broadcasts only genuinely new arrivals, not the entire existing backlog.
///
/// # Errors
/// Returns [`WallDbError::Sqlite`] on a query failure.
pub fn max_photo_canonical_id(conn: &Connection) -> Result<Option<String>, WallDbError> {
    let sql = format!("SELECT MAX(canonical_id) FROM canonical_records WHERE {PHOTO_FILTER}");
    let id: Option<String> = conn.query_row(&sql, [], |r| r.get(0))?;
    Ok(id)
}

/// Compute aggregate stats for `GET /api/stats`.
///
/// # Errors
/// Returns [`WallDbError::Sqlite`] on a query failure.
pub fn stats(conn: &Connection) -> Result<WallStats, WallDbError> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM canonical_records", [], |r| r.get(0))?;
    let photo_bearing_sql = format!("SELECT COUNT(*) FROM canonical_records WHERE {PHOTO_FILTER}");
    let photo_bearing: i64 = conn.query_row(&photo_bearing_sql, [], |r| r.get(0))?;
    let dog: i64 = conn.query_row(
        "SELECT COUNT(*) FROM canonical_records WHERE species = 'dog'",
        [],
        |r| r.get(0),
    )?;
    let cat: i64 = conn.query_row(
        "SELECT COUNT(*) FROM canonical_records WHERE species = 'cat'",
        [],
        |r| r.get(0),
    )?;
    let newest_created_at: Option<String> =
        conn.query_row("SELECT MAX(created_at) FROM canonical_records", [], |r| r.get(0))?;

    Ok(WallStats {
        total,
        photo_bearing,
        by_species: SpeciesCounts { dog, cat },
        newest_created_at,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod test_fixtures {
    //! Shared fixture helpers for wall_db and stream tests.

    use chrono::Utc;
    use homeward_schema::{
        Availability, ChipStatus, IntakeType, PetRecord, PhotoRef, Sex, Size, Species, SourceId,
        TosClass,
    };
    use rusqlite::Connection;
    use ulid::Ulid;

    /// Build an in-memory DB with the same `canonical_records` schema as
    /// `homeward-ingest::store::Store::migrate`.
    pub(crate) fn fixture_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE canonical_records (
                canonical_id    TEXT    PRIMARY KEY,
                species         TEXT    NOT NULL,
                record_json     TEXT    NOT NULL,
                availability    TEXT    NOT NULL DEFAULT 'unknown',
                last_seen       TEXT    NOT NULL,
                last_confirmed  TEXT,
                created_at      TEXT    NOT NULL
            );",
        )
        .expect("create fixture schema");
        conn
    }

    /// Deterministic ULID at a given millisecond timestamp, so tests can
    /// control ordering without depending on wall-clock time or sleeps.
    pub(crate) fn ulid_at(ms: u64, entropy: u128) -> Ulid {
        Ulid::from_parts(ms, entropy)
    }

    /// Build a fixture [`PetRecord`]. `photo` controls whether it carries a
    /// photo (the wall's inclusion filter).
    pub(crate) fn make_record(id: Ulid, species: Species, photo: bool) -> PetRecord {
        let now = Utc::now();
        PetRecord {
            canonical_id: id,
            source: SourceId::new("testsrc", TosClass::Api),
            source_animal_id: Some(id.to_string()),
            species,
            breed_primary: Some("Test Breed".to_owned()),
            breed_secondary: None,
            sex: Some(Sex::Unknown),
            age_bucket: None,
            size: Some(Size::Medium),
            colors: vec!["Brown".to_owned()],
            markings_text: Some("A very good fixture animal.".to_owned()),
            intake_type: IntakeType::Adoptable,
            availability: Availability::Adoptable,
            chip_status: ChipStatus::Unknown,
            location: None,
            found_location_text: None,
            photos: if photo {
                vec![PhotoRef {
                    url: format!("https://example.org/{id}.jpg"),
                    attribution: Some("Test Shelter".to_owned()),
                    is_primary: true,
                }]
            } else {
                vec![]
            },
            first_seen: now,
            last_seen: now,
            last_confirmed: Some(now),
            intake_date: None,
            outcome_date: None,
            secondary_provenances: vec![],
        }
    }

    /// Insert a fixture record into a [`fixture_db`] connection.
    pub(crate) fn insert(conn: &Connection, record: &PetRecord) {
        let json = serde_json::to_string(record).expect("serialize fixture record");
        let species = format!("{:?}", record.species).to_lowercase();
        let availability = format!("{:?}", record.availability).to_lowercase();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO canonical_records
               (canonical_id, species, record_json, availability, last_seen, last_confirmed, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                record.canonical_id.to_string(),
                species,
                json,
                availability,
                record.last_seen.to_rfc3339(),
                record.last_confirmed.map(|t| t.to_rfc3339()),
                now,
            ],
        )
        .expect("insert fixture record");
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{fixture_db, insert, make_record, ulid_at};
    use super::*;
    use homeward_schema::Species;

    /// AC: only photo-bearing records come back from `buddies_page`.
    #[test]
    fn photo_filter_excludes_records_without_photos() {
        let conn = fixture_db();
        insert(&conn, &make_record(ulid_at(1_000, 1), Species::Dog, true));
        insert(&conn, &make_record(ulid_at(2_000, 2), Species::Cat, false));
        insert(&conn, &make_record(ulid_at(3_000, 3), Species::Dog, true));

        let page = buddies_page(&conn, None, 10).expect("query ok");
        assert_eq!(page.items.len(), 2, "photoless record must be excluded");
        assert!(page.items.iter().all(|b| !b.primary_photo_url.is_empty()));
    }

    /// AC: `buddies_page` orders newest-first (highest ULID first) and
    /// paginates correctly via the `before` cursor.
    #[test]
    fn cursor_pagination_walks_newest_first_without_gaps_or_dupes() {
        let conn = fixture_db();
        let mut ids = Vec::new();
        for i in 0..5u64 {
            let id = ulid_at(1_000 + i * 10, u128::from(i));
            insert(&conn, &make_record(id, Species::Dog, true));
            ids.push(id.to_string());
        }
        ids.sort_unstable_by(|a, b| b.cmp(a)); // expected newest-first order

        // Page through with limit=2 and collect the full walk.
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = buddies_page(&conn, cursor.as_deref(), 2).expect("query ok");
            seen.extend(page.items.iter().map(|b| b.canonical_id.clone()));
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        assert_eq!(seen, ids, "paginated walk must match the full newest-first order exactly once each");
    }

    /// AC: `limit` is clamped to `MAX_PAGE_LIMIT` even when a caller asks for more.
    #[test]
    fn limit_is_clamped_to_max_page_limit() {
        let conn = fixture_db();
        for i in 0..3u64 {
            insert(
                &conn,
                &make_record(ulid_at(1_000 + i, u128::from(i)), Species::Dog, true),
            );
        }
        let page = buddies_page(&conn, None, 10_000).expect("query ok");
        // Only 3 rows exist; clamping to MAX_PAGE_LIMIT doesn't change that,
        // but the clamp itself must not panic or misbehave (no next_cursor).
        assert_eq!(page.items.len(), 3);
        assert!(page.next_cursor.is_none());
    }

    /// AC: `next_cursor` is `None` once the last page is reached.
    #[test]
    fn next_cursor_is_none_on_last_page() {
        let conn = fixture_db();
        insert(&conn, &make_record(ulid_at(1_000, 1), Species::Dog, true));
        let page = buddies_page(&conn, None, 100).expect("query ok");
        assert!(page.next_cursor.is_none());
    }

    /// AC (SSE watermark logic): `buddies_since(None)` returns every
    /// photo-bearing row; after advancing the watermark to the last one,
    /// a repeat call returns nothing until a genuinely new row appears.
    #[test]
    fn watermark_logic_only_returns_rows_after_the_mark() {
        let conn = fixture_db();
        insert(&conn, &make_record(ulid_at(1_000, 1), Species::Dog, true));
        insert(&conn, &make_record(ulid_at(2_000, 2), Species::Cat, true));

        let initial = buddies_since(&conn, None).expect("query ok");
        assert_eq!(initial.len(), 2);
        let watermark = initial.last().map(|b| b.canonical_id.clone());

        let none_yet = buddies_since(&conn, watermark.as_deref()).expect("query ok");
        assert!(none_yet.is_empty(), "no new rows since the watermark");

        // Simulate a new arrival with a higher canonical_id.
        insert(&conn, &make_record(ulid_at(3_000, 3), Species::Dog, true));
        let after_insert = buddies_since(&conn, watermark.as_deref()).expect("query ok");
        assert_eq!(after_insert.len(), 1, "exactly the newly inserted row");
        assert_eq!(after_insert[0].species, Species::Dog);
    }

    /// AC: `buddies_since` also respects the photo filter.
    #[test]
    fn watermark_query_skips_photoless_new_arrivals() {
        let conn = fixture_db();
        insert(&conn, &make_record(ulid_at(1_000, 1), Species::Dog, true));
        let watermark = Some(ulid_at(1_000, 1).to_string());
        insert(&conn, &make_record(ulid_at(2_000, 2), Species::Cat, false));
        let since = buddies_since(&conn, watermark.as_deref()).expect("query ok");
        assert!(since.is_empty(), "photoless new arrival must not be broadcast");
    }

    /// AC: `max_photo_canonical_id` ignores photoless rows and returns `None` on an empty table.
    #[test]
    fn max_photo_canonical_id_ignores_photoless_rows() {
        let conn = fixture_db();
        assert_eq!(max_photo_canonical_id(&conn).expect("query ok"), None);

        let photoless_high = ulid_at(9_000, 9);
        insert(&conn, &make_record(photoless_high, Species::Dog, false));
        assert_eq!(
            max_photo_canonical_id(&conn).expect("query ok"),
            None,
            "the only row has no photo"
        );

        let photo_low = ulid_at(1_000, 1);
        insert(&conn, &make_record(photo_low, Species::Cat, true));
        assert_eq!(
            max_photo_canonical_id(&conn).expect("query ok"),
            Some(photo_low.to_string())
        );
    }

    /// AC: `stats` counts total, photo-bearing, and per-species correctly.
    #[test]
    fn stats_counts_are_correct() {
        let conn = fixture_db();
        insert(&conn, &make_record(ulid_at(1_000, 1), Species::Dog, true));
        insert(&conn, &make_record(ulid_at(2_000, 2), Species::Dog, false));
        insert(&conn, &make_record(ulid_at(3_000, 3), Species::Cat, true));

        let s = stats(&conn).expect("query ok");
        assert_eq!(s.total, 3);
        assert_eq!(s.photo_bearing, 2);
        assert_eq!(s.by_species.dog, 2);
        assert_eq!(s.by_species.cat, 1);
        assert!(s.newest_created_at.is_some());
    }
}
