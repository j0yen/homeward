//! `homeward connectors` CLI — poll a connector and print normalized records.
//!
//! Usage:
//!   homeward connectors poll <name> [--since <iso-timestamp>] [--limit <N>]
//!   homeward connectors list
//!   homeward connectors coverage [--store <path>] [--json]
//!
//! # Coverage thresholds
//!
//! The `coverage` subcommand uses two explicit constants:
//! - `STALE_MULTIPLIER=3`: a source is STALE when last success exceeds cadence×3.
//! - `RECENT_WINDOW_SECS=14400`: a source is LIVE if last success is within 4 hours (14400 s).

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]
// CLI argument indexing is guarded by explicit length checks before each access.
#![allow(clippy::indexing_slicing)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process;

use chrono::DateTime;
use homeward_connectors::{
    ConnectorRegistry, Cursor,
    catalog::load_catalog,
    connectors::arcgis::ArcGisConnector,
    connectors::petfbi::{PetFbiConfig, PetFbiConnector},
    connectors::socrata::{SocrataConfig, SocrataConnector},
    coverage::{CoverageArgs, RECENT_WINDOW_SECS, STALE_MULTIPLIER, run_coverage},
    discover::run_discover_cmd,
    probe::run_probe_cmd,
};

fn build_registry() -> ConnectorRegistry {
    let mut registry = ConnectorRegistry::new();

    // Determine which Socrata sources to register.
    // If HOMEWARD_SOURCES is set, load from that TOML file.
    // If unset, fall back to the four built-in configs.
    let socrata_configs: Vec<SocrataConfig> = if let Ok(path_str) = std::env::var("HOMEWARD_SOURCES") {
        let path = std::path::Path::new(&path_str);
        match load_catalog(path) {
            Ok(catalog) => catalog.socrata,
            Err(e) => {
                eprintln!("error: HOMEWARD_SOURCES={path_str} could not be loaded: {e}");
                vec![]
            }
        }
    } else {
        vec![
            SocrataConfig::austin(),
            SocrataConfig::dallas(),
            SocrataConfig::sonoma(),
            SocrataConfig::long_beach(),
        ]
    };

    // Register Socrata connectors.
    for config in socrata_configs {
        let name = config.name.clone();
        match SocrataConnector::new(config) {
            Ok(c) => registry.register(name, Box::new(c)),
            Err(e) => eprintln!("warning: could not init {name} connector: {e}"),
        }
    }

    // Register Pet FBI if data-file UUID is set.
    if let Some(config) = PetFbiConfig::from_env() {
        match PetFbiConnector::new(config) {
            Ok(c) => registry.register("petfbi", Box::new(c)),
            Err(e) => eprintln!("warning: could not init petfbi connector: {e}"),
        }
    }

    // Register ArcGIS connectors from catalog (if HOMEWARD_SOURCES is set).
    if let Ok(path_str) = std::env::var("HOMEWARD_SOURCES") {
        let path = std::path::Path::new(&path_str);
        let arcgis_configs = match load_catalog(path) {
            Ok(catalog) => catalog.arcgis,
            Err(_) => vec![],
        };
        for config in arcgis_configs {
            let name = config.name.clone();
            match ArcGisConnector::new(config) {
                Ok(c) => registry.register(name, Box::new(c)),
                Err(e) => eprintln!("warning: could not init {name} ArcGIS connector: {e}"),
            }
        }
    }

    // Register RescueGroups if API key is set.
    if std::env::var("RESCUEGROUPS_API_KEY").is_ok() {
        use homeward_connectors::{
            RescueGroupsConnector,
            connectors::rescuegroups::RescueGroupsConfig,
        };
        match RescueGroupsConfig::from_env().and_then(RescueGroupsConnector::new) {
            Ok(c) => registry.register("rescuegroups", Box::new(c)),
            Err(e) => eprintln!("warning: could not init rescuegroups connector: {e}"),
        }
    }

    registry
}

/// Build per-source cadence hints from the registry's cadence_hint() method.
///
/// Since `ConnectorRegistry` doesn't expose cadence hints directly, we use the
/// default (4 hours) for all sources. A future iteration can thread the config
/// cadence through the registry.
fn build_cadence_hints(_registry: &ConnectorRegistry) -> HashMap<String, u64> {
    // Default: 4 hours for all sources. Future: read from registry.
    HashMap::new()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: homeward <subcommand>");
        eprintln!("       homeward connectors poll <name> [--since <ts>] [--limit <n>]");
        eprintln!("       homeward connectors list");
        eprintln!("       homeward connectors coverage [--store <path>] [--json]");
        eprintln!("       homeward probe <domain> <dataset_id> [--name <slug>] [--json]");
        eprintln!("       homeward discover [--families socrata,opendatasoft] [--metro <str>] [--limit N] [--json]");
        process::exit(1);
    }

    match args[1].as_str() {
        "connectors" => handle_connectors(&args[2..]).await,
        "probe" => {
            let code = run_probe_cmd(&args[2..]).await;
            process::exit(code);
        }
        "discover" => {
            let code = run_discover_cmd(&args[2..]).await;
            process::exit(code);
        }
        other => {
            eprintln!("unknown subcommand: {other:?}");
            process::exit(1);
        }
    }
}

