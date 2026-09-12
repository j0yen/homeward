//! AC3: Given a search with no matches, When called, Then an empty result
//! with a well-formed shape returns (no error, no nulls where lists belong).

mod common;

use common::{StdioClient, make_fixture_db, make_pet, spawn_stdio_server, tool_result_is_error, tool_result_value};
use homeward_schema::Species;
use serde_json::json;

#[tokio::test]
async fn no_matches_returns_empty_well_formed_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Fixture DB has only a cat; searching for a dog should come back empty, not error.
    let db_path = make_fixture_db(dir.path(), &[make_pet(Species::Cat, |_| {})]);

    let mut child = spawn_stdio_server(&db_path);
    let mut client = StdioClient::handshake(&mut child).await;

    let call_result = client
        .call_tool("search_pets", json!({ "species": "dog" }))
        .await;

    assert!(
        !tool_result_is_error(&call_result),
        "a zero-match search must not be a tool error: {call_result:?}"
    );

    let value = tool_result_value(&call_result);
    let pets = value.get("pets").expect("pets field present");
    assert!(pets.is_array(), "pets must be an array, not null: {value:?}");
    assert_eq!(pets.as_array().expect("array").len(), 0);
    assert_eq!(value.get("truncated"), Some(&json!(false)));

    let _ = child.start_kill();
}
