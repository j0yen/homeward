//! Shared test helpers for homeward-report unit tests.
//!
//! These are test-only; the module is cfg(test)-gated at the lib.rs level.

use chrono::{Duration, Utc};
use homeward_schema::{
    Availability, BrokeredContactToken, ChipStatus, CoarseLocation, IntakeType, LostReport,
    LostStatus, PetRecord, ShelterLocation, SourceId, Species, TosClass,
};
use ulid::Ulid;

use crate::alerts::MatchCandidate;

/// Build a minimal [`LostReport`] for testing.
#[must_use]
pub fn make_report(report_id: &str) -> LostReport {
    let now = Utc::now();
    LostReport {
        report_id: report_id.to_owned(),
        species: Species::Dog,
        breed_primary: Some("Labrador".to_owned()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        description: None,
        photos: vec![],
        last_seen: CoarseLocation {
            zip_code: Some("78701".to_owned()),
            city: Some("Austin".to_owned()),
            state: Some("TX".to_owned()),
            radius_miles: None,
        },
        contact: BrokeredContactToken::new("tok_0123456789abcdef".to_owned()),
        created: now,
        expires: now + Duration::days(90),
        status: LostStatus::Active,
    }
}

/// Build a [`MatchCandidate`] for testing.
///
/// If `stray_in_hold` is true, sets `reclaimable_until` to 3 days from now.
#[must_use]
pub fn make_candidate(score: f32, stray_in_hold: bool) -> MatchCandidate {
    let now = Utc::now();
    let reclaimable_until = if stray_in_hold {
        Some(now + Duration::days(3))
    } else {
        None
    };
    MatchCandidate {
        record: make_pet_record(Species::Dog, Some("Austin"), Some("TX")),
        score,
        reclaimable_until,
    }
}

/// Build a minimal [`PetRecord`] for testing.
#[must_use]
pub fn make_pet_record(species: Species, city: Option<&str>, state: Option<&str>) -> PetRecord {
    let now = Utc::now();
    let city_county = city.unwrap_or("Unknown City").to_owned();
    PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("test-source", TosClass::OpenData),
        source_animal_id: None,
        species,
        breed_primary: Some("Mixed".to_owned()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        markings_text: None,
        intake_type: IntakeType::Stray,
        availability: Availability::InCustody,
        chip_status: ChipStatus::NotScanned,
        location: Some(ShelterLocation::new(
            None,
            None,
            2,
            city_county,
            state.map(str::to_owned),
        )),
        found_location_text: None,
        photos: vec![],
        first_seen: now,
        last_seen: now,
        last_confirmed: None,
        intake_date: Some(now),
        outcome_date: None,
    }
}
