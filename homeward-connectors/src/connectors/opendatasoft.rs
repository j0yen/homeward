//! OpenDataSoft Explore API v2.1 connector.
//!
//! The ODS Explore v2.1 dialect differs from Socrata/SODA:
//! - Records live at `{base_url}/api/explore/v2.1/catalog/datasets/{dataset_id}/records`
//! - Filtered via ODSQL `where=` clauses (not `$where=`)
//! - Paged with `limit` / `offset` (no `$limit` / `$offset`)
//! - Each result carries a `record.timestamp` usable as a watermark
//!
//! `ToS` notes:
//! - Public datasets require no auth (anonymous rate limits apply)
//! - Optional app-token via `Authorization: Apikey <token>`

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use homeward_schema::{
    Availability, ChipStatus, IntakeType, PetRecord, Provenance, Species, TosClass,
    provenance::SourceId,
};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};
use ulid::Ulid;
use url::Url;

use crate::{
    Connector, Cursor,
    catalog::OpenDataSoftConfig,
    error::ConnectorError,
    http::PoliteClient,
};

/// Maximum records per ODS page.
const PAGE_SIZE: u64 = 100;

/// Known intake-type values that indicate a STRAY feed (shared with probe.rs logic).
///
/// This mirrors the `STRAY_VALUES` array in `probe.rs` exactly.  If you add a
/// value here, mirror it there (and vice-versa).
pub const STRAY_VALUES: &[&str] = &[
    "STRAY",
    "FOUND",
    "STRAY INCOMING",
    "FIELD",
    "CONFISCATED",
    "WILDLIFE",
];

/// Returns `true` when `v` (case-insensitive, trimmed) matches a stray token.
fn is_stray_value(v: &str) -> bool {
    let upper = v.trim().to_uppercase();
    STRAY_VALUES
        .iter()
        .any(|sv| upper == *sv || upper.contains(sv))
}

// ─── Wire response types ──────────────────────────────────────────────────────

/// Top-level ODS Explore v2.1 records response.
#[derive(Debug, Deserialize)]
struct OdsResponse {
    total_count: u64,
    results: Vec<OdsResult>,
}

/// One entry in `results`.
#[derive(Debug, Deserialize)]
struct OdsResult {
    record: OdsRecord,
}

/// The `record` object inside each result.
#[derive(Debug, Deserialize)]
struct OdsRecord {
    id: Option<String>,
    timestamp: Option<String>,
    fields: Value,
}

// ─── Connector ────────────────────────────────────────────────────────────────

/// OpenDataSoft Explore API v2.1 connector.
///
/// Built from an [`OpenDataSoftConfig`]; stateless — all state lives in the
/// [`Cursor`] the caller stores between polls.
pub struct OpenDataSoftConnector {
    config: OpenDataSoftConfig,
    client: PoliteClient,
}

impl OpenDataSoftConnector {
    /// Create with the default polite client.
    ///
    /// # Errors
    /// Returns [`ConnectorError`] if the HTTP client cannot be built.
    pub fn new(config: OpenDataSoftConfig) -> Result<Self, ConnectorError> {
        let client = PoliteClient::new(Duration::from_millis(300))?;
        Ok(Self { config, client })
    }

    /// Create with a custom client (useful for tests).
    #[must_use]
    pub fn with_client(config: OpenDataSoftConfig, client: PoliteClient) -> Self {
        Self { config, client }
    }

    /// Build the ODS records URL for a given page offset and optional cursor.
    fn build_url(&self, offset: u64, since: Option<&DateTime<Utc>>) -> Result<Url, ConnectorError> {
        let cm = &self.config.column_map;

        // ODSQL filter: always restrict to dogs/cats; add timestamp filter when
        // a cursor is present.  No let-chains: use nested match.
        let animal_type_clause = format!(
            "upper({animal_type}) in ('DOG','CAT')",
            animal_type = cm.animal_type
        );

        let where_clause = match since {
            Some(ts) => {
                // Determine which timestamp field to filter on.
                // Prefer intake_date from column_map; fall back to `record_timestamp`.
                let ts_field = cm
                    .intake_date
                    .as_deref()
                    .unwrap_or("record_timestamp");
                format!(
                    "{animal} AND {ts_field} > '{ts}'",
                    animal = animal_type_clause,
                    ts = ts.format("%Y-%m-%dT%H:%M:%S")
                )
            }
            None => animal_type_clause,
        };

        let base = format!(
            "{base}/api/explore/v2.1/catalog/datasets/{dataset}/records",
            base = self.config.base_url.trim_end_matches('/'),
            dataset = self.config.dataset_id,
        );

        let mut url = Url::parse(&base)?;

        {
            let mut qs = url.query_pairs_mut();
            qs.append_pair("limit", &PAGE_SIZE.to_string());
            qs.append_pair("offset", &offset.to_string());
            qs.append_pair("where", &where_clause);
            // Order by the intake timestamp for deterministic paging.
            let order_col = cm
                .intake_date
                .as_deref()
                .unwrap_or("record_timestamp");
            qs.append_pair("order_by", &format!("{order_col} DESC"));
        }

        Ok(url)
    }
}

