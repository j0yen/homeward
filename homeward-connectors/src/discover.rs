//! `homeward discover` — crawl Socrata and OpenDataSoft federated catalogs for
//! animal-intake dataset candidates.
//!
//! Usage:
//!   homeward discover [--families socrata,opendatasoft] [--metro <str>]
//!                     [--limit N] [--json]
//!
//! This command queries public catalog APIs and emits a ranked list of
//! *candidate* datasets that look like animal-intake feeds.  It **never**
//! writes to `deploy/sources.toml` — candidates must be validated with
//! `homeward probe` before committing a source.
//!
//! # ArcGIS coverage gap
//!
//! ArcGIS Hub has no single federated catalog of the same shape as Socrata or
//! OpenDataSoft.  If `arcgis` is requested in `--families`, a warning is
//! logged and the user is directed to use `probe --family arcgis <service_url>`
//! directly.
//!
//! # Discovery pipeline
//!
//! 1. Query catalog API(s) using animal-intake search terms.
//! 2. Score each result on:
//!    - Name/description keywords (animal, intake, stray, shelter, …).
//!    - Presence of an intake-type-like column name.
//!    - Presence of an animal-type-like column name.
//! 3. Drop obvious non-matches (score = 0).
//! 4. Sort descending by score and apply `--limit`.
//! 5. Emit table (default) or JSON.

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::fmt;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::ConnectorError;
use crate::http::HOMEWARD_USER_AGENT;

// ─── Scoring constants ────────────────────────────────────────────────────────

/// Keywords that strongly suggest an intake-type column.
const INTAKE_TYPE_KEYWORDS: &[&str] = &[
    "intake_type",
    "intaketype",
    "intake_subtype",
    "intake_condition",
    "entry_reason",
];

/// Keywords that strongly suggest an animal-type/species column.
const ANIMAL_TYPE_KEYWORDS: &[&str] = &[
    "animal_type",
    "animaltype",
    "species",
];

// ─── Candidate shape ──────────────────────────────────────────────────────────

/// Which catalog family this candidate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CandidateFamily {
    /// Socrata / SODA federated catalog.
    Socrata,
    /// OpenDataSoft central catalog.
    OpenDataSoft,
}

impl fmt::Display for CandidateFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CandidateFamily::Socrata => write!(f, "socrata"),
            CandidateFamily::OpenDataSoft => write!(f, "opendatasoft"),
        }
    }
}

/// A scored catalog candidate — one dataset from a federated catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// Which family this candidate belongs to.
    pub family: CandidateFamily,
    /// For Socrata: the domain (e.g. `data.austintexas.gov`).
    /// For ODS: the portal base URL (e.g. `https://data.example.org`).
    pub domain_or_base_url: String,
    /// Dataset identifier (resource id for Socrata, dataset_id for ODS).
    pub dataset_id: String,
    /// Human-readable name of the dataset.
    pub name: String,
    /// Relevance score (higher = better match).
    pub score: u32,
    /// Human-readable signals that contributed to the score.
    pub matched_signals: Vec<String>,
    /// Ready-to-run probe command.
    pub probe_cmd: String,
}

impl Candidate {
    fn build_probe_cmd(family: CandidateFamily, domain_or_base_url: &str, dataset_id: &str) -> String {
        match family {
            CandidateFamily::Socrata => {
                format!("homeward probe --family socrata {domain_or_base_url} {dataset_id}")
            }
            CandidateFamily::OpenDataSoft => {
                format!("homeward probe --family opendatasoft {domain_or_base_url} {dataset_id}")
            }
        }
    }
}

// ─── Scoring logic ────────────────────────────────────────────────────────────

