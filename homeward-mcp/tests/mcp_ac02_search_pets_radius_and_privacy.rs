//! AC2: Given fixture intakes near location X, When
//! `search_pets(dog, X, 50km)` is called, Then only in-radius dogs return,
//! each with a hotlinked photo URL, coarse location, and brokered shelter
//! contact -- and no person-level PII field exists in the payload.

mod common;

use common::{StdioClient, make_fixture_db, make_pet, spawn_stdio_server, tool_result_value};
use homeward_schema::{ShelterLocation, Species};
use serde_json::json;

// Austin, TX -- the search center.
const AUSTIN_LAT: f64 = 30.2672;
const AUSTIN_LON: f64 = -97.7431;
// Dallas, TX -- ~330km from Austin, outside a 50km radius.
const DALLAS_LAT: f64 = 32.7767;
const DALLAS_LON: f64 = -96.7970;

#[tokio::test]
async fn radius_search_filters_by_distance_and_redacts_output() {
    let dir = tempfile::tempdir().expect("tempdir");

    let near_dog = make_pet(Species::Dog, |r| {
        r.location = Some(ShelterLocation::new(
            Some(AUSTIN_LAT),
            Some(AUSTIN_LON),
            2,
            "Austin".to_owned(),
            Some("TX".to_owned()),
        ));
    });
    let far_dog = make_pet(Species::Dog, |r| {
        r.location = Some(ShelterLocation::new(
            Some(DALLAS_LAT),
            Some(DALLAS_LON),
            2,
            "Dallas".to_owned(),
            Some("TX".to_owned()),
        ));
    });
    let near_cat = make_pet(Species::Cat, |r| {
        r.location = Some(ShelterLocation::new(
            Some(AUSTIN_LAT),
            Some(AUSTIN_LON),
            2,
            "Austin".to_owned(),
            Some("TX".to_owned()),
        ));
    });

    let db_path = make_fixture_db(dir.path(), &[near_dog.clone(), far_dog, near_cat]);

    let mut child = spawn_stdio_server(&db_path);
    let mut client = StdioClient::handshake(&mut child).await;

    let call_result = client
        .call_tool(
            "search_pets",
            json!({ "species": "dog", "lat": AUSTIN_LAT, "lon": AUSTIN_LON, "radius_km": 50.0 }),
        )
        .await;
    let value = tool_result_value(&call_result);
    let pets = value.get("pets").and_then(serde_json::Value::as_array).expect("pets array");

    assert_eq!(pets.len(), 1, "expected exactly the near dog: {value:?}");
    let pet = &pets[0];
    assert_eq!(pet["id"], near_dog.canonical_id.to_string());

    // Hotlinked photo URL present, not raw bytes.
    let photo_urls = pet["photo_urls"].as_array().expect("photo_urls array");
    assert!(
        photo_urls
            .iter()
            .all(|u| u.as_str().is_some_and(|s| s.starts_with("http"))),
        "photo_urls must be hotlink URLs: {photo_urls:?}"
    );

    // Coarse location present (city/state), not a street address.
    assert_eq!(pet["city_county"], "Austin");
    assert_eq!(pet["state"], "TX");

    // Brokered shelter contact route present, and clearly not a raw phone/email.
    let contact = pet["shelter_contact"].as_str().expect("shelter_contact string");
    assert!(!contact.contains('@'), "contact must not be an email: {contact}");
    assert!(
        contact.starts_with("brokered-via:"),
        "contact must be the brokered attribution route, not raw contact info: {contact}"
    );

    // No person-level PII field anywhere in the payload (owner name/phone/email/address).
    let serialized = serde_json::to_string(&value).expect("serialize");
    for forbidden in ["owner", "phone", "email", "ssn", "address"] {
        assert!(
            !serialized.to_lowercase().contains(forbidden),
            "payload must not contain PII-shaped field `{forbidden}`: {serialized}"
        );
    }

    let _ = child.start_kill();
}
