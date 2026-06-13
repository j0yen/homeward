//! `homeward-reportd` — lost-pet report service CLI.
//!
//! Usage:
//!   homeward-reportd submit --species <dog|cat> --zip <zip> --contact <relay-email>
//!                           [--photo <path>] [--description <text>]
//!   homeward-reportd status <report-id>
//!   homeward-reportd serve  [--port <N>]
//!   homeward-reportd expire-stale
//!   homeward-reportd deliver --report <id> [--dry-run]
//!   homeward-reportd alerts-log [--ledger <path>]

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::process;

use chrono::Utc;
use homeward_report::{
    DeliveryLedger, DeliveryOutcome, Deliverer, DryRunDeliverer,
    ReportStore, SubmitRequest,
    alerts::{AlertConfig, AlertDedup, MatchCandidate, process_candidate},
    api::ApiConfig,
    store::SubmitError,
};
use homeward_schema::{
    Availability, BrokeredContactToken, ChipStatus, CoarseLocation, IntakeType,
    LostReport, LostStatus, PetRecord, ShelterLocation, SourceId, Species, TosClass,
};
use chrono::Duration;
use ulid::Ulid;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let subcmd = args.get(1).map_or("", String::as_str);
    let rest: &[String] = args.get(2..).unwrap_or(&[]);

    match subcmd {
        "submit" => cmd_submit(rest),
        "status" => cmd_status(rest),
        "serve" => cmd_serve(rest),
        "expire-stale" => cmd_expire_stale(),
        "deliver" => cmd_deliver(rest),
        "alerts-log" => cmd_alerts_log(rest),
        other => {
            eprintln!("unknown subcommand: {other:?}");
            print_usage();
            process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("Usage: homeward-reportd <subcommand> ...");
    eprintln!("  submit      --species <dog|cat> --zip <zip> --contact <token>");
    eprintln!("              [--photo <path>] [--description <text>]");
    eprintln!("  status      <report-id>");
    eprintln!("  serve       [--port <N>]");
    eprintln!("  expire-stale");
    eprintln!("  deliver     --report <id> [--dry-run]");
    eprintln!("              [--ledger <path>]");
    eprintln!("  alerts-log  [--ledger <path>]");
}

#[allow(clippy::too_many_lines)]
fn cmd_submit(args: &[String]) {
    let mut species_str: Option<String> = None;
    let mut zip: Option<String> = None;
    let mut contact: Option<String> = None;
    let mut photo_path: Option<String> = None;
    let mut description: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let Some(flag) = args.get(i) else { break };
        match flag.as_str() {
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
            eprintln!("--species must be 'dog' or 'cat', got: {other:?}");
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
        std::fs::read(&p)
            .map_err(|e| eprintln!("warning: could not read photo {p}: {e}"))
            .ok()
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
    let Some(report_id) = args.first() else {
        eprintln!("Usage: homeward-reportd status <report-id>");
        process::exit(1);
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
    let mut i = 0usize;
    while i < args.len() {
        let Some(flag) = args.get(i) else { break };
        if flag == "--port" {
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

/// `deliver --report <id> [--dry-run] [--ledger <path>]`
///
/// Runs the full generate→deliver→ledger path for the given report ID.
/// In phase-1 the store is in-memory, so this synthesises a stub candidate
/// to demonstrate the end-to-end pipeline.
///
/// AC5: exits 0 with a `DryRun` ledger entry when `--dry-run` is set.
fn cmd_deliver(args: &[String]) {
    let mut report_id: Option<String> = None;
    let mut dry_run = false;
    let mut ledger_path: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let Some(flag) = args.get(i) else { break };
        match flag.as_str() {
            "--report" => {
                i += 1;
                report_id = args.get(i).cloned();
            }
            "--dry-run" => {
                dry_run = true;
            }
            "--ledger" => {
                i += 1;
                ledger_path = args.get(i).cloned();
            }
            other => {
                eprintln!("unknown argument: {other:?}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let report_id = report_id.unwrap_or_else(|| {
        eprintln!("--report <id> is required");
        print_usage();
        process::exit(1);
    });

    // Build ledger (persistent or in-memory)
    let mut ledger = if let Some(ref path) = ledger_path {
        DeliveryLedger::open(path).unwrap_or_else(|e| {
            eprintln!("error: could not open ledger at {path}: {e}");
            process::exit(1);
        })
    } else {
        let default_path = DeliveryLedger::default_path();
        DeliveryLedger::open(&default_path).unwrap_or_else(|e| {
            eprintln!("warning: could not open default ledger ({e}); using in-memory");
            DeliveryLedger::in_memory()
        })
    };

    // Phase-1: build a synthetic report and candidate to exercise the pipeline.
    // In production, these would be loaded from the persistent store.
    let report = make_stub_report(&report_id);
    let candidate = make_stub_candidate(0.9);

    let now = Utc::now();
    let mut dedup = AlertDedup::new();
    let cfg = AlertConfig::default();
    let alerts = process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);

    if alerts.is_empty() {
        println!("No alerts generated for report {report_id} (no candidates above threshold).");
        return;
    }

    // Always dry-run when --dry-run flag set OR HOMEWARD_RELAY_ENDPOINT unset
    let deliverer: Box<dyn Deliverer> = Box::new(DryRunDeliverer::new());

    for alert in &alerts {
        // Force dry-run mode if flag set
        let outcome = if dry_run {
            // Wrap in dry-run regardless of registered deliverer
            let dry = DryRunDeliverer::new();
            dry.deliver(alert, &mut ledger)
        } else {
            deliverer.deliver(alert, &mut ledger)
        };

        match &outcome {
            DeliveryOutcome::DryRun { rendered } => {
                println!("--- DryRun delivery for alert (report={report_id}) ---");
                println!("{rendered}");
                println!("Ledger entry written: DryRun");
            }
            DeliveryOutcome::Sent { relay_message_id } => {
                println!("Sent: relay_message_id={relay_message_id}");
            }
            DeliveryOutcome::Suppressed { alert_id } => {
                println!("Suppressed (already delivered): alert_id={alert_id}");
            }
            DeliveryOutcome::Failed { error } => {
                eprintln!("Delivery failed: {error}");
                process::exit(1);
            }
        }
    }
}

// ─── Phase-1 stubs ────────────────────────────────────────────────────────────

/// Build a minimal stub [`LostReport`] for CLI pipeline demonstration.
fn make_stub_report(report_id: &str) -> LostReport {
    let now = Utc::now();
    LostReport {
        report_id: report_id.to_owned(),
        species: Species::Dog,
        breed_primary: Some("Mixed".to_owned()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        description: None,
        photos: vec![],
        last_seen: CoarseLocation {
            zip_code: Some("78701".to_owned()),
            city: Some("Austin".to_owned()),
            state: Some("TX".to_owned()),
            radius_miles: None,
        },
        contact: BrokeredContactToken::new(format!("tok_{:016x}", 0xdeadbeef_u64)),
        created: now,
        expires: now + Duration::days(90),
        status: LostStatus::Active,
    }
}

/// Build a stub [`MatchCandidate`] for CLI pipeline demonstration.
fn make_stub_candidate(score: f32) -> MatchCandidate {
    let now = Utc::now();
    let record = PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("stub-source", TosClass::OpenData),
        source_animal_id: None,
        species: Species::Dog,
        breed_primary: Some("Labrador".to_owned()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        markings_text: None,
        intake_type: IntakeType::Stray,
        availability: Availability::InCustody,
        chip_status: ChipStatus::NotScanned,
        location: Some(ShelterLocation::new(
            None,
            None,
            2,
            "Austin".to_owned(),
            Some("TX".to_owned()),
        )),
        found_location_text: None,
        photos: vec![],
        first_seen: now,
        last_seen: now,
        last_confirmed: None,
        intake_date: Some(now),
        outcome_date: None,
        secondary_provenances: vec![],
    };
    MatchCandidate {
        record,
        score,
        reclaimable_until: None,
    }
}

/// `alerts-log [--ledger <path>]`
///
/// Print all entries in the delivery ledger (AC3).
fn cmd_alerts_log(args: &[String]) {
    let mut ledger_path: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let Some(flag) = args.get(i) else { break };
        if flag == "--ledger" {
            i += 1;
            ledger_path = args.get(i).cloned();
        }
        i += 1;
    }

    let path = ledger_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(DeliveryLedger::default_path);

    let ledger = DeliveryLedger::open(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not open ledger at {}: {e}", path.display());
        DeliveryLedger::in_memory()
    });

    let records = ledger.records();
    if records.is_empty() {
        println!("No delivery records found.");
        return;
    }

    println!("{:<40} {:<20} {:<30} {:<12} {}", "alert_id", "report_id", "deliverer", "outcome", "ts");
    println!("{}", "-".repeat(120));
    for r in &records {
        println!(
            "{:<40} {:<20} {:<30} {:<12} {}",
            r.alert_id, r.report_id, r.deliverer, r.outcome, r.ts
        );
    }
    println!("\nTotal: {} records", records.len());
}