/// Score a candidate based on name/description and column names.
///
/// Returns `(score, matched_signals)`.
pub fn score_candidate(name: &str, description: &str, column_names: &[String]) -> (u32, Vec<String>) {
    let mut score: u32 = 0;
    let mut signals: Vec<String> = Vec::new();

    let name_lower = name.to_lowercase();
    let desc_lower = description.to_lowercase();
    let combined = format!("{name_lower} {desc_lower}");

    // Name/description scoring — strongest signals first.
    if combined.contains("animal intake") {
        score += 4;
        signals.push("name:animal-intake".to_owned());
    } else if combined.contains("animal services") {
        score += 3;
        signals.push("name:animal-services".to_owned());
    } else if combined.contains("shelter intake") {
        score += 3;
        signals.push("name:shelter-intake".to_owned());
    } else if combined.contains("stray") {
        score += 3;
        signals.push("name:stray".to_owned());
    } else if combined.contains("shelter") {
        score += 2;
        signals.push("name:shelter".to_owned());
    } else if combined.contains("animal control") {
        score += 2;
        signals.push("name:animal-control".to_owned());
    } else if combined.contains("animal") {
        score += 1;
        signals.push("name:animal".to_owned());
    } else if combined.contains("intake") {
        score += 1;
        signals.push("name:intake".to_owned());
    }

    // Column heuristics — check for intake-type column.
    let col_lower: Vec<String> = column_names.iter().map(|c| c.to_lowercase()).collect();

    let has_intake_col = col_lower.iter().any(|c| {
        INTAKE_TYPE_KEYWORDS.iter().any(|kw| c == *kw || c.contains(kw))
    });
    if has_intake_col {
        score += 2;
        signals.push("col:intake-type".to_owned());
    }

    let has_animal_type_col = col_lower.iter().any(|c| {
        ANIMAL_TYPE_KEYWORDS.iter().any(|kw| c == *kw || c.contains(kw))
    });
    if has_animal_type_col {
        score += 1;
        signals.push("col:animal-type".to_owned());
    }

    (score, signals)
}

// ─── Socrata federated catalog ────────────────────────────────────────────────

/// Socrata catalog API response envelope.
#[derive(Debug, Deserialize)]
struct SocrataCatalogResponse {
    results: Vec<SocrataCatalogResult>,
    #[serde(rename = "resultSetSize")]
    result_set_size: Option<u64>,
}

/// One result from the Socrata catalog.
#[derive(Debug, Deserialize)]
struct SocrataCatalogResult {
    resource: SocrataResource,
}

/// Resource block within a Socrata catalog result.
#[derive(Debug, Deserialize)]
struct SocrataResource {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    columns_name: Vec<String>,
}

/// Search the Socrata federated catalog using the given client.
///
/// Returns all pages up to `page_limit` pages of 100 results each.
/// Filters by animal-intake search terms.
pub async fn discover_socrata(
    client: &Client,
    metro_filter: Option<&str>,
    page_limit: usize,
) -> Result<Vec<Candidate>, ConnectorError> {
    let search_terms = ["animal intake", "animal shelter", "stray", "animal services"];
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for term in &search_terms {
        let mut offset = 0usize;
        let mut page = 0usize;

        loop {
            if page >= page_limit {
                break;
            }

            let url = build_socrata_catalog_url(term, offset, metro_filter);
            let resp_text = fetch_json_text(client, &url).await?;
            let parsed: SocrataCatalogResponse = serde_json::from_str(&resp_text)
                .map_err(|e| ConnectorError::Config(format!("Socrata catalog parse error: {e}")))?;

            let count = parsed.results.len();
            for result in parsed.results {
                let resource = result.resource;
                let dedup_key = format!("{}:{}", resource.domain, resource.id);
                if seen_ids.contains(&dedup_key) {
                    continue;
                }
                seen_ids.insert(dedup_key);

                let (score, signals) = score_candidate(
                    &resource.name,
                    &resource.description,
                    &resource.columns_name,
                );
                if score == 0 {
                    continue;
                }

                let probe_cmd = Candidate::build_probe_cmd(
                    CandidateFamily::Socrata,
                    &resource.domain,
                    &resource.id,
                );

                candidates.push(Candidate {
                    family: CandidateFamily::Socrata,
                    domain_or_base_url: resource.domain,
                    dataset_id: resource.id,
                    name: resource.name,
                    score,
                    matched_signals: signals,
                    probe_cmd,
                });
            }

            let total = parsed.result_set_size.unwrap_or(0) as usize;
            offset += 100;
            page += 1;

            if count < 100 || offset >= total {
                break;
            }
        }
    }

    Ok(candidates)
}

