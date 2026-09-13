//! AC8 (P2): Given limit=5 and a radius filter, When applied, Then
//! survivors keep their kNN relative order.

mod common;

use common::mock_embed::MockEmbedServer;
use common::{make_fixture_db, make_pet, spawn_stdio_server_with_embed, tool_result_value, StdioClient};
use homeward_schema::{PetRecord, ShelterLocation, Species};
use serde_json::json;

fn with_location(mut record: PetRecord, lat: f64, lon: f64) -> PetRecord {
    record.location = Some(ShelterLocation::new(
        Some(lat),
        Some(lon),
        2,
        "Test City".to_owned(),
        Some("TX".to_owned()),
    ));
    record
}

#[tokio::test]
async fn limit_and_radius_filter_without_reordering_survivors() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Austin, TX center. `near` sits inside a 10km radius; `far` (Dallas) does not.
    let center = (30.2672, -97.7431);
    let near_best = with_location(make_pet(Species::Dog, |_| {}), 30.27, -97.75);
    let near_second = with_location(make_pet(Species::Dog, |_| {}), 30.28, -97.73);
    let far = with_location(make_pet(Species::Dog, |_| {}), 32.7767, -96.7970); // Dallas, ~260km away

    let db_path = make_fixture_db(
        dir.path(),
        &[near_best.clone(), near_second.clone(), far.clone()],
    );

    // kNN order: far (highest score) first, then near_best, then near_second.
    // The radius filter should drop `far` while preserving the relative
    // order of the two survivors (near_best before near_second).
    let mock = MockEmbedServer::start(vec![
        (far.canonical_id.to_string(), 0.95),
        (near_best.canonical_id.to_string(), 0.80),
        (near_second.canonical_id.to_string(), 0.60),
    ])
    .await;

    let mut child = spawn_stdio_server_with_embed(&db_path, mock.addr);
    let mut client = StdioClient::handshake(&mut child).await;

    let result = client
        .call_tool(
            "match_photo",
            json!({
                "image_url": "https://example.org/lost-dog.jpg",
                "species": "dog",
                "lat": center.0,
                "lon": center.1,
                "radius_km": 10.0,
                "limit": 5
            }),
        )
        .await;
    let value = tool_result_value(&result);
    let candidates = value["candidates"].as_array().expect("candidates array");

    let ids: Vec<String> = candidates
        .iter()
        .map(|c| c["id"].as_str().expect("id").to_owned())
        .collect();

    assert_eq!(
        ids,
        vec![near_best.canonical_id.to_string(), near_second.canonical_id.to_string()],
        "far-away candidate must be dropped and survivors must keep kNN order: {ids:?}"
    );

    let _ = child.start_kill();
}
