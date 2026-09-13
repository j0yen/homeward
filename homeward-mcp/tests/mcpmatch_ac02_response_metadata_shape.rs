//! AC2: Given any successful match response, When inspected, Then it
//! carries similarity per candidate, the advisory string, and the species
//! baseline metadata.

mod common;

use common::mock_embed::MockEmbedServer;
use common::{make_fixture_db, make_pet, spawn_stdio_server_with_embed, tool_result_value, StdioClient};
use homeward_schema::Species;
use serde_json::json;

#[tokio::test]
async fn successful_match_carries_similarity_advisory_and_baseline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cat = make_pet(Species::Cat, |_| {});
    let db_path = make_fixture_db(dir.path(), &[cat.clone()]);

    let mock = MockEmbedServer::start(vec![(cat.canonical_id.to_string(), 0.83)]).await;
    let mut child = spawn_stdio_server_with_embed(&db_path, mock.addr);
    let mut client = StdioClient::handshake(&mut child).await;

    let result = client
        .call_tool(
            "match_photo",
            json!({ "image_url": "https://example.org/lost-cat.jpg", "species": "cat" }),
        )
        .await;
    let value = tool_result_value(&result);

    assert_eq!(
        value["advisory"].as_str(),
        Some("candidates-not-confirmations"),
        "response must always carry the candidates-not-confirmations advisory: {value}"
    );

    let baseline = &value["species_baseline"];
    assert_eq!(baseline["species"].as_str(), Some("cat"));
    assert!(baseline["rank1"].as_f64().is_some(), "baseline must carry rank1: {baseline}");
    assert!(baseline["rank5"].as_f64().is_some(), "baseline must carry rank5: {baseline}");
    assert!(baseline["rank20"].as_f64().is_some(), "baseline must carry rank20: {baseline}");
    assert!(
        !baseline["source"].as_str().unwrap_or_default().is_empty(),
        "baseline must cite a source: {baseline}"
    );

    let candidates = value["candidates"].as_array().expect("candidates array");
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert_eq!(candidate["id"].as_str(), Some(cat.canonical_id.to_string().as_str()));
    let similarity = candidate["similarity"].as_f64().expect("similarity present");
    assert!((similarity - 0.83).abs() < 1e-9, "similarity should pass through unchanged: {candidate}");

    let _ = child.start_kill();
}
