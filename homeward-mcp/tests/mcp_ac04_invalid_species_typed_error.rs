//! AC4: Given an invalid species value, When `search_pets` is called, Then
//! a typed MCP error returns and the server stays up (a subsequent valid
//! call still succeeds).

mod common;

use common::{StdioClient, make_fixture_db, make_pet, spawn_stdio_server, tool_result_is_error};
use homeward_schema::Species;
use serde_json::json;

#[tokio::test]
async fn invalid_species_is_a_typed_error_and_server_survives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = make_fixture_db(dir.path(), &[make_pet(Species::Dog, |_| {})]);

    let mut child = spawn_stdio_server(&db_path);
    let mut client = StdioClient::handshake(&mut child).await;

    let bad_result = client
        .call_tool("search_pets", json!({ "species": "giraffe" }))
        .await;
    assert!(
        bad_result.get("__error__").is_some() || tool_result_is_error(&bad_result),
        "invalid species must come back as a typed error, not a crash: {bad_result:?}"
    );

    // The server must still be up: a valid call right after must succeed.
    let good_result = client
        .call_tool("search_pets", json!({ "species": "dog" }))
        .await;
    assert!(
        good_result.get("__error__").is_none(),
        "server did not survive the invalid-species call: {good_result:?}"
    );
    assert!(
        !tool_result_is_error(&good_result),
        "valid follow-up call should succeed: {good_result:?}"
    );

    let _ = child.start_kill();
}
