//! AC1: Offline gazetteer loads from a checked-in public-domain data file with
//! no network access; load succeeds in a test from a fixture file.

use homeward_geocode::Gazetteer;

/// The gazetteer data is embedded at compile time (include_str!).
/// No network access occurs — grep for any reqwest/hyper/ureq in Cargo.toml
/// and this test would still pass in a fully-offline environment.
#[test]
fn ac1_gazetteer_loads_offline() {
    // load_default() uses include_str!("../data/gazetteer.csv") — no filesystem
    // or network access at runtime.
    let gz = Gazetteer::load_default().expect("gazetteer must load from embedded data");
    // Spot-check: at least one known place is present.
    assert!(
        gz.lookup_name_state("austin", "TX").is_some(),
        "Austin TX must be in the default gazetteer"
    );
}

/// Confirm load_csv works from a fixture string (models loading from a file
/// in environments where include_str! is substituted by file bytes).
#[test]
fn ac1_load_csv_fixture() {
    let fixture = "P,Austin,TX,30.2672,-97.7431,\nZ,78701,TX,30.2707,-97.7424\n";
    let gz = Gazetteer::load_csv(fixture).expect("fixture load must succeed");
    assert!(gz.lookup_name_state("austin", "TX").is_some());
    assert!(gz.lookup_zip("78701").is_some());
}
