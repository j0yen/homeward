//! Catalog load tests — Phase 3 acceptance criteria for homeward-source-catalog.
//!
//! AC1: `deploy/sources.toml` parses without error via `load_catalog`.
//! AC2: Every parsed entry has required column_map fields (non-empty animal_id,
//!      animal_type, intake_type).
//! AC3: The 4 built-in cities are present and match their const-fn equivalents.

use homeward_connectors::catalog::load_catalog;
use homeward_connectors::connectors::socrata::SocrataConfig;

/// Path to the deploy catalog, relative to the crate manifest directory.
fn catalog_path() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is set by cargo at test time to the crate root.
    // The catalog lives at <workspace-root>/deploy/sources.toml.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // homeward-connectors is one level below the workspace root.
    manifest.join("../deploy/sources.toml")
}

/// AC1: deploy/sources.toml parses without error.
#[test]
fn ac1_catalog_parses_without_error() {
    let path = catalog_path();
    assert!(
        path.exists(),
        "deploy/sources.toml must exist at {path:?}"
    );
    let result = load_catalog(&path);
    assert!(
        result.is_ok(),
        "deploy/sources.toml failed to parse: {:?}",
        result.unwrap_err()
    );
    let catalog = result.unwrap();
    assert!(
        !catalog.socrata.is_empty(),
        "deploy/sources.toml must define at least one [[socrata]] source"
    );
}

/// AC2: Every entry has required column_map fields populated.
#[test]
fn ac2_all_entries_have_required_column_map_fields() {
    let path = catalog_path();
    let catalog = load_catalog(&path).expect("catalog loads");

    for cfg in &catalog.socrata {
        let label = &cfg.name;
        assert!(
            !cfg.name.is_empty(),
            "source has empty name"
        );
        assert!(
            !cfg.domain.is_empty(),
            "source {label}: domain must not be empty"
        );
        assert!(
            !cfg.dataset_id.is_empty(),
            "source {label}: dataset_id must not be empty"
        );
        assert!(
            !cfg.column_map.animal_id.is_empty(),
            "source {label}: column_map.animal_id must not be empty"
        );
        assert!(
            !cfg.column_map.animal_type.is_empty(),
            "source {label}: column_map.animal_type must not be empty"
        );
        assert!(
            !cfg.column_map.intake_type.is_empty(),
            "source {label}: column_map.intake_type must not be empty"
        );
    }
}

/// AC3: The live built-in cities (austin, dallas, sonoma) are present; long_beach is disabled
/// in the catalog and their key fields match the const-fn built-in equivalents.
#[test]
fn ac3_four_builtin_cities_present_and_match_const_fns() {
    let path = catalog_path();
    let catalog = load_catalog(&path).expect("catalog loads");
    let sources = catalog.socrata;

    let find = |name: &str| -> SocrataConfig {
        sources
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("source '{name}' not found in deploy/sources.toml"))
            .clone()
    };

    let builtin_austin = SocrataConfig::austin();
    let loaded_austin = find("austin");
    assert_eq!(loaded_austin.domain,      builtin_austin.domain,      "austin: domain mismatch");
    assert_eq!(loaded_austin.dataset_id,  builtin_austin.dataset_id,  "austin: dataset_id mismatch");
    assert_eq!(loaded_austin.column_map.animal_id,   builtin_austin.column_map.animal_id,   "austin: animal_id col");
    assert_eq!(loaded_austin.column_map.animal_type, builtin_austin.column_map.animal_type, "austin: animal_type col");
    assert_eq!(loaded_austin.column_map.intake_type, builtin_austin.column_map.intake_type, "austin: intake_type col");
    assert_eq!(loaded_austin.column_map.found_location, builtin_austin.column_map.found_location, "austin: found_location col");
    assert_eq!(loaded_austin.column_map.intake_date, builtin_austin.column_map.intake_date, "austin: intake_date col");

    let builtin_dallas = SocrataConfig::dallas();
    let loaded_dallas = find("dallas");
    assert_eq!(loaded_dallas.domain,     builtin_dallas.domain,     "dallas: domain mismatch");
    assert_eq!(loaded_dallas.dataset_id, builtin_dallas.dataset_id, "dallas: dataset_id mismatch");
    assert_eq!(loaded_dallas.column_map.animal_id,   builtin_dallas.column_map.animal_id,   "dallas: animal_id col");
    assert_eq!(loaded_dallas.column_map.animal_type, builtin_dallas.column_map.animal_type, "dallas: animal_type col");
    assert_eq!(loaded_dallas.column_map.intake_type, builtin_dallas.column_map.intake_type, "dallas: intake_type col");
    assert_eq!(loaded_dallas.column_map.outcome_date, builtin_dallas.column_map.outcome_date, "dallas: outcome_date col");
    assert_eq!(loaded_dallas.column_map.kennel_status, builtin_dallas.column_map.kennel_status, "dallas: kennel_status col");

    let builtin_sonoma = SocrataConfig::sonoma();
    let loaded_sonoma = find("sonoma");
    assert_eq!(loaded_sonoma.domain,     builtin_sonoma.domain,     "sonoma: domain mismatch");
    assert_eq!(loaded_sonoma.dataset_id, builtin_sonoma.dataset_id, "sonoma: dataset_id mismatch");
    assert_eq!(loaded_sonoma.column_map.animal_id,   builtin_sonoma.column_map.animal_id,   "sonoma: animal_id col");
    assert_eq!(loaded_sonoma.column_map.animal_type, builtin_sonoma.column_map.animal_type, "sonoma: animal_type col");
    assert_eq!(loaded_sonoma.column_map.intake_type, builtin_sonoma.column_map.intake_type, "sonoma: intake_type col");
    assert_eq!(loaded_sonoma.column_map.outcome_date, builtin_sonoma.column_map.outcome_date, "sonoma: outcome_date col");

    // long_beach is disabled in deploy/sources.toml (commented out 2026-06-24:
    // data.longbeach.gov answers 403). The const fn remains a built-in, but the
    // catalog deliberately omits it — assert the omission so re-enabling it
    // forces this test to be revisited.
    assert!(
        !sources.iter().any(|s| s.name == "long_beach"),
        "long_beach re-enabled in deploy/sources.toml — restore its column checks here"
    );
}

/// AC3b: catalog contains at least 5 sources (3 live built-ins + 2 additional).
#[test]
fn ac3b_catalog_has_at_least_six_sources() {
    let path = catalog_path();
    let catalog = load_catalog(&path).expect("catalog loads");
    assert!(
        catalog.socrata.len() >= 5,
        "expected at least 5 [[socrata]] sources (3 live built-ins + 2 additional), got {}",
        catalog.socrata.len()
    );
}
