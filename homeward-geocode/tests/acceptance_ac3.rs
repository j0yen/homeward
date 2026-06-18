//! AC3: The parser resolves "City, ST" and "City ST" forms; unit-tested for
//! ≥3 real city/state pairs from existing source metros.

use homeward_geocode::{parse_found_location, Gazetteer, ParsedLocation};

fn gz() -> Gazetteer {
    Gazetteer::load_default().expect("load")
}

fn assert_resolved(text: &str, gz: &Gazetteer) {
    match parse_found_location(text, gz) {
        ParsedLocation::Resolved(_) => {}
        other => panic!("{text:?} expected Resolved, got {other:?}"),
    }
}

#[test]
fn ac3_city_comma_state_forms() {
    let gz = gz();
    assert_resolved("Austin, TX", &gz);
    assert_resolved("Dallas, TX", &gz);
    assert_resolved("Long Beach, CA", &gz);
    assert_resolved("Sonoma, CA", &gz);
}

#[test]
fn ac3_city_space_state_forms() {
    let gz = gz();
    assert_resolved("Austin TX", &gz);
    assert_resolved("Dallas TX", &gz);
    assert_resolved("Long Beach CA", &gz);
}

#[test]
fn ac3_city_comma_state_with_prefix() {
    let gz = gz();
    // Common shelter description formats
    assert_resolved("Near downtown, Austin, TX", &gz);
    assert_resolved("Found near shelter, Dallas, TX", &gz);
}
