//! `homeward-ingestd` — the homeward ingest daemon CLI.
//!
//! Subcommands:
//! - `homeward-ingestd run [--db <path>]` — start the ingest loop.
//! - `homeward-ingestd stats [--db <path>]` — print store statistics.
//! - `homeward-ingestd get <canonical_id> [--db <path>]` — fetch one record.

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use std::time::Duration;

use homeward_connectors::connectors::socrata::{SocrataConfig, SocrataConnector};
use homeward_embed_client::EmbedClientConfig;
use homeward_ingest::departure::DepartureConfig;
use homeward_ingest::enroll::EnrollWorker;
use homeward_ingest::orchestrator::Orchestrator;
use homeward_ingest::store::Store;
use tokio::sync::Mutex;
use tracing::info;
use ulid::Ulid;

fn default_db_path() -> PathBuf {
    dirs_or_home().join("homeward-ingest.db")
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
        .join(".local")
        .join("share")
        .join("homeward")
}

#[tokio::main]
async fn main() {
    tracing_subscriber_init();
    let args: Vec<String> = std::env::args().collect();
    let Some(subcmd) = args.get(1) else {
        print_usage();
        process::exit(1);
    };
    let rest = args.get(2..).unwrap_or(&[]);
    match subcmd.as_str() {
        "run" => cmd_run(rest).await,
        "stats" => cmd_stats(rest),
        "get" => cmd_get(rest),
        other => {
            eprintln!("unknown subcommand: {other:?}");
            print_usage();
            process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  homeward-ingestd run    [--db <path>]");
    eprintln!("  homeward-ingestd stats  [--db <path>]");
    eprintln!("  homeward-ingestd get <canonical_id> [--db <path>]");
}

fn parse_db_flag(args: &[String]) -> PathBuf {
    let mut i = 0;
    while i < args.len() {
        if args.get(i).map(String::as_str) == Some("--db") {
            if let Some(path) = args.get(i + 1) {
                return PathBuf::from(path);
            }
        }
        i += 1;
    }
    default_db_path()
}

async fn cmd_run(args: &[String]) {
    let db_path = parse_db_flag(args);
    info!(db = %db_path.display(), "opening store");

    let store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open store at {}: {e}", db_path.display());
            process::exit(1);
        }
    };
    let store = Arc::new(Mutex::new(store));
    let config = DepartureConfig::default();

    // Wire enrollment: consume delta events and enroll photos into the embed gallery.
    // Honest degradation: if the sidecar is absent, EnrollWorker skips with a warning.
    let embed_cfg = EmbedClientConfig::from_env();
    let (enroll_sink, enroll_worker) = EnrollWorker::new(embed_cfg, 512);
    tokio::spawn(enroll_worker.run());

    let mut orchestrator = Orchestrator::new(Arc::clone(&store), enroll_sink, config);

    // Register Socrata connectors (no API key required).
    for cfg in [
        SocrataConfig::austin(),
        SocrataConfig::dallas(),
        SocrataConfig::sonoma(),
        SocrataConfig::long_beach(),
    ] {
        let name = cfg.name.to_owned();
        match SocrataConnector::new(cfg) {
            Ok(c) => {
                info!(connector = %name, "registered");
                orchestrator.register(name, Box::new(c));
            }
            Err(e) => eprintln!("warning: could not init {name} connector: {e}"),
        }
    }

    // Register RescueGroups if the env key is set.
    if std::env::var("RESCUEGROUPS_API_KEY").is_ok() {
        use homeward_connectors::{
            RescueGroupsConnector,
            connectors::rescuegroups::RescueGroupsConfig,
        };
        match RescueGroupsConfig::from_env().and_then(RescueGroupsConnector::new) {
            Ok(c) => {
                info!(connector = "rescuegroups", "registered");
                orchestrator.register("rescuegroups", Box::new(c));
            }
            Err(e) => eprintln!("warning: could not init rescuegroups connector: {e}"),
        }
    }

    info!("ingest loop starting; Ctrl-C to stop");
    let tick_interval = Duration::from_secs(60);
    let mut interval = tokio::time::interval(tick_interval);
    loop {
        interval.tick().await;
        if let Err(e) = orchestrator.tick().await {
            eprintln!("tick error: {e}");
        }
        let store = store.lock().await;
        match store.stats() {
            Ok(s) => info!(total = s.total, dogs = s.dogs, cats = s.cats, departed = s.departed, "stats"),
            Err(e) => eprintln!("stats error: {e}"),
        }
    }
}

fn cmd_stats(args: &[String]) {
    let db_path = parse_db_flag(args);
    let store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open store: {e}");
            process::exit(1);
        }
    };
    match store.stats() {
        Ok(s) => {
            println!("total:      {}", s.total);
            println!("dogs:       {}", s.dogs);
            println!("cats:       {}", s.cats);
            println!("departed:   {}", s.departed);
            println!("fresh 1h:   {}", s.fresh_1h);
            println!("fresh 24h:  {}", s.fresh_24h);
            println!("stale:      {}", s.stale);
        }
        Err(e) => {
            eprintln!("stats error: {e}");
            process::exit(1);
        }
    }
}

fn cmd_get(args: &[String]) {
    let Some(id_str) = args.first() else {
        eprintln!("Usage: homeward-ingestd get <canonical_id> [--db <path>]");
        process::exit(1);
    };
    if id_str.starts_with('-') {
        eprintln!("Usage: homeward-ingestd get <canonical_id> [--db <path>]");
        process::exit(1);
    }
    let id = match id_str.parse::<Ulid>() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("invalid canonical_id {id_str:?}: {e}");
            process::exit(1);
        }
    };
    let rest = args.get(1..).unwrap_or(&[]);
    let db_path = parse_db_flag(rest);
    let store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to open store: {e}");
            process::exit(1);
        }
    };
    match store.get(id) {
        Ok(record) => match serde_json::to_string_pretty(&record) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("serialize error: {e}");
                process::exit(1);
            }
        },
        Err(homeward_ingest::store::StoreError::NotFound(_)) => {
            eprintln!("not found: {id}");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("get error: {e}");
            process::exit(1);
        }
    }
}

fn tracing_subscriber_init() {
    // Best-effort tracing setup; ignore failures.
    let _ = std::panic::catch_unwind(|| {
        tracing::subscriber::set_global_default(
            tracing_subscriber_fmt_simple(),
        )
    });
}

fn tracing_subscriber_fmt_simple() -> impl tracing::Subscriber {
    use tracing_subscriber::fmt;
    fmt().with_env_filter(
        tracing_subscriber::EnvFilter::from_default_env()
            .add_directive(tracing::Level::INFO.into()),
    )
    .finish()
}
