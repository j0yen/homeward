//! `homeward-match` CLI — rank shelter candidates for a lost-pet report.
//!
//! # Usage
//!
//! ```text
//! homeward-match report <report.json> [--radius-km <r>] [--date-window-days <d>] [--k <n>]
//! ```
//!
//! Reads a JSON [`homeward_schema::lost::LostReport`] from `<report.json>`
//! and a newline-delimited JSON stream of [`homeward_schema::PetRecord`]s
//! from stdin. Prints the ranked shortlist with per-candidate scores,
//! buckets, and explanations.
//!
//! An empty result (no plausible candidates) is reported clearly on stdout,
//! not as a non-zero exit code.
#![allow(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::single_match_else,
    clippy::match_wildcard_for_single_variants,
)]

use std::{
    collections::HashMap,
    io::{self, BufRead},
    path::PathBuf,
    process::ExitCode,
};

use homeward_match::{
    MatchParams, RankedCandidates, ScoreBucket,
    calibration::BucketThresholds,
    hold::DEFAULT_HOLD_DAYS,
    report::{MatchContext, ReportInput, match_report},
};
use homeward_schema::{PetRecord, lost::LostReport};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args[1] != "report" {
        eprintln!("Usage: homeward-match report <report.json> [--radius-km <r>] [--date-window-days <d>] [--k <n>]");
        return ExitCode::FAILURE;
    }

    let report_path = PathBuf::from(&args[2]);
    let mut params = MatchParams::default();

    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--radius-km" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<f64>() {
                        Ok(r) => params.radius_km = r,
                        Err(_) => {
                            eprintln!("error: --radius-km requires a number, got {v:?}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            "--date-window-days" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<i64>() {
                        Ok(d) => params.date_window_days = d,
                        Err(_) => {
                            eprintln!("error: --date-window-days requires an integer, got {v:?}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            "--k" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    match v.parse::<usize>() {
                        Ok(k) => params.k = k,
                        Err(_) => {
                            eprintln!("error: --k requires a positive integer, got {v:?}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
            }
            other => {
                eprintln!("warning: unknown flag {other:?} (ignored)");
            }
        }
        i += 1;
    }

    // Load report
    let report_json = match std::fs::read_to_string(&report_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read {report_path:?}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let lost_report: LostReport = match serde_json::from_str(&report_json) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: could not parse report JSON: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Load shelter records from stdin (NDJSON)
    let stdin = io::stdin();
    let mut records: Vec<PetRecord> = Vec::new();
    for (lineno, line) in stdin.lock().lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("error reading stdin line {}: {e}", lineno + 1);
                return ExitCode::FAILURE;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<PetRecord>(trimmed) {
            Ok(r) => records.push(r),
            Err(e) => {
                eprintln!("warning: skipping malformed record at line {}: {e}", lineno + 1);
            }
        }
    }

    let ctx = MatchContext {
        records: &records,
        visual_scores: &HashMap::new(),
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    let report = ReportInput {
        report_id: &lost_report.report_id,
        species: lost_report.species,
        lat: None, // CoarseLocation has no lat/lon — geo filter skips
        lon: None,
        date: lost_report.created,
        size: lost_report.size,
        colors: &lost_report.colors,
    };

    let result: RankedCandidates = match match_report(&report, &params, &ctx) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: match pipeline failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Print results
    println!("=== homeward-match: ranked shortlist ===");
    println!("report_id : {}", result.report_id);
    println!(
        "params    : radius={:.1}km  window=±{}d  k={}",
        result.params.radius_km,
        result.params.date_window_days,
        result.params.k
    );
    println!(
        "records_in: {}  candidates_out: {}",
        records.len(),
        result.candidates.len()
    );
    println!();

    if result.is_empty() {
        println!("No plausible candidates found.");
        println!("Try widening --radius-km or --date-window-days.");
        return ExitCode::SUCCESS;
    }

    for (rank, c) in result.candidates.iter().enumerate() {
        let bucket_label = match c.bucket {
            ScoreBucket::Strong => "STRONG  ",
            ScoreBucket::Possible => "possible",
            ScoreBucket::Weak => "weak    ",
        };
        print!(
            "#{rank:>2}  [{bucket_label}] score={:.3}  id={}",
            c.score, c.canonical_id
        );
        if let Some(deadline) = c.reclaimable_until {
            print!("  [RECLAIMABLE until {}]", deadline.format("%Y-%m-%d"));
        }
        println!();
        println!("     {}", c.why.narrative);
        println!(
            "     visual={:.2} attr={:.2} geo={:.2} recency={:.2}",
            c.why.visual_score, c.why.attribute_score, c.why.geo_score, c.why.recency_score,
        );
        println!();
    }

    ExitCode::SUCCESS
}
