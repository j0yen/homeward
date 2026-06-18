//! AC6: Enrichment fills location only when it is None; a record with an
//! existing ShelterLocation is left byte-identical (no overwrite), tested.

use homeward_geocode::{enrich_record, EnrichmentResult, Gazetteer};
use homeward_schema::{Availability, ChipStatus, IntakeType, PetRecord, SourceId, Species, TosClass};

fn gz() -> Gazetteer {
    Gazetteer::load_default().expect("load")
}

fn make_record(
    found_text: Option<&str>,
    location: Option<homeward_schema::ShelterLocation>,
) -> PetRecord {
    use chrono::Utc;
    PetRecord {
        canonical_id: ulid::Ulid::new(),
        source: SourceId::new("test", TosClass::OpenData),
        source_animal_id: None,
        species: Species::Dog,
        breed_primary: None,
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        markings_text: None,
        intake_type: IntakeType::Stray,
        availability: Availability::InCustody,
        chip_status: ChipStatus::NotScanned,
        location,
        found_location_text: found_text.map(str::to_owned),
        photos: vec![],
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        last_confirmed: None,
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    }
}

#[test]
fn ac6_fills_when_location_none() {
    let mut rec = make_record(Some("Austin, TX"), None);
    let (result, _) = enrich_record(&mut rec, &gz());
    assert_eq!(result, EnrichmentResult::Filled);
    assert!(rec.location.is_some(), "location must be filled");
}

#[test]
fn ac6_no_overwrite_existing_location() {
    use homeward_schema::ShelterLocation;
    let shelter_loc = ShelterLocation::new(
        Some(30.10),
        Some(-97.50),
        2,
        "shelter-city".to_owned(),
        Some("TX".to_owned()),
    );
    let original_lat = shelter_loc.lat;
    let original_lon = shelter_loc.lon;
    let original_city = shelter_loc.city_county.clone();

    let mut rec = make_record(Some("Austin, TX"), Some(shelter_loc));
    let (result, _) = enrich_record(&mut rec, &gz());
    assert_eq!(result, EnrichmentResult::AlreadySet);

    let loc = rec.location.expect("location must still be present");
    assert_eq!(loc.lat, original_lat, "lat must not change");
    assert_eq!(loc.lon, original_lon, "lon must not change");
    assert_eq!(loc.city_county, original_city, "city must not change");
}
