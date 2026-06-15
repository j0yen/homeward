//! `homeward-connectors probe` — classify a Socrata dataset as a usable
//! STRAY-bearing animal-intake feed, or emit an honest red verdict.
//!
//! Usage:
//!   homeward probe <domain> <dataset_id> [--name <slug>] [--json]
//!
//! Fetches SODA column metadata and a one-row sample, then classifies columns
//! into `SocrataColumnMap` slots. Emits GREEN (TOML block) or RED (verdict).

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::{
    connectors::socrata::SocrataColumnMap,
    error::ConnectorError,
    http::PoliteClient,
};

// ─── Stray value set ─────────────────────────────────────────────────────────

/// Known intake-type values that indicate a STRAY feed.
const STRAY_VALUES: &[&str] = &[
    "STRAY",
    "FOUND",
    "STRAY INCOMING",
    "FIELD",
    "CONFISCATED",
    "WILDLIFE",
];

fn is_stray_value(v: &str) -> bool {
    let upper = v.trim().to_uppercase();
    STRAY_VALUES.iter().any(|sv| upper == *sv || upper.contains(sv))
}

// ─── Column heuristics ────────────────────────────────────────────────────────

/// Confidence in a column mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confidence {
    /// The column value was actually observed to contain a STRAY/FOUND value.
    Confirmed,
    /// Column name matched a heuristic but no confirming value was observed.
    Guessed,
}

impl Confidence {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Guessed => "guessed",
        }
    }
}

/// A candidate column mapping with confidence annotation.
#[derive(Debug, Clone)]
pub struct MappedColumn {
    /// The actual source column name.
    pub source_col: String,
    /// The slot this maps to (e.g. "animal_id").
    pub slot: &'static str,
    /// Confidence level.
    pub confidence: Confidence,
}

/// A column entry from SODA column metadata.
#[derive(Debug)]
struct SodaColumn {
    field_name: String,
    name: String,
}

