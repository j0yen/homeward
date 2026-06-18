//! AC9: A record enriched from found-text becomes geo-filterable by the
//! existing GET /intake ZIP/state path.
//!
//! The report API's geo filter operates on PetRecord.location.state and
//! location.lat/lon. This test documents that an enriched record has those
//! fields set to values that would pass a state/ZIP filter — verified at the
//! unit level (the report binary is a separate crate and out of unit scope).

use homeward_geocode::{enrich_record, EnrichmentResult, Gazetteer};
use homeward_schema::{Availability, ChipStatus, IntakeType, PetRecord, SourceId, Species, TosClass};

fn gz() -> Gazetteer {
    Gazetteer::load_default().expect("load")
}

fn stray_record_tx(found_text: &str) -> PetRecord {
    use chrono::Utc;
    PetRecord {
        canonical_id: ulid::Ulid::new(),
        source: SourceId::new("test-tx", TosClass::OpenData),
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

/// Simulate the state filter used by GET /intake?state=TX
fn state_filter(record: &PetRecord, state: &str) -> bool {
    record
        .location
        .as_ref()
        .and_then(|loc| loc.state.as_deref())
        .map(|s| s.eq_ignore_ascii_case(state))
        .unwrap_or(false)
}

/// Simulate the lat/lon bounding-box filter used by GET /intake?zip=...
fn bbox_filter(record: &PetRecord, min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> bool {
    record.location.as_ref().map(|loc| {
        let lat = loc.lat.unwrap_or(f64::NAN);
        let lon = loc.lon.unwrap_or(f64::NAN);
        #[allow(clippy::float_arithmetic)]
        let in_box = lat >= min_lat && lat <= max_lat && lon >= min_lon && lon <= max_lon;
        in_box
    }).unwrap_or(false)
}

#[test]
fn ac9_enriched_record_passes_state_filter() {
    let gz = gz();
    let mut rec = stray_record_tx("Austin, TX");
    let (result, _) = enrich_record(&mut rec, &gz);
    assert_eq!(result, EnrichmentResult::Filled);
    assert!(
        state_filter(&rec, "TX"),
        "enriched record should pass TX state filter"
    );
}

#[test]
fn ac9_enriched_record_passes_bbox_filter_for_austin() {
    let gz = gz();
    let mut rec = stray_record_tx("78701");
    let (result, _) = enrich_record(&mut rec, &gz);
    assert_eq!(result, EnrichmentResult::Filled);
    // Austin TX bounding box (coarse): lat 29.5–31.0, lon -98.5 to -97.0
    assert!(
        bbox_filter(&rec, 29.5, 31.0, -98.5, -97.0),
        "enriched record should be inside Austin TX bounding box"
    );
}

#[test]
fn ac9_un_enriched_record_fails_filter() {
    let mut rec = stray_record_tx("xyzzy frobnitz");
    let gz = gz();
    let (result, _) = enrich_record(&mut rec, &gz);
    assert_eq!(result, EnrichmentResult::NotResolved);
    assert!(
        !state_filter(&rec, "TX"),
        "un-enriched record must NOT pass state filter"
    );
}
