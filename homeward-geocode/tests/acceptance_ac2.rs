//! AC2: The parser resolves a 5-digit ZIP to a coarse centroid; unit-tested
//! for a known ZIP against an expected coarse cell.

use homeward_geocode::{parse_found_location, Gazetteer, ParsedLocation};

#[test]
fn ac2_zip_78701_resolves_to_austin_coarse() {
    let gz = Gazetteer::load_default().expect("load");
    let loc = match parse_found_location("78701", &gz) {
        ParsedLocation::Resolved(l) => l,
        other => panic!("expected Resolved, got {other:?}"),
    };
    let lat = loc.lat.expect("lat must be present");
    let lon = loc.lon.expect("lon must be present");
    // Austin TX centroid rounded to precision 2: lat ~30.27, lon ~-97.74
    assert!(
        (lat - 30.27_f64).abs() < 0.015,
        "lat={lat} not near Austin (expected ~30.27)"
    );
    assert!(
        (lon - (-97.74_f64)).abs() < 0.015,
        "lon={lon} not near Austin (expected ~-97.74)"
    );
    assert_eq!(loc.state.as_deref(), Some("TX"), "state must be TX");
}

#[test]
fn ac2_zip_90801_resolves_to_long_beach_coarse() {
    let gz = Gazetteer::load_default().expect("load");
    let loc = match parse_found_location("90801", &gz) {
        ParsedLocation::Resolved(l) => l,
        other => panic!("expected Resolved for 90801, got {other:?}"),
    };
    let lat = loc.lat.expect("lat");
    // Long Beach CA: ~33.78
    assert!((lat - 33.78_f64).abs() < 0.02, "lat={lat}");
    assert_eq!(loc.state.as_deref(), Some("CA"));
}
