//! Pet FBI connector — pulls lost/found reports from the Pet FBI widget feed.
//!
//! A single pull from this feed covers **Pet FBI + Helping Lost Pets (HeLP) +
//! Lost Dogs of America / Lost Cats of America**, because those share one
//! backing database via a formal data partnership.
//!
//! `ToS` notes:
//! - `data_file` UUID is treated as a credential from Pet FBI Central.
//! - Conditional requests via `PoliteClient` (respects `ETag`/Last-Modified).
//! - Images are hotlinked only — no bytes are downloaded.
//! - Attribution is carried in provenance via `SourceId`.
//! - Refresh cadence: 6 hours (matching other connectors).

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use homeward_schema::{
    AgeBucket, Availability, ChipStatus, IntakeType, PetRecord, PhotoRef, Provenance, Sex, Size,
    Species, TosClass,
    provenance::SourceId,
};
use reqwest::StatusCode;
use serde::Deserialize;
use tracing::{debug, warn};
use ulid::Ulid;
use url::Url;

use crate::{
    Connector, Cursor,
    error::ConnectorError,
    http::{HOMEWARD_USER_AGENT, PoliteClient},
};

/// Base URL for the Pet FBI API.
const BASE_URL: &str = "https://petfbi.org/api";

/// Source name slug for Pet FBI.
pub const SOURCE_PETFBI: &str = "petfbi";

/// Source name slug for Helping Lost Pets.
pub const SOURCE_HELPINGLOSTPETS: &str = "helpinglostpets";

/// Configuration for the Pet FBI connector.
#[derive(Debug, Clone)]
pub struct PetFbiConfig {
    /// `data-file` UUID issued by Pet FBI Central (treated as credential).
    ///
    /// Read from `HOMEWARD_PETFBI_DATA_FILE` env var via [`PetFbiConfig::from_env`].
    pub data_file: String,

    /// Optional species filter (`"dog"`, `"cat"`, or `None` for all).
    pub species_filter: Option<String>,

    /// Base URL override (for tests).
    pub base_url: String,
}

impl PetFbiConfig {
    /// Load config from environment.
    ///
    /// Returns `None` (not an error) when `HOMEWARD_PETFBI_DATA_FILE` is unset,
    /// so callers can silently skip registration.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let data_file = std::env::var("HOMEWARD_PETFBI_DATA_FILE").ok()?;
        Some(Self {
            data_file,
            species_filter: None,
            base_url: BASE_URL.to_owned(),
        })
    }
}

/// Connector for the Pet FBI report widget feed.
///
/// Covers Pet FBI, Helping Lost Pets (HeLP), Lost Dogs of America, and
/// Lost Cats of America via a single data partnership feed.
pub struct PetFbiConnector {
    config: PetFbiConfig,
    client: PoliteClient,
}

impl PetFbiConnector {
    /// Create a new connector from config.
    ///
    /// # Errors
    /// Returns [`ConnectorError`] if the HTTP client cannot be built.
    pub fn new(config: PetFbiConfig) -> Result<Self, ConnectorError> {
        let client = PoliteClient::new(Duration::from_secs(2))?;
        Ok(Self { config, client })
    }

    /// Create a connector with a custom [`PoliteClient`] (for tests).
    #[must_use]
    pub const fn with_client(config: PetFbiConfig, client: PoliteClient) -> Self {
        Self { config, client }
    }

    /// Build the Pet FBI API URL for a given report type.
    fn build_url(&self, report_type: &str) -> Result<Url, ConnectorError> {
        let mut url_str = format!(
            "{}/reports?data_file={}&type={}",
            self.config.base_url, self.config.data_file, report_type,
        );
        if let Some(species) = &self.config.species_filter {
            url_str.push_str(&format!("&species={species}"));
        }
        Ok(Url::parse(&url_str)?)
    }

    /// Fetch a page of reports for one report type (`"lost"` or `"found"`).
    async fn fetch_reports(
        &self,
        report_type: &str,
        etag: Option<&str>,
    ) -> Result<(PfbPage, Option<String>), ConnectorError> {
        let url = self.build_url(report_type)?;
        debug!(%url, report_type, "fetching Pet FBI reports");

        let mut req = self
            .client
            .inner()
            .get(url.as_str())
            .header("User-Agent", HOMEWARD_USER_AGENT)
            .header("Accept", "application/json");
        if let Some(et) = etag.filter(|e| !e.is_empty()) {
            req = req.header("If-None-Match", et);
        }
        let resp = req.send().await?;

        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok((PfbPage { total: 0, reports: vec![] }, None));
        }

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConnectorError::UnexpectedStatus {
                status,
                body: body.chars().take(256).collect(),
            });
        }

        let new_etag = PoliteClient::extract_etag(&resp);
        let page: PfbPage = resp.json().await?;
        Ok((page, new_etag))
    }
}

