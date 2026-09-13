//! AC2 (PRD-homeward-ingest-location-backfill-missing): given the existing
//! 185k-row production DB, a backfill pass against already-ingested
//! RescueGroups records (using stored `source_animal_id`s to re-fetch
//! location) makes a measurable MAJORITY of them gain non-null `location`.
//! 100% is not required (some source records genuinely lack a location
//! upstream) but 0% must not persist — the defect this PRD fixes.
//!
//! See `homeward-connectors/tests/locbackfill_ac1_location_populated.rs`'s
//! header for why this file uses the `locbackfill_ac<N>` prefix instead of
//! bare `ac2_*` naming (this crate's own `backfill_tests.rs` already owns
//! bare `ac1`..`ac8` for the unrelated population-backfill PRD).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use homeward_connectors::connectors::rescuegroups::{RescueGroupsConfig, RescueGroupsConnector};
use homeward_ingest::backfill;
use homeward_ingest::store::Store;
use homeward_schema::geo::ShelterLocation;
use homeward_schema::provenance::{SourceId, TosClass};
use homeward_schema::{ChipStatus, PetRecord, Species, intake::{Availability, IntakeType}};
use ulid::Ulid;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn seed_existing(store: &mut Store, source_animal_id: &str, location: Option<ShelterLocation>) {
    let rec = PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("rescuegroups", TosClass::Api),
        source_animal_id: Some(source_animal_id.to_owned()),
        species: Species::Dog,
        breed_primary: None,
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        markings_text: None,
        intake_type: IntakeType::Adoptable,
        availability: Availability::Adoptable,
        chip_status: ChipStatus::Unknown,
        location,
        found_location_text: None,
        photos: vec![],
        first_seen: chrono::Utc::now(),
        last_seen: chrono::Utc::now(),
        last_confirmed: Some(chrono::Utc::now()),
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    };
    store.upsert(&rec).expect("seed upsert");
}

fn rg_animal_with_optional_location(id: &str, with_location: bool, included_out: &mut Vec<serde_json::Value>) -> serde_json::Value {
    let relationships = if with_location {
        let loc_id = format!("{id}-loc");
        included_out.push(serde_json::json!({
            "type": "locations",
            "id": loc_id,
            "attributes": { "city": "Austin", "state": "TX", "lat": 30.27, "lon": -97.74 },
        }));
        serde_json::json!({
            "locations": { "data": [ { "type": "locations", "id": loc_id } ] },
        })
    } else {
        serde_json::Value::Null
    };
    serde_json::json!({
        "id": id,
        "type": "animals",
        "attributes": {
            "updatedDate": "2024-01-15T10:00:00Z",
            "createdDate": "2024-01-10T08:00:00Z",
        },
        "relationships": relationships,
    })
}

fn rg_page(animals: Vec<serde_json::Value>, included: Vec<serde_json::Value>) -> serde_json::Value {
    let n = animals.len() as u64;
    serde_json::json!({
        "data": animals,
        "meta": {
            "count": n, "countReturned": n, "pageReturned": 1, "pages": 1, "limit": 250, "transactionId": "t",
        },
        "included": included,
    })
}

#[tokio::test]
async fn locbackfill_ac2_majority_of_null_locations_backfilled_not_100_not_0() {
    let server = MockServer::start().await;

    let dog_ids: Vec<String> = (1..=10).map(|n| format!("rg-loc-dog-{n:02}")).collect();
    let mut included = Vec::new();
    let animals: Vec<_> = dog_ids
        .iter()
        .enumerate()
        .map(|(i, id)| rg_animal_with_optional_location(id, i < 8, &mut included))
        .collect();
    let cats_page = rg_page(vec![], vec![]);
    let dogs_page = rg_page(animals, included);

    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/dogs"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&dogs_page))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/public/animals/search/available/cats"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&cats_page))
        .mount(&server)
        .await;

    let config = RescueGroupsConfig { api_key: "test-key".to_owned(), base_url: server.uri() };
    let connector = RescueGroupsConnector::new(config).expect("connector");

    let mut store = Store::open_in_memory().expect("store");

    for id in &dog_ids[..9] {
        seed_existing(&mut store, id, None);
    }
    let preexisting_loc = ShelterLocation::new(Some(1.0), Some(2.0), 2, "PreExisting".to_owned(), None);
    seed_existing(&mut store, &dog_ids[9], Some(preexisting_loc.clone()));

    assert_eq!(store.count_with_location().unwrap(), 1, "only the seeded pre-existing row starts non-null");

    let report = backfill::run_location_backfill(&mut store, &connector)
        .await
        .expect("location backfill run");

    assert_eq!(report.dogs.scanned, 10);
    assert_eq!(report.dogs.updated, 8, "8 of 10 fetched records had usable upstream location data");
    assert_eq!(report.dogs.already_had_location, 1, "the pre-seeded non-null row must not be re-counted as updated");
    assert_eq!(report.dogs.source_missing_location, 1, "dog-09 has no locations relationship upstream");
    assert_eq!(report.db_with_location_before, 1);
    assert_eq!(report.db_with_location_after, 9, "8 updated + 1 pre-existing = 9 of 10 rows non-null");

    let majority_threshold = 10 / 2;
    assert!(
        report.db_with_location_after > majority_threshold,
        "AC2: a measurable majority must now be non-null, got {}/10",
        report.db_with_location_after
    );
    assert!(report.db_with_location_after < 10, "AC2 does not require 100%");
    assert!(report.db_with_location_before < report.db_with_location_after, "AC2: 0% must not persist — this run must move the needle");

    let preexisting_id = store.find_by_source_animal_id("rescuegroups", &dog_ids[9]).unwrap().expect("row present");
    let preexisting_rec = store.get(preexisting_id).unwrap();
    assert_eq!(preexisting_rec.location, Some(preexisting_loc), "already-populated location must never be overwritten");

    for id in &dog_ids[..8] {
        let cid = store.find_by_source_animal_id("rescuegroups", id).unwrap().expect("row present");
        let rec = store.get(cid).unwrap();
        let loc = rec.location.expect("must be backfilled");
        assert_eq!(loc.city_county, "Austin");
    }

    let dog09_id = store.find_by_source_animal_id("rescuegroups", &dog_ids[8]).unwrap().expect("row present");
    let dog09 = store.get(dog09_id).unwrap();
    assert!(dog09.location.is_none());
}
