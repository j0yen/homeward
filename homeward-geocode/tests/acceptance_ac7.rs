//! AC7: The fill is provenance-tagged so downstream can tell a found-text
//! geocode from a shelter-reported location; asserted on an enriched record.

use homeward_geocode::{enrich_record, EnrichmentResult, Gazetteer, LocationSource};
use homeward_schema::{Availability, ChipStatus, IntakeType, PetRecord, SourceId, Species, TosClass};

fn gz() -> Gazetteer {
    Gazetteer::load_default().expect("load")
}

fn stray_record(found_text: &str) -> PetRecord {
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
        location: None,
        found_location_text: Some(found_text.to_owned()),
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
fn ac7_provenance_tag_is_found_text() {
    let mut rec = stray_record("Austin, TX");
    let (result, source) = enrich_record(&mut rec, &gz());
    assert_eq!(result, EnrichmentResult::Filled, "should have filled");
    assert_eq!(
        source,
        Some(LocationSource::FoundText),
        "source must be FoundText, not Shelter"
    );
}

#[test]
fn ac7_shelter_location_has_no_found_text_tag() {
    use homeward_schema::ShelterLocation;
    use chrono::Utc;
    // A record whose location is already set (shelter-reported)
    let shelter_loc = ShelterLocation::new(Some(30.27), Some(-97.74), 2, "Austin".into(), Some("TX".into()));
    let mut rec = PetRecord {
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
        location: Some(shelter_loc),
        found_location_text: None,
        photos: vec![],
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        last_confirmed: None,
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    };
    let (_result, source) = enrich_record(&mut rec, &gz());
    // No FoundText tag when AlreadySet
    assert!(
        source.is_none(),
        "shelter-reported location must not be tagged FoundText"
    );
}
