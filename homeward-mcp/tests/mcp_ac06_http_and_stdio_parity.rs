//! AC6: Given the same entry point, When started with `--http`, Then the
//! same tool calls succeed over streamable HTTP as over stdio.

mod common;

use common::{
    HttpClient, StdioClient, make_fixture_db, make_pet, spawn_http_server, spawn_stdio_server,
    tool_result_is_error, tool_result_value,
};
use homeward_schema::Species;
use serde_json::json;

#[tokio::test]
async fn same_tool_call_succeeds_over_stdio_and_http() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pet = make_pet(Species::Dog, |_| {});
    let db_path = make_fixture_db(dir.path(), &[pet.clone()]);

    // stdio
    let mut stdio_child = spawn_stdio_server(&db_path);
    let mut stdio_client = StdioClient::handshake(&mut stdio_child).await;
    let stdio_result = stdio_client
        .call_tool("search_pets", json!({ "species": "dog" }))
        .await;
    assert!(!tool_result_is_error(&stdio_result), "stdio call failed: {stdio_result:?}");
    let stdio_value = tool_result_value(&stdio_result);

    // streamable HTTP, same entry point, same fixture DB.
    let (mut http_child, addr) = spawn_http_server(&db_path).await;
    let mut http_client = HttpClient::handshake(&addr).await;
    let http_result = http_client
        .call_tool("search_pets", json!({ "species": "dog" }))
        .await;
    assert!(
        http_result.get("__error__").is_none(),
        "http call errored: {http_result:?}"
    );
    let http_value = tool_result_value(&http_result);

    assert_eq!(
        stdio_value, http_value,
        "search_pets must return the same result over stdio and http"
    );
    let pets = http_value.get("pets").and_then(serde_json::Value::as_array).expect("pets array");
    assert_eq!(pets.len(), 1);
    assert_eq!(pets[0]["id"], pet.canonical_id.to_string());

    let _ = stdio_child.start_kill();
    let _ = http_child.start_kill();
}
