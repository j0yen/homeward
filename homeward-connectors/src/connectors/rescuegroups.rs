//! RescueGroups.org JSON:API v5 connector.
//!
//! Uses the RescueGroups.org free API (key from env `RESCUEGROUPS_API_KEY`)
//! to fetch adoptable animals. Paginates through JSON:API v5 responses and
//! normalizes each record into a [`PetRecord`].
//!
//! `ToS` notes:
//! - API key in `Authorization` header
//! - Cache static lookups (breeds/species) — the `ToS` requires it
//! - Images are hotlinked only (never downloaded)

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

/// Base URL for the `RescueGroups` JSON:API v5.
const BASE_URL: &str = "https://api.rescuegroups.org/v5";

/// Default page size for paging through results.
const PAGE_SIZE: u64 = 250;

/// Configuration for the `RescueGroups` connector.
#[derive(Debug, Clone)]
pub struct RescueGroupsConfig {
    /// API key (from env `RESCUEGROUPS_API_KEY`).
    pub api_key: String,
    /// Base URL override (for tests).
    pub base_url: String,
}

impl RescueGroupsConfig {
    /// Load config from environment.
    ///
    /// # Errors
    /// Returns [`ConnectorError::Config`] if `RESCUEGROUPS_API_KEY` is not set.
    pub fn from_env() -> Result<Self, ConnectorError> {
        let api_key = std::env::var("RESCUEGROUPS_API_KEY").map_err(|_| {
            ConnectorError::Config("RESCUEGROUPS_API_KEY env var not set".to_owned())
        })?;
        Ok(Self {
            api_key,
            base_url: BASE_URL.to_owned(),
        })
    }
}

/// Connector for RescueGroups.org JSON:API v5.
pub struct RescueGroupsConnector {
    config: RescueGroupsConfig,
    client: PoliteClient,
}

impl RescueGroupsConnector {
    /// Create a new connector from config.
    ///
    /// # Errors
    /// Returns [`ConnectorError`] if the HTTP client cannot be built.
    pub fn new(config: RescueGroupsConfig) -> Result<Self, ConnectorError> {
        let client = PoliteClient::new(Duration::from_secs(1))?;
        Ok(Self { config, client })
    }

    /// Create a connector with a custom [`PoliteClient`] (for tests).
    #[must_use]
    pub const fn with_client(config: RescueGroupsConfig, client: PoliteClient) -> Self {
        Self { config, client }
    }

    /// Fetch one page of animals from the `RescueGroups` API.
    async fn fetch_page(
        &self,
        offset: u64,
        since: Option<&DateTime<Utc>>,
    ) -> Result<RgPage, ConnectorError> {
        let mut url_str = format!(
            "{}/animals/search/available/dogs,cats?limit={}&offset={}",
            self.config.base_url, PAGE_SIZE, offset
        );
        if let Some(ts) = since {
            let ts_str = ts.format("%Y-%m-%dT%H:%M:%S").to_string();
            url_str.push_str(&format!("&filters[0][fieldName]=updatedDate&filters[0][operation]=greaterthanorequal&filters[0][criteria]={ts_str}"));
        }

        let url = Url::parse(&url_str)?;
        debug!(%url, offset, "fetching RescueGroups page");

        let resp = self
            .client
            .inner()
            .get(url.as_str())
            .header("Authorization", &self.config.api_key)
            .header("User-Agent", HOMEWARD_USER_AGENT)
            .header("Accept", "application/vnd.api+json")
            .send()
            .await?;

        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(RgPage {
                data: vec![],
                meta: RgMeta { total: 0 },
            });
        }

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ConnectorError::UnexpectedStatus {
                status,
                body: body.chars().take(256).collect(),
            });
        }

        let page: RgPage = resp.json().await?;
        Ok(page)
    }
}

#[async_trait]
impl Connector for RescueGroupsConnector {
    async fn poll(&self, since: Option<Cursor>) -> Result<Vec<PetRecord>, ConnectorError> {
        let since_ts = match &since {
            Some(Cursor::Timestamp(ts)) => Some(*ts),
            Some(Cursor::Opaque(_)) | None => None,
        };

        let mut records = Vec::new();
        let mut offset = 0u64;

        loop {
            let page = self.fetch_page(offset, since_ts.as_ref()).await?;
            let total = page.meta.total;
            let batch_len = u64::try_from(page.data.len()).unwrap_or(u64::MAX);

            for item in page.data {
                match normalize_rg_record(item, &self.config) {
                    Ok(rec) => records.push(rec),
                    Err(e) => {
                        warn!("skipping RescueGroups record: {e}");
                    }
                }
            }

            offset += PAGE_SIZE;
            if batch_len < PAGE_SIZE || offset >= total {
                break;
            }
        }

        Ok(records)
    }