/// Heuristic keywords for each slot.
///
/// The first match wins; order within each list goes most → least specific.
fn slot_keywords(slot: &'static str) -> &'static [&'static str] {
    match slot {
        "animal_id"       => &["animal_id", "animalid", "animal_number", "id"],
        "animal_type"     => &["animal_type", "animaltype", "species", "type"],
        "intake_type"     => &["intake_type", "intaketype", "intake_subtype", "intake_condition", "entry_reason"],
        "intake_date"     => &["intake_date", "intakedate", "datetime", "intake_datetime", "date_intake", "entry_date"],
        "found_location"  => &["found_location", "foundlocation", "location_found", "found_address", "where_found"],
        "chip_status"     => &["chip_status", "chipstatus", "microchip", "chip"],
        "kennel_status"   => &["kennel_status", "kennelstatus", "status", "custody_status"],
        "breed"           => &["breed", "primary_breed", "breed_primary"],
        "name"            => &["name", "animal_name", "pet_name"],
        "color"           => &["color", "colour", "primary_color"],
        "outcome_date"    => &["outcome_date", "outcomedate", "outcome_datetime", "date_outcome"],
        _                 => &[],
    }
}

// ─── Probe client trait (injectable for tests) ───────────────────────────────

/// Injectable HTTP fetcher for the probe — real or fixture-backed in tests.
#[async_trait::async_trait]
pub trait ProbeClient: Send + Sync {
    /// Fetch raw JSON text from a URL.
    async fn fetch_json(&self, url: &str) -> Result<String, ProbeError>;
}

/// Real HTTP implementation using `PoliteClient`.
pub struct RealProbeClient {
    polite: PoliteClient,
}

impl RealProbeClient {
    /// Create a real probe client.
    ///
    /// # Errors
    /// Returns `ProbeError` if the HTTP client cannot be built.
    pub fn new() -> Result<Self, ProbeError> {
        let polite = PoliteClient::new(Duration::from_millis(300))
            .map_err(|e| ProbeError::Http(e.to_string()))?;
        Ok(Self { polite })
    }

    /// Build from an existing `reqwest::Client` (for tests).
    #[must_use]
    pub fn from_reqwest(client: Client) -> Self {
        Self {
            polite: PoliteClient::from_client(client, Duration::from_millis(0)),
        }
    }
}

#[async_trait::async_trait]
impl ProbeClient for RealProbeClient {
    async fn fetch_json(&self, url: &str) -> Result<String, ProbeError> {
        let parsed = Url::parse(url).map_err(|e| ProbeError::Http(e.to_string()))?;
        let resp = self
            .polite
            .get(&parsed, None, None)
            .await
            .map_err(|e| match e {
                ConnectorError::UnexpectedStatus { status, .. } if status == 404 => {
                    ProbeError::NotFound {
                        url: url.to_owned(),
                    }
                }
                other => ProbeError::Http(other.to_string()),
            })?;
        let status = resp.status().as_u16();
        if status == 404 {
            return Err(ProbeError::NotFound { url: url.to_owned() });
        }
        resp.text().await.map_err(|e| ProbeError::Http(e.to_string()))
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Errors that can occur during a probe.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// Dataset not found (404).
    #[error("dataset not found: {url}")]
    NotFound {
        /// The URL that returned 404.
        url: String,
    },
    /// Network or HTTP error.
    #[error("HTTP error: {0}")]
    Http(String),
    /// JSON parse error.
    #[error("JSON parse error: {0}")]
    Json(String),
}

// ─── Probe result types ───────────────────────────────────────────────────────

/// The outcome of a probe run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Dataset is a usable STRAY-bearing feed.
    Green,
    /// Dataset is not usable; reason explains why.
    Red,
}

/// The full probe result.
#[derive(Debug)]
pub struct ProbeResult {
    /// Overall verdict.
    pub verdict: Verdict,
    /// Human-readable reason (for RED) or summary (for GREEN).
    pub reason: String,
    /// The column map assembled from probing (None on hard RED before mapping).
    pub column_map: Option<ColumnMapDraft>,
    /// The slug/name for the source (from --name or derived from dataset_id).
    pub source_name: String,
    /// Domain probed.
    pub domain: String,
    /// Dataset ID probed.
    pub dataset_id: String,
}

/// A draft column map with per-column confidence annotations.
#[derive(Debug, Clone)]
pub struct ColumnMapDraft {
    /// Mapped animal ID column.
    pub animal_id: Option<MappedColumn>,
    /// Mapped animal type/species column.
    pub animal_type: Option<MappedColumn>,
    /// Mapped intake type column (may be confirmed STRAY-bearing).
    pub intake_type: Option<MappedColumn>,
    /// Mapped intake date column.
    pub intake_date: Option<MappedColumn>,
    /// Mapped found-location column.
    pub found_location: Option<MappedColumn>,
    /// Mapped chip/microchip status column.
    pub chip_status: Option<MappedColumn>,
    /// Mapped kennel/custody status column.
    pub kennel_status: Option<MappedColumn>,
    /// Mapped breed column.
    pub breed: Option<MappedColumn>,
    /// Mapped animal name column.
    pub name: Option<MappedColumn>,
    /// Mapped color column.
    pub color: Option<MappedColumn>,
    /// Mapped outcome date column.
    pub outcome_date: Option<MappedColumn>,
}

impl ColumnMapDraft {
    fn empty() -> Self {
        Self {
            animal_id: None,
            animal_type: None,
            intake_type: None,
            intake_date: None,
            found_location: None,
            chip_status: None,
            kennel_status: None,
            breed: None,
            name: None,
            color: None,
            outcome_date: None,
        }
    }

    /// Convert to a `SocrataColumnMap` for TOML round-trip testing.
    #[must_use]
    pub fn to_column_map(&self) -> Option<SocrataColumnMap> {
        Some(SocrataColumnMap {
            animal_id: self.animal_id.as_ref()?.source_col.clone(),
            animal_type: self.animal_type.as_ref()?.source_col.clone(),
            intake_type: self.intake_type.as_ref()?.source_col.clone(),
            outcome_date: self.outcome_date.as_ref().map(|c| c.source_col.clone()),
            kennel_status: self.kennel_status.as_ref().map(|c| c.source_col.clone()),
            found_location: self.found_location.as_ref().map(|c| c.source_col.clone()),
            chip_status: self.chip_status.as_ref().map(|c| c.source_col.clone()),
            intake_date: self.intake_date.as_ref().map(|c| c.source_col.clone()),
            breed: self.breed.as_ref().map(|c| c.source_col.clone()),
            name: self.name.as_ref().map(|c| c.source_col.clone()),
            color: self.color.as_ref().map(|c| c.source_col.clone()),
        })
    }
}

// ─── JSON output for --json flag ─────────────────────────────────────────────

/// Structured JSON output emitted when `--json` is passed to `probe`.
#[derive(Debug, Serialize)]
pub struct JsonOutput {
    /// `"GREEN"` or `"RED"`.
    pub verdict: String,
    /// Human-readable explanation.
    pub reason: String,
    /// Draft column map or TOML block (GREEN), or partial findings (RED).
    pub draft: Option<serde_json::Value>,
}

// ─── Core probe logic ─────────────────────────────────────────────────────────

/// Run the probe against a Socrata dataset.
///
/// # Errors
/// Returns `ProbeError` on HTTP, JSON, or 404 errors.
pub async fn run_probe(
    client: &dyn ProbeClient,
    domain: &str,
    dataset_id: &str,
    source_name: &str,
) -> Result<ProbeResult, ProbeError> {
    // 1. Fetch column metadata.
    let cols_url = format!("https://{domain}/api/views/{dataset_id}/columns.json");
    let cols_text = client.fetch_json(&cols_url).await.map_err(|e| match e {
        ProbeError::NotFound { .. } => ProbeError::NotFound {
            url: format!("dataset {dataset_id} on {domain}"),
        },
        other => other,
    })?;

    let cols_json: Value =
        serde_json::from_str(&cols_text).map_err(|e| ProbeError::Json(e.to_string()))?;

    let columns = parse_soda_columns(&cols_json);

    if columns.is_empty() {
        return Ok(ProbeResult {
            verdict: Verdict::Red,
            reason: format!(
                "no columns returned from {domain}/api/views/{dataset_id}/columns.json; \
                 dataset may not exist or may not be a SODA dataset"
            ),
            column_map: None,
            source_name: source_name.to_owned(),
            domain: domain.to_owned(),
            dataset_id: dataset_id.to_owned(),
        });
    }

    // 2. Fetch a one-row sample.
    let sample_url = format!(
        "https://{domain}/resource/{dataset_id}.json?$limit=1"
    );
    let sample_text = client.fetch_json(&sample_url).await.unwrap_or_default();
    let sample_rows: Vec<Value> =
        serde_json::from_str(&sample_text).unwrap_or_default();
    let sample_row = sample_rows.into_iter().next().unwrap_or(Value::Null);

    // 3. Classify columns.
    let mut draft = ColumnMapDraft::empty();

    // Map each slot to the best matching column.
    let slots: &[(&str, fn(&ColumnMapDraft) -> bool)] = &[
        ("animal_id", |d| d.animal_id.is_some()),
        ("animal_type", |d| d.animal_type.is_some()),
        ("intake_type", |d| d.intake_type.is_some()),
        ("intake_date", |d| d.intake_date.is_some()),
        ("found_location", |d| d.found_location.is_some()),
        ("chip_status", |d| d.chip_status.is_some()),
        ("kennel_status", |d| d.kennel_status.is_some()),
        ("breed", |d| d.breed.is_some()),
        ("name", |d| d.name.is_some()),
        ("color", |d| d.color.is_some()),
        ("outcome_date", |d| d.outcome_date.is_some()),
    ];

    // Track which column field_names have been claimed.
    let mut claimed: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (slot, _) in slots {
        if let Some(col) = best_match_for_slot(slot, &columns, &claimed) {
            let confidence = determine_confidence(slot, &col, &sample_row);
            let mc = MappedColumn {
                source_col: col.field_name.clone(),
                slot,
                confidence,
            };
            claimed.insert(col.field_name.clone());
            assign_to_draft(&mut draft, slot, mc);
        }
    }

    // 4. Check intake_type stray confirmation.
    let stray_confirmed = draft
        .intake_type
        .as_ref()
        .map_or(false, |c| c.confidence == Confidence::Confirmed);

    // 5. Determine verdict.
    if draft.animal_id.is_none() {
        return Ok(ProbeResult {
            verdict: Verdict::Red,
            reason: format!(
                "no animal_id-like column found in dataset {dataset_id} on {domain}; \
                 cannot uniquely identify animals"
            ),
            column_map: Some(draft),
            source_name: source_name.to_owned(),
            domain: domain.to_owned(),
            dataset_id: dataset_id.to_owned(),
        });
    }

    if draft.animal_type.is_none() {
        return Ok(ProbeResult {
            verdict: Verdict::Red,
            reason: format!(
                "no animal_type/species column found in dataset {dataset_id}; \
                 cannot filter dogs and cats"
            ),
            column_map: Some(draft),
            source_name: source_name.to_owned(),
            domain: domain.to_owned(),
            dataset_id: dataset_id.to_owned(),
        });
    }

    if draft.intake_type.is_none() {
        return Ok(ProbeResult {
            verdict: Verdict::Red,
            reason: format!(
                "no intake_type column found in dataset {dataset_id}; \
                 this appears to be an adoptable-only feed, not a stray-intake feed"
            ),
            column_map: Some(draft),
            source_name: source_name.to_owned(),
            domain: domain.to_owned(),
            dataset_id: dataset_id.to_owned(),
        });
    }

    if !stray_confirmed {
        return Ok(ProbeResult {
            verdict: Verdict::Red,
            reason: format!(
                "intake_type column found in dataset {dataset_id} but no STRAY/FOUND value \
                 observed in the sample; this may be an adoptable-only feed, not a stray-intake \
                 feed. Column guessed: {}",
                draft.intake_type.as_ref().map_or("?", |c| &c.source_col)
            ),
            column_map: Some(draft),
            source_name: source_name.to_owned(),
            domain: domain.to_owned(),
            dataset_id: dataset_id.to_owned(),
        });
    }

    Ok(ProbeResult {
        verdict: Verdict::Green,
        reason: format!(
            "dataset {dataset_id} on {domain} has required columns \
             (animal_id, animal_type, intake_type) with confirmed STRAY values"
        ),
        column_map: Some(draft),
        source_name: source_name.to_owned(),
        domain: domain.to_owned(),
        dataset_id: dataset_id.to_owned(),
    })
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_soda_columns(json: &Value) -> Vec<SodaColumn> {
    // The columns endpoint returns an array of column objects.
    let arr = match json.as_array() {
        Some(a) => a,
        None => return vec![],
    };

    arr.iter()
        .filter_map(|col| {
            let field_name = col
                .get("fieldName")
                .or_else(|| col.get("field_name"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)?;
            // Skip system columns (starting with :)
            if field_name.starts_with(':') {
                return None;
            }
            let name = col
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&field_name)
                .to_owned();
            Some(SodaColumn { field_name, name })
        })
        .collect()
}

fn best_match_for_slot<'a>(
    slot: &'static str,
    columns: &'a [SodaColumn],
    claimed: &std::collections::HashSet<String>,
) -> Option<&'a SodaColumn> {
    let keywords = slot_keywords(slot);
    // Try keywords in order (most specific first).
    for kw in keywords {
        for col in columns {
            if claimed.contains(&col.field_name) {
                continue;
            }
            let field_lower = col.field_name.to_lowercase();
            if field_lower == *kw {
                return Some(col);
            }
        }
    }
    // Fallback: substring match.
    for kw in keywords {
        for col in columns {
            if claimed.contains(&col.field_name) {
                continue;
            }
            let field_lower = col.field_name.to_lowercase();
            let name_lower = col.name.to_lowercase();
            if field_lower.contains(kw) || name_lower.contains(kw) {
                return Some(col);
            }
        }
    }
    None
}

