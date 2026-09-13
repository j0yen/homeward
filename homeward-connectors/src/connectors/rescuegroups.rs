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

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use homeward_schema::{
    AgeBucket, Availability, ChipStatus, IntakeType, PetRecord, PhotoRef, Provenance, Sex, Size,
    Species, TosClass,
    geo::ShelterLocation,
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

/// Maximum number of retry attempts on 429/5xx for a single page fetch.
///
/// `PoliteClient::get` already retries GETs; the search endpoint is a POST
/// (filters travel in the body) so it bypasses that path entirely and, until
/// this constant's retry loop below, had no backoff of its own — a 429 on a
/// `fetch_page` call surfaced immediately as a hard error instead of backing
/// off and resuming (homeward-ingest-backfill AC3).
const PAGE_MAX_RETRIES: u32 = 5;

/// Base delay for `fetch_page`'s exponential backoff (doubled per attempt,
/// overridden by a `Retry-After` header when the server sends one).
const PAGE_BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Species queried one at a time — the `search/available` endpoint only
/// accepts a single species segment per request (a comma-joined list 404s).
const SPECIES: [&str; 2] = ["dogs", "cats"];

/// JSON:API filter field name for the delta-poll "since" watermark.
///
/// Verified live: a bare `updatedDate` fieldName 400s — `{"errors":[{"source":
/// {"pointer":"/data/filters/0/fieldName/updatedDate"},"title":"Invalid field",
/// "detail":"updatedDate is not a valid filter field"}]}`. Filterable animal
/// fields are namespaced under the resource type; `animals.updatedDate`
/// returns 200. This is currently the only filter fieldName the connector
/// sends — if a second filter is ever added, it needs the same `animals.`
/// prefix.
const UPDATED_DATE_FILTER_FIELD: &str = "animals.updatedDate";

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

/// Build the search URL for one page of one `species` (`"dogs"` or `"cats"`).
///
/// The endpoint lives under `/public` and takes exactly one species segment —
/// a comma-joined list like `dogs,cats` 404s on the real API.
///
/// Paginates via `page` (1-based), not `offset`: verified live that the real
/// API silently ignores `offset` entirely (identical `pageReturned:1` content
/// no matter what offset is sent), while `page` advances correctly (distinct
/// records, `meta.pageReturned` matching the requested page). The JSON:API-
/// standard `page[number]`/`page[size]` form was also tried and returns no
/// data at all — `page` (bare) is the only form that works against this API.
/// `include` query value requesting every related resource the connector
/// maps: `colors`/`pictures` (already normalized) plus `locations`/`orgs`
/// (PRD-homeward-ingest-location-backfill-missing — without this, RG's v5
/// API omits both from `included[]` and the connector has nothing to
/// resolve a [`ShelterLocation`] from, regardless of what the mapping code
/// does). Explicit and exhaustive rather than relying on RG's undocumented
/// "default includes": the real API returns colors/pictures with no
/// `include` param at all today, but adding `include=locations,orgs` alone
/// would silently narrow the response to just those two and drop the
/// colors/pictures this connector already depends on.
const INCLUDE_PARAM: &str = "colors,pictures,locations,orgs";

fn build_search_url(base_url: &str, species: &str, page: u64) -> String {
    format!(
        "{base_url}/public/animals/search/available/{species}?limit={PAGE_SIZE}&page={page}&include={INCLUDE_PARAM}"
    )
}

/// Build the JSON:API `filters` array for a delta poll's "since" watermark
/// (an empty array for a full poll, i.e. `since.is_none()`).
///
/// `criteria` must be RFC3339 — verified live: a space-separated, non-RFC3339
/// timestamp (e.g. `"2026-08-13 02:44:31"`) 500s (`{"errors":[{"status":500,
/// "title":"System error","detail":"We encountered a system error and
/// couldn't continue."}]}`), while `"2026-08-13T02:44:31Z"` and
/// `"2026-08-13T02:44:31.658043006Z"` both return 200. `to_rfc3339_opts`
/// with `use_z = true` matches the verified-good whole-seconds form exactly.
fn build_filters(since: Option<&DateTime<Utc>>) -> Vec<serde_json::Value> {
    since
        .map(|ts| {
            vec![serde_json::json!({
                "fieldName": UPDATED_DATE_FILTER_FIELD,
                "operation": "greaterthanorequal",
                "criteria": ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })]
        })
        .unwrap_or_default()
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

    /// Fetch one page of animals of a single `species` (`"dogs"` or `"cats"`)
    /// from the `RescueGroups` API.
    ///
    /// The `search/available/{species}` endpoint lives under `/public` and
    /// only accepts one species segment per request — a comma-joined list
    /// (e.g. `dogs,cats`) 404s. It is a JSON:API "search" action, so filters
    /// travel in a POST body rather than the query string; `limit`/`offset`
    /// stay as query params.
    async fn fetch_page(
        &self,
        species: &str,
        page: u64,
        since: Option<&DateTime<Utc>>,
    ) -> Result<RgPage, ConnectorError> {
        let url_str = build_search_url(&self.config.base_url, species, page);
        let url = Url::parse(&url_str)?;

        let filters = build_filters(since);
        let body = serde_json::json!({ "data": { "filters": filters } });
        let body_bytes = serde_json::to_vec(&body)?;

        debug!(%url, page, species, "fetching RescueGroups page");

        let mut attempt = 0u32;
        loop {
            let resp = self
                .client
                .inner()
                .post(url.as_str())
                .header("Authorization", &self.config.api_key)
                .header("User-Agent", HOMEWARD_USER_AGENT)
                .header("Accept", "application/vnd.api+json")
                .header("Content-Type", "application/vnd.api+json")
                .body(body_bytes.clone())
                .send()
                .await?;

            let status = resp.status();

            if status == StatusCode::NOT_MODIFIED {
                return Ok(RgPage {
                    data: vec![],
                    meta: RgMeta {
                        total: 0,
                        count_returned: 0,
                        page_returned: 0,
                        pages: 0,
                    },
                    included: vec![],
                });
            }

            // 429/503: back off (Retry-After if present, else exponential)
            // and retry the SAME page — never advance on a rate-limited
            // response. This is what turns a rate-limited backfill run into
            // a log of backoff events rather than a hard failure.
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
            {
                if attempt >= PAGE_MAX_RETRIES {
                    return Err(ConnectorError::RateLimitExhausted {
                        host: url.host_str().unwrap_or("").to_owned(),
                    });
                }
                let retry_after = resp
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs);
                let backoff = retry_after
                    .unwrap_or_else(|| PAGE_BACKOFF_BASE * 2u32.saturating_pow(attempt));
                warn!(
                    attempt,
                    page,
                    species,
                    status = status.as_u16(),
                    ?backoff,
                    "RescueGroups rate-limited — backing off"
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
                continue;
            }

            if !status.is_success() {
                let status_code = status.as_u16();
                let body = resp.text().await.unwrap_or_default();
                return Err(ConnectorError::UnexpectedStatus {
                    status: status_code,
                    body: body.chars().take(256).collect(),
                });
            }

            let page: RgPage = resp.json().await?;
            return Ok(page);
        }
    }

    /// Fetch and normalize one page of a single `species`, for callers that
    /// drive their own page-by-page loop (e.g. a resumable backfill) instead
    /// of consuming the whole-population [`Connector::poll`].
    ///
    /// Always a full (non-delta) fetch — `since` is not exposed here because
    /// a historical backfill has no watermark to filter on. Reuses the exact
    /// same `fetch_page` path (and its page-based pagination + 429 backoff)
    /// that [`Connector::poll`] uses, per the "never offset" requirement.
    ///
    /// # Errors
    /// Propagates [`ConnectorError`] on transport, rate-limit-exhausted, or
    /// deserialization failure.
    pub async fn fetch_normalized_page(
        &self,
        species: &str,
        page: u64,
    ) -> Result<RgPageResult, ConnectorError> {
        let rg_page = self.fetch_page(species, page, None).await?;
        let species_enum = species_from_query(species);
        let included = build_included_index(&rg_page.included);

        let mut records = Vec::with_capacity(rg_page.data.len());
        for item in rg_page.data {
            match normalize_rg_record(item, species_enum, &included, &self.config) {
                Ok(rec) => records.push(rec),
                Err(e) => warn!("skipping RescueGroups record: {e}"),
            }
        }

        Ok(RgPageResult {
            records,
            page_returned: rg_page.meta.page_returned,
            pages_total: rg_page.meta.pages,
            count_returned: rg_page.meta.count_returned,
            total: rg_page.meta.total,
        })
    }
}

/// Result of one normalized page fetch via [`RescueGroupsConnector::fetch_normalized_page`].
#[derive(Debug, Clone)]
pub struct RgPageResult {
    /// Normalized records for this page (records that failed normalization
    /// are skipped with a warning, not included here).
    pub records: Vec<PetRecord>,
    /// 1-based page number the API actually served (source of truth for
    /// loop termination — see [`build_search_url`]'s doc comment).
    pub page_returned: u64,
    /// Total number of pages available for this query.
    pub pages_total: u64,
    /// Records returned on this page (defensive fallback for termination).
    pub count_returned: u64,
    /// Total matching records across all pages, for coverage estimates.
    pub total: u64,
}

impl RgPageResult {
    /// `true` when this was the last page for its species (mirrors the
    /// termination condition `Connector::poll` uses internally).
    #[must_use]
    pub const fn is_last_page(&self) -> bool {
        self.page_returned >= self.pages_total || self.count_returned < PAGE_SIZE
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
        let mut seen_ids = HashSet::new();

        for species in SPECIES {
            let species_enum = species_from_query(species);
            let mut page_num = 1u64;

            loop {
                let page = self.fetch_page(species, page_num, since_ts.as_ref()).await?;
                let pages_total = page.meta.pages;
                let page_returned = page.meta.page_returned;
                let count_returned = page.meta.count_returned;
                let included = build_included_index(&page.included);

                for item in page.data {
                    if !seen_ids.insert(item.id.clone()) {
                        // Dedup: guard against the same animal ID showing up
                        // in more than one species page.
                        continue;
                    }
                    match normalize_rg_record(item, species_enum, &included, &self.config) {
                        Ok(rec) => records.push(rec),
                        Err(e) => {
                            warn!("skipping RescueGroups record: {e}");
                        }
                    }
                }

                // `offset` is silently ignored by the real API (verified live:
                // identical `pageReturned:1` content no matter what offset was
                // sent) — `page` is what actually advances. Terminate on
                // `pageReturned >= pages` (also covers the zero-match case,
                // where `pages` is 0) or a short page (`countReturned < limit`),
                // mirroring the old offset-based dual condition.
                if page_returned >= pages_total || count_returned < PAGE_SIZE {
                    break;
                }
                page_num += 1;
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
//
// The real `/public/animals/search/available/{species}` payload is a JSON:API
// envelope: `data[]` holds sparse animal resources (attributes + relationship
// *references*), and `included[]` holds the full resources those references
// point to (pictures, colors, breeds, species, statuses, locations, orgs —
// matched by `(type, id)`). Species is deliberately NOT read from the
// payload: the real API exposes it only as a `relationships.species`
// reference, but the connector already knows which species it asked for
// (one request per species — see `SPECIES`), so that's passed in directly
// instead of round-tripping through `included`.

#[derive(Debug, Deserialize)]
struct RgPage {
    #[serde(default)]
    data: Vec<RgAnimal>,
    meta: RgMeta,
    #[serde(default)]
    included: Vec<RgIncludedItem>,
}

#[derive(Debug, Deserialize)]
struct RgMeta {
    /// Total matching records across all pages (real key is `count`, not
    /// `totalRecords` — the old name was never valid against the live API).
    /// Not read by the pagination loop (`pages`/`pageReturned` decide
    /// termination); exposed via [`RgPageResult::total`] for backfill
    /// coverage estimates (dry-run page/coverage reporting).
    #[serde(rename = "count")]
    total: u64,
    /// Records returned on this page. Used only as a defensive fallback for
    /// loop termination (see `poll`); normally `pages`/`pageReturned` decide.
    #[serde(default, rename = "countReturned")]
    count_returned: u64,
    /// 1-based page number this response actually served. Verified live: the
    /// API echoes back which page it served regardless of what page was
    /// requested, so this — not the requested page number — is the source of
    /// truth for "have we reached the last page".
    #[serde(default, rename = "pageReturned")]
    page_returned: u64,
    /// Total number of pages available for this query (0 on a zero-match
    /// page). Paired with `pageReturned` to decide when to stop paging.
    #[serde(default)]
    pages: u64,
}

#[derive(Debug, Deserialize)]
struct RgAnimal {
    id: String,
    #[serde(rename = "type")]
    _type: String,
    attributes: RgAttributes,
    relationships: Option<RgRelationships>,
}

/// Animal attributes. Every field is `Option` — the real API omits most
/// attributes on a per-record basis (e.g. `breedSecondary`, `sizeGroup`)
/// depending on what the source shelter filled in. Unknown/unlisted
/// attributes (there are ~44-54 per record) are silently ignored by serde.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RgAttributes {
    sex: Option<String>,
    age_string: Option<String>,
    #[serde(rename = "sizeGroup")]
    size_group: Option<String>,
    /// Animal's name at shelter (not mapped to `PetRecord` yet).
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(rename = "breedString")]
    breed_string: Option<String>,
    #[serde(rename = "breedPrimary")]
    primary_breed: Option<String>,
    #[serde(rename = "breedSecondary")]
    secondary_breed: Option<String>,
    #[serde(rename = "descriptionText")]
    description: Option<String>,
    #[serde(rename = "updatedDate")]
    updated_date: Option<String>,
    /// Record creation timestamp — the closest real-API equivalent of the
    /// old (never-actually-present) `pubDate` field; used for `first_seen`.
    #[serde(rename = "createdDate")]
    created_date: Option<String>,
    /// Not present on adoptable-animal records in practice, kept optional
    /// in case a future source populates it.
    found_location: Option<String>,
    /// Not present in the live v5 payload today; kept optional so the
    /// connector degrades gracefully rather than erroring if it appears.
    chip_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RgRelationships {
    colors: Option<RgRelationshipData>,
    pictures: Option<RgRelationshipData>,
    /// The animal's shelter location(s) — real payloads carry `city`,
    /// `state`, `lat`, `lon` on the included `locations` resource (verified
    /// against the RG v5 API docs; PRD-homeward-ingest-location-backfill-missing).
    locations: Option<RgRelationshipData>,
    /// The animal's org (shelter/rescue). Some orgs expose their own
    /// `city`/`state`/`lat`/`lon` directly (no separate `locations`
    /// resource); used as a fallback when `locations` is absent.
    orgs: Option<RgRelationshipData>,
}

/// A JSON:API relationship: just resource references (`type` + `id`) — the
/// full resource lives in the top-level `included[]` array.
#[derive(Debug, Deserialize)]
struct RgRelationshipData {
    #[serde(default)]
    data: Vec<RgResourceRef>,
}

#[derive(Debug, Deserialize)]
struct RgResourceRef {
    #[serde(rename = "type")]
    kind: String,
    id: String,
}

/// One entry of the JSON:API `included[]` array. `attributes` is kept as
/// raw JSON since its shape varies by `type` (pictures/colors/breeds/etc.)
/// and we only need to pick a couple of fields out of a couple of types.
#[derive(Debug, Deserialize)]
struct RgIncludedItem {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    #[serde(default)]
    attributes: serde_json::Value,
}

/// Lookup from `(type, id)` to attributes, built once per page so
/// relationship references can be resolved against `included`.
type IncludedIndex = HashMap<(String, String), serde_json::Value>;

fn build_included_index(included: &[RgIncludedItem]) -> IncludedIndex {
    included
        .iter()
        .map(|item| ((item.kind.clone(), item.id.clone()), item.attributes.clone()))
        .collect()
}

/// Map the species query segment (`"dogs"`/`"cats"`) to the schema enum.
/// `SPECIES` only ever yields these two values, so the fallback is
/// unreachable in practice, not a silent misclassification risk.
fn species_from_query(species: &str) -> Species {
    match species {
        "cats" => Species::Cat,
        _ => Species::Dog,
    }
}

/// Resolve a `pictures` relationship into `PhotoRef`s via `included`.
/// Prefers the `large` variant (a reasonably sized hotlink), falling back to
/// `original`/`small`. `order == 1` is treated as the primary photo — the
/// real payload has no `isPrimary` flag, just a 1-indexed `order`.
fn resolve_photos(refs: &[RgResourceRef], included: &IncludedIndex) -> Vec<PhotoRef> {
    refs.iter()
        .filter_map(|r| {
            let attrs = included.get(&(r.kind.clone(), r.id.clone()))?;
            let url = attrs
                .get("large")
                .or_else(|| attrs.get("original"))
                .or_else(|| attrs.get("small"))
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())?;
            let is_primary = attrs.get("order").and_then(serde_json::Value::as_u64) == Some(1);
            Some(PhotoRef {
                url: url.to_owned(),
                attribution: None,
                is_primary,
            })
        })
        .collect()
}

/// Resolve a `colors` relationship into color names via `included`.
fn resolve_colors(refs: &[RgResourceRef], included: &IncludedIndex) -> Vec<String> {
    refs.iter()
        .filter_map(|r| {
            included
                .get(&(r.kind.clone(), r.id.clone()))
                .and_then(|attrs| attrs.get("name"))
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        })
        .collect()
}

/// Decimal places lat/lon are rounded to for RescueGroups-sourced
/// locations — matches the ±1.1 km convention already used elsewhere in
/// this codebase (`ShelterLocation::new`'s own doc comment, and every
/// other call site in this workspace).
const LOCATION_PRECISION: u8 = 2;

/// Resolve an animal's coarse [`ShelterLocation`] from its `locations`
/// relationship, falling back to its `orgs` relationship when no
/// `locations` resource is present (some orgs expose city/state/lat/lon
/// directly on the org resource with no separate location record —
/// PRD-homeward-ingest-location-backfill-missing AC1).
///
/// Returns `None` when neither relationship resolves to a resource with a
/// usable `city` or `state` — a real, honest gap (some source records
/// genuinely lack shelter location data), not a mapping defect.
fn resolve_location(
    location_refs: &[RgResourceRef],
    org_refs: &[RgResourceRef],
    included: &IncludedIndex,
) -> Option<ShelterLocation> {
    let attrs = location_refs
        .iter()
        .chain(org_refs.iter())
        .find_map(|r| included.get(&(r.kind.clone(), r.id.clone())))?;

    let city = attrs
        .get("city")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let state = attrs
        .get("state")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    let lat = attrs.get("lat").and_then(serde_json::Value::as_f64);
    let lon = attrs.get("lon").and_then(serde_json::Value::as_f64);

    // `city_county` is required on `ShelterLocation`; fall back to state
    // when only that is present, and give up (return `None`) when the
    // resolved resource carries neither — coarse geo with nothing
    // human-readable to show isn't worth fabricating a placeholder for.
    let city_county = city.map(ToOwned::to_owned).or_else(|| state.clone())?;

    Some(ShelterLocation::new(lat, lon, LOCATION_PRECISION, city_county, state))
}

// ─── Normalization ─────────────────────────────────────────────────────────

fn normalize_rg_record(
    animal: RgAnimal,
    species: Species,
    included: &IncludedIndex,
    _config: &RescueGroupsConfig,
) -> Result<PetRecord, ConnectorError> {
    let attr = &animal.attributes;

    let now = Utc::now();

    let last_seen = attr
        .updated_date
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(now, |dt| dt.with_timezone(&Utc));

    let first_seen = attr
        .created_date
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(last_seen, |dt| dt.with_timezone(&Utc));

    let sex = attr.sex.as_deref().map(parse_sex);
    let age_bucket = attr.age_string.as_deref().map(parse_age);
    let size = attr.size_group.as_deref().map(parse_size);

    let breed_primary = attr
        .primary_breed
        .clone()
        .or_else(|| attr.breed_string.clone());
    let breed_secondary = attr.secondary_breed.clone();

    let chip_status = attr
        .chip_status
        .as_deref()
        .map_or(ChipStatus::Unknown, parse_chip_status);

    let picture_refs: &[RgResourceRef] = animal
        .relationships
        .as_ref()
        .and_then(|r| r.pictures.as_ref())
        .map(|p| p.data.as_slice())
        .unwrap_or_default();
    let photos = resolve_photos(picture_refs, included);

    let color_refs: &[RgResourceRef] = animal
        .relationships
        .as_ref()
        .and_then(|r| r.colors.as_ref())
        .map(|c| c.data.as_slice())
        .unwrap_or_default();
    let colors = resolve_colors(color_refs, included);

    let location_refs: &[RgResourceRef] = animal
        .relationships
        .as_ref()
        .and_then(|r| r.locations.as_ref())
        .map(|l| l.data.as_slice())
        .unwrap_or_default();
    let org_refs: &[RgResourceRef] = animal
        .relationships
        .as_ref()
        .and_then(|r| r.orgs.as_ref())
        .map(|o| o.data.as_slice())
        .unwrap_or_default();
    let location = resolve_location(location_refs, org_refs, included);

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
        colors,
        markings_text: attr.description.clone(),
        intake_type: IntakeType::Adoptable,
        availability: Availability::Adoptable,
        chip_status,
        location,
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

    /// Build a minimal but structurally real `RgAnimal`: no `species`
    /// attribute (the real payload never has one — it's supplied by the
    /// caller, matching the per-species request), `createdDate` instead of
    /// the old fictitious `pubDate`.
    fn make_animal(id: &str, extra: serde_json::Value) -> RgAnimal {
        let mut attrs = json!({
            "updatedDate": "2024-01-15T10:00:00Z",
            "createdDate": "2024-01-10T08:00:00Z",
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
            json!({
                "breedPrimary": "Labrador Retriever",
                "sex": "male",
                "ageString": "adult",
                "sizeGroup": "large",
            }),
        );
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let rec = normalize_rg_record(animal, Species::Dog, &IncludedIndex::new(), &config)
            .expect("normalize");
        assert_eq!(rec.species, Species::Dog);
        assert_eq!(rec.intake_type, IntakeType::Adoptable);
        assert_eq!(rec.source.tos_class, TosClass::Api);
        assert!(rec.photos.is_empty()); // no pictures relationship in fixture
        assert!(rec.breed_primary.as_deref() == Some("Labrador Retriever"));
    }

    #[test]
    fn normalizes_cat_record() {
        let animal = make_animal(
            "cat-1",
            json!({
                "breedPrimary": "Domestic Shorthair",
                "sex": "spayed female",
                "ageString": "young",
            }),
        );
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let rec = normalize_rg_record(animal, Species::Cat, &IncludedIndex::new(), &config)
            .expect("normalize");
        assert_eq!(rec.species, Species::Cat);
        assert_eq!(rec.sex, Some(Sex::SpayedFemale));
    }

    #[test]
    fn mixed_species_both_normalized() {
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };
        let dog = make_animal("dog-2", json!({}));
        let cat = make_animal("cat-2", json!({}));
        let dog_rec = normalize_rg_record(dog, Species::Dog, &IncludedIndex::new(), &config)
            .expect("dog");
        let cat_rec = normalize_rg_record(cat, Species::Cat, &IncludedIndex::new(), &config)
            .expect("cat");
        assert_eq!(dog_rec.species, Species::Dog);
        assert_eq!(cat_rec.species, Species::Cat);
    }

    // ─── Location mapping (PRD-homeward-ingest-location-backfill-missing) ─────
    // Inline module-level regression coverage, additional to the crate's own
    // `tests/locbackfill_ac*` integration tests (which exercise the same
    // mapping through the public `fetch_normalized_page` API end-to-end).

    #[test]
    fn location_populated_from_locations_relationship() {
        let animal: RgAnimal = serde_json::from_value(json!({
            "id": "dog-loc-1",
            "type": "animals",
            "attributes": {
                "updatedDate": "2024-01-15T10:00:00Z",
                "createdDate": "2024-01-10T08:00:00Z",
            },
            "relationships": {
                "locations": { "data": [ { "type": "locations", "id": "loc-1" } ] },
            },
        }))
        .expect("test fixture parse");

        let mut included = IncludedIndex::new();
        included.insert(
            ("locations".to_owned(), "loc-1".to_owned()),
            json!({ "city": "Austin", "state": "TX", "lat": 30.2672, "lon": -97.7431 }),
        );

        let config = RescueGroupsConfig { api_key: "test".to_owned(), base_url: "http://localhost".to_owned() };
        let rec = normalize_rg_record(animal, Species::Dog, &included, &config).expect("normalize");

        let loc = rec.location.expect("location must be populated from the `locations` relationship");
        assert_eq!(loc.city_county, "Austin");
        assert_eq!(loc.state.as_deref(), Some("TX"));
        assert_eq!(loc.lat, Some(30.27));
        assert_eq!(loc.lon, Some(-97.74));
    }

    #[test]
    fn location_falls_back_to_org_when_no_locations_relationship() {
        let animal: RgAnimal = serde_json::from_value(json!({
            "id": "dog-loc-2",
            "type": "animals",
            "attributes": {
                "updatedDate": "2024-01-15T10:00:00Z",
                "createdDate": "2024-01-10T08:00:00Z",
            },
            "relationships": {
                "orgs": { "data": [ { "type": "orgs", "id": "org-1" } ] },
            },
        }))
        .expect("test fixture parse");

        let mut included = IncludedIndex::new();
        included.insert(("orgs".to_owned(), "org-1".to_owned()), json!({ "city": "Dallas", "state": "TX" }));

        let config = RescueGroupsConfig { api_key: "test".to_owned(), base_url: "http://localhost".to_owned() };
        let rec = normalize_rg_record(animal, Species::Dog, &included, &config).expect("normalize");

        let loc = rec.location.expect("location must fall back to the `orgs` relationship");
        assert_eq!(loc.city_county, "Dallas");
        assert_eq!(loc.state.as_deref(), Some("TX"));
    }

    #[test]
    fn location_is_none_when_neither_relationship_present() {
        let animal = make_animal("dog-loc-3", json!({}));
        let config = RescueGroupsConfig { api_key: "test".to_owned(), base_url: "http://localhost".to_owned() };
        let rec = normalize_rg_record(animal, Species::Dog, &IncludedIndex::new(), &config).expect("normalize");
        assert!(
            rec.location.is_none(),
            "no locations/orgs relationship in the payload must map to None, not a fabricated location"
        );
    }

    #[test]
    fn species_from_query_maps_dogs_and_cats() {
        assert_eq!(species_from_query("dogs"), Species::Dog);
        assert_eq!(species_from_query("cats"), Species::Cat);
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

    // ─── URL construction (regression for the /public 404) ───────────────────

    #[test]
    fn build_search_url_uses_public_prefix() {
        let url = build_search_url("https://api.rescuegroups.org/v5", "dogs", 1);
        assert_eq!(
            url,
            "https://api.rescuegroups.org/v5/public/animals/search/available/dogs?limit=250&page=1&include=colors,pictures,locations,orgs"
        );
    }

    // Regression note (PRD-homeward-ingest-location-backfill-missing): the
    // `include` param's value is a comma-joined list of RESOURCE TYPES
    // (`colors,pictures,locations,orgs`), not species — this test only
    // guards against comma-joining the *species path segment* the way the
    // old (never-valid) `dogs,cats` request did, not against commas
    // anywhere in the URL.
    #[test]
    fn build_search_url_never_comma_joins_species() {
        for species in SPECIES {
            let url = build_search_url("https://api.rescuegroups.org/v5", species, 1);
            assert!(
                !url.contains(&format!("available/{species},")),
                "species path segment must not be comma-joined: {url}"
            );
            assert!(url.contains(&format!("available/{species}?")));
        }
    }

    // Regression: `offset` is silently ignored by the real API (verified live
    // — identical `pageReturned:1` content no matter what offset was sent).
    // `page` is the query param that actually advances.
    #[test]
    fn build_search_url_carries_page_and_page_size() {
        let url = build_search_url("https://api.rescuegroups.org/v5", "cats", 3);
        assert_eq!(
            url,
            "https://api.rescuegroups.org/v5/public/animals/search/available/cats?limit=250&page=3&include=colors,pictures,locations,orgs"
        );
    }

    // ─── Filter construction (regression for the 400 "not a valid filter field") ─

    #[test]
    fn build_filters_full_poll_is_empty() {
        assert_eq!(build_filters(None), Vec::<serde_json::Value>::new());
    }

    #[test]
    fn build_filters_delta_poll_uses_animals_namespaced_field() {
        let ts = DateTime::parse_from_rfc3339("2024-01-15T10:00:00Z")
            .expect("ts")
            .with_timezone(&Utc);
        let filters = build_filters(Some(&ts));
        assert_eq!(filters.len(), 1);
        assert_eq!(
            filters[0]["fieldName"], "animals.updatedDate",
            "bare `updatedDate` 400s live — the API namespaces filterable \
             fields under the resource type"
        );
        assert_eq!(filters[0]["operation"], "greaterthanorequal");
        assert_eq!(filters[0]["criteria"], "2024-01-15T10:00:00Z");
    }

    #[test]
    fn build_filters_never_sends_bare_updated_date_field_name() {
        let ts = Utc::now();
        let filters = build_filters(Some(&ts));
        assert_ne!(
            filters[0]["fieldName"], "updatedDate",
            "bare updatedDate is rejected with HTTP 400 by the real API"
        );
    }

    #[test]
    fn build_filters_criteria_is_rfc3339_not_space_separated() {
        // Regression for the HTTP 500 "System error": a space-separated,
        // non-RFC3339 criteria (e.g. "2026-08-13 02:44:31") 500s live; RFC3339
        // with a trailing Z (with or without fractional seconds) returns 200.
        let ts = Utc::now();
        let filters = build_filters(Some(&ts));
        let criteria = filters[0]["criteria"].as_str().expect("criteria must be a string");

        assert!(!criteria.contains(' '), "criteria must not be space-separated: {criteria}");
        assert!(criteria.contains('T'), "criteria must use the RFC3339 'T' separator: {criteria}");
        assert!(criteria.ends_with('Z'), "criteria must end with 'Z': {criteria}");
        DateTime::parse_from_rfc3339(criteria)
            .unwrap_or_else(|e| panic!("criteria must parse as RFC3339: {criteria}: {e}"));
    }

    #[test]
    fn species_list_is_dogs_and_cats_one_at_a_time() {
        assert_eq!(SPECIES, ["dogs", "cats"]);
    }

    // ─── Deserialization regression: real v5 `/public` payload shape ──────────
    //
    // Captured live (3 records each) from the real API. Guards against the
    // production "error decoding response body" regression: these exact
    // structs must deserialize this exact JSON:API envelope (string `id`,
    // `meta.count` not `totalRecords`, relationship refs resolved via
    // `included`, ~50 mostly-optional attributes).

    #[test]
    fn deserializes_real_dogs_sample_and_populates_records() {
        let raw = include_str!("../../tests/fixtures/rg-sample-dogs.json");
        let page: RgPage = serde_json::from_str(raw).expect("real dogs sample must deserialize");
        assert_eq!(page.data.len(), 3);

        let included = build_included_index(&page.included);
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };

        let mut saw_doli = false;
        for animal in page.data {
            let id = animal.id.clone();
            let rec = normalize_rg_record(animal, Species::Dog, &included, &config)
                .expect("normalize real dog record");
            assert!(!rec.source_animal_id.as_deref().unwrap_or_default().is_empty());
            assert_eq!(rec.species, Species::Dog);
            if id == "10131543" {
                saw_doli = true;
                assert_eq!(rec.breed_primary.as_deref(), Some("Husky"));
                assert!(!rec.photos.is_empty(), "Doli must have a photo resolved via `included`");
                assert!(rec.photos[0].url.starts_with("https://cdn.rescuegroups.org"));
            }
        }
        assert!(saw_doli, "expected animal 10131543 (Doli) in the dogs sample");
    }

    #[test]
    fn deserializes_real_cats_sample_and_populates_records() {
        let raw = include_str!("../../tests/fixtures/rg-sample-cats.json");
        let page: RgPage = serde_json::from_str(raw).expect("real cats sample must deserialize");
        assert_eq!(page.data.len(), 3);

        let included = build_included_index(&page.included);
        let config = RescueGroupsConfig {
            api_key: "test".to_owned(),
            base_url: "http://localhost".to_owned(),
        };

        let mut saw_stowaway = false;
        for animal in page.data {
            let id = animal.id.clone();
            let rec = normalize_rg_record(animal, Species::Cat, &included, &config)
                .expect("normalize real cat record");
            assert!(!rec.source_animal_id.as_deref().unwrap_or_default().is_empty());
            assert_eq!(rec.species, Species::Cat);
            if id == "10013509" {
                saw_stowaway = true;
                assert_eq!(rec.breed_primary.as_deref(), Some("Domestic Short Hair"));
                assert!(!rec.colors.is_empty(), "Stowaway must have a color resolved via `included`");
                assert!(!rec.photos.is_empty(), "Stowaway must have a photo resolved via `included`");
            }
        }
        assert!(saw_stowaway, "expected animal 10013509 (Stowaway) in the cats sample");
    }

    // ─── Deserialization regression: zero-match page omits `data` entirely ────
    //
    // The real API doesn't send `"data":[]` on a zero-match page — it omits
    // the `data` key altogether, leaving only `meta`. Without `#[serde(default)]`
    // on `RgPage::data`, this fails with "missing field `data`", which surfaces
    // as the intermittent "HTTP error: error decoding response body" poll
    // failure (roughly half of poll cycles, whenever a page's filter matches
    // zero records).

    #[test]
    fn deserializes_zero_match_page_with_no_data_key() {
        let raw = r#"{"meta":{"count":0,"countReturned":0,"pageReturned":1,"limit":250,"pages":0,"transactionId":"x"}}"#;
        let page: RgPage = serde_json::from_str(raw).expect("zero-match page must deserialize");
        assert!(page.data.is_empty());
    }

    // ─── Pagination regression: `offset` is ignored, `page` advances ──────────
    //
    // Verified live against the real API: sending `offset=0` vs `offset=250`
    // (or any other value) returns byte-identical `data` and `pageReturned:1`
    // every time — `offset` is silently ignored. `page=N` is the query param
    // that actually advances (confirmed: `page=2` returns distinct record ids
    // and echoes `pageReturned:2`). These tests cover the `RgMeta` fields the
    // new page-loop termination logic depends on, and the loop logic itself.

    #[test]
    fn rg_meta_deserializes_pagination_fields() {
        let raw = r#"{"count":31893,"countReturned":5,"pageReturned":2,"limit":5,"pages":6379,"transactionId":"x"}"#;
        let meta: RgMeta = serde_json::from_str(raw).expect("meta must deserialize");
        assert_eq!(meta.total, 31893);
        assert_eq!(meta.count_returned, 5);
        assert_eq!(meta.page_returned, 2);
        assert_eq!(meta.pages, 6379);
    }

    #[test]
    fn rg_meta_pagination_fields_default_when_absent() {
        // Only `count` is guaranteed present on every response shape observed
        // live (e.g. the zero-match payload). The rest must default rather
        // than fail deserialization.
        let raw = r#"{"count":0}"#;
        let meta: RgMeta = serde_json::from_str(raw).expect("meta must deserialize");
        assert_eq!(meta.total, 0);
        assert_eq!(meta.count_returned, 0);
        assert_eq!(meta.page_returned, 0);
        assert_eq!(meta.pages, 0);
    }

    /// Mirrors the loop-termination condition in `Connector::poll` for
    /// `RescueGroupsConnector`: `page_returned >= pages_total || count_returned
    /// < PAGE_SIZE`. Exercised directly here (rather than only through the
    /// wiremock integration test) so the termination arithmetic itself is
    /// pinned independent of HTTP plumbing.
    fn should_stop_paging(page_returned: u64, pages_total: u64, count_returned: u64) -> bool {
        page_returned >= pages_total || count_returned < PAGE_SIZE
    }

    #[test]
    fn pagination_loop_continues_when_more_pages_remain() {
        // Full page (250 of 250), page 1 of many → keep going.
        assert!(!should_stop_paging(1, 6379, PAGE_SIZE));
    }

    #[test]
    fn pagination_loop_stops_on_last_page() {
        // Page number caught up to the total page count → stop.
        assert!(should_stop_paging(6379, 6379, PAGE_SIZE));
    }

    #[test]
    fn pagination_loop_stops_on_short_page_even_if_pages_looks_bigger() {
        // Defensive fallback: a short page (fewer than `limit` records) always
        // stops the loop, regardless of what `pages`/`pageReturned` say.
        assert!(should_stop_paging(1, 6379, PAGE_SIZE - 1));
    }

    #[test]
    fn pagination_loop_stops_immediately_on_zero_match_page() {
        // The zero-match shape: pages=0, pageReturned=1, countReturned=0.
        assert!(should_stop_paging(1, 0, 0));
    }
}
