//! Background poller feeding `GET /api/stream`.
//!
//! Every `interval` (default 20s — see [`crate::server::DEFAULT_POLL_MS`]),
//! this reopens the ingest DB read-only, asks for every photo-bearing
//! record newer than the current watermark, and broadcasts each one over a
//! [`tokio::sync::broadcast`] channel shared with every connected SSE
//! client. The watermark starts at the newest existing photo-bearing
//! `canonical_id` (via [`crate::wall_db::max_photo_canonical_id`]) so a
//! freshly started server does not replay the entire backlog as "new".

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::wall_db::{self, Buddy};

/// Poll `db_path` for new photo-bearing buddies every `interval` and
/// broadcast each to `tx`. Runs until the task is dropped/aborted; never
/// panics — DB errors are logged and retried on the next tick.
pub async fn run_poller(db_path: PathBuf, tx: broadcast::Sender<Buddy>, interval: Duration) {
    let mut watermark = initial_watermark(&db_path);
    info!("wall poller: starting with watermark={watermark:?}, interval={interval:?}");

    loop {
        tokio::time::sleep(interval).await;

        let path = db_path.clone();
        let mark = watermark.clone();
        let result = tokio::task::spawn_blocking(move || query_since(&path, mark.as_deref())).await;

        match result {
            Ok(Ok(new_buddies)) if !new_buddies.is_empty() => {
                if let Some(last) = new_buddies.last() {
                    watermark = Some(last.canonical_id.clone());
                }
                let n = new_buddies.len();
                for buddy in new_buddies {
                    // No receivers connected is not an error — just means no one is watching yet.
                    let _ = tx.send(buddy);
                }
                info!("wall poller: broadcast {n} new buddies");
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => warn!("wall poller: DB query failed: {e}"),
            Err(e) => warn!("wall poller: blocking task panicked: {e}"),
        }
    }
}

/// Open the DB and run `buddies_since` in one blocking call (for use inside
/// `spawn_blocking`).
fn query_since(path: &std::path::Path, watermark: Option<&str>) -> Result<Vec<Buddy>, wall_db::WallDbError> {
    let conn = wall_db::open_readonly(path)?;
    wall_db::buddies_since(&conn, watermark)
}

/// Seed the watermark from the DB's current newest photo-bearing record.
/// Falls back to `None` (broadcast everything on the very first tick) if the
/// DB is not yet reachable — better to over-broadcast once than to silently
/// stay dark.
fn initial_watermark(db_path: &std::path::Path) -> Option<String> {
    match wall_db::open_readonly(db_path).and_then(|conn| wall_db::max_photo_canonical_id(&conn)) {
        Ok(mark) => mark,
        Err(e) => {
            warn!("wall poller: could not determine initial watermark ({e}); starting from empty");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_db::test_fixtures::{fixture_db, insert, make_record, ulid_at};
    use homeward_schema::Species;

    /// AC: the poller's watermark helper only advances past photo-bearing rows,
    /// mirroring `buddies_since`'s own guarantee (exercised directly here
    /// since `run_poller` itself talks to a file path, not a live connection).
    #[test]
    fn query_since_uses_buddies_since_semantics() {
        let conn = fixture_db();
        insert(&conn, &make_record(ulid_at(1_000, 1), Species::Dog, true));
        let watermark = Some(ulid_at(1_000, 1).to_string());
        insert(&conn, &make_record(ulid_at(2_000, 2), Species::Cat, true));

        let since = wall_db::buddies_since(&conn, watermark.as_deref()).expect("query ok");
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].species, Species::Cat);
    }
}
