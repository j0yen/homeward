//! AC8: Enrichment is idempotent: running it twice yields the same record.

use homeward_geocode::{enrich_record, EnrichmentResult, Gazetteer};
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
fn ac8_idempotent_run_twice() {
    let gz = gz();
    let mut rec = stray_record("Austin, TX");

    let (r1, s1) = enrich_record(&mut rec, &gz);
    assert_eq!(r1, EnrichmentResult::Filled);
    let loc_after_first = rec.location.clone().expect("filled after first run");

    // Second run: location is already set.
    let (r2, s2) = enrich_record(&mut rec, &gz);
    assert_eq!(r2, EnrichmentResult::AlreadySet);
    assert!(s2.is_none(), "no source on second run");
    let loc_after_second = rec.location.clone().expect("still set after second run");

    // The location must be byte-identical.
    assert_eq!(loc_after_first.lat, loc_after_second.lat, "lat unchanged");
    assert_eq!(loc_after_first.lon, loc_after_second.lon, "lon unchanged");
    assert_eq!(
        loc_after_first.city_county, loc_after_second.city_county,
        "city unchanged"
    );
    assert_eq!(
        loc_after_first.precision, loc_after_second.precision,
        "precision unchanged"
    );

    let _ = s1; // suppress warning
}
