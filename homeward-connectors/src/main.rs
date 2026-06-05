//! `homeward connectors` CLI — poll a connector and print normalized records.
//!
//! Usage:
//!   homeward connectors poll <name> [--since <iso-timestamp>] [--limit <N>]
//!   homeward connectors list

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]
// CLI argument indexing is guarded by explicit length checks before each access.
#![allow(clippy::indexing_slicing)]

use std::process;

use chrono::DateTime;
use homeward_connectors::{
    ConnectorRegistry, Cursor,
    connectors::socrata::{SocrataConfig, SocrataConnector},
};

fn build_registry() -> ConnectorRegistry {
    let mut registry = ConnectorRegistry::new();

    // Register Socrata connectors (no API key required).
    for config in [
        SocrataConfig::austin(),
        SocrataConfig::dallas(),
        SocrataConfig::sonoma(),
        SocrataConfig::long_beach(),
    ] {
        let name = config.name;
        match SocrataConnector::new(config) {
            Ok(c) => registry.register(name, Box::new(c)),
            Err(e) => eprintln!("warning: could not init {name} connector: {e}"),
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

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: homeward <subcommand>");
        eprintln!("       homeward connectors poll <name> [--since <ts>] [--limit <n>]");
        eprintln!("       homeward connectors list");
        process::exit(1);
    }

    match args[1].as_str() {
        "connectors" => handle_connectors(&args[2..]).await,
        other => {
            eprintln!("unknown subcommand: {other:?}");
            process::exit(1);
        }
    }
}

async fn handle_connectors(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: homeward connectors <poll|list> ...");
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
