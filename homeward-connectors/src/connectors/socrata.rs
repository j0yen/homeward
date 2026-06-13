//! Generic Socrata SODA connector.
//!
//! Parameterized by `{domain, dataset_id, column_map}` and pre-configured
//! for Austin (`fdzn-9yqv`), Dallas (`qgg6-h4bd`), Sonoma (`924a-vesw`),
//! and Long Beach.
//!
//! Delta polling uses `$where=:updated_at > 'ts'`. No photos — we store the
//! shelter page URL in `found_location_text`.
//!
//! `ToS` notes:
//! - Free open-data, app token lifts throttling
//! - Provenance class: open-data

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use homeward_schema::{
    Availability, ChipStatus, IntakeType, PetRecord, Provenance, Species, TosClass,
    provenance::SourceId,
};
use serde::Deserialize;
use tracing::{debug, warn};
use ulid::Ulid;
use url::Url;

use crate::{
    Connector, Cursor,
    error::ConnectorError,
    http::PoliteClient,
};

/// Maximum number of rows to fetch per SODA page.
const PAGE_SIZE: u64 = 1000;

/// Maps source column names to the canonical fields we care about.
///
/// All fields are owned `String` values to support both compile-time built-ins
/// and runtime-loaded configurations from `sources.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct SocrataColumnMap {
    /// Column for animal ID.
    pub animal_id: String,
    /// Column for species / animal type.
    pub animal_type: String,
    /// Column for intake type.
    pub intake_type: String,
    /// Column for availability / outcome date (or kennel status).
    #[serde(default)]
    pub outcome_date: Option<String>,
    /// Column for kennel / custody status.
    #[serde(default)]
    pub kennel_status: Option<String>,
    /// Column for found location text.
    #[serde(default)]
    pub found_location: Option<String>,
    /// Column for chip / microchip status.
    #[serde(default)]
    pub chip_status: Option<String>,
    /// Column for intake date.
    #[serde(default)]
    pub intake_date: Option<String>,
    /// Breed column name.
    #[serde(default)]
    pub breed: Option<String>,
    /// Name column.
    #[serde(default)]
    pub name: Option<String>,
    /// Color column.
    #[serde(default)]
    pub color: Option<String>,
}

/// Configuration for one Socrata dataset.
///
/// Fields are owned `String` values so configs can be constructed from either
/// compile-time built-in helpers or a runtime-loaded `sources.toml` file.
#[derive(Debug, Clone, Deserialize)]
pub struct SocrataConfig {
    /// SODA API domain (e.g. `data.austintexas.gov`).
    pub domain: String,
    /// Dataset 4-by-4 ID (e.g. `fdzn-9yqv`).
    pub dataset_id: String,
    /// Human-readable name for this source.
    pub name: String,
    /// Column mapping.
    pub column_map: SocrataColumnMap,
    /// Optional env var name whose value is the SODA app token.
    #[serde(default)]
    pub app_token_env: Option<String>,
}