async fn handle_connectors(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: homeward connectors <poll|list|coverage> ...");
        process::exit(1);
    }

    match args[0].as_str() {
        "list" => {
            let registry = build_registry();
            let mut names: Vec<&str> = registry.names().collect();
            names.sort_unstable();
            for name in names {
                println!("{name}");
            }
        }
        "poll" => handle_poll(args).await,
        "coverage" => handle_coverage(args),
        other => {
            eprintln!("unknown connectors subcommand: {other:?}");
            process::exit(1);
        }
    }
}

/// Parse `--since`/`--limit` flags from `poll` arguments.
fn parse_poll_flags(args: &[String]) -> (Option<Cursor>, Option<usize>) {
    let mut since: Option<Cursor> = None;
    let mut limit: Option<usize> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--since requires a value");
                    process::exit(1);
                }
                match DateTime::parse_from_rfc3339(&args[i]) {
                    Ok(dt) => since = Some(Cursor::Timestamp(dt.with_timezone(&chrono::Utc))),
                    Err(e) => {
                        eprintln!("invalid --since timestamp: {e}");
                        process::exit(1);
                    }
                }
            }
            "--limit" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--limit requires a value");
                    process::exit(1);
                }
                match args[i].parse::<usize>() {
                    Ok(n) => limit = Some(n),
                    Err(e) => {
                        eprintln!("invalid --limit value: {e}");
                        process::exit(1);
                    }
                }
            }
            other => {
                eprintln!("unknown argument: {other:?}");
                process::exit(1);
            }
        }
        i += 1;
    }
    (since, limit)
}

async fn handle_poll(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: homeward connectors poll <name> [--since <ts>] [--limit <n>]");
        process::exit(1);
    }
    let name = &args[1];
    let (since, limit) = parse_poll_flags(args);

    let registry = build_registry();
    let connector = match registry.get(name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
    };

    let mut records = match connector.poll(since).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("poll error: {e}");
            process::exit(1);
        }
    };

    if let Some(n) = limit {
        records.truncate(n);
    }

    for rec in &records {
        match serde_json::to_string(rec) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("serialization error: {e}"),
        }
    }

    eprintln!("polled {} records from {name}", records.len());
}

/// Parse and run the `coverage` subcommand.
///
/// Usage: homeward connectors coverage [--store <path>] [--json]
///
/// Thresholds:
///   STALE_MULTIPLIER={STALE_MULTIPLIER}  — source is STALE when last success > cadence×{STALE_MULTIPLIER}
///   RECENT_WINDOW_SECS={RECENT_WINDOW_SECS} — source is LIVE if last success within {r}h
///
/// Status values: LIVE, STALE, SILENT, UNREACHABLE, UNKNOWN (no store)
fn handle_coverage(args: &[String]) {
    // args[0] == "coverage"; flags start at args[1]
    let mut store_path: Option<PathBuf> = None;
    let mut json_output = false;
    let mut i = 1;

    // These are referenced in the doc-comment; suppress unused-variable lint.
    let _stale_mult = STALE_MULTIPLIER;
    let r = RECENT_WINDOW_SECS / 3600;
    let _ = r;

    while i < args.len() {
        match args[i].as_str() {
            "--store" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("--store requires a path argument");
                    process::exit(1);
                }
                store_path = Some(PathBuf::from(&args[i]));
            }
            "--json" => {
                json_output = true;
            }
            "--help" | "-h" => {
                print_coverage_help();
                return;
            }
            other => {
                eprintln!("unknown coverage argument: {other:?}");
                eprintln!("Run `homeward connectors coverage --help` for usage.");
                process::exit(1);
            }
        }
        i += 1;
    }

    let registry = build_registry();
    let cadence_hints = build_cadence_hints(&registry);

    let coverage_args = CoverageArgs {
        store_path: store_path.as_deref(),
        json: json_output,
        registry: &registry,
        cadence_hints,
    };

    if let Err(e) = run_coverage(&coverage_args) {
        eprintln!("coverage error: {e}");
        process::exit(1);
    }
}

fn print_coverage_help() {
    println!("homeward connectors coverage — catchment report");
    println!();
    println!("USAGE:");
    println!("  homeward connectors coverage [--store <path>] [--json]");
    println!();
    println!("OPTIONS:");
    println!("  --store <path>   Path to the homeward ingest SQLite store.");
    println!("                   When omitted, only registry metadata is shown;");
    println!("                   record counts and last_success are marked 'unknown'.");
    println!("  --json           Emit structured JSON ({{sources: [...], holes: [...]}}).");
    println!("  --help           Show this help message.");
    println!();
    println!("STATUS VALUES:");
    println!("  LIVE         Recent success (within RECENT_WINDOW_SECS) and records > 0.");
    println!("  STALE        Last success older than cadence × STALE_MULTIPLIER.");
    println!("  SILENT       Registered but zero records ever ingested.");
    println!("  UNREACHABLE  Source never successfully polled (no cursor advancement).");
    println!("  UNKNOWN      No --store provided; status cannot be determined.");
    println!();
    println!("THRESHOLDS (explicit constants):");
    println!("  STALE_MULTIPLIER  = {STALE_MULTIPLIER}  (cadence × {STALE_MULTIPLIER} = stale threshold)");
    println!("  RECENT_WINDOW_SECS= {RECENT_WINDOW_SECS}  ({}h — live window)", RECENT_WINDOW_SECS / 3600);
    println!();
    println!("ENVIRONMENT:");
    println!("  HOMEWARD_SOURCES   Path to sources.toml catalog (overrides built-in sources).");
}
