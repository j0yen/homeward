//! `homeward-reportd` — lost-pet report service CLI.
//!
//! Usage:
//!   homeward-reportd submit --species <dog|cat> --zip <zip> --contact <relay-email>
//!                           [--photo <path>] [--description <text>]
//!   homeward-reportd status <report-id>
//!   homeward-reportd serve  [--port <N>]
//!   homeward-reportd expire-stale

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::process;

use chrono::Utc;
use homeward_report::{
    ReportStore, SubmitRequest,
    alerts::AlertConfig,
    api::ApiConfig,
    store::SubmitError,
};
use homeward_schema::{CoarseLocation, Species};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    match args[1].as_str() {
        "submit" => cmd_submit(&args[2..]),
        "status" => cmd_status(&args[2..]),
        "serve" => cmd_serve(&args[2..]),
        "expire-stale" => cmd_expire_stale(),
        other => {
            eprintln!("unknown subcommand: {other:?}");
            print_usage();
            process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("Usage: homeward-reportd <subcommand> ...");
    eprintln!("  submit  --species <dog|cat> --zip <zip> --contact <token>");
    eprintln!("          [--photo <path>] [--description <text>]");
    eprintln!("  status  <report-id>");
    eprintln!("  serve   [--port <N>]");
    eprintln!("  expire-stale");
}

fn cmd_submit(args: &[String]) {
    let mut species_str: Option<String> = None;
    let mut zip: Option<String> = None;
    let mut contact: Option<String> = None;
    let mut photo_path: Option<String> = None;
    let mut description: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--species" => {
                i += 1;
                species_str = args.get(i).cloned();
            }
            "--zip" => {
                i += 1;
                zip = args.get(i).cloned();
            }
            "--contact" => {
                i += 1;
                contact = args.get(i).cloned();
            }
            "--photo" => {
                i += 1;
                photo_path = args.get(i).cloned();
            }
            "--description" => {
                i += 1;
                description = args.get(i).cloned();
            }
            other => {
                eprintln!("unknown argument: {other:?}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let species = match species_str.as_deref() {
        Some("dog") => Species::Dog,
        Some("cat") => Species::Cat,
        other => {
            eprintln!("--species must be 'dog' or 'cat', got: {:?}", other);
            process::exit(1);
        }
    };

    let zip_code = zip.unwrap_or_else(|| {
        eprintln!("--zip is required");
        process::exit(1);
    });

    let raw_contact = contact.unwrap_or_else(|| {
        eprintln!("--contact is required");
        process::exit(1);
    });

    let photo_bytes = photo_path.and_then(|p| {
        std::fs::read(&p).map_err(|e| eprintln!("warning: could not read photo {p}: {e}")).ok()
    });

    let mut store = ReportStore::new();
    let req = SubmitRequest {
        species,
        breed_primary: None,
        breed_secondary: None,
        description,
        photo_bytes,
        last_seen: CoarseLocation {
            zip_code: Some(zip_code),
            city: None,
            state: None,
            radius_miles: None,
        },
        raw_contact,
        ttl_secs: None,
    };

    match homeward_report::submit(req, &mut store, Utc::now()) {
        Ok(report) => {
            println!("Report submitted: {}", report.report_id);
            println!("Contact token: {}", report.contact.token());
            println!("Expires: {}", report.expires);
        }
        Err(SubmitError::Duplicate(id)) => {
            eprintln!("error: duplicate report_id {id}");
            process::exit(1);
        }
    }
}

fn cmd_status(args: &[String]) {
    let report_id = match args.first() {
        Some(id) => id,
        None => {
            eprintln!("Usage: homeward-reportd status <report-id>");
            process::exit(1);
        }
    };

    // In production this would load from persistent store.
    // For the CLI stub, we demonstrate the API surface.
    eprintln!("info: status for {report_id} would be fetched from the persistent store.");
    eprintln!("      (phase-1 implementation: store is in-memory only)");
    println!("report_id: {report_id}");
    println!("status: unknown (not persisted in this run)");
}

fn cmd_serve(args: &[String]) {
    let mut port: u16 = 8080;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--port" {
            i += 1;
            port = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(8080);
        }
        i += 1;
    }

    let _cfg = ApiConfig::default();
    let _alert_cfg = AlertConfig::default();

    println!("homeward-reportd: open read API would listen on :{port}");
    println!("(phase-1: HTTP server not wired; use the library API directly)");
}

fn cmd_expire_stale() {
    let mut store = ReportStore::new();
    let expired = homeward_report::expire_stale(&mut store, Utc::now());
    println!("expired {} stale reports", expired.len());
}
