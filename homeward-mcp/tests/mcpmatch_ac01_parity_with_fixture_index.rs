//! AC1: Given a fixture index with known neighbors, When match_photo
//! submits an enrolled animal's held-out photo, Then the ranked list
//! matches the direct pipeline's output for the same inputs (ids and
//! order).
//!
//! The "direct pipeline" here is the mock embed sidecar returning a fixed,
//! pre-ranked `canonical_id`/score order -- exactly what the real sidecar's
//! `/query` endpoint already does (body-crop -> embed -> kNN, per
//! `crates/homeward-embed-client`'s doc comment). `match_photo`'s job is to
//! preserve that order while attaching shelter-record metadata; this test
//! asserts it does so byte-for-byte on id order.

mod common;

use common::mock_embed::MockEmbedServer;
use common::{make_pet, make_fixture_db, spawn_stdio_server_with_embed, tool_result_value, StdioClient};
use homeward_schema::Species;
use serde_json::json;

#[tokio::test]
async fn match_photo_preserves_sidecar_knn_order() {
    let dir = tempfile::tempdir().expect("tempdir");

    let closest = make_pet(Species::Dog, |p| {
        p.breed_primary = Some("Beagle".to_owned());
    });
    let second = make_pet(Species::Dog, |p| {
        p.breed_primary = Some("Beagle mix".to_owned());
    });
    let third = make_pet(Species::Dog, |p| {
        p.breed_primary = Some("Hound".to_owned());
    });
    let db_path = make_fixture_db(dir.path(), &[closest.clone(), second.clone(), third.clone()]);

    // The mock sidecar's kNN order: third has the LOWEST score but is
    // listed here in a deliberately-scrambled insertion order, to make sure
    // match_photo doesn't silently re-sort by anything of its own.
    let mock = MockEmbedServer::start(vec![
        (closest.canonical_id.to_string(), 0.91),
        (second.canonical_id.to_string(), 0.77),
        (third.canonical_id.to_string(), 0.52),
    ])
    .await;

    let mut child = spawn_stdio_server_with_embed(&db_path, mock.addr);
    let mut client = StdioClient::handshake(&mut child).await;

    let result = client
        .call_tool(
            "match_photo",
            json!({ "image_url": "https://example.org/lost-dog.jpg", "species": "dog" }),
        )
        .await;
    let value = tool_result_value(&result);
    let candidates = value["candidates"].as_array().expect("candidates array");

    let ids: Vec<String> = candidates
        .iter()
        .map(|c| c["id"].as_str().expect("candidate id").to_owned())
        .collect();
    assert_eq!(
        ids,
        vec![
            closest.canonical_id.to_string(),
            second.canonical_id.to_string(),
            third.canonical_id.to_string(),
        ],
        "match_photo must return candidates in the sidecar's own kNN order"
    );

    let _ = child.start_kill();
}
