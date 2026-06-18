//! AC4: Ambiguous or unrecognised text returns None (no guessing); tested with
//! garbage input and an unknown city.

use homeward_geocode::{parse_found_location, Gazetteer, ParsedLocation};

fn gz() -> Gazetteer {
    Gazetteer::load_default().expect("load")
}

fn assert_not_resolved(text: &str, gz: &Gazetteer) {
    match parse_found_location(text, gz) {
        ParsedLocation::Resolved(loc) => {
            panic!("{text:?} should NOT have resolved, got {loc:?}")
        }
        ParsedLocation::NotInGazetteer | ParsedLocation::Unrecognised => {}
    }
}

#[test]
fn ac4_garbage_input() {
    let gz = gz();
    assert_not_resolved("xyzzy frobnitz", &gz);
    assert_not_resolved("????? ????", &gz);
    assert_not_resolved("   ", &gz);
    assert_not_resolved("", &gz);
}

#[test]
fn ac4_unknown_city() {
    let gz = gz();
    // Real state, invented city
    assert_not_resolved("Xanadu, TX", &gz);
    assert_not_resolved("Metropolis, NY", &gz);
    assert_not_resolved("Gotham, CA", &gz);
}

#[test]
fn ac4_partial_state_code_not_guessed() {
    let gz = gz();
    // "T" is not a valid 2-letter state — should not resolve
    assert_not_resolved("Austin T", &gz);
}

#[test]
fn ac4_number_that_is_not_a_zip() {
    let gz = gz();
    // 4-digit and 6-digit numbers are not ZIPs
    assert_not_resolved("1234", &gz);
    assert_not_resolved("123456", &gz);
}
