//! AC5: Resolved points are rounded to the existing privacy precision (±~1.1 km),
//! asserted by checking the coordinate decimal places match ShelterLocation's
//! default precision.

use homeward_geocode::{parse_found_location, Gazetteer, ParsedLocation};

fn gz() -> Gazetteer {
    Gazetteer::load_default().expect("load")
}

/// Assert that a float has at most `max_dp` decimal places of significant
/// precision (i.e. multiplied by 10^max_dp and rounded is an integer).
fn assert_at_most_dp(val: f64, max_dp: u8, label: &str) {
    #[allow(clippy::float_arithmetic)]
    let scale = 10_f64.powi(i32::from(max_dp));
    #[allow(clippy::float_arithmetic)]
    let rounded = (val * scale).round() / scale;
    let diff = (val - rounded).abs();
    assert!(
        diff < 1e-9,
        "{label}: {val} has more than {max_dp} decimal places of precision (diff={diff})"
    );
}

#[test]
fn ac5_zip_result_at_precision_2() {
    let gz = gz();
    let loc = match parse_found_location("78701", &gz) {
        ParsedLocation::Resolved(l) => l,
        other => panic!("expected Resolved, got {other:?}"),
    };
    assert_eq!(loc.precision, 2, "precision field must be 2");
    if let Some(lat) = loc.lat {
        assert_at_most_dp(lat, 2, "lat");
    }
    if let Some(lon) = loc.lon {
        assert_at_most_dp(lon, 2, "lon");
    }
}

#[test]
fn ac5_city_state_result_at_precision_2() {
    let gz = gz();
    let loc = match parse_found_location("Austin, TX", &gz) {
        ParsedLocation::Resolved(l) => l,
        other => panic!("expected Resolved, got {other:?}"),
    };
    assert_eq!(loc.precision, 2, "precision field must be 2");
    if let Some(lat) = loc.lat {
        assert_at_most_dp(lat, 2, "lat");
    }
    if let Some(lon) = loc.lon {
        assert_at_most_dp(lon, 2, "lon");
    }
}
