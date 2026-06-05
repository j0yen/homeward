//! AC2: Species::from_str_strict rejects unrecognized species strings.

use homeward_schema::{Species, UnknownSpeciesError};

#[test]
fn ac2_known_species_dog() {
    let s = Species::from_str_strict("dog").expect("dog should parse");
    assert_eq!(s, Species::Dog);
}

#[test]
fn ac2_known_species_cat() {
    let s = Species::from_str_strict("cat").expect("cat should parse");
    assert_eq!(s, Species::Cat);
}

#[test]
fn ac2_known_species_aliases() {
    assert_eq!(
        Species::from_str_strict("canine").expect("canine should parse"),
        Species::Dog
    );
    assert_eq!(
        Species::from_str_strict("feline").expect("feline should parse"),
        Species::Cat
    );
}

#[test]
fn ac2_unknown_species_fails_with_typed_error() {
    let result = Species::from_str_strict("fish");
    match result {
        Err(UnknownSpeciesError(s)) => assert_eq!(s, "fish"),
        Ok(_) => panic!("fish should not parse as a known species"),
    }
}

#[test]
fn ac2_unknown_species_no_silent_default() {
    // A variety of unknown inputs all produce Err, none silently become Dog or Cat.
    for unknown in &["rabbit", "bird", "reptile", "unknown", "", "  "] {
        assert!(
            Species::from_str_strict(unknown).is_err(),
            "expected Err for {unknown:?}"
        );
    }
}
