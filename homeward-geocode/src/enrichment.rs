//! Enrichment policy: fill [`PetRecord::location`] from `found_location_text`
//! when `location` is `None`.
//!
//! - Never overwrites a real shelter location.
//! - Provenance-tagged via [`LocationSource::FoundText`].
//! - Idempotent: calling twice yields the same record.

use homeward_schema::PetRecord;

use crate::{
    gazetteer::{Gazetteer, LocationSource},
    parser::{parse_found_location, ParsedLocation},
};

/// Outcome of [`enrich_record`].
#[derive(Debug, Clone, PartialEq)]
pub enum EnrichmentResult {
    /// `location` was `None` and was successfully filled from `found_location_text`.
    Filled,
    /// `location` was already set; record left unchanged.
    AlreadySet,
    /// `found_location_text` was absent or could not be parsed; no change.
    NotResolved,
}

/// Fill `record.location` from `found_location_text` when `location` is `None`.
///
/// Policy:
/// - Only fills when `record.location.is_none()`.
/// - Records `location_source` on the returned `ShelterLocation` indirectly via
///   [`LocationSource::FoundText`] (the tag lives in [`GeoTaggedRecord`] or is
///   carried by the caller convention — see the module-level docs).
/// - Returns [`EnrichmentResult`] so callers can log or observe what happened.
///
/// Because the function mutates `record` in place the operation is idempotent:
/// a second call finds `location.is_some()` → returns `AlreadySet`.
pub fn enrich_record(
    record: &mut PetRecord,
    gazetteer: &Gazetteer,
) -> (EnrichmentResult, Option<LocationSource>) {
    // Policy: never overwrite a real shelter location.
    if record.location.is_some() {
        return (EnrichmentResult::AlreadySet, None);
    }

    let Some(ref text) = record.found_location_text.clone() else {
        return (EnrichmentResult::NotResolved, None);
    };

    match parse_found_location(text, gazetteer) {
        ParsedLocation::Resolved(loc) => {
            record.location = Some(loc);
            (EnrichmentResult::Filled, Some(LocationSource::FoundText))
        }
        ParsedLocation::NotInGazetteer | ParsedLocation::Unrecognised => {
            (EnrichmentResult::NotResolved, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeward_schema::{
        Availability, ChipStatus, IntakeType, PetRecord, SourceId, Species, TosClass,
    };
    use ulid::Ulid;

    fn make_record(
        found_text: Option<&str>,
        location: Option<homeward_schema::ShelterLocation>,
    ) -> PetRecord {
        use chrono::Utc;
        PetRecord {
            canonical_id: Ulid::new(),
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

    fn gz() -> Gazetteer {
        Gazetteer::load_default().expect("load")
    }

    #[test]
    fn fills_when_location_none() {
        let mut rec = make_record(Some("Austin, TX"), None);
        let (result, source) = enrich_record(&mut rec, &gz());
        assert_eq!(result, EnrichmentResult::Filled);
        assert_eq!(source, Some(LocationSource::FoundText));
        assert!(rec.location.is_some());
    }

    #[test]
    fn does_not_overwrite_existing_location() {
        use homeward_schema::ShelterLocation;
        let existing = ShelterLocation::new(Some(30.1), Some(-97.5), 2, "existing".into(), None);
        let mut rec = make_record(Some("Austin, TX"), Some(existing.clone()));
        let (result, source) = enrich_record(&mut rec, &gz());
        assert_eq!(result, EnrichmentResult::AlreadySet);
        assert!(source.is_none());
        // location is byte-identical to original
        let loc = rec.location.expect("location");
        assert_eq!(loc.lat, existing.lat);
        assert_eq!(loc.lon, existing.lon);
        assert_eq!(loc.city_county, existing.city_county);
    }

    #[test]
    fn no_found_text_returns_not_resolved() {
        let mut rec = make_record(None, None);
        let (result, source) = enrich_record(&mut rec, &gz());
        assert_eq!(result, EnrichmentResult::NotResolved);
        assert!(source.is_none());
        assert!(rec.location.is_none());
    }

    #[test]
    fn garbage_text_returns_not_resolved() {
        let mut rec = make_record(Some("xyzzy frobnitz"), None);
        let (result, _) = enrich_record(&mut rec, &gz());
        assert_eq!(result, EnrichmentResult::NotResolved);
        assert!(rec.location.is_none());
    }

    #[test]
    fn idempotent_two_runs_same_result() {
        let mut rec = make_record(Some("Austin, TX"), None);
        let gz = gz();
        let (result1, _) = enrich_record(&mut rec, &gz);
        assert_eq!(result1, EnrichmentResult::Filled);

        // Second call: location is now Some, so AlreadySet is returned.
        let (result2, _) = enrich_record(&mut rec, &gz);
        assert_eq!(result2, EnrichmentResult::AlreadySet);

        // Record is unchanged from after first call.
        let loc = rec.location.expect("location");
        // Austin lat at precision 2
        assert_eq!(loc.precision, 2);
    }

    #[test]
    fn provenance_tagged_as_found_text() {
        let mut rec = make_record(Some("78701"), None);
        let (result, source) = enrich_record(&mut rec, &gz());
        assert_eq!(result, EnrichmentResult::Filled);
        assert_eq!(source, Some(LocationSource::FoundText));
    }
}