fn determine_confidence(
    slot: &'static str,
    col: &SodaColumn,
    sample_row: &Value,
) -> Confidence {
    if slot == "intake_type" {
        // Check if sample has a STRAY value in this column.
        if let Some(val) = sample_row.get(&col.field_name).and_then(|v| v.as_str()) {
            if is_stray_value(val) {
                return Confidence::Confirmed;
            }
        }
        // Also check values from any field with "stray" in them.
        if let Some(obj) = sample_row.as_object() {
            for (k, v) in obj {
                if k.to_lowercase().contains("intake") {
                    if let Some(s) = v.as_str() {
                        if is_stray_value(s) {
                            return Confidence::Confirmed;
                        }
                    }
                }
            }
        }
        return Confidence::Guessed;
    }
    // For non-intake_type slots, confirmed = value present in sample.
    if sample_row.get(&col.field_name).is_some() {
        Confidence::Confirmed
    } else {
        Confidence::Guessed
    }
}

fn assign_to_draft(draft: &mut ColumnMapDraft, slot: &str, mc: MappedColumn) {
    match slot {
        "animal_id"      => draft.animal_id = Some(mc),
        "animal_type"    => draft.animal_type = Some(mc),
        "intake_type"    => draft.intake_type = Some(mc),
        "intake_date"    => draft.intake_date = Some(mc),
        "found_location" => draft.found_location = Some(mc),
        "chip_status"    => draft.chip_status = Some(mc),
        "kennel_status"  => draft.kennel_status = Some(mc),
        "breed"          => draft.breed = Some(mc),
        "name"           => draft.name = Some(mc),
        "color"          => draft.color = Some(mc),
        "outcome_date"   => draft.outcome_date = Some(mc),
        _ => {}
    }
}

