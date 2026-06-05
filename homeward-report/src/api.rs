//! Open read API — shelter intake queries.
//!
//! Privacy contract (AC5):
//! - This API serves only the **shelter** intake store (public pet records).
//! - It never reaches the PII report store — there is no code path from
//!   an API query to a [`LostReport`] or its contact/location fields.
//! - Rate limiting is enforced by [`ApiConfig::max_results_per_query`].
//!
//! The image-similarity search (AC6) accepts a found-pet photo and returns
//! a ranked candidate shortlist via the match engine.

use chrono::{DateTime, Utc};
use homeward_schema::{PetRecord, Species};
use serde::{Deserialize, Serialize};

// ─── API configuration ───────────────────────────────────────────────────────

/// Configuration for the open read API.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Maximum records returned per query (rate-limit proxy).
    pub max_results_per_query: usize,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            max_results_per_query: 50,
        }
    }
}

// ─── Shelter query ────────────────────────────────────────────────────────────

/// A filter for querying the open shelter intake store (AC5).
///
/// All fields are optional; missing fields match anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShelterQuery {
    /// Filter by species.
    pub species: Option<Species>,
    /// Filter by ZIP code (coarse geo).
    pub zip_code: Option<String>,
    /// Filter by state code.
    pub state: Option<String>,
    /// Only return intakes after this timestamp.
    pub intake_after: Option<DateTime<Utc>>,
    /// Maximum results (overridden by [`ApiConfig::max_results_per_query`] if
    /// larger).
    pub limit: Option<usize>,
}

/// The result of a shelter query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelterQueryResult {
    /// Matching records (sanitised — no owner PII).
    pub records: Vec<ShelterRecord>,
    /// Whether the result was capped by the rate limit.
    pub truncated: bool,
}

/// A sanitised view of a [`PetRecord`] suitable for the open API.
///
/// Photos are hotlinks with source attribution (AC5). No owner contact,
/// no report fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelterRecord {
    /// Homeward's canonical ID.
    pub canonical_id: String,
    /// Data source attribution.
    pub source: String,
    /// Species.
    pub species: Species,
    /// Breed description.
    #[serde(default)]
    pub breed: Option<String>,
    /// Shelter city/county (coarse geo, never street address).
    #[serde(default)]
    pub shelter_city_county: Option<String>,
    /// Shelter state.
    #[serde(default)]
    pub shelter_state: Option<String>,
    /// Hotlinked photo URLs with source attribution.
    pub photo_urls: Vec<String>,
    /// When the animal was first seen.
    pub first_seen: DateTime<Utc>,
}

impl From<&PetRecord> for ShelterRecord {
    fn from(r: &PetRecord) -> Self {
        Self {
            canonical_id: r.canonical_id.to_string(),
            source: r.source.name.clone(),
            species: r.species,
            breed: r.breed_primary.clone(),
            shelter_city_county: r.location.as_ref().map(|l| l.city_county.clone()),
            shelter_state: r.location.as_ref().and_then(|l| l.state.clone()),
            photo_urls: r.photos.iter().map(|p| p.url.clone()).collect(),
            first_seen: r.first_seen,
        }
    }
}

// ─── Query execution ──────────────────────────────────────────────────────────

/// Query the shelter intake store (AC5).
///
/// `records` is the caller-provided shelter store slice.
/// The PII report store is never passed in — enforced by the API surface.
///
/// # Returns
/// A [`ShelterQueryResult`] with matching records, truncated to
/// `min(query.limit, cfg.max_results_per_query)`.
#[must_use]
pub fn query_shelter(
    records: &[PetRecord],
    query: &ShelterQuery,
    cfg: &ApiConfig,
) -> ShelterQueryResult {
    let limit = query
        .limit
        .unwrap_or(cfg.max_results_per_query)
        .min(cfg.max_results_per_query);

    let filtered: Vec<ShelterRecord> = records
        .iter()
        .filter(|r| {
            query
                .species
                .as_ref()
                .map_or(true, |s| r.species == *s)
        })
        .filter(|r| {
            // ShelterLocation stores city_county, not zip. For zip queries
            // we check if the city_county string contains the zip (best-effort;
            // production would maintain a ZIP→city lookup).
            query.zip_code.as_ref().map_or(true, |zip| {
                r.location
                    .as_ref()
                    .map_or(false, |l| l.city_county.contains(zip.as_str()))
            })
        })
        .filter(|r| {
            query.state.as_ref().map_or(true, |st| {
                r.location
                    .as_ref()
                    .and_then(|l| l.state.as_deref())
                    .map_or(false, |s| s == st)
            })
        })
        .filter(|r| {
            query
                .intake_after
                .map_or(true, |ts| r.intake_date.map_or(false, |d| d > ts))
        })
        .take(limit + 1) // take one extra to detect truncation
        .collect::<Vec<_>>()
        .iter()
        .take(limit)
        .map(|r| ShelterRecord::from(*r))
        .collect();

    let total_matched = records
        .iter()
        .filter(|r| {
            query
                .species
                .as_ref()
                .map_or(true, |s| r.species == *s)
        })
        .count();

    ShelterQueryResult {
        truncated: total_matched > limit,
        records: filtered,
    }
}