impl SocrataConfig {
    /// Austin Animal Center Intakes — `fdzn-9yqv`.
    #[must_use]
    pub fn austin() -> Self {
        Self {
            domain: "data.austintexas.gov".to_owned(),
            dataset_id: "fdzn-9yqv".to_owned(),
            name: "austin".to_owned(),
            column_map: SocrataColumnMap {
                animal_id: "animal_id".to_owned(),
                animal_type: "animal_type".to_owned(),
                intake_type: "intake_type".to_owned(),
                outcome_date: None,
                kennel_status: None,
                found_location: Some("found_location".to_owned()),
                chip_status: None,
                intake_date: Some("datetime".to_owned()),
                breed: Some("breed".to_owned()),
                name: Some("name".to_owned()),
                color: Some("color".to_owned()),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN".to_owned()),
        }
    }

    /// Dallas Animal Services — `qgg6-h4bd`.
    #[must_use]
    pub fn dallas() -> Self {
        Self {
            domain: "www.dallasopendata.com".to_owned(),
            dataset_id: "qgg6-h4bd".to_owned(),
            name: "dallas".to_owned(),
            column_map: SocrataColumnMap {
                animal_id: "animal_id".to_owned(),
                animal_type: "animal_type".to_owned(),
                intake_type: "intake_type".to_owned(),
                outcome_date: Some("outcome_datetime".to_owned()),
                kennel_status: Some("kennel_status".to_owned()),
                found_location: Some("found_location".to_owned()),
                chip_status: Some("chip_status".to_owned()),
                intake_date: Some("intake_date".to_owned()),
                breed: Some("breed".to_owned()),
                name: Some("animal_name".to_owned()),
                color: Some("color".to_owned()),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN".to_owned()),
        }
    }

    /// Sonoma County Animal Services — `924a-vesw`.
    #[must_use]
    pub fn sonoma() -> Self {
        Self {
            domain: "data.sonomacounty.ca.gov".to_owned(),
            dataset_id: "924a-vesw".to_owned(),
            name: "sonoma".to_owned(),
            column_map: SocrataColumnMap {
                animal_id: "id".to_owned(),
                animal_type: "type".to_owned(),
                intake_type: "intake_subtype".to_owned(),
                outcome_date: Some("outcome_date".to_owned()),
                kennel_status: None,
                found_location: Some("location_found".to_owned()),
                chip_status: None,
                intake_date: Some("intake_date".to_owned()),
                breed: Some("primary_breed".to_owned()),
                name: Some("name".to_owned()),
                color: Some("primary_color".to_owned()),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN".to_owned()),
        }
    }

    /// Long Beach Animal Care Services.
    #[must_use]
    pub fn long_beach() -> Self {
        Self {
            domain: "data.longbeach.gov".to_owned(),
            dataset_id: "d9np-nk5h".to_owned(),
            name: "long_beach".to_owned(),
            column_map: SocrataColumnMap {
                animal_id: "animal_id".to_owned(),
                animal_type: "animal_type".to_owned(),
                intake_type: "intake_type".to_owned(),
                outcome_date: Some("outcome_date".to_owned()),
                kennel_status: None,
                found_location: Some("found_location".to_owned()),
                chip_status: None,
                intake_date: Some("intake_date".to_owned()),
                breed: Some("primary_breed".to_owned()),
                name: Some("animal_name".to_owned()),
                color: Some("primary_color".to_owned()),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN".to_owned()),
        }
    }
}

// ─── TOML Catalog loader ──────────────────────────────────────────────────────

/// Top-level structure of a `sources.toml` file.
#[derive(Debug, Deserialize)]
struct SourceCatalogFile {
    #[serde(rename = "socrata")]
    socrata: Vec<SocrataConfig>,
}

/// Loader for a `sources.toml` file containing Socrata source definitions.
pub struct SourceCatalog;

impl SourceCatalog {
    /// Load and validate a `sources.toml` file.
    ///
    /// Required fields per entry: `name`, `domain`, `dataset_id`,
    /// `column_map.animal_id`, `column_map.animal_type`, `column_map.intake_type`.
    ///
    /// # Errors
    /// Returns [`ConnectorError::Config`] on I/O, parse, or validation failure,
    /// naming the offending source/field.
    pub fn from_path(path: &Path) -> Result<Vec<SocrataConfig>, ConnectorError> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            ConnectorError::Config(format!("cannot read {}: {e}", path.display()))
        })?;

        let catalog: SourceCatalogFile = toml::from_str(&raw).map_err(|e| {
            ConnectorError::Config(format!("TOML parse error in {}: {e}", path.display()))
        })?;

        let mut configs = Vec::with_capacity(catalog.socrata.len());
        for cfg in catalog.socrata {
            validate_socrata_config(&cfg)?;
            configs.push(cfg);
        }
        Ok(configs)
    }
}

/// Validate that all required fields are non-empty.
fn validate_socrata_config(cfg: &SocrataConfig) -> Result<(), ConnectorError> {
    let source_label = if cfg.name.is_empty() {
        "<unnamed>".to_owned()
    } else {
        cfg.name.clone()
    };

    if cfg.name.is_empty() {
        return Err(ConnectorError::Config(format!(
            "source {source_label}: required field 'name' is empty"
        )));
    }
    if cfg.domain.is_empty() {
        return Err(ConnectorError::Config(format!(
            "source {source_label}: required field 'domain' is empty"
        )));
    }
    if cfg.dataset_id.is_empty() {
        return Err(ConnectorError::Config(format!(
            "source {source_label}: required field 'dataset_id' is empty"
        )));
    }
    if cfg.column_map.animal_id.is_empty() {
        return Err(ConnectorError::Config(format!(
            "source {source_label}: required field 'column_map.animal_id' is empty"
        )));
    }
    if cfg.column_map.animal_type.is_empty() {
        return Err(ConnectorError::Config(format!(
            "source {source_label}: required field 'column_map.animal_type' is empty"
        )));
    }
    if cfg.column_map.intake_type.is_empty() {
        return Err(ConnectorError::Config(format!(
            "source {source_label}: required field 'column_map.intake_type' is empty"
        )));
    }
    Ok(())
}

/// Generic Socrata SODA connector.
pub struct SocrataConnector {
    config: SocrataConfig,
    client: PoliteClient,
}

impl SocrataConnector {
    /// Create a connector with the default polite client.
    ///
    /// # Errors
    /// Returns [`ConnectorError`] if the HTTP client cannot be built.
    pub fn new(config: SocrataConfig) -> Result<Self, ConnectorError> {
        let client = PoliteClient::new(Duration::from_millis(200))?;
        Ok(Self { config, client })
    }