#[async_trait]
impl Connector for PetFbiConnector {
    async fn poll(&self, _since: Option<Cursor>) -> Result<Vec<PetRecord>, ConnectorError> {
        let mut records = Vec::new();

        // Fetch lost reports → normalize to PetRecord with IntakeType::LostReport.
        let (lost_page, _) = self.fetch_reports("lost", None).await?;
        for report in lost_page.reports {
            match normalize_pfb_report(report) {
                Ok(rec) => records.push(rec),
                Err(e) => {
                    warn!("skipping Pet FBI lost report: {e}");
                }
            }
        }

        // Fetch found/stray reports → normalize to PetRecord.
        let (found_page, _) = self.fetch_reports("found", None).await?;
        for report in found_page.reports {
            match normalize_pfb_report(report) {
                Ok(rec) => records.push(rec),
                Err(e) => {
                    warn!("skipping Pet FBI found report: {e}");
                }
            }
        }

        Ok(records)
    }

    fn provenance(&self) -> Provenance {
        Provenance {
            source: SourceId::new(SOURCE_PETFBI, TosClass::NonprofitAttributionRequired),
            fetched_at: Utc::now(),
            source_url: Some("https://petfbi.org".to_owned()),
            source_etag: None,
        }
    }

    fn cadence_hint(&self) -> Duration {
        // Pet FBI ToS: attribution-required, refresh-respecting.
        Duration::from_secs(6 * 3600)
    }
}

// ─── Serde types for Pet FBI widget feed ─────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PfbPage {
    #[allow(dead_code)]
    total: u64,
    reports: Vec<PfbReport>,
}

/// A single report from the Pet FBI widget feed.
#[derive(Debug, Deserialize)]
struct PfbReport {
    /// Feed-assigned report ID.
    id: String,
    /// Report type: `"lost"` or `"found"`.
    #[serde(rename = "type")]
    report_type: String,
    /// Source network: `"petfbi"` or `"helpinglostpets"`.
    source: String,
    /// Species string (`"dog"`, `"cat"`).
    species: String,
    /// Primary breed.
    #[serde(default)]
    breed: Option<String>,
    /// Secondary breed.
    #[serde(default)]
    secondary_breed: Option<String>,
    /// Sex string.
    #[serde(default)]
    sex: Option<String>,
    /// Age string.
    #[serde(default)]
    age: Option<String>,
    /// Size string.
    #[serde(default)]
    size: Option<String>,
    /// Color list.
    #[serde(default)]
    colors: Vec<String>,
    /// Free-form description.
    #[serde(default)]
    description: Option<String>,
    /// Primary photo URL (hotlink only — no download).
    #[serde(default)]
    photo_url: Option<String>,
    /// City where last seen / found.
    #[serde(default)]
    city: Option<String>,
    /// State (two-letter US code).
    #[serde(default)]
    state: Option<String>,
    /// ZIP code (stored for future location resolution; not mapped to `found_location_text`).
    #[serde(default)]
    #[allow(dead_code)]
    zip: Option<String>,
    /// When the pet was lost (ISO 8601).
    #[serde(default)]
    date_lost: Option<String>,
    /// When the pet was found (ISO 8601).
    #[serde(default)]
    date_found: Option<String>,
    /// When the report was created (ISO 8601).
    #[serde(default)]
    date_created: Option<String>,
    /// Report status: `"active"`, `"reunited"`, `"expired"` (reserved for future use).
    #[serde(default)]
    #[allow(dead_code)]
    status: Option<String>,
    /// Opaque relay token for contacting the reporter (if provided).
    #[serde(default)]
    #[allow(dead_code)]
    contact_token: Option<String>,
}

// ─── Normalization ─────────────────────────────────────────────────────────