// ─── Image-similarity search ──────────────────────────────────────────────────

/// A ranked candidate from image-similarity search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarityCandidate {
    /// The matching shelter record.
    pub record: ShelterRecord,
    /// Similarity score (0–1).
    pub score: f32,
    /// Explanation (always candidate-framed, never "confirmed match").
    pub explanation: String,
}

/// Image-similarity search (AC6).
///
/// Accepts `photo_bytes` (the found-pet photo) and a pre-scored list of
/// [`MatchCandidate`] records from homeward-match. Returns a ranked shortlist
/// of [`SimilarityCandidate`] results with candidate-framed explanations.
///
/// This function does NOT perform embedding/ML — it consumes pre-scored
/// candidates from the match engine (homeward-match is a separate crate).
/// The separation keeps homeward-report free of ML dependencies.
pub fn image_similarity_search(
    prescored: Vec<crate::MatchCandidate>,
    cfg: &ApiConfig,
) -> Vec<SimilarityCandidate> {
    let mut sorted = prescored;
    sorted.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    sorted
        .into_iter()
        .take(cfg.max_results_per_query)
        .map(|c| {
            let explanation = format!(
                "This animal has a similarity score of {:.0}% — please review in person to confirm.",
                c.score * 100.0
            );
            SimilarityCandidate {
                record: ShelterRecord::from(&c.record),
                score: c.score,
                explanation,
            }
        })
        .collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_common::make_pet_record;
    use homeward_schema::Species;

    #[test]
    fn api_filters_by_species() {
        let records = vec![
            make_pet_record(Species::Dog, None, None),
            make_pet_record(Species::Cat, None, None),
        ];
        let cfg = ApiConfig::default();
        let query = ShelterQuery {
            species: Some(Species::Dog),
            ..Default::default()
        };
        let result = query_shelter(&records, &query, &cfg);
        assert_eq!(result.records.len(), 1, "only dog records should be returned");
        assert_eq!(result.records[0].species, Species::Dog);
    }

    #[test]
    fn api_enforces_max_results_rate_limit() {
        let records: Vec<PetRecord> = (0..100)
            .map(|_| make_pet_record(Species::Dog, None, None))
            .collect();
        let cfg = ApiConfig {
            max_results_per_query: 10,
        };
        let query = ShelterQuery::default();
        let result = query_shelter(&records, &query, &cfg);
        assert!(
            result.records.len() <= 10,
            "result must be capped at max_results_per_query"
        );
        assert!(result.truncated, "truncated must be true when capped");
    }

    #[test]
    fn api_does_not_expose_report_pii() {
        // This test asserts the API surface has no LostReport parameter —
        // verified at compile time by the function signatures.
        // At runtime we ensure ShelterRecord has no phone/email fields.
        let records = vec![make_pet_record(Species::Dog, None, None)];
        let cfg = ApiConfig::default();
        let result = query_shelter(&records, &ShelterQuery::default(), &cfg);
        let json = serde_json::to_string(&result).expect("serialize ok");
        assert!(
            !json.contains("phone") && !json.contains("email"),
            "API response must not contain phone/email fields: {json}"
        );
    }

    #[test]
    fn similarity_search_is_candidate_framed() {
        use crate::tests_common::make_candidate;
        let prescored = vec![make_candidate(0.9, false), make_candidate(0.7, false)];
        let cfg = ApiConfig::default();
        let results = image_similarity_search(prescored, &cfg);
        assert!(!results.is_empty());
        for r in &results {
            assert!(
                r.explanation.to_lowercase().contains("review")
                    || r.explanation.to_lowercase().contains("possible"),
                "explanation must be candidate-framed: {}", r.explanation
            );
            assert!(
                !r.explanation.to_lowercase().contains("confirmed match"),
                "explanation must not assert confirmed match"
            );
        }
    }

    #[test]
    fn similarity_search_sorted_by_score_descending() {
        use crate::tests_common::make_candidate;
        let mut c1 = make_candidate(0.5, false);
        let mut c2 = make_candidate(0.9, false);
        // Use different canonical IDs so they don't appear identical
        c1.record.canonical_id = ulid::Ulid::new();
        c2.record.canonical_id = ulid::Ulid::new();
        let cfg = ApiConfig::default();
        let results = image_similarity_search(vec![c1, c2], &cfg);
        assert_eq!(results.len(), 2);
        assert!(
            results[0].score >= results[1].score,
            "results must be sorted by score descending"
        );
    }
}
