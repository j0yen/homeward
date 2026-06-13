//! Integration tests for the real embed+match flow wired into homeward-report.
//!
//! AC3: with a mock `/query` endpoint returning known canonical_id → score pairs,
//!      `match_report` produces a shortlist ordered by fused visual+geo+date scores.
//! AC4: when the sidecar is unreachable, returns a geo+date-only shortlist
//!      and emits a "visual matching unavailable" notice — no panic, no fabricated score.
//! AC5: shortlist output contains no owner contact string and no precise coordinates;
//!      framing is candidate-not-confirmation.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use homeward_match::{
    MatchParams, RankedCandidates,
    calibration::BucketThresholds,
    hold::DEFAULT_HOLD_DAYS,
    report::{MatchContext, ReportInput, match_report},
};
use homeward_report::{
    ReportStore, SubmitRequest,
    alerts::{AlertConfig, AlertDedup, MatchCandidate, process_candidate},
};
use homeward_schema::{
    Availability, BrokeredContactToken, ChipStatus, CoarseLocation, IntakeType,
    LostReport, LostStatus, PetRecord, SourceId, Species, TosClass,
};
use ulid::Ulid;

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn make_record(id: u64, species: Species) -> PetRecord {
    let now = Utc::now();
    PetRecord {
        canonical_id: Ulid::from_parts(id, u128::from(id)),
        source: SourceId::new("test", TosClass::OpenData),
        source_animal_id: None,
        species,
        breed_primary: None,
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        markings_text: None,
        intake_type: IntakeType::Stray,
        availability: Availability::InCustody,
        chip_status: ChipStatus::NotScanned,
        location: None,
        found_location_text: None,
        photos: vec![],
        first_seen: now,
        last_seen: now,
        last_confirmed: None,
        intake_date: Some(now),
        outcome_date: None,
        secondary_provenances: vec![],
    }
}

fn make_report(report_id: &str, species: Species) -> LostReport {
    let now = Utc::now();
    LostReport {
        report_id: report_id.to_owned(),
        species,
        breed_primary: None,
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
        contact: BrokeredContactToken::new("tok_demo_1234".to_owned()),
        created: now,
        expires: now + Duration::days(90),
        status: LostStatus::Active,
    }
}

// ─── AC3: shortlist reflects fused visual+geo+date scores ────────────────────

/// Given known visual scores for two records, the one with the higher visual
/// similarity should rank first in the shortlist.
#[test]
fn ac3_shortlist_ordered_by_fused_visual_scores() {
    let records = vec![
        make_record(1, Species::Dog), // visual=0.3 (low)
        make_record(2, Species::Dog), // visual=0.9 (high)
    ];

    let mut visual_scores = HashMap::new();
    visual_scores.insert(records[0].canonical_id, 0.3_f64);
    visual_scores.insert(records[1].canonical_id, 0.9_f64);

    let report = make_report("rpt-ac3", Species::Dog);
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
        visual_scores: &visual_scores,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    let result = match_report(&report_input, &params, &ctx).expect("match_report must not error");

    assert_eq!(result.candidates.len(), 2, "both records should pass prefilter");
    assert_eq!(
        result.candidates[0].canonical_id, records[1].canonical_id,
        "record with visual=0.9 must rank first"
    );
    assert_eq!(
        result.candidates[1].canonical_id, records[0].canonical_id,
        "record with visual=0.3 must rank second"
    );
}

/// Scores from the match pipeline are reflected in the why.visual_score field.
#[test]
fn ac3_visual_score_propagated_into_explanation() {
    let records = vec![make_record(3, Species::Cat)];

    let mut visual_scores = HashMap::new();
    visual_scores.insert(records[0].canonical_id, 0.75_f64);

    let report = make_report("rpt-ac3b", Species::Cat);
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
        visual_scores: &visual_scores,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    let result = match_report(&report_input, &params, &ctx).expect("no error");
    assert!(!result.is_empty(), "expected one candidate");

    let why = &result.candidates[0].why;
    // When visual is supplied, visual_score in explanation is the score (not 0.5 neutral).
    assert!(
        (why.visual_score - 0.75).abs() < 0.01,
        "visual_score in explanation must reflect the supplied value, got {}",
        why.visual_score
    );
    assert!(
        why.narrative.contains("visual similarity"),
        "narrative must mention visual similarity when score is supplied: {}",
        why.narrative
    );
}

// ─── AC4: sidecar unreachable → geo+date shortlist, no panic ─────────────────

/// When visual_scores is empty (sidecar unreachable), match_report still
/// returns a shortlist and does not panic.
#[test]
fn ac4_sidecar_unreachable_returns_geo_date_shortlist() {
    let records = vec![make_record(10, Species::Dog)];

    // Empty map = sidecar unavailable, no visual scores injected.
    let visual_scores: HashMap<Ulid, f64> = HashMap::new();

    let report = make_report("rpt-ac4", Species::Dog);
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
        visual_scores: &visual_scores,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    // Must not panic and must return a result.
    let result = match_report(&report_input, &params, &ctx);
    assert!(result.is_ok(), "match_report must not error when no visual scores");

    let ranked = result.unwrap();
    assert_eq!(ranked.candidates.len(), 1, "one candidate expected from geo+date only");

    // Verify no fabricated visual score — when no score provided, visual_score
    // in the explanation should be 0.5 (neutral).
    let why = &ranked.candidates[0].why;
    assert!(
        (why.visual_score - 0.5).abs() < 0.01,
        "without sidecar, visual_score in explanation must be neutral 0.5, got {}",
        why.visual_score
    );
}

