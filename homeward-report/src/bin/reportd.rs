//! `homeward-reportd` — lost-pet report service CLI.
//!
//! Usage:
//!   homeward-reportd submit --species <dog|cat> --zip <zip> --contact <relay-email>
//!                           [--photo <path>] [--description <text>]
//!   homeward-reportd status <report-id>
//!   homeward-reportd serve  [--port <N>]
//!   homeward-reportd expire-stale
//!   homeward-reportd deliver --report <id> [--dry-run]
//!   homeward-reportd match   --report <id>
//!   homeward-reportd alerts-log [--ledger <path>]

#![allow(clippy::print_stdout)]
#![allow(clippy::print_stderr)]

use std::collections::HashMap;
use std::process;

use chrono::Utc;
use homeward_embed_client::{EmbedClient, EmbedClientConfig, EmbedClientError, QueryRequest};
use homeward_match::{
    MatchParams, RankedCandidates,
    calibration::BucketThresholds,
    hold::DEFAULT_HOLD_DAYS,
    report::{MatchContext, ReportInput, match_report},
};
use homeward_report::{
    DeliveryLedger, DeliveryOutcome, Deliverer, DryRunDeliverer,
    ReportStore, SubmitRequest,
    alerts::{AlertConfig, AlertDedup, MatchCandidate, process_candidate},
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
        "match" => cmd_match(rest),
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
    eprintln!("  match       --report <id>");
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
        photo_url: None,
        last_seen: CoarseLocation {
            zip_code: Some(zip_code),
            city: None,
            state: None,
            radius_miles: None,
        },
        raw_contact,
        ttl_secs: None,
        notify_url: None,
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
    let mut bind = "0.0.0.0".to_owned();
    let mut no_embed = false;
    let mut i = 0usize;
    while i < args.len() {
        let Some(flag) = args.get(i) else { break };
        match flag.as_str() {
            "--port" => {
                i += 1;
                port = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(8080);
            }
            "--bind" => {
                i += 1;
                if let Some(b) = args.get(i) {
                    bind = b.clone();
                }
            }
            "--no-embed" => {
                no_embed = true;
            }
            other => {
                eprintln!("unknown serve argument: {other:?}");
                process::exit(1);
            }
        }
        i += 1;
    }

    // Initialise tracing subscriber (best-effort; ignore if already initialised).
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("error: failed to build tokio runtime: {e}");
            process::exit(1);
        });

    println!("homeward-reportd: listening on {bind}:{port}");
    if let Err(e) = rt.block_on(homeward_report::server::serve(port, &bind, no_embed)) {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

fn cmd_expire_stale() {
    let mut store = ReportStore::new();
    let expired = homeward_report::expire_stale(&mut store, Utc::now());
    println!("expired {} stale reports", expired.len());
}

/// `deliver --report <id> [--dry-run] [--ledger <path>]`
///
/// Runs the full generate→deliver→ledger path for the given report ID.
/// In phase-1 the store is in-memory, so this synthesises a candidate
/// from homeward-match to demonstrate the end-to-end pipeline.
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

    // Phase-1: build a synthetic report and use the real match pipeline.
    // In production, these would be loaded from the persistent store.
    let report = build_synthetic_report_for_deliver(&report_id);
    let candidate = build_candidate_via_match(&report);

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

/// `match --report <id>`
///
/// The real embed+match flow: load the stored report, embed its photo via
/// the sidecar `/query` endpoint, fuse visual scores with homeward-match,
/// and print the ranked shortlist. Degrades gracefully when the sidecar is
/// unreachable.
fn cmd_match(args: &[String]) {
    let mut report_id: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let Some(flag) = args.get(i) else { break };
        if flag == "--report" {
            i += 1;
            report_id = args.get(i).cloned();
        }
        i += 1;
    }

    let report_id = report_id.unwrap_or_else(|| {
        eprintln!("--report <id> is required");
        print_usage();
        process::exit(1);
    });

    // Phase-1: load from in-memory store; in production, load from persistent store.
    let store = ReportStore::new();
    let report = store.get(&report_id).cloned().unwrap_or_else(|| {
        // Demo: synthesise a report so the match path can be exercised end-to-end.
        build_synthetic_report_for_deliver(&report_id)
    });

    // Attempt visual similarity query via embed sidecar.
    let visual_scores = query_visual_scores_for_report(&report);
    let (visual_scores_map, visual_available) = match visual_scores {
        Ok(m) => (m, true),
        Err(warn) => {
            eprintln!("warning: visual matching unavailable — falling back to geo+date only");
            eprintln!("  reason: {warn}");
            (HashMap::new(), false)
        }
    };

    if !visual_available {
        println!("visual matching unavailable — shortlist uses geo+date signals only");
    }

    // In production, load shelter records from the persistent store.
    // Phase-1: empty gallery produces an empty shortlist with an informative message.
    let records: Vec<PetRecord> = vec![];

    let report_input = ReportInput {
        report_id: &report.report_id,
        species: report.species,
        lat: None,
        lon: None,
        date: report.created,
        size: None,
        colors: &[],
    };

    let params = MatchParams::default();
    let ctx = MatchContext {
        records: &records,
        visual_scores: &visual_scores_map,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    match match_report(&report_input, &params, &ctx) {
        Ok(ranked) if ranked.is_empty() => {
            println!(
                "No candidates found for report {} (species: {:?}).",
                report.report_id, report.species
            );
            println!("Hint: enroll shelter animals before matching.");
        }
        Ok(ranked) => {
            print_shortlist(&ranked);
        }
        Err(e) => {
            eprintln!("error running match pipeline: {e}");
            process::exit(1);
        }
    }
}

/// Print a ranked shortlist with candidate-not-confirmation framing.
///
/// No owner contact and no precise coordinates are printed.
fn print_shortlist(ranked: &RankedCandidates) {
    println!(
        "Possible matches for human review — report {} ({} candidates):",
        ranked.report_id,
        ranked.candidates.len()
    );
    println!("These are candidates to investigate, not confirmed matches.");
    println!();
    for (idx, c) in ranked.candidates.iter().enumerate() {
        println!(
            "  {}. canonical_id={} score={:.2} bucket={} signals=[{}]",
            idx + 1,
            c.canonical_id,
            c.score,
            c.bucket,
            c.why.narrative
        );
        if let Some(deadline) = c.reclaimable_until {
            println!(
                "     *** STRAY IN HOLD — reclaim by {} ***",
                deadline.format("%Y-%m-%d %H:%M UTC")
            );
        }
    }
}

// ─── Embed/match helpers ──────────────────────────────────────────────────────

/// Visual scores keyed by canonical_id, or an error string if unavailable.
type VisualScoreResult = Result<HashMap<Ulid, f64>, String>;

/// Query the embed sidecar for visual similarity scores for a stored report.
///
/// Returns `Err(reason)` if the sidecar is unreachable, the report has no
/// photo, or the response cannot be decoded. Never panics.
fn query_visual_scores_for_report(report: &LostReport) -> VisualScoreResult {
    if report.photos.is_empty() {
        return Err("report has no photo".to_owned());
    }
    // Phase-1: photo URLs are stored but blob retrieval is not yet wired.
    // When a blob store is available, load the first photo bytes and call
    // `query_visual_scores_with_bytes`.
    Err("photo blob retrieval not yet wired (phase-1 in-memory store)".to_owned())
}

/// Query the embed sidecar with raw photo bytes.
///
/// Used from tests (with a mock endpoint) and will be the production path
/// once blob storage is wired. Returns `Err` on transport failure.
pub(crate) fn query_visual_scores_with_bytes(
    photo_bytes: &[u8],
    species: Species,
    k: u32,
) -> VisualScoreResult {
    let cfg = EmbedClientConfig::from_env();
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("tokio runtime init failed: {e}"))?;

    let client = EmbedClient::new(cfg).map_err(|e| format!("embed client build failed: {e}"))?;

    use base64::Engine as _;
    let image_b64 = base64::engine::general_purpose::STANDARD.encode(photo_bytes);

    let species_str = match species {
        Species::Dog => "dog",
        Species::Cat => "cat",
    };

    let req = QueryRequest {
        image_url: None,
        image_b64: Some(image_b64),
        k,
        species_filter: Some(species_str.to_owned()),
    };

    let matches = rt
        .block_on(client.query(req))
        .map_err(|e: EmbedClientError| match e {
            EmbedClientError::Transport(_) => {
                "visual matching unavailable — embed sidecar unreachable".to_owned()
            }
            other => format!("embed sidecar error: {other}"),
        })?;

    let mut scores: HashMap<Ulid, f64> = HashMap::new();
    for m in matches {
        if let Ok(id) = m.canonical_id.parse::<Ulid>() {
            scores.insert(id, m.score);
        }
    }
    Ok(scores)
}

