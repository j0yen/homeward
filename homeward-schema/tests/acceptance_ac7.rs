//! AC7: Geo coarsening rounds lat/lon to configured precision on construction.

use homeward_schema::geo::ShelterLocation;

#[test]
fn ac7_precise_coord_stored_at_coarse_precision_2dp() {
    let loc = ShelterLocation::new(
        Some(47.123_456),
        Some(-122.654_321),
        2,
        "Seattle".into(),
        None,
    );
    // Precision 2 → round to 2 decimal places.
    assert_eq!(
        loc.lat,
        Some(47.12),
        "lat should be rounded to 2 dp, got {:?}",
        loc.lat
    );
    assert_eq!(
        loc.lon,
        Some(-122.65),
        "lon should be rounded to 2 dp, got {:?}",
        loc.lon
    );
    assert_eq!(loc.precision, 2);
}

#[test]
fn ac7_precision_0_stores_integer_degrees() {
    let loc = ShelterLocation::new(Some(47.9_f64), Some(-122.1_f64), 0, "Seattle".into(), None);
    assert_eq!(loc.lat, Some(48.0));
    assert_eq!(loc.lon, Some(-122.0));
}

#[test]
fn ac7_precision_4_preserves_more_detail() {
    let loc =
        ShelterLocation::new(Some(47.123_456), Some(-122.654_321), 4, "Seattle".into(), None);
    assert_eq!(loc.lat, Some(47.1235));
    assert_eq!(loc.lon, Some(-122.6543));
}

#[test]
fn ac7_none_coords_stay_none() {
    let loc = ShelterLocation::new(None, None, 2, "Seattle".into(), None);
    assert!(loc.lat.is_none());
    assert!(loc.lon.is_none());
}

#[test]
fn ac7_coarsened_location_json_roundtrip() {
    let loc = ShelterLocation::new(
        Some(47.123_456),
        Some(-122.654_321),
        2,
        "Seattle".into(),
        Some("WA".into()),
    );
    let json = serde_json::to_string(&loc).expect("serialize ShelterLocation");
    let back: ShelterLocation =
        serde_json::from_str(&json).expect("deserialize ShelterLocation");
    assert_eq!(back.lat, loc.lat);
    assert_eq!(back.lon, loc.lon);
    assert_eq!(back.precision, loc.precision);
    assert_eq!(back.city_county, loc.city_county);
}