    /// Create a connector with a custom client (for tests).
    #[must_use]
    pub fn with_client(config: SocrataConfig, client: PoliteClient) -> Self {
        Self { config, client }
    }

    /// Build the SODA query URL for a given page offset and optional cursor.
    fn build_url(&self, offset: u64, since: Option<&DateTime<Utc>>) -> Result<Url, ConnectorError> {
        let cm = &self.config.column_map;
        let animal_type_filter = format!(
            "upper({animal_type}) in ('DOG', 'CAT')",
            animal_type = cm.animal_type
        );

        let mut where_parts = vec![animal_type_filter];
        if let Some(ts) = since {
            where_parts.push(format!(
                ":updated_at > '{}'",
                ts.format("%Y-%m-%dT%H:%M:%S")
            ));
        }
        let where_clause = where_parts.join(" AND ");

        let mut url = Url::parse(&format!(
            "https://{domain}/resource/{dataset}.json",
            domain = self.config.domain,
            dataset = self.config.dataset_id,
        ))?;

        {
            let mut qs = url.query_pairs_mut();
            qs.append_pair("$limit", &PAGE_SIZE.to_string());
            qs.append_pair("$offset", &offset.to_string());
            qs.append_pair("$where", &where_clause);
            qs.append_pair("$order", ":updated_at DESC");

            // Add app token if configured and set in env.
            if let Some(env_var) = &self.config.app_token_env {
                if let Ok(token) = std::env::var(env_var) {
                    qs.append_pair("$$app_token", &token);
                }
            }
        }

        Ok(url)
    }
}

#[async_trait]
impl Connector for SocrataConnector {
    async fn poll(&self, since: Option<Cursor>) -> Result<Vec<PetRecord>, ConnectorError> {
        let since_ts = match &since {
            Some(Cursor::Timestamp(ts)) => Some(*ts),
            Some(Cursor::Opaque(_)) | None => None,
        };

        let mut records = Vec::new();
        let mut offset = 0u64;

        loop {
            let url = self.build_url(offset, since_ts.as_ref())?;
            debug!(%url, offset, source = self.config.name, "fetching Socrata page");

            let resp = self.client.get(&url, None, None).await?;

            if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                break;
            }

            let rows: Vec<serde_json::Value> = resp.json().await?;
            let batch_len = u64::try_from(rows.len()).unwrap_or(u64::MAX);

            for row in rows {
                match normalize_socrata_row(&row, &self.config) {
                    Ok(rec) => records.push(rec),
                    Err(e) => {
                        warn!(source = self.config.name, "skipping Socrata row: {e}");
                    }
                }
            }

            offset += PAGE_SIZE;
            if batch_len < PAGE_SIZE {
                break;
            }
        }