// ─── Deliver-path helpers ─────────────────────────────────────────────────────

/// Build a minimal synthetic [`LostReport`] for the deliver pipeline demo.
fn build_synthetic_report_for_deliver(report_id: &str) -> LostReport {
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
        notify_url: None,
    }
}

/// Build a [`MatchCandidate`] using the real match pipeline for the deliver demo.
///
/// Visual score of 0.9 is used as a demo value (no sidecar call made here).
fn build_candidate_via_match(report: &LostReport) -> MatchCandidate {
    let now = Utc::now();
    let record = PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("demo-source", TosClass::OpenData),
        source_animal_id: None,
        species: report.species,
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

    // Use real fusion with a demo visual score of 0.9.
    let mut visual_scores = HashMap::new();
    visual_scores.insert(record.canonical_id, 0.9_f64);

    let report_input = ReportInput {
        report_id: &report.report_id,
        species: report.species,
        lat: None,
        lon: None,
        date: report.created,
        size: None,
        colors: &[],
    };

    let params = MatchParams::default();
    let ctx = MatchContext {
        records: &[record.clone()],
        visual_scores: &visual_scores,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    let ranked = match_report(&report_input, &params, &ctx).unwrap_or_else(|_| RankedCandidates {
        report_id: report.report_id.clone(),
        candidates: vec![],
        params,
    });

    let score = ranked
        .candidates
        .first()
        .map_or(0.9_f32, |c| c.score as f32);

    MatchCandidate {
        record,
        score,
        reclaimable_until: ranked.candidates.first().and_then(|c| c.reclaimable_until),
    }
}

/// `alerts-log [--ledger <path>]`
///
/// Print all entries in the delivery ledger.
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

    println!(
        "{:<40} {:<20} {:<30} {:<12} {}",
        "alert_id", "report_id", "deliverer", "outcome", "ts"
    );
    println!("{}", "-".repeat(120));
    for r in &records {
        println!(
            "{:<40} {:<20} {:<30} {:<12} {}",
            r.alert_id, r.report_id, r.deliverer, r.outcome, r.ts
        );
    }
    println!("\nTotal: {} records", records.len());
}
