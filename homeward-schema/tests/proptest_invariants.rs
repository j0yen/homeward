//! Property-based invariant tests for homeward-schema types.

use homeward_schema::geo::ShelterLocation;
use homeward_schema::{Species, UnknownSpeciesError};
use proptest::prelude::*;

proptest! {
    /// Any known species round-trips through JSON without change.
    #[test]
    fn species_json_roundtrip(species in prop_oneof![Just(Species::Dog), Just(Species::Cat)]) {
        let json = serde_json::to_string(&species).expect("serialize");
        let back: Species = serde_json::from_str(&json).expect("deserialize");
        prop_assert_eq!(back, species);
    }

    /// Geo coarsening: the stored value has at most `precision` significant
    /// decimal digits (tested by verifying the residual is below a
    /// floating-point–appropriate epsilon scaled to the precision).
    #[test]
    fn geo_precision_bounded(
        lat in -90.0_f64..=90.0_f64,
        lon in -180.0_f64..=180.0_f64,
        precision in 0_u8..=6_u8,
    ) {
        let loc = ShelterLocation::new(Some(lat), Some(lon), precision, "City".into(), None);
        // After round-trip through (v * factor).round() / factor, the residual
        // should be < 0.5 ULP of the factor — i.e., the stored value is the
        // nearest representable float to the mathematically-rounded result.
        // We allow a generous epsilon of 5e-7 (well within GPS precision needs).
        let epsilon = 5e-7_f64;
        if let Some(stored_lat) = loc.lat {
            let factor = 10_f64.powi(i32::from(precision));
            #[allow(clippy::float_arithmetic)]
            let residual = ((stored_lat * factor) - (stored_lat * factor).round()).abs();
            prop_assert!(residual < epsilon, "lat residual too large: {residual}");
        }
        if let Some(stored_lon) = loc.lon {
            let factor = 10_f64.powi(i32::from(precision));
            #[allow(clippy::float_arithmetic)]
            let residual = ((stored_lon * factor) - (stored_lon * factor).round()).abs();
            prop_assert!(residual < epsilon, "lon residual too large: {residual}");
        }
    }

    /// Species::from_str_strict never silently returns a species for
    /// strings that are not exact known values.
    #[test]
    fn unknown_species_always_errors(s in "[a-zA-Z0-9 ]{1,30}") {
        let lower = s.trim().to_lowercase();
        let known = matches!(lower.as_str(), "dog" | "canine" | "cat" | "feline");
        let result = Species::from_str_strict(&s);
        if known {
            prop_assert!(result.is_ok(), "known species {s:?} should parse");
        } else {
            prop_assert!(
                matches!(result, Err(UnknownSpeciesError(_))),
                "unknown species {s:?} should fail, got {result:?}"
            );
        }
    }
}