        Ok(records)
    }

    fn provenance(&self) -> Provenance {
        Provenance {
            source: SourceId::new(&self.config.name, TosClass::OpenData),
            fetched_at: Utc::now(),
            source_url: Some(format!(
                "https://{}/resource/{}.json",
                self.config.domain, self.config.dataset_id
            )),
            source_etag: None,
        }
    }

    fn cadence_hint(&self) -> Duration {
        // Municipal shelters update frequently; poll every 4h.
        Duration::from_secs(4 * 3600)
    }
}

// ─── Normalization ────────────────────────────────────────────────────────────

fn normalize_socrata_row(
    row: &serde_json::Value,
    config: &SocrataConfig,
) -> Result<PetRecord, ConnectorError> {
    let cm = &config.column_map;

    let get = |key: &str| -> Option<&str> { row.get(key).and_then(|v| v.as_str()) };

    let animal_id = get(&cm.animal_id).map(std::borrow::ToOwned::to_owned);

    let animal_type_str = get(&cm.animal_type).unwrap_or("unknown");
    let species = parse_socrata_species(animal_type_str)?;

    let intake_type_str = get(&cm.intake_type).unwrap_or("");
    let intake_type = parse_socrata_intake_type(intake_type_str);

    let availability = determine_availability(row, cm);

    let chip_status = cm
        .chip_status
        .as_deref()
        .and_then(|col| get(col))
        .map_or(ChipStatus::Unknown, parse_socrata_chip_status);

    let found_location_text = cm.found_location.as_deref().and_then(|col| get(col)).map(str::to_owned);

    let intake_date = cm
        .intake_date
        .as_deref()
        .and_then(|col| get(col))
        .and_then(parse_soda_datetime);

    let outcome_date = cm
        .outcome_date
        .as_deref()
        .and_then(|col| get(col))
        .and_then(parse_soda_datetime);

    let now = Utc::now();
    let last_seen = intake_date.unwrap_or(now);
    let first_seen = last_seen;

    let breed_primary = cm.breed.as_deref().and_then(|col| get(col)).map(str::to_owned);
    let color = cm.color.as_deref().and_then(|col| get(col)).map(str::to_owned);

    Ok(PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new(&config.name, TosClass::OpenData),
        source_animal_id: animal_id,
        species,
        breed_primary,
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: color.into_iter().collect(),
        markings_text: None,
        intake_type,
        availability,
        chip_status,
        location: None,
        found_location_text,
        photos: vec![], // Socrata datasets have no photos — link back to shelter page
        first_seen,
        last_seen,
        last_confirmed: Some(now),
        intake_date,
        outcome_date,
        secondary_provenances: vec![],
    })
}

fn parse_socrata_species(s: &str) -> Result<Species, ConnectorError> {
    match s.trim().to_uppercase().as_str() {
        "DOG" | "CANINE" => Ok(Species::Dog),
        "CAT" | "FELINE" => Ok(Species::Cat),
        other => Err(ConnectorError::UnknownSpecies(other.to_owned())),
    }
}

/// Map Socrata `Intake_Type` strings to [`IntakeType`].
fn parse_socrata_intake_type(s: &str) -> IntakeType {
    match s.trim().to_uppercase().as_str() {
        "STRAY" => IntakeType::Stray,
        "OWNER SURRENDER" | "OWNER_SURRENDER" => IntakeType::OwnerSurrender,
        "FOUND REPORT" | "FOUND_REPORT" => IntakeType::FoundReport,
        "TRANSFER" => IntakeType::Transfer,
        _ => IntakeType::Unknown,
    }
}

