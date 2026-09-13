//! AC1: Given the server on stdio with a fixture DB, When a client calls
//! `tools/list`, Then exactly `search_pets`, `get_pet`, and `match_photo`
//! appear with JSON schemas, and `recent_intakes` lists as a resource.
//!
//! `match_photo` (PRD-homeward-mcp-photo-match) is an additive tool on this
//! server -- per that PRD's own "Migration / compatibility" section,
//! "tools/list gains one entry" -- so the original "exactly 2 tools"
//! assertion here is updated to 3 rather than left stale.

mod common;

use common::{StdioClient, make_fixture_db, make_pet, spawn_stdio_server, tool_names};
use homeward_schema::Species;
use serde_json::Value;

#[tokio::test]
async fn tools_list_has_exactly_search_pets_get_pet_and_match_photo_with_schemas() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = make_fixture_db(dir.path(), &[make_pet(Species::Dog, |_| {})]);

    let mut child = spawn_stdio_server(&db_path);
    let mut client = StdioClient::handshake(&mut child).await;

    let list_result = client.request("tools/list", Value::Null).await;
    let tools = tool_names(&list_result);

    assert_eq!(
        tools.len(),
        3,
        "expected exactly 3 tools, got: {:?}",
        tools.keys().collect::<Vec<_>>()
    );
    assert!(tools.contains_key("search_pets"), "search_pets missing");
    assert!(tools.contains_key("get_pet"), "get_pet missing");
    assert!(tools.contains_key("match_photo"), "match_photo missing");

    for (name, tool) in &tools {
        let schema = tool
            .get("inputSchema")
            .unwrap_or_else(|| panic!("{name} has no inputSchema"));
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{name}'s inputSchema should be a JSON-Schema object"
        );
    }

    let resources_result = client.request("resources/list", Value::Null).await;
    let resources = resources_result
        .get("resources")
        .and_then(Value::as_array)
        .expect("resources array");
    assert!(
        resources
            .iter()
            .any(|r| r.get("name").and_then(Value::as_str) == Some("recent_intakes")),
        "recent_intakes resource missing: {resources_result:?}"
    );

    let _ = child.start_kill();
}