// ─── Output formatters ────────────────────────────────────────────────────────

/// Format a GREEN probe result as a `[[socrata]]` TOML block.
#[must_use]
pub fn format_green_toml(result: &ProbeResult) -> String {
    let draft = match &result.column_map {
        Some(d) => d,
        None => return String::new(),
    };

    let mut out = String::new();
    out.push_str("[[socrata]]\n");
    out.push_str(&format!("name = {:?}\n", result.source_name));
    out.push_str(&format!("domain = {:?}\n", result.domain));
    out.push_str(&format!("dataset_id = {:?}\n", result.dataset_id));
    out.push('\n');
    out.push_str("[socrata.column_map]\n");

    fn fmt_col(mc: &Option<MappedColumn>) -> Option<(&str, &str)> {
        mc.as_ref().map(|c| (c.source_col.as_str(), c.confidence.as_str()))
    }

    if let Some((col, conf)) = fmt_col(&draft.animal_id) {
        out.push_str(&format!("animal_id = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.animal_type) {
        out.push_str(&format!("animal_type = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.intake_type) {
        out.push_str(&format!("intake_type = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.intake_date) {
        out.push_str(&format!("intake_date = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.found_location) {
        out.push_str(&format!("found_location = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.chip_status) {
        out.push_str(&format!("chip_status = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.kennel_status) {
        out.push_str(&format!("kennel_status = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.breed) {
        out.push_str(&format!("breed = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.name) {
        out.push_str(&format!("name = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.color) {
        out.push_str(&format!("color = {:?}  # {conf}\n", col));
    }
    if let Some((col, conf)) = fmt_col(&draft.outcome_date) {
        out.push_str(&format!("outcome_date = {:?}  # {conf}\n", col));
    }

    out
}

/// Format a probe result as JSON (for --json flag).
#[must_use]
pub fn format_json(result: &ProbeResult) -> String {
    let draft_value = result.column_map.as_ref().and_then(|d| {
        if result.verdict == Verdict::Green {
            Some(serde_json::json!({
                "toml": format_green_toml(result),
            }))
        } else {
            // Even for RED, include what we found.
            let map = d.to_column_map();
            map.map(|m| serde_json::json!({
                "animal_id": m.animal_id,
                "animal_type": m.animal_type,
                "intake_type": m.intake_type,
            }))
        }
    });

    let output = JsonOutput {
        verdict: match result.verdict {
            Verdict::Green => "GREEN".to_owned(),
            Verdict::Red => "RED".to_owned(),
        },
        reason: result.reason.clone(),
        draft: draft_value,
    };

    serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_owned())
}

// ─── CLI entry point ──────────────────────────────────────────────────────────

/// Run the `probe` subcommand from CLI args.
///
/// Returns the exit code (0 = GREEN, 1 = RED, 2 = error).
pub async fn run_probe_cmd(args: &[String]) -> i32 {
    if args.len() < 2 {
        eprintln!("Usage: homeward probe <domain> <dataset_id> [--name <slug>] [--json]");
        return 2;
    }

    let domain = &args[0];
    let dataset_id = &args[1];

    let mut source_name = dataset_id.replace('-', "_");
    let mut json_output = false;
    let mut i = 2;

    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--name requires a value");
                    return 2;
                }
                source_name = args[i].clone();
            }
            "--json" => {
                json_output = true;
            }
            other => {
                eprintln!("unknown argument: {other:?}");
                return 2;
            }
        }
        i += 1;
    }

    let client = match RealProbeClient::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: could not build HTTP client: {e}");
            return 2;
        }
    };

    let result = match run_probe(&client, domain, dataset_id, &source_name).await {
        Ok(r) => r,
        Err(ProbeError::NotFound { url }) => {
            let msg = format!("dataset {dataset_id} not found: {url}");
            if json_output {
                let out = JsonOutput {
                    verdict: "RED".to_owned(),
                    reason: msg.clone(),
                    draft: None,
                };
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            } else {
                eprintln!("RED: {msg}");
            }
            return 1;
        }
        Err(e) => {
            let msg = format!("probe error for dataset {dataset_id}: {e}");
            if json_output {
                let out = JsonOutput {
                    verdict: "RED".to_owned(),
                    reason: msg.clone(),
                    draft: None,
                };
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            } else {
                eprintln!("RED: {msg}");
            }
            return 1;
        }
    };

    if json_output {
        println!("{}", format_json(&result));
    } else {
        match result.verdict {
            Verdict::Green => {
                println!("# GREEN: {}", result.reason);
                println!();
                println!("{}", format_green_toml(&result));
            }
            Verdict::Red => {
                eprintln!("RED: {}", result.reason);
            }
        }
    }

    match result.verdict {
        Verdict::Green => 0,
        Verdict::Red => 1,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    //! Unit tests for the probe module — all use fixture-backed HTTP (no live network).
    use super::*;
    use std::collections::HashMap;

    /// Fixture-backed probe client for tests.
    pub struct FixtureClient {
        /// Map from URL substring to JSON response body.
        responses: HashMap<String, String>,
    }

    impl FixtureClient {
        /// Create a new fixture client with no responses registered.
        pub fn new() -> Self {
            Self {
                responses: HashMap::new(),
            }
        }

        /// Register a fixture: any URL containing `url_contains` returns `body`.
        pub fn add(&mut self, url_contains: &str, body: &str) {
            self.responses.insert(url_contains.to_owned(), body.to_owned());
        }
    }

    #[async_trait::async_trait]
    impl ProbeClient for FixtureClient {
        async fn fetch_json(&self, url: &str) -> Result<String, ProbeError> {
            for (key, body) in &self.responses {
                if url.contains(key.as_str()) {
                    return Ok(body.clone());
                }
            }
            Err(ProbeError::NotFound { url: url.to_owned() })
        }
    }

    fn austin_columns_json() -> String {
        // Realistic Austin animal shelter column metadata.
        serde_json::json!([
            {"fieldName": "animal_id", "name": "Animal ID", "dataTypeName": "text"},
            {"fieldName": "name", "name": "Name", "dataTypeName": "text"},
            {"fieldName": "datetime", "name": "DateTime", "dataTypeName": "calendar_date"},
            {"fieldName": "monthyear", "name": "MonthYear", "dataTypeName": "text"},
            {"fieldName": "found_location", "name": "Found Location", "dataTypeName": "text"},
            {"fieldName": "intake_type", "name": "Intake Type", "dataTypeName": "text"},
            {"fieldName": "intake_condition", "name": "Intake Condition", "dataTypeName": "text"},
            {"fieldName": "animal_type", "name": "Animal Type", "dataTypeName": "text"},
            {"fieldName": "sex_upon_intake", "name": "Sex upon Intake", "dataTypeName": "text"},
            {"fieldName": "age_upon_intake", "name": "Age upon Intake", "dataTypeName": "text"},
            {"fieldName": "breed", "name": "Breed", "dataTypeName": "text"},
            {"fieldName": "color", "name": "Color", "dataTypeName": "text"},
        ])
        .to_string()
    }

    fn austin_sample_json() -> String {
        // One-row sample with STRAY value.
        serde_json::json!([{
            "animal_id": "A786884",
            "name": "Buddy",
            "datetime": "2024-01-15T10:00:00.000",
            "monthyear": "January 2024",
            "found_location": "4521 Loyola Ln in Austin (TX)",
            "intake_type": "Stray",
            "intake_condition": "Normal",
            "animal_type": "Dog",
            "sex_upon_intake": "Neutered Male",
            "age_upon_intake": "3 years",
            "breed": "Labrador Retriever Mix",
            "color": "Black/White",
        }])
        .to_string()
    }

    fn no_stray_columns_json() -> String {
        // Dataset with an intake_type column but no STRAY values in sample.
        serde_json::json!([
            {"fieldName": "animal_id", "name": "Animal ID"},
            {"fieldName": "animal_type", "name": "Animal Type"},
            {"fieldName": "intake_type", "name": "Intake Type"},
        ])
        .to_string()
    }

    fn no_stray_sample_json() -> String {
        serde_json::json!([{
            "animal_id": "X001",
            "animal_type": "Dog",
            "intake_type": "Owner Surrender",
        }])
        .to_string()
    }

    fn no_animal_id_columns_json() -> String {
        // Dataset missing any animal_id-like column.
        serde_json::json!([
            {"fieldName": "animal_type", "name": "Animal Type"},
            {"fieldName": "intake_type", "name": "Intake Type"},
            {"fieldName": "breed", "name": "Breed"},
        ])
        .to_string()
    }

    fn no_animal_id_sample_json() -> String {
        serde_json::json!([{
            "animal_type": "Dog",
            "intake_type": "Stray",
            "breed": "Lab Mix",
        }])
        .to_string()
    }

    #[tokio::test]
    async fn ac1_austin_fixture_green_required_columns_match() {
        let mut client = FixtureClient::new();
        client.add("columns.json", &austin_columns_json());
        client.add("resource/fdzn-9yqv.json", &austin_sample_json());

        let result = run_probe(&client, "data.austintexas.gov", "fdzn-9yqv", "austin")
            .await
            .expect("probe should succeed");

        assert_eq!(result.verdict, Verdict::Green, "expected GREEN: {}", result.reason);

        let draft = result.column_map.as_ref().expect("should have column map");

        // AC1: required mappings match hand-authored SocrataConfig::austin().
        let builtin = crate::connectors::socrata::SocrataConfig::austin();
        assert_eq!(
            draft.animal_id.as_ref().map(|c| c.source_col.as_str()),
            Some(builtin.column_map.animal_id.as_str()),
            "animal_id mismatch"
        );
        assert_eq!(
            draft.animal_type.as_ref().map(|c| c.source_col.as_str()),
            Some(builtin.column_map.animal_type.as_str()),
            "animal_type mismatch"
        );
        assert_eq!(
            draft.intake_type.as_ref().map(|c| c.source_col.as_str()),
            Some(builtin.column_map.intake_type.as_str()),
            "intake_type mismatch"
        );
    }

    #[tokio::test]
    async fn ac2_green_output_parses_via_source_catalog() {
        use crate::catalog::load_catalog;
        use std::io::Write;

        let mut client = FixtureClient::new();
        client.add("columns.json", &austin_columns_json());
        client.add("resource/fdzn-9yqv.json", &austin_sample_json());

        let result = run_probe(&client, "data.austintexas.gov", "fdzn-9yqv", "austin")
            .await
            .expect("probe should succeed");

        assert_eq!(result.verdict, Verdict::Green);

        // Generate TOML and parse it through SourceCatalog::from_path.
        let toml_text = format_green_toml(&result);
        // The TOML block has inline comments (# confirmed) — strip them for parsing.
        let clean_toml: String = toml_text
            .lines()
            .map(|line| {
                // Remove inline # comments from value lines.
                if let Some(idx) = line.find("  #") {
                    let (left, _) = line.split_at(idx);
                    left.to_owned()
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut f = tempfile::NamedTempFile::new().expect("tempfile");
        f.write_all(clean_toml.as_bytes()).expect("write");
        let path = f.path().to_owned();

        let catalog = load_catalog(&path).expect("load_catalog should parse GREEN output");
        assert_eq!(catalog.socrata.len(), 1, "should produce exactly one [[socrata]] config");
        assert_eq!(catalog.socrata[0].name, "austin");
        assert!(!catalog.socrata[0].column_map.animal_id.is_empty());
    }

    #[tokio::test]
    async fn ac3_no_stray_fixture_yields_red() {
        let mut client = FixtureClient::new();
        client.add("columns.json", &no_stray_columns_json());
        client.add("resource/", &no_stray_sample_json());

        let result = run_probe(&client, "data.example.gov", "xxxx-1234", "test")
            .await
            .expect("probe should not error");

        assert_eq!(result.verdict, Verdict::Red, "expected RED");
        assert!(
            result.reason.contains("STRAY") || result.reason.contains("stray"),
            "reason should mention STRAY signal: {}",
            result.reason
        );
    }

    #[tokio::test]
    async fn ac3_missing_animal_id_yields_red_distinct_message() {
        let mut client = FixtureClient::new();
        client.add("columns.json", &no_animal_id_columns_json());
        client.add("resource/", &no_animal_id_sample_json());

        let result = run_probe(&client, "data.example.gov", "yyyy-5678", "test")
            .await
            .expect("probe should not error");

        assert_eq!(result.verdict, Verdict::Red, "expected RED for missing animal_id");
        assert!(
            result.reason.contains("animal_id"),
            "reason should mention animal_id: {}",
            result.reason
        );
        // Distinct message from no-STRAY case.
        assert!(
            !result.reason.contains("adoptable-only"),
            "should not say adoptable-only for missing animal_id: {}",
            result.reason
        );
    }

    #[tokio::test]
    async fn ac4_intake_type_confirmed_only_when_stray_observed() {
        // GREEN: stray observed → confirmed.
        let mut client = FixtureClient::new();
        client.add("columns.json", &austin_columns_json());
        client.add("resource/fdzn-9yqv.json", &austin_sample_json());

        let result = run_probe(&client, "data.austintexas.gov", "fdzn-9yqv", "austin")
            .await
            .expect("probe should succeed");

        let draft = result.column_map.as_ref().unwrap();
        assert_eq!(
            draft.intake_type.as_ref().map(|c| &c.confidence),
            Some(&Confidence::Confirmed),
            "intake_type should be confirmed when STRAY observed"
        );

        // No-stray: intake_type found but not confirmed (guessed).
        let mut client2 = FixtureClient::new();
        client2.add("columns.json", &no_stray_columns_json());
        client2.add("resource/", &no_stray_sample_json());

        let result2 = run_probe(&client2, "data.example.gov", "xxxx-1234", "test")
            .await
            .expect("probe should not error");

        if let Some(draft2) = &result2.column_map {
            if let Some(it) = &draft2.intake_type {
                assert_eq!(
                    it.confidence,
                    Confidence::Guessed,
                    "intake_type should be guessed when no STRAY observed"
                );
            }
        }
    }

    #[tokio::test]
    async fn ac5_404_yields_typed_error_not_panic() {
        // FixtureClient with no matching entries → NotFound.
        let client = FixtureClient::new();
        let err = run_probe(&client, "data.example.gov", "notexist-0000", "test")
            .await;
        // Either an Err(ProbeError::NotFound) or a RED verdict (we handle both).
        match err {
            Err(ProbeError::NotFound { url }) => {
                assert!(url.contains("notexist-0000"), "error should include dataset id: {url}");
            }
            Ok(result) => {
                // Probe returned a RED result (empty columns path).
                assert_eq!(result.verdict, Verdict::Red);
            }
            Err(e) => {
                panic!("unexpected error: {e}");
            }
        }
    }

    #[tokio::test]
    async fn ac5_json_flag_emits_structured_output() {
        let mut client = FixtureClient::new();
        client.add("columns.json", &austin_columns_json());
        client.add("resource/fdzn-9yqv.json", &austin_sample_json());

        let result = run_probe(&client, "data.austintexas.gov", "fdzn-9yqv", "austin")
            .await
            .expect("probe should succeed");

        let json_str = format_json(&result);
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("json output should parse");

        assert!(parsed.get("verdict").is_some(), "json should have verdict");
        assert!(parsed.get("reason").is_some(), "json should have reason");
        assert!(parsed.get("draft").is_some(), "json should have draft");
        assert_eq!(
            parsed["verdict"].as_str(),
            Some("GREEN"),
            "verdict should be GREEN"
        );
    }

    #[test]
    fn is_stray_value_matches_known_values() {
        assert!(is_stray_value("STRAY"));
        assert!(is_stray_value("Stray"));
        assert!(is_stray_value("stray incoming"));
        assert!(is_stray_value("FOUND"));
        assert!(!is_stray_value("Owner Surrender"));
        assert!(!is_stray_value("Transfer"));
    }
}