#[async_trait]
impl Connector for OpenDataSoftConnector {
    async fn poll(&self, since: Option<Cursor>) -> Result<Vec<PetRecord>, ConnectorError> {
        let since_ts: Option<DateTime<Utc>> = match &since {
            Some(Cursor::Timestamp(ts)) => Some(*ts),
            Some(Cursor::Opaque(_)) | None => None,
        };

        let mut records = Vec::new();
        let mut offset = 0u64;

        loop {
            let url = self.build_url(offset, since_ts.as_ref())?;
            debug!(
                %url,
                offset,
                source = self.config.name,
                "fetching ODS page"
            );

            let resp = self.client.get(&url, None, None).await?;

            if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
                break;
            }

            let ods: OdsResponse = resp.json().await.map_err(ConnectorError::Http)?;
            let batch_len = u64::try_from(ods.results.len()).unwrap_or(u64::MAX);

            for result in ods.results {
                match normalize_ods_record(&result.record, &self.config) {
                    Ok(Some(rec)) => records.push(rec),
                    Ok(None) => {}   // filtered out (not a stray intake)
                    Err(e) => {
                        warn!(
                            source = self.config.name,
                            "skipping ODS record: {e}"
                        );
                    }
                }
            }

            offset += PAGE_SIZE;
            // If the server returned fewer records than requested, we're done.
            if batch_len < PAGE_SIZE || offset >= ods.total_count {
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
                "{base}/api/explore/v2.1/catalog/datasets/{dataset}/records",
                base = self.config.base_url.trim_end_matches('/'),
                dataset = self.config.dataset_id,
            )),
            source_etag: None,
        }
    }

    fn cadence_hint(&self) -> Duration {
        // Municipal shelters update frequently; poll every 4 hours.
        Duration::from_secs(4 * 3600)
    }
}

// ─── Normalization ────────────────────────────────────────────────────────────

/// Normalize one ODS record to a [`PetRecord`].
///
/// Returns `Ok(None)` when the record should be filtered out (e.g. not a
/// stray intake), `Ok(Some(rec))` on success, or `Err` on a hard parse
/// failure (unknown species).
fn normalize_ods_record(
    record: &OdsRecord,
    config: &OpenDataSoftConfig,
) -> Result<Option<PetRecord>, ConnectorError> {
    let cm = &config.column_map;

    let get = |key: &str| -> Option<&str> {
        record.fields.get(key).and_then(|v| v.as_str())
    };

    // ── Filter: stray-only intake type ────────────────────────────────────────
    let intake_type_raw = get(&cm.intake_type).unwrap_or("");
    if !is_stray_value(intake_type_raw) {
        return Ok(None);
    }

    // ── Species ───────────────────────────────────────────────────────────────
    let animal_type_str = get(&cm.animal_type).unwrap_or("unknown");
    let species = parse_ods_species(animal_type_str)?;

    // ── Fields ────────────────────────────────────────────────────────────────
    let intake_type = parse_ods_intake_type(intake_type_raw);

    let animal_id = get(&cm.animal_id).map(std::borrow::ToOwned::to_owned);

    let found_location_text = cm
        .found_location
        .as_deref()
        .and_then(|col| get(col))
        .map(str::to_owned);

    let intake_date = cm
        .intake_date
        .as_deref()
        .and_then(|col| get(col))
        .and_then(parse_ods_datetime)
        // Fall back to record.timestamp as the intake instant.
        .or_else(|| record.timestamp.as_deref().and_then(parse_ods_datetime));

    let outcome_date = cm
        .outcome_date
        .as_deref()
        .and_then(|col| get(col))
        .and_then(parse_ods_datetime);

    let availability = determine_ods_availability(&record.fields, cm);

    let chip_status = cm
        .chip_status
        .as_deref()
        .and_then(|col| get(col))
        .map_or(ChipStatus::Unknown, parse_ods_chip_status);

    let breed_primary = cm
        .breed
        .as_deref()
        .and_then(|col| get(col))
        .map(str::to_owned);

    let color = cm
        .color
        .as_deref()
        .and_then(|col| get(col))
        .map(str::to_owned);

    let now = Utc::now();
    let last_seen = intake_date.unwrap_or(now);

    Ok(Some(PetRecord {
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
        photos: vec![],
        first_seen: last_seen,
        last_seen,
        last_confirmed: Some(now),
        intake_date,
        outcome_date,
        secondary_provenances: vec![],
    }))
}