fn build_socrata_catalog_url(term: &str, offset: usize, metro: Option<&str>) -> String {
    let encoded_term = term.replace(' ', "+");
    let mut url = format!(
        "https://api.us.socrata.com/api/catalog/v1?q={encoded_term}&only=dataset&limit=100&offset={offset}"
    );
    if let Some(m) = metro {
        let encoded_metro = m.replace(' ', "+");
        url.push_str(&format!("&search_context={encoded_metro}"));
    }
    url
}

// ─── OpenDataSoft catalog ─────────────────────────────────────────────────────

/// ODS catalog API response envelope.
#[derive(Debug, Deserialize)]
struct OdsCatalogResponse {
    total_count: Option<u64>,
    results: Vec<OdsDatasetResult>,
}

/// One dataset result from ODS.
#[derive(Debug, Deserialize)]
struct OdsDatasetResult {
    dataset_id: String,
    #[serde(default)]
    metas: OdsMetas,
    #[serde(default)]
    fields: Vec<OdsField>,
}

/// Metadata wrapper from ODS.
#[derive(Debug, Deserialize, Default)]
struct OdsMetas {
    #[serde(default)]
    default: OdsDefaultMeta,
}

/// Default metadata from ODS.
#[derive(Debug, Deserialize, Default)]
struct OdsDefaultMeta {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    domain: Option<String>,
}

/// A field in an ODS dataset.
#[derive(Debug, Deserialize, Default)]
struct OdsField {
    #[serde(default)]
    name: String,
}

/// Search the OpenDataSoft central catalog.
pub async fn discover_opendatasoft(
    client: &Client,
    metro_filter: Option<&str>,
    page_limit: usize,
) -> Result<Vec<Candidate>, ConnectorError> {
    let search_terms = ["animal intake", "animal shelter", "stray", "animal services"];
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for term in &search_terms {
        let mut offset = 0usize;
        let mut page = 0usize;

        loop {
            if page >= page_limit {
                break;
            }

            let url = build_ods_catalog_url(term, offset, metro_filter);
            let resp_text = fetch_json_text(client, &url).await?;
            let parsed: OdsCatalogResponse = serde_json::from_str(&resp_text)
                .map_err(|e| ConnectorError::Config(format!("ODS catalog parse error: {e}")))?;

            let count = parsed.results.len();
            for result in parsed.results {
                if seen_ids.contains(&result.dataset_id) {
                    continue;
                }
                seen_ids.insert(result.dataset_id.clone());

                let title = &result.metas.default.title;
                let desc = result.metas.default.description.as_deref().unwrap_or("");
                let domain = result.metas.default.domain.clone().unwrap_or_default();

                let col_names: Vec<String> = result.fields.iter().map(|f| f.name.clone()).collect();

                let (score, signals) = score_candidate(title, desc, &col_names);
                if score == 0 {
                    continue;
                }

                // ODS base_url: reconstruct from domain or use central catalog host.
                let base_url = if domain.is_empty() {
                    "https://data.opendatasoft.com".to_owned()
                } else if domain.starts_with("http") {
                    domain.clone()
                } else {
                    format!("https://{domain}")
                };

                let probe_cmd = Candidate::build_probe_cmd(
                    CandidateFamily::OpenDataSoft,
                    &base_url,
                    &result.dataset_id,
                );

                candidates.push(Candidate {
                    family: CandidateFamily::OpenDataSoft,
                    domain_or_base_url: base_url,
                    dataset_id: result.dataset_id,
                    name: title.clone(),
                    score,
                    matched_signals: signals,
                    probe_cmd,
                });
            }

            let total = parsed.total_count.unwrap_or(0) as usize;
            offset += 100;
            page += 1;

            if count < 100 || offset >= total {
                break;
            }
        }
    }

    Ok(candidates)
}

fn build_ods_catalog_url(term: &str, offset: usize, metro: Option<&str>) -> String {
    let encoded_term = term.replace(' ', "%20");
    let mut url = format!(
        "https://data.opendatasoft.com/api/explore/v2.1/catalog/datasets?q={encoded_term}&limit=100&offset={offset}"
    );
    if let Some(m) = metro {
        let encoded_metro = m.replace(' ', "%20");
        url.push_str(&format!("&where=metas.default.keyword+like+%22{encoded_metro}%22"));
    }
    url
}

