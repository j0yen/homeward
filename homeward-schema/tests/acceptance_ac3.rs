//! AC3: IntakeType and Availability are distinct fields; validator flags
//! the stray-hold contradiction.

use chrono::Utc;
use homeward_schema::{
    chip::ChipStatus,
    intake::{Availability, IntakeType},
    provenance::{SourceId, TosClass},
    validate_pet_record, PetRecord, Species, ValidationError,
};
use ulid::Ulid;

fn make_pet(intake_type: IntakeType, availability: Availability) -> PetRecord {
    PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("test", TosClass::Api),
        source_animal_id: None,
        species: Species::Dog,
        breed_primary: None,
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        markings_text: None,
        intake_type,
        availability,
        chip_status: ChipStatus::NotScanned,
        location: None,
        found_location_text: None,
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
fn ac3_intake_and_availability_are_distinct_fields() {
    // Both fields are independently accessible on PetRecord.
    let pet = make_pet(IntakeType::Stray, Availability::InCustody);
    assert_eq!(pet.intake_type, IntakeType::Stray);
    assert_eq!(pet.availability, Availability::InCustody);
}

#[test]
fn ac3_stray_adoptable_is_flagged() {
    let pet = make_pet(IntakeType::Stray, Availability::Adoptable);
    let result = validate_pet_record(&pet);
    assert_eq!(
        result,
        Err(ValidationError::StrayHoldConflict),
        "Stray + Adoptable must return StrayHoldConflict"
    );
}

#[test]
fn ac3_stray_in_custody_is_valid() {
    let pet = make_pet(IntakeType::Stray, Availability::InCustody);
    assert!(
        validate_pet_record(&pet).is_ok(),
        "Stray + InCustody is a valid combination"
    );
}

#[test]
fn ac3_owner_surrender_adoptable_is_valid() {
    let pet = make_pet(IntakeType::OwnerSurrender, Availability::Adoptable);
    assert!(
        validate_pet_record(&pet).is_ok(),
        "OwnerSurrender + Adoptable is valid"
    );
}

#[test]
fn ac3_found_report_departed_is_valid() {
    let pet = make_pet(IntakeType::FoundReport, Availability::Departed);
    assert!(
        validate_pet_record(&pet).is_ok(),
        "FoundReport + Departed is valid"
    );
}