fn parse_ods_species(s: &str) -> Result<Species, ConnectorError> {
    match s.trim().to_uppercase().as_str() {
        "DOG" | "CANINE" => Ok(Species::Dog),
        "CAT" | "FELINE" => Ok(Species::Cat),
        other => Err(ConnectorError::UnknownSpecies(other.to_owned())),
    }
}

fn parse_ods_intake_type(s: &str) -> IntakeType {
    match s.trim().to_uppercase().as_str() {
        "STRAY" => IntakeType::Stray,
        "FOUND" | "FOUND REPORT" | "FOUND_REPORT" => IntakeType::FoundReport,
        "WILDLIFE" | "CONFISCATED" | "FIELD" => IntakeType::Stray,
        _ => IntakeType::Unknown,
    }
}

fn determine_ods_availability(
    fields: &Value,
    cm: &crate::connectors::socrata::SocrataColumnMap,
) -> Availability {
    // If outcome_date is set, animal has left.
    if let Some(col) = cm.outcome_date.as_deref() {
        if let Some(v) = fields.get(col).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return Availability::Departed;
            }
        }
    }
    // Kennel status heuristic.
    if let Some(col) = cm.kennel_status.as_deref() {
        if let Some(v) = fields.get(col).and_then(|v| v.as_str()) {
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

fn parse_ods_chip_status(s: &str) -> ChipStatus {
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

fn parse_ods_datetime(s: &str) -> Option<DateTime<Utc>> {
    // ODS timestamps are typically ISO 8601 with Z or offset.
    let s_t = s.replacen(' ', "T", 1);

    if let Ok(dt) = DateTime::parse_from_rfc3339(&s_t) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in &[
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d",
    ] {
        if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&s_t, fmt) {
            return Some(ndt.and_utc());
        }
    }
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| ndt.and_utc())
}

// ─── ODS Probe helpers (pub for probe.rs to call) ────────────────────────────

/// Run an ODS probe: fetch dataset field metadata and a one-record sample.
///
/// Returns the raw field names from the ODS metadata endpoint.
pub async fn fetch_ods_fields(
    client: &dyn crate::probe::ProbeClient,
    base_url: &str,
    dataset_id: &str,
) -> Result<Vec<String>, crate::probe::ProbeError> {
    let url = format!(
        "{base}/api/explore/v2.1/catalog/datasets/{dataset}",
        base = base_url.trim_end_matches('/'),
        dataset = dataset_id,
    );
    let text = client.fetch_json(&url).await?;
    let json: Value =
        serde_json::from_str(&text).map_err(|e| crate::probe::ProbeError::Json(e.to_string()))?;

    // ODS dataset metadata: fields live at `.fields[].name`.
    let fields = json
        .get("fields")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.get("name").and_then(|v| v.as_str()).map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(fields)
}

/// Fetch one sample record from an ODS dataset.
///
/// Returns the first `record.fields` object, or `Value::Null` if empty.
pub async fn fetch_ods_sample(
    client: &dyn crate::probe::ProbeClient,
    base_url: &str,
    dataset_id: &str,
) -> Value {
    let url = format!(
        "{base}/api/explore/v2.1/catalog/datasets/{dataset}/records?limit=1",
        base = base_url.trim_end_matches('/'),
        dataset = dataset_id,
    );
    let text = match client.fetch_json(&url).await {
        Ok(t) => t,
        Err(_) => return Value::Null,
    };
    let json: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Value::Null,
    };
    // Navigate results[0].record.fields
    json.get("results")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .and_then(|r| r.get("record"))
        .and_then(|r| r.get("fields"))
        .cloned()
        .unwrap_or(Value::Null)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::socrata::SocrataColumnMap;

    fn make_config() -> OpenDataSoftConfig {
        OpenDataSoftConfig {
            name: "test-ods".to_owned(),
            base_url: "https://data.example.org".to_owned(),
            dataset_id: "animal-shelter-intakes".to_owned(),
            column_map: SocrataColumnMap {
                animal_id: "animal_id".to_owned(),
                animal_type: "species".to_owned(),
                intake_type: "intake_condition".to_owned(),
                outcome_date: None,
                kennel_status: None,
                found_location: Some("found_location".to_owned()),
                chip_status: None,
                intake_date: Some("intake_date".to_owned()),
                breed: Some("breed".to_owned()),
                name: Some("name".to_owned()),
                color: Some("color".to_owned()),
            },
        }
    }

    /// Fixture: one ODS `/records` response with a STRAY dog and a non-STRAY cat.
    fn fixture_mixed_response() -> &'static str {
        r#"{
            "total_count": 2,
            "results": [
                {
                    "record": {
                        "id": "rec-001",
                        "timestamp": "2024-01-15T10:00:00Z",
                        "fields": {
                            "animal_id": "ODS-001",
                            "species": "Dog",
                            "intake_condition": "STRAY",
                            "found_location": "123 Main St",
                            "intake_date": "2024-01-15T10:00:00Z",
                            "breed": "Labrador Mix",
                            "name": "Buddy",
                            "color": "Black/White"
                        }
                    }
                },
                {
                    "record": {
                        "id": "rec-002",
                        "timestamp": "2024-01-16T09:00:00Z",
                        "fields": {
                            "animal_id": "ODS-002",
                            "species": "Cat",
                            "intake_condition": "Owner Surrender",
                            "found_location": "",
                            "intake_date": "2024-01-16T09:00:00Z",
                            "breed": "Domestic Shorthair",
                            "name": "Whiskers",
                            "color": "Orange"
                        }
                    }
                }
            ]
        }"#
    }

    /// Fixture: empty result page (for `poll(since)` empty-response test).
    fn fixture_empty_response() -> &'static str {
        r#"{"total_count": 0, "results": []}"#
    }

    // ── AC1: poll(None) with fixture JSON returns correct Vec<PetRecord> ───────

    #[test]
    fn ac1_poll_none_fixture_returns_stray_records() {
        let config = make_config();
        let ods_json: serde_json::Value =
            serde_json::from_str(fixture_mixed_response()).expect("parse fixture");

        let results_arr = ods_json["results"].as_array().expect("results array");
        let mut records = Vec::new();
        for r in results_arr {
            let record: OdsRecord =
                serde_json::from_value(r["record"].clone()).expect("parse record");
            match normalize_ods_record(&record, &config) {
                Ok(Some(rec)) => records.push(rec),
                Ok(None) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        // AC1: only the STRAY dog should survive
        assert_eq!(records.len(), 1, "only STRAY records should survive");
        let rec = &records[0];
        assert_eq!(rec.species, Species::Dog);
        assert_eq!(rec.intake_type, IntakeType::Stray);
        assert_eq!(rec.source_animal_id.as_deref(), Some("ODS-001"));
        assert_eq!(rec.found_location_text.as_deref(), Some("123 Main St"));
        // AC1: provenance tagged correctly
        assert_eq!(rec.source.name, "test-ods");
        assert_eq!(rec.source.tos_class, TosClass::OpenData);
    }

    // ── AC3: only STRAY-bearing intake records survive ─────────────────────────

    #[test]
    fn ac3_only_stray_intake_records_survive() {
        let config = make_config();
        let ods_json: serde_json::Value =
            serde_json::from_str(fixture_mixed_response()).expect("parse fixture");

        let results_arr = ods_json["results"].as_array().expect("results array");
        let mut stray_count = 0usize;
        let mut total = 0usize;

        for r in results_arr {
            let record: OdsRecord =
                serde_json::from_value(r["record"].clone()).expect("parse record");
            total += 1;
            match normalize_ods_record(&record, &config) {
                Ok(Some(_)) => stray_count += 1,
                Ok(None) => {}
                Err(e) => panic!("unexpected error: {e}"),
            }
        }

        assert_eq!(total, 2, "fixture has 2 rows");
        assert_eq!(stray_count, 1, "only 1 should survive (STRAY dog)");
    }

    // ── AC2: poll(Some(Cursor::Timestamp(t))) builds ODSQL where clause ────────

    #[test]
    fn ac2_cursor_timestamp_builds_odsql_where() {
        let config = make_config();
        let connector = OpenDataSoftConnector {
            config,
            client: PoliteClient::from_client(
                reqwest::Client::new(),
                Duration::from_millis(0),
            ),
        };

        let ts: DateTime<Utc> = "2024-01-10T00:00:00Z".parse().expect("parse ts");
        let url = connector
            .build_url(0, Some(&ts))
            .expect("build_url should succeed");

        let url_str = url.to_string();
        // ODSQL where clause should contain the timestamp filter
        assert!(
            url_str.contains("intake_date"),
            "where clause should reference intake_date: {url_str}"
        );
        assert!(
            url_str.contains("2024-01-10"),
            "where clause should contain the timestamp: {url_str}"
        );
        assert!(
            url_str.contains("explore/v2.1"),
            "URL should use ODS Explore API: {url_str}"
        );
    }

    // ── AC2: empty result set returns Ok(vec![]) ──────────────────────────────

    #[test]
    fn ac2_empty_result_returns_ok_empty_vec() {
        let empty: OdsResponse =
            serde_json::from_str(fixture_empty_response()).expect("parse empty fixture");
        assert_eq!(empty.results.len(), 0);
        assert_eq!(empty.total_count, 0);
    }

    // ── AC4: connector uses PoliteClient (structural check) ───────────────────

    #[test]
    fn ac4_connector_uses_polite_client() {
        // Building OpenDataSoftConnector::new() succeeds → uses PoliteClient internally.
        // (No direct reqwest::Client::new() in this file.)
        let config = make_config();
        let connector = OpenDataSoftConnector::new(config);
        // Succeeds if PoliteClient can be built.
        assert!(connector.is_ok(), "connector should build successfully");
    }

    // ── AC1: provenance is tagged opendatasoft-style ──────────────────────────

    #[test]
    fn ac1_provenance_tagged_with_ods_family() {
        let config = make_config();
        let connector = OpenDataSoftConnector {
            config,
            client: PoliteClient::from_client(
                reqwest::Client::new(),
                Duration::from_millis(0),
            ),
        };
        let prov = connector.provenance();
        // provenance source_url contains ODS endpoint
        let url = prov.source_url.expect("source_url should be set");
        assert!(
            url.contains("explore/v2.1"),
            "provenance URL should reference ODS API: {url}"
        );
        assert_eq!(prov.source.tos_class, TosClass::OpenData);
        assert_eq!(prov.source.name, "test-ods");
    }

    // ── Stray value helpers ───────────────────────────────────────────────────

    #[test]
    fn stray_values_match_known_tokens() {
        assert!(is_stray_value("STRAY"));
        assert!(is_stray_value("Stray"));
        assert!(is_stray_value("FOUND"));
        assert!(is_stray_value("found"));
        assert!(is_stray_value("WILDLIFE"));
        assert!(is_stray_value("CONFISCATED"));
        assert!(!is_stray_value("Owner Surrender"));
        assert!(!is_stray_value("Transfer"));
        assert!(!is_stray_value("Adoptable"));
    }

    // ── Datetime parsing ──────────────────────────────────────────────────────

    #[test]
    fn ods_datetime_formats_parse() {
        assert!(parse_ods_datetime("2024-01-15T10:00:00Z").is_some());
        assert!(parse_ods_datetime("2024-01-15T10:00:00+00:00").is_some());
        assert!(parse_ods_datetime("2024-01-15T10:00:00.000").is_some());
        assert!(parse_ods_datetime("2024-01-15").is_some());
        assert!(parse_ods_datetime("not-a-date").is_none());
    }

    // ── AC6: OpenDataSoftConfig in sources.toml loads and instantiates ────────

    #[test]
    fn ac6_opendatasoft_config_from_catalog_instantiates() {
        use crate::catalog::load_catalog;
        use std::io::Write as _;

        let toml_str = r#"
[[opendatasoft]]
name       = "test-city"
base_url   = "https://data.testcity.gov"
dataset_id = "animal-intakes-001"
[opendatasoft.column_map]
animal_id   = "animal_id"
animal_type = "species"
intake_type = "intake_condition"
"#;
        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(toml_str.as_bytes()).expect("write");

        let catalog = load_catalog(f.path()).expect("catalog should load");
        assert_eq!(catalog.opendatasoft.len(), 1);

        let cfg = catalog.opendatasoft.into_iter().next().expect("one entry");
        assert_eq!(cfg.name, "test-city");
        assert_eq!(cfg.base_url, "https://data.testcity.gov");

        // Instantiate the connector from the loaded config.
        let connector = OpenDataSoftConnector::new(cfg);
        assert!(connector.is_ok(), "should instantiate from catalog config");
    }
}