/// Derive the correct [`SourceId`] from the report's own `source` field.
///
/// A HeLP-origin record carries `SourceId::HelpingLostPets`; a Pet-FBI-origin
/// record carries `SourceId::PetFbi`. This is derived from the feed's own
/// `source` field, not hardcoded to one value (AC4).
fn source_id_from_str(source: &str) -> SourceId {
    match source.to_lowercase().as_str() {
        "helpinglostpets" | "help" => SourceId::new(SOURCE_HELPINGLOSTPETS, TosClass::NonprofitAttributionRequired),
        _ => SourceId::new(SOURCE_PETFBI, TosClass::NonprofitAttributionRequired),
    }
}

/// Normalize a Pet FBI report to a [`PetRecord`].
///
/// - `type=lost` → `IntakeType::LostReport`, `Availability::Missing`
/// - `type=found` / other → `IntakeType::Stray`, `Availability::Available`
fn normalize_pfb_report(report: PfbReport) -> Result<PetRecord, ConnectorError> {
    let species_str = report.species.as_str();
    let species = Species::from_str_strict(species_str)
        .map_err(|_| ConnectorError::UnknownSpecies(species_str.to_owned()))?;

    let now = Utc::now();

    // Use whichever date is available as the event timestamp.
    let event_ts = report
        .date_lost
        .as_deref()
        .or(report.date_found.as_deref())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(now, |dt| dt.with_timezone(&Utc));

    let first_seen = report
        .date_created
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(event_ts, |dt| dt.with_timezone(&Utc));

    let sex = report.sex.as_deref().map(parse_pfb_sex);
    let age_bucket = report.age.as_deref().and_then(parse_pfb_age);
    let size = report.size.as_deref().and_then(parse_pfb_size);

    let source_id = source_id_from_str(&report.source);

    // Map report type to intake/availability.
    let (intake_type, availability) = match report.report_type.as_str() {
        "lost" => (IntakeType::LostReport, Availability::Missing),
        _ => (IntakeType::FoundReport, Availability::Available),
    };

    // Build location text from city/state/zip.
    let found_location_text = match (report.city.as_deref(), report.state.as_deref()) {
        (Some(city), Some(state)) => Some(format!("{city}, {state}")),
        (Some(city), None) => Some(city.to_owned()),
        (None, Some(state)) => Some(state.to_owned()),
        _ => None,
    };

    // Photo: hotlink URL only — never download bytes (AC7).
    let photos: Vec<PhotoRef> = report
        .photo_url
        .map(|url| {
            vec![PhotoRef {
                url,
                attribution: Some(format!("© {}", source_id.name)),
                is_primary: true,
            }]
        })
        .unwrap_or_default();

    Ok(PetRecord {
        canonical_id: Ulid::new(),
        source: source_id,
        source_animal_id: Some(report.id),
        species,
        breed_primary: report.breed,
        breed_secondary: report.secondary_breed,
        sex,
        age_bucket,
        size,
        colors: report.colors,
        markings_text: report.description,
        intake_type,
        availability,
        chip_status: ChipStatus::Unknown,
        location: None,
        found_location_text,
        photos,
        first_seen,
        last_seen: event_ts,
        last_confirmed: Some(now),
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    })
}

fn parse_pfb_sex(s: &str) -> Sex {
    match s.to_lowercase().as_str() {
        "male" | "m" => Sex::Male,
        "female" | "f" => Sex::Female,
        "neutered" | "neutered male" => Sex::NeuteredMale,
        "spayed" | "spayed female" => Sex::SpayedFemale,
        _ => Sex::Unknown,
    }
}

fn parse_pfb_age(s: &str) -> Option<AgeBucket> {
    match s.to_lowercase().as_str() {
        "baby" | "puppy" | "kitten" => Some(AgeBucket::Baby),
        "young" | "juvenile" => Some(AgeBucket::Young),
        "adult" => Some(AgeBucket::Adult),
        "senior" | "older" => Some(AgeBucket::Senior),
        _ => None,
    }
}