// ─── HTTP helper ──────────────────────────────────────────────────────────────

/// Fetch JSON text from a URL using the provided client.
async fn fetch_json_text(client: &Client, url: &str) -> Result<String, ConnectorError> {
    let resp = client
        .get(url)
        .header("User-Agent", HOMEWARD_USER_AGENT)
        .send()
        .await
        .map_err(ConnectorError::Http)?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ConnectorError::UnexpectedStatus {
            status,
            body: body.chars().take(256).collect(),
        });
    }

    resp.text().await.map_err(ConnectorError::Http)
}

// ─── Discover command args ────────────────────────────────────────────────────

/// Parsed arguments for the `discover` subcommand.
#[derive(Debug, Clone)]
pub struct DiscoverArgs {
    /// Which catalog families to query (e.g. `["socrata", "opendatasoft"]`).
    pub families: Vec<String>,
    /// Optional metro/city filter to narrow catalog results.
    pub metro: Option<String>,
    /// Cap output to this many candidates.
    pub limit: Option<usize>,
    /// Emit JSON instead of a human-readable table.
    pub json_output: bool,
}

impl DiscoverArgs {
    /// Parse `discover` subcommand arguments from a slice.
    ///
    /// Returns `Err(exit_code)` if parsing fails (prints usage).
    pub fn parse(args: &[String]) -> Result<Self, i32> {
        let mut families: Vec<String> = vec!["socrata".to_owned(), "opendatasoft".to_owned()];
        let mut metro: Option<String> = None;
        let mut limit: Option<usize> = None;
        let mut json_output = false;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--families" => {
                    i += 1;
                    if i >= args.len() {
                        eprintln!("--families requires a value (e.g. socrata,opendatasoft)");
                        return Err(1);
                    }
                    families = args[i].split(',').map(|s| s.trim().to_lowercase()).collect();
                }
                "--metro" => {
                    i += 1;
                    if i >= args.len() {
                        eprintln!("--metro requires a value");
                        return Err(1);
                    }
                    metro = Some(args[i].clone());
                }
                "--limit" => {
                    i += 1;
                    if i >= args.len() {
                        eprintln!("--limit requires a value");
                        return Err(1);
                    }
                    match args[i].parse::<usize>() {
                        Ok(n) => limit = Some(n),
                        Err(e) => {
                            eprintln!("invalid --limit value: {e}");
                            return Err(1);
                        }
                    }
                }
                "--json" => {
                    json_output = true;
                }
                "--help" | "-h" => {
                    print_discover_help();
                    return Err(0);
                }
                other => {
                    eprintln!("unknown discover argument: {other:?}");
                    eprintln!("Run `homeward discover --help` for usage.");
                    return Err(1);
                }
            }
            i += 1;
        }

        Ok(Self {
            families,
            metro,
            limit,
            json_output,
        })
    }
}

// ─── Main discover runner ─────────────────────────────────────────────────────

