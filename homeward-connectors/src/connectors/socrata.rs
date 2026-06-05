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

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use homeward_schema::{
    Availability, ChipStatus, IntakeType, PetRecord, Provenance, Species, TosClass,
    provenance::SourceId,
};
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
#[derive(Debug, Clone)]
pub struct SocrataColumnMap {
    /// Column for animal ID.
    pub animal_id: &'static str,
    /// Column for species / animal type.
    pub animal_type: &'static str,
    /// Column for intake type.
    pub intake_type: &'static str,
    /// Column for availability / outcome date (or kennel status).
    pub outcome_date: Option<&'static str>,
    /// Column for kennel / custody status.
    pub kennel_status: Option<&'static str>,
    /// Column for found location text.
    pub found_location: Option<&'static str>,
    /// Column for chip / microchip status.
    pub chip_status: Option<&'static str>,
    /// Column for intake date.
    pub intake_date: Option<&'static str>,
    /// Breed column name.
    pub breed: Option<&'static str>,
    /// Name column.
    pub name: Option<&'static str>,
    /// Color column.
    pub color: Option<&'static str>,
}

/// Configuration for one Socrata dataset.
#[derive(Debug, Clone)]
pub struct SocrataConfig {
    /// SODA API domain (e.g. `data.austintexas.gov`).
    pub domain: &'static str,
    /// Dataset 4-by-4 ID (e.g. `fdzn-9yqv`).
    pub dataset_id: &'static str,
    /// Human-readable name for this source.
    pub name: &'static str,
    /// Column mapping.
    pub column_map: SocrataColumnMap,
    /// Optional SODA app token (from env).
    pub app_token_env: Option<&'static str>,
}

impl SocrataConfig {
    /// Austin Animal Center Intakes — `fdzn-9yqv`.
    #[must_use]
    pub const fn austin() -> Self {
        Self {
            domain: "data.austintexas.gov",
            dataset_id: "fdzn-9yqv",
            name: "austin",
            column_map: SocrataColumnMap {
                animal_id: "animal_id",
                animal_type: "animal_type",
                intake_type: "intake_type",
                outcome_date: None,
                kennel_status: None,
                found_location: Some("found_location"),
                chip_status: None,
                intake_date: Some("datetime"),
                breed: Some("breed"),
                name: Some("name"),
                color: Some("color"),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN"),
        }
    }

    /// Dallas Animal Services — `qgg6-h4bd`.
    #[must_use]
    pub const fn dallas() -> Self {
        Self {
            domain: "www.dallasopendata.com",
            dataset_id: "qgg6-h4bd",
            name: "dallas",
            column_map: SocrataColumnMap {
                animal_id: "animal_id",
                animal_type: "animal_type",
                intake_type: "intake_type",
                outcome_date: Some("outcome_datetime"),
                kennel_status: Some("kennel_status"),
                found_location: Some("found_location"),
                chip_status: Some("chip_status"),
                intake_date: Some("intake_date"),
                breed: Some("breed"),
                name: Some("animal_name"),
                color: Some("color"),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN"),
        }
    }

    /// Sonoma County Animal Services — `924a-vesw`.
    #[must_use]
    pub const fn sonoma() -> Self {
        Self {
            domain: "data.sonomacounty.ca.gov",
            dataset_id: "924a-vesw",
            name: "sonoma",
            column_map: SocrataColumnMap {
                animal_id: "id",
                animal_type: "type",
                intake_type: "intake_subtype",
                outcome_date: Some("outcome_date"),
                kennel_status: None,
                found_location: Some("location_found"),
                chip_status: None,
                intake_date: Some("intake_date"),
                breed: Some("primary_breed"),
                name: Some("name"),
                color: Some("primary_color"),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN"),
        }
    }

    /// Long Beach Animal Care Services.
    #[must_use]
    pub const fn long_beach() -> Self {
        Self {
            domain: "data.longbeach.gov",
            dataset_id: "d9np-nk5h",
            name: "long_beach",
            column_map: SocrataColumnMap {
                animal_id: "animal_id",
                animal_type: "animal_type",
                intake_type: "intake_type",
                outcome_date: Some("outcome_date"),
                kennel_status: None,
                found_location: Some("found_location"),
                chip_status: None,
                intake_date: Some("intake_date"),
                breed: Some("primary_breed"),
                name: Some("animal_name"),
                color: Some("primary_color"),
            },
            app_token_env: Some("SOCRATA_APP_TOKEN"),
        }
    }
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
    pub const fn with_client(config: SocrataConfig, client: PoliteClient) -> Self {
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
            if let Some(env_var) = self.config.app_token_env {
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
            source: SourceId::new(self.config.name, TosClass::OpenData),
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

    let animal_id = get(cm.animal_id).map(std::borrow::ToOwned::to_owned);

    let animal_type_str = get(cm.animal_type).unwrap_or("unknown");
    let species = parse_socrata_species(animal_type_str)?;

    let intake_type_str = get(cm.intake_type).unwrap_or("");
    let intake_type = parse_socrata_intake_type(intake_type_str);

    let availability = determine_availability(row, cm);

    let chip_status = cm
        .chip_status
        .and_then(get)
        .map_or(ChipStatus::Unknown, parse_socrata_chip_status);

    let found_location_text = cm.found_location.and_then(get).map(str::to_owned);

    let intake_date = cm
        .intake_date
        .and_then(get)
        .and_then(parse_soda_datetime);

    let outcome_date = cm
        .outcome_date
        .and_then(get)
        .and_then(parse_soda_datetime);

    let now = Utc::now();
    let last_seen = intake_date.unwrap_or(now);
    let first_seen = last_seen;

    let breed_primary = cm.breed.and_then(get).map(str::to_owned);
    let color = cm.color.and_then(get).map(str::to_owned);

    Ok(PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new(config.name, TosClass::OpenData),
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
    if let Some(col) = cm.outcome_date {
        if let Some(v) = row.get(col).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return Availability::Departed;
            }
        }
    }
    // Sonoma: null outcome_date means still in custody.
    // Dallas: kennel_status
    if let Some(col) = cm.kennel_status {
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
}
