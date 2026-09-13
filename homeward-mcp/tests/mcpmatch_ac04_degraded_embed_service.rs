//! AC4: Given the embed service is stopped, When match_photo is called,
//! Then a typed degraded error returns within 5 s and a subsequent
//! search_pets call still succeeds.

mod common;

use std::time::{Duration, Instant};

use common::mock_embed::MockEmbedServer;
use common::{make_fixture_db, make_pet, spawn_stdio_server_with_embed, tool_result_is_error, StdioClient};
use homeward_schema::Species;
use serde_json::json;

#[tokio::test]
async fn embed_service_down_yields_fast_typed_error_and_server_stays_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let dog = make_pet(Species::Dog, |_| {});
    let db_path = make_fixture_db(dir.path(), &[dog]);

    // Reserve-then-release a port: nothing is listening there, simulating a
    // stopped embed sidecar without depending on a fixed "surely closed"
    // port number.
    let closed_addr = MockEmbedServer::closed_port().await;

    let mut child = spawn_stdio_server_with_embed(&db_path, closed_addr);
    let mut client = StdioClient::handshake(&mut child).await;

    let started = Instant::now();
    let result = client
        .call_tool(
            "match_photo",
            json!({ "image_url": "https://example.org/lost-dog.jpg", "species": "dog" }),
        )
        .await;
    let elapsed = started.elapsed();

    assert!(
        result.get("__error__").is_some() || tool_result_is_error(&result),
        "an unreachable embed service must produce a typed error: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "degraded error must return within 5s, took {elapsed:?}"
    );
    let serialized = serde_json::to_string(&result).unwrap_or_default();
    assert!(
        !serialized.contains("panicked at") && !serialized.contains("RUST_BACKTRACE"),
        "error payload must not leak a Rust panic/backtrace: {serialized}"
    );

    // The server itself must still be alive and answering unrelated tools.
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "server process must not have exited after an embed-degraded call"
    );
    let follow_up = client
        .call_tool("search_pets", json!({ "species": "dog" }))
        .await;
    assert!(
        !tool_result_is_error(&follow_up) && follow_up.get("__error__").is_none(),
        "search_pets must still succeed after a degraded match_photo call: {follow_up:?}"
    );

    let _ = child.start_kill();
}