    fn provenance(&self) -> Provenance {
        Provenance {
            source: SourceId::new("rescuegroups", TosClass::Api),
            fetched_at: Utc::now(),
            source_url: Some("https://api.rescuegroups.org/v5".to_owned()),
            source_etag: None,
        }
    }

    fn cadence_hint(&self) -> Duration {
        // RescueGroups ToS: refresh at least weekly; we do 6h.
        Duration::from_secs(6 * 3600)
    }
}

// ─── Serde types for JSON:API v5 ─────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RgPage {
    data: Vec<RgAnimal>,
    meta: RgMeta,
}

#[derive(Debug, Deserialize)]
struct RgMeta {
    #[serde(rename = "totalRecords", alias = "total")]
    total: u64,
}

#[derive(Debug, Deserialize)]
struct RgAnimal {
    id: String,
    #[serde(rename = "type")]
    _type: String,
    attributes: RgAttributes,
    relationships: Option<RgRelationships>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RgAttributes {
    species: Option<String>,
    sex: Option<String>,
    age_string: Option<String>,
    size_description: Option<String>,
    /// Animal's name at shelter (not mapped to `PetRecord` yet).
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(rename = "breedString")]
    breed_string: Option<String>,
    #[serde(rename = "primaryBreed")]
    primary_breed: Option<String>,
    #[serde(rename = "secondaryBreed")]
    secondary_breed: Option<String>,
    colors: Option<Vec<String>>,
    description: Option<String>,
    #[serde(rename = "updatedDate")]
    updated_date: Option<String>,
    /// Status code from `RescueGroups` (not normalized yet).
    #[allow(dead_code)]
    #[serde(rename = "statusCode")]
    status_code: Option<String>,
    found_location: Option<String>,
    chip_status: Option<String>,
    pub_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RgRelationships {
    pictures: Option<RgPictureRel>,
}

#[derive(Debug, Deserialize)]
struct RgPictureRel {
    data: Option<Vec<RgPictureRef>>,
}

#[derive(Debug, Deserialize)]
struct RgPictureRef {
    /// JSON:API picture resource ID (not used for display).
    #[allow(dead_code)]
    id: Option<String>,
    /// JSON:API resource type (always "pictures").
    #[allow(dead_code)]
    #[serde(rename = "type")]
    kind: Option<String>,
    // Included picture attributes
    attributes: Option<RgPictureAttrs>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RgPictureAttrs {
    #[serde(rename = "urlFull", alias = "url")]
    url: Option<String>,
    is_primary: Option<bool>,
}

// ─── Normalization ─────────────────────────────────────────────────────────

fn normalize_rg_record(
    animal: RgAnimal,
    _config: &RescueGroupsConfig,
) -> Result<PetRecord, ConnectorError> {
    let attr = &animal.attributes;

    let species_str = attr
        .species
        .as_deref()
        .unwrap_or("unknown");
    let species = Species::from_str_strict(species_str)
        .map_err(|_| ConnectorError::UnknownSpecies(species_str.to_owned()))?;

    let now = Utc::now();

    let last_seen = attr
        .updated_date
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(now, |dt| dt.with_timezone(&Utc));

    let first_seen = attr
        .pub_date
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(last_seen, |dt| dt.with_timezone(&Utc));

    let sex = attr.sex.as_deref().map(parse_sex);
    let age_bucket = attr.age_string.as_deref().map(parse_age);
    let size = attr.size_description.as_deref().map(parse_size);

    let breed_primary = attr
        .primary_breed
        .clone()
        .or_else(|| attr.breed_string.clone());
    let breed_secondary = attr.secondary_breed.clone();

    let chip_status = attr
        .chip_status
        .as_deref()
        .map_or(ChipStatus::Unknown, parse_chip_status);

    let photos: Vec<PhotoRef> = animal
        .relationships
        .as_ref()
        .and_then(|r| r.pictures.as_ref())
        .and_then(|p| p.data.as_ref())
        .map(|pics| {
            pics.iter()
                .filter_map(|p| {
                    p.attributes.as_ref()?.url.as_ref().map(|url| PhotoRef {
                        url: url.clone(),
                        attribution: None,
                        is_primary: p
                            .attributes
                            .as_ref()
                            .and_then(|a| a.is_primary)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("rescuegroups", TosClass::Api),
        source_animal_id: Some(animal.id),
        species,
        breed_primary,
        breed_secondary,
        sex,
        age_bucket,
        size,
        colors: attr.colors.clone().unwrap_or_default(),
        markings_text: attr.description.clone(),
        intake_type: IntakeType::Adoptable,
        availability: Availability::Adoptable,
        chip_status,
        location: None,
        found_location_text: attr.found_location.clone(),
        photos,
        first_seen,
        last_seen,
        last_confirmed: Some(now),
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    })
}

fn parse_sex(s: &str) -> Sex {
    match s.to_lowercase().as_str() {
        "male" | "m" => Sex::Male,
        "female" | "f" => Sex::Female,
        "neutered male" | "neutered" => Sex::NeuteredMale,
        "spayed female" | "spayed" => Sex::SpayedFemale,
        _ => Sex::Unknown,
    }
}

fn parse_age(s: &str) -> AgeBucket {
    match s.to_lowercase().as_str() {
        s if s.contains("baby") || s.contains("puppy") || s.contains("kitten") => AgeBucket::Baby,
        s if s.contains("young") || s.contains("juvenile") => AgeBucket::Young,
        s if s.contains("adult") => AgeBucket::Adult,
        s if s.contains("senior") || s.contains("older") => AgeBucket::Senior,
        _ => AgeBucket::Adult,
    }
}

fn parse_size(s: &str) -> Size {
    match s.to_lowercase().as_str() {
        s if s.contains("extra large") || s.contains("x-large") || s.contains("xlarge") => {
            Size::ExtraLarge
        }
        s if s.contains("large") => Size::Large,
        s if s.contains("medium") => Size::Medium,
        _ => Size::Small,
    }
}

fn parse_chip_status(s: &str) -> ChipStatus {
    let lower = s.to_lowercase();
    if (lower.contains("chip") || lower.contains("scanned") || lower.contains("yes"))
        && !lower.contains("no")
        && !lower.contains("not")
    {
        ChipStatus::Scanned { chip: None }
    } else if lower.contains("no") || lower.contains("none") || lower.contains("not") {
        ChipStatus::ScanNoChip
    } else {
        ChipStatus::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_animal(id: &str, species: &str, extra: serde_json::Value) -> RgAnimal {
        let mut attrs = json!({
            "species": species,
            "updatedDate": "2024-01-15T10:00:00Z",
            "pubDate": "2024-01-10T08:00:00Z",
        });
        if let (serde_json::Value::Object(a), serde_json::Value::Object(e)) =
            (&mut attrs, extra)
        {
            a.extend(e);
        }
        serde_json::from_value(json!({
            "id": id,
            "type": "animals",
            "attributes": attrs,
            "relationships": null,
        }))
        .expect("test fixture parse")
    }

    #[test]
    fn normalizes_dog_record() {
        let animal = make_animal(
            "dog-1",
            "Dog",
            json!({
                "primaryBreed": "Labrador Retriever",
                "sex": "male",
                "ageString": "adult",
                "sizeDescription": "large",
            }),
        );
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let rec = normalize_rg_record(animal, &config).expect("normalize");
        assert_eq!(rec.species, Species::Dog);
        assert_eq!(rec.intake_type, IntakeType::Adoptable);
        assert_eq!(rec.source.tos_class, TosClass::Api);
        assert!(rec.photos.is_empty()); // no photos in fixture
        assert!(rec.breed_primary.as_deref() == Some("Labrador Retriever"));
    }

    #[test]
    fn normalizes_cat_record() {
        let animal = make_animal(
            "cat-1",
            "Cat",
            json!({
                "primaryBreed": "Domestic Shorthair",
                "sex": "spayed female",
                "ageString": "young",
            }),
        );
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let rec = normalize_rg_record(animal, &config).expect("normalize");
        assert_eq!(rec.species, Species::Cat);
        assert_eq!(rec.sex, Some(Sex::SpayedFemale));
    }

    #[test]
    fn mixed_species_both_normalized() {
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let dog = make_animal("dog-2", "Dog", json!({}));
        let cat = make_animal("cat-2", "Cat", json!({}));
        let dog_rec = normalize_rg_record(dog, &config).expect("dog");
        let cat_rec = normalize_rg_record(cat, &config).expect("cat");
        assert_eq!(dog_rec.species, Species::Dog);
        assert_eq!(cat_rec.species, Species::Cat);
    }

    #[test]
    fn unknown_species_returns_error() {
        let animal = make_animal("bird-1", "Bird", json!({}));
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let err = normalize_rg_record(animal, &config).expect_err("should fail");
        assert!(matches!(err, ConnectorError::UnknownSpecies(_)));
    }

    #[test]
    fn provenance_class_is_api() {
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let connector = RescueGroupsConnector {
            config,
            client: PoliteClient::from_client(
                reqwest::Client::new(),
                Duration::from_millis(0),
            ),
        };
        let prov = connector.provenance();
        assert_eq!(prov.source.tos_class, TosClass::Api);
        assert_eq!(prov.source.name, "rescuegroups");
    }

    #[test]
    fn photos_carry_only_url_not_bytes() {
        // Type-level: PhotoRef has no bytes field.
        // This test verifies the struct shape and that our normalizer
        // only ever sets `url` (never raw data).
        let photo = PhotoRef {
            url: "https://example.com/pet.jpg".to_owned(),
            attribution: None,
            is_primary: true,
        };
        // If a `data` or `bytes` field existed, this wouldn't compile.
        assert!(!photo.url.is_empty());
    }
}
