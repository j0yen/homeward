//! AC4 (PRD-homeward-ingest-location-backfill-missing): given a fixture
//! RescueGroups payload that includes org/location data, the connector's
//! mapping unit test asserts `location.is_some()` — so this regression class
//! (a connector silently hardcoding `location: None`) can't reappear
//! unnoticed.
//!
//! See `locbackfill_ac1_location_populated.rs`'s header for why this file
//! uses the `locbackfill_ac<N>` prefix rather than bare `ac4_*` naming.

use std::time::Duration;

use homeward_connectors::connectors::rescuegroups::{RescueGroupsConfig, RescueGroupsConnector};
use homeward_connectors::http::{HOMEWARD_USER_AGENT, PoliteClient};
use reqwest::Client;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn polite_client() -> PoliteClient {
    let client = Client::builder().user_agent(HOMEWARD_USER_AGENT).build().expect("client");
    PoliteClient::from_client(client, Duration::from_millis(0))
}

fn fixture_page() -> serde_json::Value {
    serde_json::json!({
        "data": [
            {
                "id": "rg-fixture-cat-1",
                "type": "animals",
                "attributes": {
                    "updatedDate": "2024-02-01T00:00:00Z",
                    "createdDate": "2024-01-20T00:00:00Z",
                },
                "relationships": {
                    "orgs": { "data": [ { "type": "orgs", "id": "org-9" } ] },
                    "locations": { "data": [ { "type": "locations", "id": "loc-9" } ] },
                },
            }
        ],
        "included": [
            {
                "type": "orgs",
                "id": "org-9",
                "attributes": { "city": "Dallas", "state": "TX" },
            },
            {
                "type": "locations",
                "id": "loc-9",
                "attributes": { "city": "Fort Worth", "state": "TX", "lat": 32.7555, "lon": -97.3308 },
            }
        ],
        "meta": {
            "count": 1,
            "countReturned": 1,
            "pageReturned": 1,
            "pages": 1,
            "limit": 250,
            "transactionId": "t",
        },
    })
}

#[tokio::test]
async fn locbackfill_ac4_fixture_org_location_data_maps_to_some() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v5/public/animals/search/available/cats"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture_page().to_string()))
        .mount(&mock_server)
        .await;

    let config = RescueGroupsConfig {
        api_key: "test-key".to_owned(),
        base_url: format!("{}/v5", mock_server.uri()),
    };
    let connector = RescueGroupsConnector::with_client(config, polite_client());

    let result = connector.fetch_normalized_page("cats", 1).await.expect("fetch_normalized_page");
    assert_eq!(result.records.len(), 1, "expected exactly one fixture record");

    assert!(
        result.records[0].location.is_some(),
        "AC4: a fixture payload carrying org/location data must map to Some(..), not None"
    );
}