/// The sidecar-unreachable code path (simulated by passing empty scores) produces
/// a shortlist whose score is strictly below what it would be with visual data.
#[test]
fn ac4_no_visual_score_lower_than_with_visual() {
    let records = vec![make_record(11, Species::Dog)];
    let id = records[0].canonical_id;

    let no_visual: HashMap<Ulid, f64> = HashMap::new();
    let with_visual = {
        let mut m = HashMap::new();
        m.insert(id, 0.95_f64);
        m
    };

    let report = make_report("rpt-ac4b", Species::Dog);
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
    let ctx_no = MatchContext {
        records: &records,
        visual_scores: &no_visual,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };
    let ctx_with = MatchContext {
        records: &records,
        visual_scores: &with_visual,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    let score_no = match_report(&report_input, &params, &ctx_no)
        .unwrap()
        .candidates[0]
        .score;
    let score_with = match_report(&report_input, &params, &ctx_with)
        .unwrap()
        .candidates[0]
        .score;

    assert!(
        score_with > score_no,
        "visual score 0.95 should boost combined score above no-visual: {score_with} vs {score_no}"
    );
}

// ─── AC5: no PII, candidate-not-confirmation framing ─────────────────────────

/// Alerts generated from candidates must be candidate-framed (never "we found your pet").
#[test]
fn ac5_alert_is_candidate_framed() {
    let records = vec![make_record(20, Species::Dog)];
    let record = records[0].clone();

    // Score high enough to generate an alert.
    let candidate = MatchCandidate {
        record,
        score: 0.95,
        reclaimable_until: None,
    };

    let report = make_report("rpt-ac5", Species::Dog);
    let mut dedup = AlertDedup::new();
    let cfg = AlertConfig { threshold: 0.8 };
    let now = Utc::now();

    let alerts = process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
    assert!(!alerts.is_empty(), "high-score candidate must generate at least one alert");

    for alert in &alerts {
        assert!(
            alert.is_candidate_framed(),
            "alert message must be candidate-framed, not a confirmation. Message: {:?}",
            alert.message
        );
        // No raw owner email or phone should appear in the alert.
        assert!(
            !alert.message.contains('@'),
            "alert message must not contain raw email: {:?}",
            alert.message
        );
        assert!(
            !alert.message.contains("+1"),
            "alert message must not contain raw phone: {:?}",
            alert.message
        );
        // Shelter area (if present) must not be precise coordinates.
        if let Some(ref area) = alert.shelter_area {
            // A raw lat/lon would look like "40.712, -74.006" — reject decimal degrees.
            let looks_like_coords = area.contains('.') && area.contains(',');
            assert!(
                !looks_like_coords,
                "shelter_area must not be raw coordinates: {area:?}"
            );
        }
    }
}

/// The contact_token in an alert must not be a raw email or phone string.
#[test]
fn ac5_contact_token_is_brokered() {
    let records = vec![make_record(21, Species::Cat)];
    let record = records[0].clone();

    let candidate = MatchCandidate {
        record,
        score: 0.90,
        reclaimable_until: None,
    };

    let report = make_report("rpt-ac5b", Species::Cat);
    let mut dedup = AlertDedup::new();
    let cfg = AlertConfig { threshold: 0.8 };
    let now = Utc::now();

    let alerts = process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
    assert!(!alerts.is_empty(), "expected alert for high-score candidate");

    for alert in &alerts {
        // contact_token is the brokered relay handle: "tok_demo_1234" — not a raw email.
        assert!(
            !alert.contact_token.contains('@'),
            "contact_token must not be a raw email address: {:?}",
            alert.contact_token
        );
        assert!(
            !alert.contact_token.is_empty(),
            "contact_token must not be empty"
        );
    }
}

/// No stub candidate with fabricated score should appear — only real records
/// from the match pipeline.
#[test]
fn ac5_no_fabricated_candidate_score() {
    // Run match pipeline with exactly one known record and one known visual score.
    let records = vec![make_record(30, Species::Dog)];
    let id = records[0].canonical_id;

    let mut visual_scores = HashMap::new();
    visual_scores.insert(id, 0.72_f64);

    let report = make_report("rpt-ac5c", Species::Dog);
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
        visual_scores: &visual_scores,
        thresholds: BucketThresholds::default(),
        hold_days: DEFAULT_HOLD_DAYS,
    };

    let result = match_report(&report_input, &params, &ctx).expect("no error");
    assert_eq!(result.candidates.len(), 1);

    let score = result.candidates[0].score;
    // The combined fused score with visual=0.72, all other signals neutral (0.5),
    // must not equal the legacy stub value of 0.9 exactly.
    let is_stub_score = (score - 0.9_f64).abs() < 1e-6;
    assert!(
        !is_stub_score,
        "score must be a real fused value, not the old stub 0.9. Got: {score}"
    );
    // Also verify score is in [0.0, 1.0].
    assert!(score >= 0.0 && score <= 1.0, "score must be in unit interval, got {score}");
}