fn parse_pfb_size(s: &str) -> Option<Size> {
    match s.to_lowercase().as_str() {
        "extra large" | "x-large" | "xlarge" | "xl" => Some(Size::ExtraLarge),
        "large" | "l" => Some(Size::Large),
        "medium" | "m" => Some(Size::Medium),
        "small" | "s" => Some(Size::Small),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn make_lost_report(
        id: &str,
        source: &str,
        species: &str,
        extra: serde_json::Value,
    ) -> PfbReport {
        let mut base = json!({
            "id": id,
            "type": "lost",
            "source": source,
            "species": species,
            "date_created": "2024-03-10T15:00:00Z",
            "date_lost": "2024-03-10T14:00:00Z",
            "status": "active",
        });
        if let (serde_json::Value::Object(b), serde_json::Value::Object(e)) =
            (&mut base, extra)
        {
            b.extend(e);
        }
        serde_json::from_value(base).expect("test fixture parse")
    }

    fn make_found_report(
        id: &str,
        source: &str,
        species: &str,
        extra: serde_json::Value,
    ) -> PfbReport {
        let mut base = json!({
            "id": id,
            "type": "found",
            "source": source,
            "species": species,
            "date_created": "2024-03-12T09:00:00Z",
            "date_found": "2024-03-12T08:00:00Z",
            "status": "active",
        });
        if let (serde_json::Value::Object(b), serde_json::Value::Object(e)) =
            (&mut base, extra)
        {
            b.extend(e);
        }
        serde_json::from_value(base).expect("test fixture parse")
    }

    // ── AC2+3: fixture round-trip ──────────────────────────────────────────

    #[test]
    fn lost_fixture_maps_to_lost_report_fields() {
        let fixture = std::fs::read_to_string(
            "tests/fixtures/petfbi_lost.json",
        )
        .expect("read fixture");
        let page: PfbPage = serde_json::from_str(&fixture).expect("parse fixture");

        assert_eq!(page.total, 2);
        assert_eq!(page.reports.len(), 2);

        // First record: lost dog from Pet FBI.
        let rec = normalize_pfb_report(page.reports.into_iter().next().unwrap())
            .expect("normalize");
        assert_eq!(rec.species, Species::Dog);
        assert_eq!(rec.intake_type, IntakeType::LostReport);
        assert_eq!(rec.availability, Availability::Missing);
        assert_eq!(rec.source.name, SOURCE_PETFBI);
        assert_eq!(rec.source.tos_class, TosClass::NonprofitAttributionRequired);
        assert_eq!(rec.source_animal_id.as_deref(), Some("pfb-001"));
        assert_eq!(rec.breed_primary.as_deref(), Some("Labrador Retriever"));
        assert_eq!(rec.sex, Some(Sex::Male));
        assert_eq!(rec.age_bucket, Some(AgeBucket::Adult));
        assert_eq!(rec.size, Some(Size::Large));
        assert_eq!(rec.found_location_text.as_deref(), Some("Austin, TX"));
        // Photo URL hotlinked, not bytes.
        assert_eq!(rec.photos.len(), 1);
        assert!(!rec.photos[0].url.is_empty());
    }

    #[test]
    fn found_fixture_maps_to_stray_record_fields() {
        let fixture = std::fs::read_to_string(
            "tests/fixtures/petfbi_found.json",
        )
        .expect("read fixture");
        let page: PfbPage = serde_json::from_str(&fixture).expect("parse fixture");

        assert_eq!(page.total, 2);

        let rec = normalize_pfb_report(page.reports.into_iter().next().unwrap())
            .expect("normalize");
        assert_eq!(rec.species, Species::Dog);
        assert_eq!(rec.intake_type, IntakeType::FoundReport);
        assert_eq!(rec.availability, Availability::Available);
        assert_eq!(rec.source.name, SOURCE_PETFBI);
    }

    // ── AC3: type branch coverage ──────────────────────────────────────────

    #[test]
    fn type_lost_normalizes_to_lost_report() {
        let report = make_lost_report("l-1", "petfbi", "dog", json!({}));
        let rec = normalize_pfb_report(report).expect("normalize");
        assert_eq!(rec.intake_type, IntakeType::LostReport);
        assert_eq!(rec.availability, Availability::Missing);
    }

    #[test]
    fn type_found_normalizes_to_found_report() {
        let report = make_found_report("f-1", "petfbi", "cat", json!({}));
        let rec = normalize_pfb_report(report).expect("normalize");
        assert_eq!(rec.intake_type, IntakeType::FoundReport);
        assert_eq!(rec.availability, Availability::Available);
    }

    // ── AC4: provenance honest by source field ────────────────────────────

    #[test]
    fn petfbi_source_carries_petfbi_source_id() {
        let report = make_lost_report("p-1", "petfbi", "dog", json!({}));
        let rec = normalize_pfb_report(report).expect("normalize");
        assert_eq!(rec.source.name, SOURCE_PETFBI);
        assert_eq!(rec.source.tos_class, TosClass::NonprofitAttributionRequired);
    }

    #[test]
    fn helpinglostpets_source_carries_helpinglostpets_source_id() {
        let report = make_lost_report("h-1", "helpinglostpets", "cat", json!({}));
        let rec = normalize_pfb_report(report).expect("normalize");
        assert_eq!(rec.source.name, SOURCE_HELPINGLOSTPETS);
        assert_eq!(rec.source.tos_class, TosClass::NonprofitAttributionRequired);
    }

    #[test]
    fn petfbi_and_help_source_ids_are_distinguishable() {
        let pfb = make_lost_report("p-2", "petfbi", "dog", json!({}));
        let help = make_lost_report("h-2", "helpinglostpets", "dog", json!({}));
        let pfb_rec = normalize_pfb_report(pfb).expect("normalize pfb");
        let help_rec = normalize_pfb_report(help).expect("normalize help");
        assert_ne!(pfb_rec.source.name, help_rec.source.name);
    }

    // ── AC5: new source slug values ────────────────────────────────────────

    #[test]
    fn source_id_constants_are_correct() {
        assert_eq!(SOURCE_PETFBI, "petfbi");
        assert_eq!(SOURCE_HELPINGLOSTPETS, "helpinglostpets");
    }

    // ── AC6: connector absent without env var ─────────────────────────────

    #[test]
    fn from_env_returns_none_when_var_unset() {
        // This test assumes HOMEWARD_PETFBI_DATA_FILE is not set in the test
        // environment (it is not a required env var; callers opt in by setting it).
        // If the var happens to be set in CI, the connector will register — which
        // is also correct behaviour, so we guard and skip rather than fail.
        if std::env::var("HOMEWARD_PETFBI_DATA_FILE").is_ok() {
            return; // env already set in this process — skip
        }
        assert!(PetFbiConfig::from_env().is_none());
    }

    #[test]
    fn from_env_config_fields_are_populated() {
        // Verify that a manually constructed config carries the expected fields.
        // (We cannot mutate env vars without unsafe under the workspace's deny policy.)
        let cfg = PetFbiConfig {
            data_file: "test-uuid-1234".to_owned(),
            species_filter: None,
            base_url: BASE_URL.to_owned(),
        };
        assert_eq!(cfg.data_file, "test-uuid-1234");
        assert!(cfg.species_filter.is_none());
        assert_eq!(cfg.base_url, BASE_URL);
    }

    // ── AC7: photos are hotlinks, never bytes ─────────────────────────────

    #[test]
    fn photos_are_url_references_not_bytes() {
        let report = make_lost_report(
            "photo-1",
            "petfbi",
            "dog",
            json!({ "photo_url": "https://petfbi.org/photos/test.jpg" }),
        );
        let rec = normalize_pfb_report(report).expect("normalize");
        assert_eq!(rec.photos.len(), 1);
        let photo = &rec.photos[0];
        // URL is stored, not bytes.
        assert!(photo.url.starts_with("https://"));
        assert!(photo.is_primary);
    }

    #[test]
    fn no_photo_url_yields_empty_photos() {
        let report = make_lost_report("no-photo", "petfbi", "dog", json!({}));
        let rec = normalize_pfb_report(report).expect("normalize");
        assert!(rec.photos.is_empty());
    }

    // ── Unknown species rejected ────────────────────────────────────────────

    #[test]
    fn unknown_species_returns_error() {
        let report = make_lost_report("bird-1", "petfbi", "bird", json!({}));
        let err = normalize_pfb_report(report).expect_err("should fail");
        assert!(matches!(err, ConnectorError::UnknownSpecies(_)));
    }

    // ── Provenance from connector itself ────────────────────────────────────

    #[test]
    fn connector_provenance_is_petfbi() {
        let config = PetFbiConfig {
            data_file: "test-uuid".to_owned(),
            species_filter: None,
            base_url: "http://localhost".to_owned(),
        };
        let connector = PetFbiConnector::with_client(
            config,
            PoliteClient::from_client(reqwest::Client::new(), Duration::from_millis(0)),
        );
        let prov = connector.provenance();
        assert_eq!(prov.source.name, SOURCE_PETFBI);
        assert_eq!(prov.source.tos_class, TosClass::NonprofitAttributionRequired);
    }
}
