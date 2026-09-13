//! AC1 (PRD-homeward-ingest-location-backfill-missing): given a RescueGroups
//! API animal payload that includes an org/location relationship, the
//! connector maps it to a `PetRecord` with `location = Some(ShelterLocation
//! { .. })` carrying real city/state (and lat/lon when the API provides
//! them) — not the old hardcoded `None`.
//!
//! Drives the fix through the connector's public `fetch_normalized_page`
//! API against a mocked RG v5 response, rather than calling private mapping
//! internals directly (mirrors this crate's existing `integration_connector.rs`
//! convention). Uses the `locbackfill_ac<N>` file-prefix convention
//! (`test_prefix: locbackfill` in the PRD frontmatter) because this crate's
//! `tests/integration_connector.rs` already owns bare `ac1`..`ac7` test
//! function names for an earlier, unrelated PRD — see build-contract.md's
//! `test_prefix` note on avoiding false AC-pairing in a shared crate.

use std::time::Duration;

use homeward_connectors::connectors::rescuegroups::{RescueGroupsConfig, RescueGroupsConnector};
use homeward_connectors::http::{HOMEWARD_USER_AGENT, PoliteClient};
use reqwest::Client;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn polite_client() -> PoliteClient {
    let client = Client::builder().user_agent(HOMEWARD_USER_AGENT).build().expect("client");
    PoliteClient::from_client(client, Duration::from_millis(0))
}

#[tokio::test]
async fn locbackfill_ac1_location_populated_from_locations_relationship() {
    let mock_server = MockServer::start().await;

    let page = serde_json::json!({
        "data": [
            {
                "id": "rg-loc-dog-1",
                "type": "animals",
                "attributes": {
                    "updatedDate": "2024-01-15T10:00:00Z",
                    "createdDate": "2024-01-10T08:00:00Z",
                    "breedPrimary": "Labrador Retriever",
                },
                "relationships": {
                    "locations": { "data": [ { "type": "locations", "id": "loc-1" } ] },
                },
            }
        ],
        "included": [
            {
                "type": "locations",
                "id": "loc-1",
                "attributes": {
                    "city": "Austin",
                    "state": "TX",
                    "postalcode": "78701",
                    "lat": 30.2672,
                    "lon": -97.7431,
                },
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
    });

    Mock::given(method("POST"))
        .and(path("/v5/public/animals/search/available/dogs"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(page.to_string()))
        .mount(&mock_server)
        .await;

    let config = RescueGroupsConfig {
        api_key: "test-key".to_owned(),
        base_url: format!("{}/v5", mock_server.uri()),
    };
    let connector = RescueGroupsConnector::with_client(config, polite_client());

    let result = connector.fetch_normalized_page("dogs", 1).await.expect("fetch_normalized_page");
    assert_eq!(result.records.len(), 1);

    let rec = &result.records[0];
    let loc = rec
        .location
        .as_ref()
        .expect("AC1: location must be Some(..), not the old hardcoded None");
    assert_eq!(loc.city_county, "Austin", "AC1: real city must be mapped");
    assert_eq!(loc.state.as_deref(), Some("TX"), "AC1: real state must be mapped");
    assert_eq!(loc.lat, Some(30.27), "AC1: lat must be mapped (coarsened to 2dp)");
    assert_eq!(loc.lon, Some(-97.74), "AC1: lon must be mapped (coarsened to 2dp)");

    let received = mock_server.received_requests().await.expect("requests");
    assert!(
        received.iter().any(|r| r
            .url
            .query()
            .is_some_and(|q| q.contains("include=") && q.contains("locations") && q.contains("orgs"))),
        "request must ask RG to include locations/orgs, got queries: {:?}",
        received.iter().map(|r| r.url.query()).collect::<Vec<_>>()
    );
}