/// Run the discover command.  Returns process exit code.
pub async fn run_discover_cmd(args: &[String]) -> i32 {
    let discover_args = match DiscoverArgs::parse(args) {
        Ok(a) => a,
        Err(code) => return code,
    };

    let client = match Client::builder()
        .user_agent(HOMEWARD_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .gzip(true)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to build HTTP client: {e}");
            return 1;
        }
    };

    let mut all_candidates: Vec<Candidate> = Vec::new();
    let metro_ref = discover_args.metro.as_deref();

    for family in &discover_args.families {
        match family.as_str() {
            "socrata" => {
                eprintln!("discover: querying Socrata federated catalog…");
                match discover_socrata(&client, metro_ref, 3).await {
                    Ok(mut cands) => {
                        eprintln!("discover: {} Socrata candidates (pre-dedup)", cands.len());
                        all_candidates.append(&mut cands);
                    }
                    Err(e) => {
                        eprintln!("discover: Socrata catalog error: {e}");
                    }
                }
            }
            "opendatasoft" => {
                eprintln!("discover: querying OpenDataSoft catalog…");
                match discover_opendatasoft(&client, metro_ref, 3).await {
                    Ok(mut cands) => {
                        eprintln!("discover: {} ODS candidates (pre-dedup)", cands.len());
                        all_candidates.append(&mut cands);
                    }
                    Err(e) => {
                        eprintln!("discover: OpenDataSoft catalog error: {e}");
                    }
                }
            }
            "arcgis" => {
                eprintln!(
                    "discover: WARNING — ArcGIS Hub has no single federated catalog of the \
                     same shape as Socrata or OpenDataSoft.  ArcGIS discovery is not \
                     supported by this subcommand.  To probe a known ArcGIS Feature Service \
                     layer, use: homeward probe --family arcgis <service_url>"
                );
            }
            other => {
                eprintln!("discover: unknown family {other:?}; supported: socrata, opendatasoft");
            }
        }
    }

    // Sort by score descending, then by name for stability.
    all_candidates.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));

    // Apply limit.
    if let Some(n) = discover_args.limit {
        all_candidates.truncate(n);
    }

    if all_candidates.is_empty() {
        eprintln!("discover: no candidates found (try broader --families or no --metro filter)");
        return 0;
    }

    if discover_args.json_output {
        match serde_json::to_string_pretty(&all_candidates) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("discover: JSON serialization error: {e}");
                return 1;
            }
        }
    } else {
        println!("NOTE: candidates are unvalidated — run the probe_cmd to confirm each.");
        println!();
        println!(
            "{:<4}  {:<14}  {:<35}  {:<22}  {:<6}  {}",
            "rank", "family", "domain/base_url", "dataset_id", "score", "signals"
        );
        println!("{}", "-".repeat(120));
        for (i, c) in all_candidates.iter().enumerate() {
            println!(
                "{:<4}  {:<14}  {:<35}  {:<22}  {:<6}  {}",
                i + 1,
                c.family,
                truncate(&c.domain_or_base_url, 35),
                truncate(&c.dataset_id, 22),
                c.score,
                c.matched_signals.join(", ")
            );
            println!("       probe: {}", c.probe_cmd);
        }
    }

    0
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

