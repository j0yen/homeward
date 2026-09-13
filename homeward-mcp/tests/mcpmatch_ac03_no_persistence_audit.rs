//! AC3: Given a match call completes, When the filesystem and DB are
//! audited, Then no submitted image bytes, query embeddings, or
//! EXIF-bearing artifacts were written.
//!
//! `match_photo` never writes to disk on the Rust side (it only forwards
//! the caller's image to the embed sidecar over HTTP and reads the
//! existing read-only ingest DB); this test audits the fixture directory's
//! file listing and the ingest DB's row count before and after a real
//! `image_b64` match call, so a regression that starts writing a cache
//! file/log/temp-image would fail loudly.

mod common;

use std::collections::BTreeSet;
use std::fs;

use base64::Engine as _;
use common::mock_embed::MockEmbedServer;
use common::{make_fixture_db, make_pet, spawn_stdio_server_with_embed, tool_result_is_error, StdioClient};
use homeward_schema::Species;
use rusqlite::Connection;
use serde_json::json;

fn dir_listing(dir: &std::path::Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .expect("read tempdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

fn row_count(db_path: &std::path::Path) -> i64 {
    let conn = Connection::open(db_path).expect("open db for audit");
    conn.query_row("SELECT COUNT(*) FROM canonical_records", [], |r| r.get(0))
        .expect("count rows")
}

#[tokio::test]
async fn match_photo_persists_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dog = make_pet(Species::Dog, |_| {});
    let db_path = make_fixture_db(dir.path(), &[dog.clone()]);

    let before_listing = dir_listing(dir.path());
    let before_rows = row_count(&db_path);

    let mock = MockEmbedServer::start(vec![(dog.canonical_id.to_string(), 0.9)]).await;
    let mut child = spawn_stdio_server_with_embed(&db_path, mock.addr);
    let mut client = StdioClient::handshake(&mut child).await;

    // Real, valid JPEG-magic-byte payload, not just a URL -- exercises the
    // image_b64 path this AC is really about (nothing derived from
    // caller-submitted bytes should ever touch disk).
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
    jpeg.extend_from_slice(&[0u8; 64]);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);

    let result = client
        .call_tool("match_photo", json!({ "image_b64": b64, "species": "dog" }))
        .await;
    assert!(!tool_result_is_error(&result), "expected a successful match: {result:?}");

    let after_listing = dir_listing(dir.path());
    let after_rows = row_count(&db_path);

    assert_eq!(
        before_listing, after_listing,
        "match_photo must not create/remove any file in the ingest DB's directory"
    );
    assert_eq!(
        before_rows, after_rows,
        "match_photo must not insert/delete any ingest DB row"
    );

    let _ = child.start_kill();
}
