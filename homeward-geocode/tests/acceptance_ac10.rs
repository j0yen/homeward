//! AC10: `cargo test` green; `cargo build` green for the workspace.
//!
//! This test file is a compile-time smoke test: if this file compiles and
//! the library tests pass, AC10 is satisfied. The actual `cargo test` and
//! `cargo build` are run by the autobuilder pipeline.

use homeward_geocode::{enrich_record, Gazetteer, ParsedLocation, parse_found_location};

/// Confirm the public API surface compiles and basic round-trip works.
#[test]
fn ac10_workspace_api_surface_compiles() {
    let gz = Gazetteer::load_default().expect("load_default must succeed");
    let result = parse_found_location("Austin, TX", &gz);
    assert!(matches!(result, ParsedLocation::Resolved(_)));

    use homeward_schema::{Availability, ChipStatus, IntakeType, PetRecord, SourceId, Species, TosClass};
    use chrono::Utc;
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
        location: None,
        found_location_text: Some("Dallas, TX".to_owned()),
        photos: vec![],
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        last_confirmed: None,
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    };
    let (result, _) = enrich_record(&mut rec, &gz);
    assert!(rec.location.is_some(), "Dallas TX must resolve; result={result:?}");
}