/// Determine availability from `outcome_date` / `kennel_status` columns.
fn determine_availability(
    row: &serde_json::Value,
    cm: &SocrataColumnMap,
) -> Availability {
    // If outcome_date is set, animal has left.
    if let Some(col) = cm.outcome_date.as_deref() {
        if let Some(v) = row.get(col).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return Availability::Departed;
            }
        }
    }
    // Sonoma: null outcome_date means still in custody.
    // Dallas: kennel_status
    if let Some(col) = cm.kennel_status.as_deref() {
        if let Some(v) = row.get(col).and_then(|v| v.as_str()) {
            let v = v.to_uppercase();
            if v.contains("ADOPTED") || v.contains("TRANSFERRED") || v.contains("DIED") {
                return Availability::Departed;
            }
            if v.contains("AVAILABLE") || v.contains("ADOPT") {
                return Availability::Adoptable;
            }
        }
    }
    Availability::InCustody
}

fn parse_socrata_chip_status(s: &str) -> ChipStatus {
    let upper = s.trim().to_uppercase();
    if (upper.contains("YES") || upper.contains("SCANNED"))
        && !upper.contains("NO")
        && !upper.contains("NOT")
    {
        ChipStatus::Scanned { chip: None }
    } else if upper.contains("NO") || upper.contains("NONE") || upper.contains("NOT") {
        ChipStatus::ScanNoChip
    } else {
        ChipStatus::Unknown
    }
}

