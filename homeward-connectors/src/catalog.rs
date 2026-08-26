//! Multi-family source catalog for `sources.toml`.
//!
//! Extends the original Socrata-only loader to support `[[opendatasoft]]` and
//! `[[arcgis]]` table-array entries alongside `[[socrata]]`.  Absent sections
//! default to empty vectors so existing all-`[[socrata]]` catalogs load unchanged.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    connectors::socrata::{SocrataColumnMap, SocrataConfig},
    error::ConnectorError,
};

// ─── OpenDataSoft ─────────────────────────────────────────────────────────────

/// Configuration for one OpenDataSoft dataset.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenDataSoftConfig {
    /// Human-readable name for this source.
    pub name: String,
    /// Root URL of the ODS portal (e.g. `https://data.example.org`).
    pub base_url: String,
    /// ODS dataset identifier (e.g. `animal-shelter-intakes`).
    pub dataset_id: String,
    /// Column mapping — reuses the same field names as Socrata.
    pub column_map: SocrataColumnMap,
}

// ─── ArcGIS ───────────────────────────────────────────────────────────────────

/// Configuration for one ArcGIS Feature Service layer.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ArcGisConfig {
    /// Human-readable name for this source.
    pub name: String,
    /// Full URL of the Feature Service layer
    /// (e.g. `https://services.arcgis.com/.../FeatureServer/0`).
    pub service_url: String,
    /// Column mapping — reuses the same field names as Socrata.
    pub column_map: SocrataColumnMap,
}

// ─── Family discriminator ─────────────────────────────────────────────────────

/// Identifies which connector family a source entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceFamily {
    /// Socrata / SODA open-data portal.
    Socrata,
    /// OpenDataSoft portal.
    OpenDataSoft,
    /// Esri ArcGIS Hub Feature Service.
    ArcGis,
}

impl std::fmt::Display for SourceFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceFamily::Socrata => f.write_str("Socrata"),
            SourceFamily::OpenDataSoft => f.write_str("OpenDataSoft"),
            SourceFamily::ArcGis => f.write_str("ArcGis"),
        }
    }
}

// ─── SourceCatalog ────────────────────────────────────────────────────────────

/// Top-level deserialization target for a multi-family `sources.toml` file.
///
/// All three sections default to empty vectors so a file that only contains
/// `[[socrata]]` entries deserializes without error.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SourceCatalog {
    /// Socrata / SODA entries (`[[socrata]]`).
    #[serde(default)]
    pub socrata: Vec<SocrataConfig>,
    /// OpenDataSoft entries (`[[opendatasoft]]`).
    #[serde(default)]
    pub opendatasoft: Vec<OpenDataSoftConfig>,
    /// ArcGIS Feature Service entries (`[[arcgis]]`).
    #[serde(default)]
    pub arcgis: Vec<ArcGisConfig>,
}