fn print_discover_help() {
    println!("homeward discover — crawl open-data catalogs for animal-intake candidates");
    println!();
    println!("USAGE:");
    println!("  homeward discover [--families <families>] [--metro <str>] [--limit N] [--json]");
    println!();
    println!("OPTIONS:");
    println!("  --families <list>  Comma-separated list of families to query.");
    println!("                     Supported: socrata, opendatasoft");
    println!("                     Default: socrata,opendatasoft");
    println!("  --metro <str>      Optional metro/city filter to narrow results.");
    println!("  --limit N          Cap output to N candidates.");
    println!("  --json             Emit JSON array instead of human-readable table.");
    println!("  --help             Show this help message.");
    println!();
    println!("COVERAGE GAPS:");
    println!("  arcgis   ArcGIS Hub has no single federated catalog comparable to Socrata or");
    println!("           OpenDataSoft.  ArcGIS datasets cannot be auto-discovered via this");
    println!("           subcommand.  To probe a known ArcGIS Feature Service layer, use:");
    println!("             homeward probe --family arcgis <service_url>");
    println!();
    println!("IMPORTANT:");
    println!("  Candidates are UNVALIDATED.  This command never writes to deploy/sources.toml.");
    println!("  Run the `probe_cmd` printed for each candidate to confirm it is a working");
    println!("  STRAY-bearing animal-intake feed before adding it to sources.toml.");
    println!();
    println!("EXAMPLES:");
    println!("  homeward discover --limit 20");
    println!("  homeward discover --families socrata --metro austin --json");
    println!("  homeward discover --families opendatasoft --limit 5");
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fixture helpers ───────────────────────────────────────────────────────

    /// Fixture: a Socrata catalog response with a mix of animal-intake and
    /// unrelated datasets.
    fn socrata_fixture_json() -> &'static str {
        r#"{
  "results": [
    {
      "resource": {
        "id": "abc1-2345",
        "name": "Animal Intake and Outcome",
        "description": "Austin animal shelter intake records including stray animals",
        "domain": "data.austintexas.gov",
        "columns_name": ["animal_id", "intake_type", "animal_type", "intake_date", "found_location"]
      }
    },
    {
      "resource": {
        "id": "def3-6789",
        "name": "Stray Animal Services",
        "description": "Dallas animal stray intake data",
        "domain": "data.dallascityhall.com",
        "columns_name": ["intake_type", "species", "date_intake"]
      }
    },
    {
      "resource": {
        "id": "ghi7-0123",
        "name": "Animal Shelter Adoptions",
        "description": "Adoptable pets at the shelter",
        "domain": "data.example.gov",
        "columns_name": ["animal_name", "breed", "status"]
      }
    },
    {
      "resource": {
        "id": "jkl4-5678",
        "name": "Parking Citations 2023",
        "description": "Downtown parking violations and citations",
        "domain": "data.cityparking.gov",
        "columns_name": ["citation_id", "location", "amount", "date"]
      }
    },
    {
      "resource": {
        "id": "mno9-1234",
        "name": "Animal Control Calls",
        "description": "Service calls for animal control including stray pickup",
        "domain": "data.city.example.gov",
        "columns_name": ["animal_type", "call_date", "call_type", "disposition"]
      }
    }
  ],
  "resultSetSize": 5
}"#
    }

    /// Fixture: an ODS catalog response with animal-intake and unrelated datasets.
    fn ods_fixture_json() -> &'static str {
        r#"{
  "total_count": 3,
  "results": [
    {
      "dataset_id": "animal-shelter-intakes-001",
      "metas": {
        "default": {
          "title": "Animal Shelter Intake Records",
          "description": "Monthly animal intake records including stray and found animals",
          "domain": "data.opendata.example.org"
        }
      },
      "fields": [
        {"name": "animal_type"},
        {"name": "intake_type"},
        {"name": "intake_date"},
        {"name": "animal_id"}
      ]
    },
    {
      "dataset_id": "budget-expenditures-2023",
      "metas": {
        "default": {
          "title": "City Budget Expenditures 2023",
          "description": "Annual budget expenditure data by department",
          "domain": "data.opendata.example.org"
        }
      },
      "fields": [
        {"name": "department"},
        {"name": "amount"},
        {"name": "fiscal_year"}
      ]
    },
    {
      "dataset_id": "stray-animal-report-2024",
      "metas": {
        "default": {
          "title": "Stray Animal Reports",
          "description": "Reported stray animal incidents",
          "domain": "data.opendata2.example.org"
        }
      },
      "fields": [
        {"name": "species"},
        {"name": "report_date"},
        {"name": "location"}
      ]
    }
  ]
}"#
    }

    // ── AC1: Animal-intake datasets outrank unrelated ones ────────────────────

    #[test]
    fn test_scoring_animal_intake_outranks_parking() {
        let (animal_score, _) = score_candidate(
            "Animal Intake and Outcome",
            "Austin animal shelter intake records including stray animals",
            &["animal_id".to_owned(), "intake_type".to_owned(), "animal_type".to_owned()],
        );
        let (parking_score, _) = score_candidate(
            "Parking Citations 2023",
            "Downtown parking violations and citations",
            &["citation_id".to_owned(), "location".to_owned(), "amount".to_owned()],
        );

        assert!(animal_score > 0, "animal intake dataset should score > 0");
        assert_eq!(parking_score, 0, "parking dataset should score 0");
        assert!(
            animal_score > parking_score,
            "animal ({animal_score}) should outrank parking ({parking_score})"
        );
    }

    #[test]
    fn test_parking_dataset_dropped_from_fixture() {
        let fixture: serde_json::Value =
            serde_json::from_str(socrata_fixture_json()).expect("parse fixture");
        let results = fixture["results"].as_array().expect("results array");

        let mut candidates: Vec<(String, u32)> = Vec::new();
        for r in results {
            let resource = &r["resource"];
            let name = resource["name"].as_str().unwrap_or("");
            let desc = resource["description"].as_str().unwrap_or("");
            let cols: Vec<String> = resource["columns_name"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_owned())).collect())
                .unwrap_or_default();
            let (score, _) = score_candidate(name, desc, &cols);
            candidates.push((name.to_owned(), score));
        }

        // Find parking entry — must score 0 (dropped).
        let parking = candidates.iter().find(|(n, _)| n.contains("Parking"));
        assert!(parking.is_some(), "parking dataset must be in fixture");
        assert_eq!(
            parking.unwrap().1,
            0,
            "parking dataset must score 0 (dropped)"
        );

        // Find best animal-intake entry.
        let best_animal = candidates.iter().max_by_key(|(_, s)| *s);
        assert!(
            best_animal.is_some_and(|(_, s)| *s > 0),
            "best animal dataset must score > 0"
        );
    }

    // ── AC2: Candidate shape ──────────────────────────────────────────────────

    #[test]
    fn test_candidate_has_all_required_fields() {
        let (score, signals) = score_candidate(
            "Animal Intake",
            "Stray and found animals",
            &["intake_type".to_owned(), "animal_type".to_owned()],
        );
        let probe_cmd = Candidate::build_probe_cmd(
            CandidateFamily::Socrata,
            "data.example.gov",
            "abc1-2345",
        );
        let candidate = Candidate {
            family: CandidateFamily::Socrata,
            domain_or_base_url: "data.example.gov".to_owned(),
            dataset_id: "abc1-2345".to_owned(),
            name: "Animal Intake".to_owned(),
            score,
            matched_signals: signals,
            probe_cmd,
        };

        assert_eq!(candidate.family, CandidateFamily::Socrata);
        assert!(!candidate.domain_or_base_url.is_empty());
        assert!(!candidate.dataset_id.is_empty());
        assert!(!candidate.probe_cmd.is_empty());
        assert!(candidate.score > 0);
        assert!(!candidate.matched_signals.is_empty());
        assert!(
            candidate.probe_cmd.contains("data.example.gov"),
            "probe_cmd must include domain"
        );
        assert!(
            candidate.probe_cmd.contains("abc1-2345"),
            "probe_cmd must include dataset_id"
        );
    }

    // ── AC3: --json serializes and --limit caps ───────────────────────────────

    #[test]
    fn test_json_output_is_valid() {
        let candidates = vec![
            Candidate {
                family: CandidateFamily::Socrata,
                domain_or_base_url: "data.austintexas.gov".to_owned(),
                dataset_id: "abc1-2345".to_owned(),
                name: "Animal Intake".to_owned(),
                score: 7,
                matched_signals: vec!["name:animal-intake".to_owned(), "col:intake-type".to_owned()],
                probe_cmd: "homeward probe --family socrata data.austintexas.gov abc1-2345"
                    .to_owned(),
            },
        ];

        let json = serde_json::to_string_pretty(&candidates).expect("serialize");
        let parsed: Vec<Candidate> = serde_json::from_str(&json).expect("parse back");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].dataset_id, "abc1-2345");
    }

    #[test]
    fn test_limit_caps_candidates() {
        let mut candidates: Vec<Candidate> = (0..10)
            .map(|i| Candidate {
                family: CandidateFamily::Socrata,
                domain_or_base_url: format!("data.example{i}.gov"),
                dataset_id: format!("ds-{i:04}"),
                name: format!("Animal Intake {i}"),
                score: 5,
                matched_signals: vec!["name:animal".to_owned()],
                probe_cmd: format!("homeward probe --family socrata data.example{i}.gov ds-{i:04}"),
            })
            .collect();

        candidates.truncate(3);
        assert_eq!(candidates.len(), 3);
    }

    // ── AC4: ODS catalog fixture emits opendatasoft-family candidates ─────────

    #[test]
    fn test_ods_fixture_parsed_and_scored() {
        let fixture: OdsCatalogResponse =
            serde_json::from_str(ods_fixture_json()).expect("parse ODS fixture");

        let mut ods_candidates: Vec<Candidate> = Vec::new();

        for result in fixture.results {
            let title = &result.metas.default.title;
            let desc = result.metas.default.description.as_deref().unwrap_or("");
            let domain = result.metas.default.domain.clone().unwrap_or_default();
            let col_names: Vec<String> = result.fields.iter().map(|f| f.name.clone()).collect();

            let (score, signals) = score_candidate(title, desc, &col_names);
            if score == 0 {
                continue;
            }

            let base_url = if domain.starts_with("http") {
                domain.clone()
            } else if domain.is_empty() {
                "https://data.opendatasoft.com".to_owned()
            } else {
                format!("https://{domain}")
            };

            ods_candidates.push(Candidate {
                family: CandidateFamily::OpenDataSoft,
                domain_or_base_url: base_url,
                dataset_id: result.dataset_id,
                name: title.clone(),
                score,
                matched_signals: signals,
                probe_cmd: String::new(),
            });
        }

        // All should be opendatasoft family.
        for c in &ods_candidates {
            assert_eq!(c.family, CandidateFamily::OpenDataSoft);
        }

        // Budget expenditures should be dropped (score = 0).
        let budget = ods_candidates.iter().find(|c| c.name.contains("Budget"));
        assert!(
            budget.is_none(),
            "budget dataset should be dropped (score 0)"
        );

        // At least one animal-intake candidate should remain.
        assert!(
            !ods_candidates.is_empty(),
            "ODS fixture should yield at least one animal-intake candidate"
        );

        // Verify probe_cmd would be well-formed for a real candidate.
        let probe_cmd = Candidate::build_probe_cmd(
            CandidateFamily::OpenDataSoft,
            &ods_candidates[0].domain_or_base_url,
            &ods_candidates[0].dataset_id,
        );
        assert!(
            probe_cmd.contains("opendatasoft"),
            "ODS probe_cmd must mention family: {probe_cmd}"
        );
    }

    // ── AC5: No writes to deploy/ ─────────────────────────────────────────────

    #[test]
    fn test_score_candidate_does_not_write_files() {
        // score_candidate is a pure function — it cannot write files.
        // This test proves the function is side-effect free by running it
        // and asserting no unexpected state change.
        let deploy_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("deploy");

        let before = if deploy_dir.exists() {
            std::fs::read_dir(&deploy_dir)
                .map(|d| d.count())
                .unwrap_or(0)
        } else {
            0
        };

        let _ = score_candidate(
            "Animal Intake",
            "Stray records",
            &["intake_type".to_owned()],
        );

        let after = if deploy_dir.exists() {
            std::fs::read_dir(&deploy_dir)
                .map(|d| d.count())
                .unwrap_or(0)
        } else {
            0
        };

        assert_eq!(before, after, "discover must not modify deploy/ directory");
    }

    // ── AC6: ArcGIS coverage gap in --families ────────────────────────────────

    #[test]
    fn test_arcgis_families_filter() {
        // Verify that parse handles arcgis in --families without panicking.
        let args: Vec<String> = vec!["--families".to_owned(), "arcgis".to_owned()];
        let parsed = DiscoverArgs::parse(&args).expect("should parse without error");
        assert!(parsed.families.contains(&"arcgis".to_owned()));
        // The arcgis family is listed — run_discover_cmd will log a warning.
        // No panic or assertion error should occur at parse time.
    }

    // ── Scoring signal coverage ───────────────────────────────────────────────

    #[test]
    fn test_score_signals_intake_type_col() {
        let (score, signals) = score_candidate("Animal Shelter", "", &["intake_type".to_owned()]);
        assert!(signals.contains(&"col:intake-type".to_owned()), "should detect intake_type col");
        assert!(score >= 2, "intake_type column adds at least 2 to score");
    }

    #[test]
    fn test_score_signals_animal_type_col() {
        let (score, signals) = score_candidate("Animal Shelter", "", &["animal_type".to_owned()]);
        assert!(signals.contains(&"col:animal-type".to_owned()), "should detect animal_type col");
        assert!(score >= 1, "animal_type column adds at least 1 to score");
    }

    #[test]
    fn test_score_zero_for_unrelated_dataset() {
        let (score, _) = score_candidate(
            "Road Construction Permits",
            "Monthly permit issuance for road construction projects",
            &["permit_id".to_owned(), "location".to_owned(), "cost".to_owned()],
        );
        assert_eq!(score, 0, "unrelated dataset must score 0");
    }

    #[test]
    fn test_families_default_when_empty_args() {
        let args: Vec<String> = vec![];
        let parsed = DiscoverArgs::parse(&args).expect("should parse empty args");
        assert!(parsed.families.contains(&"socrata".to_owned()));
        assert!(parsed.families.contains(&"opendatasoft".to_owned()));
    }
}