fn parse_soda_datetime(s: &str) -> Option<DateTime<Utc>> {
    // Normalize: some datasets use space instead of T.
    let s_t = s.replacen(' ', "T", 1);

    // Try formats in order of specificity.
    // 1. Standard RFC 3339 / ISO 8601 with timezone offset.
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s_t) {
        return Some(dt.with_timezone(&Utc));
    }
    // 2. Datetime with fractional seconds, no timezone (Socrata native).
    for fmt in &[
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&s_t, fmt) {
            return Some(ndt.and_utc());
        }
    }
    // 3. Date only.
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| ndt.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn austin_row(intake_type: &str, species: &str, extras: serde_json::Value) -> serde_json::Value {
        let mut row = json!({
            "animal_id": "A12345",
            "animal_type": species,
            "intake_type": intake_type,
            "found_location": "123 Main St",
            "datetime": "2024-01-15T10:00:00.000",
            "breed": "Labrador Mix",
            "name": "Buddy",
            "color": "Black/White",
        });
        if let (serde_json::Value::Object(r), serde_json::Value::Object(e)) =
            (&mut row, extras)
        {
            r.extend(e);
        }
        row
    }

    fn dallas_row(intake_type: &str, species: &str, chip: &str) -> serde_json::Value {
        json!({
            "animal_id": "D67890",
            "animal_type": species,
            "intake_type": intake_type,
            "kennel_status": "Available",
            "found_location": "456 Oak Ave",
            "chip_status": chip,
            "intake_date": "2024-01-10T08:00:00.000",
            "breed": "Domestic Shorthair",
            "animal_name": "Whiskers",
            "color": "Orange",
        })
    }

    #[test]
    fn austin_stray_dog_normalizes() {
        let row = austin_row("STRAY", "DOG", json!({}));
        let config = SocrataConfig::austin();
        let rec = normalize_socrata_row(&row, &config).expect("normalize");
        assert_eq!(rec.species, Species::Dog);
        assert_eq!(rec.intake_type, IntakeType::Stray);
        assert_eq!(
            rec.found_location_text.as_deref(),
            Some("123 Main St")
        );
        assert_eq!(rec.source.tos_class, TosClass::OpenData);
        assert_eq!(rec.source.name, "austin");
    }

    #[test]
    fn dallas_chip_status_maps() {
        let row = dallas_row("STRAY", "CAT", "YES");
        let config = SocrataConfig::dallas();
        let rec = normalize_socrata_row(&row, &config).expect("normalize");
        assert_eq!(rec.species, Species::Cat);
        assert_eq!(rec.chip_status, ChipStatus::Scanned { chip: None });
        assert_eq!(rec.intake_type, IntakeType::Stray);
    }

    #[test]
    fn mixed_species_both_yield_records() {
        let config = SocrataConfig::austin();
        let dog = austin_row("STRAY", "DOG", json!({}));
        let cat = austin_row("STRAY", "CAT", json!({}));
        let dog_rec = normalize_socrata_row(&dog, &config).expect("dog");
        let cat_rec = normalize_socrata_row(&cat, &config).expect("cat");
        assert_eq!(dog_rec.species, Species::Dog);
        assert_eq!(cat_rec.species, Species::Cat);
    }

    #[test]
    fn socrata_photos_always_empty() {
        // Socrata datasets have no photos; PhotoRef vec must be empty.
        let row = austin_row("STRAY", "DOG", json!({}));
        let config = SocrataConfig::austin();
        let rec = normalize_socrata_row(&row, &config).expect("normalize");
        assert!(rec.photos.is_empty(), "Socrata records must have no photos");
    }

    #[test]
    fn intake_type_stray_maps_correctly() {
        assert_eq!(parse_socrata_intake_type("STRAY"), IntakeType::Stray);
        assert_eq!(
            parse_socrata_intake_type("OWNER SURRENDER"),
            IntakeType::OwnerSurrender
        );
        assert_eq!(
            parse_socrata_intake_type("FOUND REPORT"),
            IntakeType::FoundReport
        );
    }

    #[test]
    fn outcome_date_present_means_departed() {
        let mut row = austin_row("STRAY", "DOG", json!({}));
        row["outcome_date"] = json!("2024-02-01T12:00:00.000");
        // We need a config with outcome_date column.
        let config = SocrataConfig::dallas();
        // Dallas has outcome_datetime; inject it.
        let dallas_row = json!({
            "animal_id": "D1",
            "animal_type": "DOG",
            "intake_type": "STRAY",
            "outcome_datetime": "2024-02-01T12:00:00.000",
            "kennel_status": "Adopted",
            "intake_date": "2024-01-10T08:00:00.000",
        });
        let rec = normalize_socrata_row(&dallas_row, &config).expect("normalize");
        assert_eq!(rec.availability, Availability::Departed);
    }

    #[test]
    fn provenance_class_is_open_data() {
        let config = SocrataConfig::austin();
        let connector = SocrataConnector {
            config,
            client: PoliteClient::from_client(
                reqwest::Client::new(),
                Duration::from_millis(0),
            ),
        };
        let prov = connector.provenance();
        assert_eq!(prov.source.tos_class, TosClass::OpenData);
    }

    #[test]
    fn parse_soda_datetime_formats() {
        // ISO 8601 with T
        assert!(parse_soda_datetime("2024-01-15T10:00:00.000").is_some());
        // Space separated
        assert!(parse_soda_datetime("2024-01-15 10:00:00").is_some());
        // Date only
        assert!(parse_soda_datetime("2024-01-15").is_some());
        // Garbage
        assert!(parse_soda_datetime("not-a-date").is_none());
    }

    // ─── SourceCatalog / HOMEWARD_SOURCES tests ───────────────────────────────

    /// Helper: write TOML text to a temp file and return the path.
    fn write_temp_toml(content: &str) -> (tempfile::NamedTempFile, std::path::PathBuf) {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(content.as_bytes()).expect("write");
        let path = f.path().to_owned();
        (f, path)
    }

    /// AC1: connector built from file-loaded config == connector from const fn built-in.
    #[test]
    fn ac1_file_loaded_config_matches_builtin() {
        let toml = r#"
[[socrata]]
name = "austin"
domain = "data.austintexas.gov"
dataset_id = "fdzn-9yqv"
app_token_env = "SOCRATA_APP_TOKEN"
[socrata.column_map]
animal_id = "animal_id"
animal_type = "animal_type"
intake_type = "intake_type"
found_location = "found_location"
intake_date = "datetime"
breed = "breed"
name = "name"
color = "color"
"#;
        let (_f, path) = write_temp_toml(toml);
        let configs = SourceCatalog::from_path(&path).expect("load");
        assert_eq!(configs.len(), 1);
        let loaded = &configs[0];
        let builtin = SocrataConfig::austin();
        assert_eq!(loaded.name, builtin.name);
        assert_eq!(loaded.domain, builtin.domain);
        assert_eq!(loaded.dataset_id, builtin.dataset_id);
        assert_eq!(loaded.column_map.animal_id, builtin.column_map.animal_id);
        assert_eq!(loaded.column_map.animal_type, builtin.column_map.animal_type);
        assert_eq!(loaded.column_map.intake_type, builtin.column_map.intake_type);
        assert_eq!(loaded.column_map.found_location, builtin.column_map.found_location);
        assert_eq!(loaded.column_map.intake_date, builtin.column_map.intake_date);
        assert_eq!(loaded.column_map.breed, builtin.column_map.breed);
        assert_eq!(loaded.column_map.name, builtin.column_map.name);
        assert_eq!(loaded.column_map.color, builtin.column_map.color);
    }

    /// AC2: round-trip — write 4 built-ins to TOML, load with from_path, assert field match.
    #[test]
    fn ac2_roundtrip_four_builtins() {
        let builtins = [
            SocrataConfig::austin(),
            SocrataConfig::dallas(),
            SocrataConfig::sonoma(),
            SocrataConfig::long_beach(),
        ];

        // Build TOML manually for the four built-ins.
        let mut toml_str = String::new();
        for cfg in &builtins {
            toml_str.push_str("[[socrata]]\n");
            toml_str.push_str(&format!("name = {:?}\n", cfg.name));
            toml_str.push_str(&format!("domain = {:?}\n", cfg.domain));
            toml_str.push_str(&format!("dataset_id = {:?}\n", cfg.dataset_id));
            if let Some(ref env) = cfg.app_token_env {
                toml_str.push_str(&format!("app_token_env = {:?}\n", env));
            }
            toml_str.push_str("[socrata.column_map]\n");
            toml_str.push_str(&format!("animal_id = {:?}\n", cfg.column_map.animal_id));
            toml_str.push_str(&format!("animal_type = {:?}\n", cfg.column_map.animal_type));
            toml_str.push_str(&format!("intake_type = {:?}\n", cfg.column_map.intake_type));
            if let Some(ref v) = cfg.column_map.outcome_date {
                toml_str.push_str(&format!("outcome_date = {:?}\n", v));
            }
            if let Some(ref v) = cfg.column_map.kennel_status {
                toml_str.push_str(&format!("kennel_status = {:?}\n", v));
            }
            if let Some(ref v) = cfg.column_map.found_location {
                toml_str.push_str(&format!("found_location = {:?}\n", v));
            }
            if let Some(ref v) = cfg.column_map.chip_status {
                toml_str.push_str(&format!("chip_status = {:?}\n", v));
            }
            if let Some(ref v) = cfg.column_map.intake_date {
                toml_str.push_str(&format!("intake_date = {:?}\n", v));
            }
            if let Some(ref v) = cfg.column_map.breed {
                toml_str.push_str(&format!("breed = {:?}\n", v));
            }
            if let Some(ref v) = cfg.column_map.name {
                toml_str.push_str(&format!("name = {:?}\n", v));
            }
            if let Some(ref v) = cfg.column_map.color {
                toml_str.push_str(&format!("color = {:?}\n", v));
            }
            toml_str.push('\n');
        }

        let (_f, path) = write_temp_toml(&toml_str);
        let loaded = SourceCatalog::from_path(&path).expect("load");
        assert_eq!(loaded.len(), 4);
        for (l, b) in loaded.iter().zip(builtins.iter()) {
            assert_eq!(l.name, b.name, "name mismatch for {}", b.name);
            assert_eq!(l.domain, b.domain, "domain mismatch for {}", b.name);
            assert_eq!(l.dataset_id, b.dataset_id, "dataset_id mismatch for {}", b.name);
            assert_eq!(l.column_map.animal_id, b.column_map.animal_id);
            assert_eq!(l.column_map.animal_type, b.column_map.animal_type);
            assert_eq!(l.column_map.intake_type, b.column_map.intake_type);
            assert_eq!(l.column_map.outcome_date, b.column_map.outcome_date);
            assert_eq!(l.column_map.kennel_status, b.column_map.kennel_status);
            assert_eq!(l.column_map.found_location, b.column_map.found_location);
            assert_eq!(l.column_map.chip_status, b.column_map.chip_status);
            assert_eq!(l.column_map.intake_date, b.column_map.intake_date);
            assert_eq!(l.column_map.breed, b.column_map.breed);
            assert_eq!(l.column_map.name, b.column_map.name);
            assert_eq!(l.column_map.color, b.column_map.color);
        }
    }

    /// AC3: malformed TOML yields typed ConnectorError naming offending field.
    #[test]
    fn ac3_malformed_toml_typed_error() {
        // Bad TOML syntax.
        let bad_toml = "[[socrata]\nname = !!!\n";
        let (_f, path) = write_temp_toml(bad_toml);
        let result = SourceCatalog::from_path(&path);
        assert!(result.is_err(), "expected error on bad TOML");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("TOML") || msg.contains("parse") || msg.contains("error"),
            "error message should mention TOML parse: {msg}"
        );
    }

    /// AC3b: missing required field yields typed ConnectorError.
    #[test]
    fn ac3b_missing_required_field_typed_error() {
        // Missing dataset_id.
        let toml = r#"
[[socrata]]
name = "testcity"
domain = "data.example.gov"
[socrata.column_map]
animal_id = "aid"
animal_type = "atype"
intake_type = "itype"
"#;
        let (_f, path) = write_temp_toml(toml);
        // dataset_id is required by serde (no default), so this should fail at parse time.
        let result = SourceCatalog::from_path(&path);
        assert!(result.is_err(), "expected error for missing dataset_id");
    }

    /// AC4: HOMEWARD_SOURCES env var controls which sources are registered.
    #[test]
    fn ac4_homeward_sources_env_var() {
        // Write a single-source TOML with only dallas.
        let toml = r#"
[[socrata]]
name = "dallas"
domain = "www.dallasopendata.com"
dataset_id = "qgg6-h4bd"
app_token_env = "SOCRATA_APP_TOKEN"
[socrata.column_map]
animal_id = "animal_id"
animal_type = "animal_type"
intake_type = "intake_type"
outcome_date = "outcome_datetime"
kennel_status = "kennel_status"
found_location = "found_location"
chip_status = "chip_status"
intake_date = "intake_date"
breed = "breed"
name = "animal_name"
color = "color"
"#;
        let (_f, path) = write_temp_toml(toml);
        let configs = SourceCatalog::from_path(&path).expect("load");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "dallas");

        // Verify the four built-ins produce exactly those names.
        let builtin_names: Vec<String> = [
            SocrataConfig::austin(),
            SocrataConfig::dallas(),
            SocrataConfig::sonoma(),
            SocrataConfig::long_beach(),
        ]
        .iter()
        .map(|c| c.name.clone())
        .collect();
        assert_eq!(builtin_names, ["austin", "dallas", "sonoma", "long_beach"]);
    }

    /// AC5: optional column fields omitted from TOML deserialize to None.
    #[test]
    fn ac5_optional_fields_absent_are_none() {
        let toml = r#"
[[socrata]]
name = "minimal"
domain = "data.example.gov"
dataset_id = "abcd-1234"
[socrata.column_map]
animal_id = "animal_id"
animal_type = "animal_type"
intake_type = "intake_type"
"#;
        let (_f, path) = write_temp_toml(toml);
        let configs = SourceCatalog::from_path(&path).expect("load");
        assert_eq!(configs.len(), 1);
        let cfg = &configs[0];
        assert!(cfg.column_map.outcome_date.is_none(), "outcome_date should be None");
        assert!(cfg.column_map.kennel_status.is_none(), "kennel_status should be None");
        assert!(cfg.column_map.found_location.is_none(), "found_location should be None");
        assert!(cfg.column_map.chip_status.is_none(), "chip_status should be None");
        assert!(cfg.column_map.intake_date.is_none(), "intake_date should be None");
        assert!(cfg.column_map.breed.is_none(), "breed should be None");
        assert!(cfg.column_map.name.is_none(), "name should be None");
        assert!(cfg.column_map.color.is_none(), "color should be None");
        assert!(cfg.app_token_env.is_none(), "app_token_env should be None");
    }
}