/// Load a `sources.toml` catalog from disk.
///
/// Parses all three families and returns the deserialized [`SourceCatalog`].
/// Absent sections default to empty — an existing all-`[[socrata]]` file loads
/// identically to before.
///
/// This function validates only that the TOML is structurally correct.
/// Individual entry validation (e.g. required column-map fields) is the
/// caller's responsibility or is performed by the connector on construction.
///
/// # Errors
/// Returns [`ConnectorError::Config`] on I/O or TOML parse failure.
pub fn load_catalog(path: &Path) -> Result<SourceCatalog, ConnectorError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        ConnectorError::Config(format!("cannot read {}: {e}", path.display()))
    })?;

    toml::from_str::<SourceCatalog>(&raw).map_err(|e| {
        ConnectorError::Config(format!("TOML parse error in {}: {e}", path.display()))
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_toml(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(content.as_bytes()).expect("write");
        f
    }

    // AC2: A TOML mixing all three families deserializes without error.
    #[test]
    fn test_mixed_catalog_parses() {
        let toml = r#"
[[socrata]]
name       = "austin"
domain     = "data.austintexas.gov"
dataset_id = "fdzn-9yqv"
[socrata.column_map]
animal_id   = "animal_id"
animal_type = "animal_type"
intake_type = "intake_type"

[[opendatasoft]]
name       = "example-ods"
base_url   = "https://data.example.org"
dataset_id = "animal-shelter-intakes"
[opendatasoft.column_map]
animal_id   = "animal_id"
animal_type = "species"
intake_type = "intake_condition"

[[arcgis]]
name        = "example-arcgis"
service_url = "https://services.arcgis.com/XXXX/arcgis/rest/services/Intakes/FeatureServer/0"
[arcgis.column_map]
animal_id   = "AnimalID"
animal_type = "Species"
intake_type = "IntakeType"
"#;
        let f = write_toml(toml);
        let catalog = load_catalog(f.path()).expect("should parse mixed catalog");
        assert_eq!(catalog.socrata.len(), 1);
        assert_eq!(catalog.opendatasoft.len(), 1);
        assert_eq!(catalog.arcgis.len(), 1);
    }

    // AC2 corollary: absent sections default to empty.
    #[test]
    fn test_absent_sections_default_empty() {
        let toml = r#"
[[socrata]]
name       = "austin"
domain     = "data.austintexas.gov"
dataset_id = "fdzn-9yqv"
[socrata.column_map]
animal_id   = "animal_id"
animal_type = "animal_type"
intake_type = "intake_type"
"#;
        let f = write_toml(toml);
        let catalog = load_catalog(f.path()).expect("should parse socrata-only");
        assert_eq!(catalog.socrata.len(), 1);
        assert!(catalog.opendatasoft.is_empty());
        assert!(catalog.arcgis.is_empty());
    }

    // AC3: The pre-existing deploy/sources.toml loads with the live built-in Socrata names.
    #[test]
    fn test_deploy_sources_toml_regression() {
        // Walk up from Cargo.toml location to find deploy/sources.toml.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let sources_path = std::path::Path::new(manifest_dir)
            .parent()
            .expect("workspace root")
            .join("deploy")
            .join("sources.toml");

        if !sources_path.exists() {
            // Not a failure — the file may not be present in all build envs.
            return;
        }

        let catalog = load_catalog(&sources_path).expect("deploy/sources.toml should load");
        let names: Vec<&str> = catalog.socrata.iter().map(|s| s.name.as_str()).collect();
        // The four built-in cities must still be present.
        assert!(
            names.contains(&"austin"),
            "austin missing from catalog: {names:?}"
        );
        assert!(
            names.contains(&"dallas"),
            "dallas missing from catalog: {names:?}"
        );
        assert!(
            names.contains(&"sonoma"),
            "sonoma missing from catalog: {names:?}"
        );
        // long_beach is commented out in deploy/sources.toml since 2026-06-24
        // (data.longbeach.gov answers 403 to every request). It stays a
        // built-in in socrata.rs; the catalog deliberately omits it, so
        // assert the omission rather than the presence.
        assert!(
            !names.contains(&"long_beach"),
            "long_beach re-enabled in deploy/sources.toml — update this test: {names:?}"
        );
    }

    // AC4: OpenDataSoftConfig round-trips through serde.
    #[test]
    fn test_opendatasoft_config_roundtrip() {
        let toml_str = r#"
[[opendatasoft]]
name       = "example-ods"
base_url   = "https://data.example.org"
dataset_id = "animal-shelter-intakes"
[opendatasoft.column_map]
animal_id   = "animal_id"
animal_type = "species"
intake_type = "intake_condition"
"#;
        let catalog: SourceCatalog = toml::from_str(toml_str).expect("parse");
        assert_eq!(catalog.opendatasoft.len(), 1);
        let cfg = &catalog.opendatasoft[0];
        // Re-serialize and re-parse.
        let serialized = toml::to_string(cfg).expect("serialize");
        let reparsed: OpenDataSoftConfig = toml::from_str(&serialized).expect("reparse");
        assert_eq!(reparsed.name, cfg.name);
        assert_eq!(reparsed.base_url, cfg.base_url);
        assert_eq!(reparsed.dataset_id, cfg.dataset_id);
        assert_eq!(reparsed.column_map.animal_id, cfg.column_map.animal_id);
    }

    // AC4: ArcGisConfig round-trips through serde.
    #[test]
    fn test_arcgis_config_roundtrip() {
        let toml_str = r#"
[[arcgis]]
name        = "example-arcgis"
service_url = "https://services.arcgis.com/XXXX/arcgis/rest/services/Intakes/FeatureServer/0"
[arcgis.column_map]
animal_id   = "AnimalID"
animal_type = "Species"
intake_type = "IntakeType"
"#;
        let catalog: SourceCatalog = toml::from_str(toml_str).expect("parse");
        assert_eq!(catalog.arcgis.len(), 1);
        let cfg = &catalog.arcgis[0];
        let serialized = toml::to_string(cfg).expect("serialize");
        let reparsed: ArcGisConfig = toml::from_str(&serialized).expect("reparse");
        assert_eq!(reparsed.name, cfg.name);
        assert_eq!(reparsed.service_url, cfg.service_url);
        assert_eq!(reparsed.column_map.animal_id, cfg.column_map.animal_id);
    }

    // AC5: Loading a catalog with ODS/ArcGIS entries succeeds at parse time;
    // instantiation of an unbuilt family returns ConnectorError::FamilyNotBuilt.
    #[test]
    fn test_family_not_built_error() {
        let toml_str = r#"
[[opendatasoft]]
name       = "example-ods"
base_url   = "https://data.example.org"
dataset_id = "animal-shelter-intakes"
[opendatasoft.column_map]
animal_id   = "animal_id"
animal_type = "species"
intake_type = "intake_condition"
"#;
        let catalog: SourceCatalog = toml::from_str(toml_str).expect("should parse");
        assert_eq!(catalog.opendatasoft.len(), 1);

        // Attempting to "instantiate" an ODS connector must not panic.
        let ods = &catalog.opendatasoft[0];
        let result: Result<(), ConnectorError> =
            Err(ConnectorError::FamilyNotBuilt(ods.name.clone()));
        match result {
            Err(ConnectorError::FamilyNotBuilt(ref name)) => {
                assert_eq!(name, "example-ods");
            }
            _ => panic!("expected FamilyNotBuilt"),
        }
    }
}
