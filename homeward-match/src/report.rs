//! End-to-end match pipeline — prefilter → score fusion → sort → rank.
//!
//! This module wires together [`crate::filter::prefilter`],
//! [`crate::fusion::fuse_scores`], [`crate::hold`], and
//! [`crate::calibration`] into the top-level `match_report` function.

use chrono::Utc;
use homeward_schema::{PetRecord, Size, Species};
use ulid::Ulid;

use crate::{
    Candidate, MatchError, MatchParams, RankedCandidates,
    calibration::{BucketThresholds, assign_bucket},
    filter::{PrefilterInput, ReportFilter, prefilter, record_report_distance_km},
    fusion::{FusionInput, fuse_scores},
    hold::stray_hold_deadline,
};

/// Context for a single match run.
///
/// The caller is responsible for providing the full record set (or a
/// pre-loaded view); this struct holds references to avoid cloning the
/// store on every call.
pub struct MatchContext<'a> {
    /// All shelter records available for matching.
    pub records: &'a [PetRecord],

    /// Visual similarity scores keyed by `canonical_id`, in [0.0, 1.0].
    /// Pass an empty map when no photos are available.
    pub visual_scores: &'a std::collections::HashMap<Ulid, f64>,

    /// Calibration thresholds. Use [`BucketThresholds::default()`] when
    /// you have not yet fit thresholds on labeled data.
    pub thresholds: BucketThresholds,

    /// Hold duration used for stray-hold flagging. Defaults to
    /// [`crate::hold::DEFAULT_HOLD_DAYS`] when jurisdiction is unknown.
    pub hold_days: i64,
}

/// Report-side data needed by the match pipeline.
///
/// Groups the report fields to stay within the 5-argument limit.
#[derive(Debug, Clone)]
pub struct ReportInput<'a> {
    /// Unique identifier for this report.
    pub report_id: &'a str,
    /// Species of the lost animal.
    pub species: Species,
    /// Last-seen latitude (coarse).
    pub lat: Option<f64>,
    /// Last-seen longitude (coarse).
    pub lon: Option<f64>,
    /// When the report was created.
    pub date: chrono::DateTime<Utc>,
    /// Approximate size of the lost animal.
    pub size: Option<Size>,
    /// Color tokens from the report.
    pub colors: &'a [String],
}

/// Run the full match pipeline for a lost report.
///
/// Steps:
/// 1. Build [`PrefilterInput`] slim views from `ctx.records`.
/// 2. Apply structured prefilter (species, availability, geo, date).
/// 3. Score each passing record via [`fuse_scores`].
/// 4. Assign calibrated bucket.
/// 5. Flag stray-hold candidates and compute `reclaimable_until`.
/// 6. Sort descending by score; reclaimable strays precede equal-scored
///    non-strays.
/// 7. Truncate to `params.k`.
///
/// Returns [`RankedCandidates`]. An empty list is not an error.
///
/// # Errors
/// Returns [`MatchError`] if `params` fails validation.
pub fn match_report(
    report: &ReportInput<'_>,
    params: &MatchParams,
    ctx: &MatchContext<'_>,
) -> Result<RankedCandidates, MatchError> {
    params.validate()?;

    // Step 1 + 2: prefilter
    let slim: Vec<PrefilterInput> = ctx.records.iter().map(PrefilterInput::from).collect();
    let rf = ReportFilter {
        species: report.species,
        lat: report.lat,
        lon: report.lon,
        date: report.date,
    };
    let passing_ids = prefilter(&slim, rf, params);

    let passing_set: std::collections::HashSet<Ulid> = passing_ids.into_iter().collect();

    // Step 3–5: score
    let now = Utc::now();
    let mut candidates: Vec<Candidate> = ctx
        .records
        .iter()
        .filter(|r| passing_set.contains(&r.canonical_id))
        .map(|record| {
            let dist_km = record_report_distance_km(
                record.location.as_ref().and_then(|l| l.lat),
                record.location.as_ref().and_then(|l| l.lon),
                report.lat,
                report.lon,
            );
            let visual_score = ctx.visual_scores.get(&record.canonical_id).copied();

            let input = FusionInput {
                record,
                visual_score,
                dist_km,
                report_size: report.size,
                report_colors: report.colors,
                report_date: report.date,
            };
            let (score, why) = fuse_scores(&input, params);
            let bucket = assign_bucket(score, ctx.thresholds);
            let reclaimable_until = stray_hold_deadline(record, ctx.hold_days)
                .filter(|&deadline| now < deadline);

            Candidate {
                canonical_id: record.canonical_id,
                score,
                bucket,
                why,
                reclaimable_until,
            }
        })
        .collect();

    // Step 6: sort — reclaimable strays first, then by score desc
    candidates.sort_by(|a, b| {
        let a_reclaim = a.reclaimable_until.is_some();
        let b_reclaim = b.reclaimable_until.is_some();
        match (a_reclaim, b_reclaim) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b
                .score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal),
        }
    });

    // Step 7: truncate
    candidates.truncate(params.k);

    Ok(RankedCandidates {
        report_id: report.report_id.to_owned(),
        candidates,
        params: params.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use homeward_schema::{Availability, ChipStatus, IntakeType, Size, Species};
    use homeward_schema::provenance::{SourceId, TosClass};
    use std::collections::HashMap;

    use crate::hold::DEFAULT_HOLD_DAYS;
    use crate::calibration::BucketThresholds;

    fn base_date() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap()
    }

    fn make_record(
        id: u64,
        species: Species,
        avail: Availability,
        intake_type: IntakeType,
        size: Option<Size>,
        colors: Vec<String>,
        intake_offset_days: Option<i64>,
    ) -> PetRecord {
        let canonical_id = Ulid::from_parts(id, u128::from(id));
        let intake_date = intake_offset_days.map(|d| base_date() + chrono::Duration::days(d));
        PetRecord {
            canonical_id,
            source: SourceId::new("test", TosClass::OpenData),
            source_animal_id: None,
            species,
            breed_primary: None,
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size,
            colors,
            markings_text: None,
            intake_type,
            availability: avail,
            chip_status: ChipStatus::NotScanned,
            location: None,
            found_location_text: None,
            photos: vec![],
            first_seen: base_date(),
            last_seen: base_date(),
            last_confirmed: None,
            intake_date,
            outcome_date: None,
        }
    }

    static EMPTY_SCORES: std::sync::OnceLock<HashMap<ulid::Ulid, f64>> = std::sync::OnceLock::new();

    fn default_ctx<'a>(records: &'a [PetRecord], scores: &'a HashMap<ulid::Ulid, f64>) -> MatchContext<'a> {
        MatchContext {
            records,
            visual_scores: scores,
            thresholds: BucketThresholds::default(),
            hold_days: DEFAULT_HOLD_DAYS,
        }
    }

    fn simple_report<'a>(species: Species, colors: &'a [String]) -> ReportInput<'a> {
        ReportInput {
            report_id: "test",
            species,
            lat: None,
            lon: None,
            date: base_date(),
            size: None,
            colors,
        }
    }

    #[test]
    fn cat_report_never_returns_dog() {
        let records = vec![
            make_record(1, Species::Dog, Availability::InCustody, IntakeType::Stray, None, vec![], Some(0)),
            make_record(2, Species::Cat, Availability::InCustody, IntakeType::Stray, None, vec![], Some(0)),
        ];
        let scores = EMPTY_SCORES.get_or_init(HashMap::new).clone();
        let ctx = default_ctx(&records, &scores);
        let params = MatchParams::default();
        let report = simple_report(Species::Cat, &[]);
        let result = match_report(&report, &params, &ctx).unwrap();
        assert_eq!(result.candidates.len(), 1, "only cat should appear");
        assert_eq!(
            result.candidates[0].canonical_id,
            records[1].canonical_id
        );
    }

    #[test]
    fn departed_records_never_appear() {
        let records = vec![
            make_record(1, Species::Dog, Availability::Departed, IntakeType::OwnerSurrender, None, vec![], Some(0)),
            make_record(2, Species::Dog, Availability::InCustody, IntakeType::Stray, None, vec![], Some(0)),
        ];
        let scores = EMPTY_SCORES.get_or_init(HashMap::new).clone();
        let ctx = default_ctx(&records, &scores);
        let params = MatchParams::default();
        let report = simple_report(Species::Dog, &[]);
        let result = match_report(&report, &params, &ctx).unwrap();
        assert_eq!(result.candidates.len(), 1, "Departed record must not appear");
        assert_eq!(result.candidates[0].canonical_id, records[1].canonical_id);
    }

    #[test]
    fn stray_in_hold_sorted_first() {
        let now = Utc::now();
        let report_date = now;

        // Record 1: stray in custody with intake yesterday — within 5-day hold.
        let mut r1 = make_record(
            1,
            Species::Dog,
            Availability::InCustody,
            IntakeType::Stray,
            None,
            vec![],
            None,
        );
        r1.intake_date = Some(now - chrono::Duration::days(1));

        // Record 2: owner surrender intook today (not reclaimable).
        let mut r2 = make_record(
            2,
            Species::Dog,
            Availability::InCustody,
            IntakeType::OwnerSurrender,
            None,
            vec![],
            None,
        );
        r2.intake_date = Some(now);

        let records = vec![r1, r2];
        let scores = EMPTY_SCORES.get_or_init(HashMap::new).clone();
        let ctx = default_ctx(&records, &scores);
        let params = MatchParams::default();
        let report = ReportInput {
            report_id: "r3",
            species: Species::Dog,
            lat: None,
            lon: None,
            date: report_date,
            size: None,
            colors: &[],
        };
        let result = match_report(&report, &params, &ctx).unwrap();

        assert!(
            !result.is_empty(),
            "expected at least one candidate; got 0"
        );
        assert!(
            result.candidates[0].reclaimable_until.is_some(),
            "reclaimable stray should be sorted first; candidates: {:?}",
            result.candidates.iter().map(|c| (c.canonical_id, c.reclaimable_until)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_result_is_not_an_error() {
        let records: Vec<PetRecord> = vec![];
        let scores = EMPTY_SCORES.get_or_init(HashMap::new).clone();
        let ctx = default_ctx(&records, &scores);
        let params = MatchParams::default();
        let report = simple_report(Species::Dog, &[]);
        let result = match_report(&report, &params, &ctx).unwrap();
        assert!(result.is_empty(), "empty store should yield empty candidates, not error");
    }

    #[test]
    fn fusion_score_is_deterministic() {
        let black = vec!["black".to_owned()];
        let records = vec![
            make_record(
                1,
                Species::Dog,
                Availability::InCustody,
                IntakeType::Stray,
                Some(Size::Medium),
                black.clone(),
                Some(0),
            ),
        ];
        let scores = EMPTY_SCORES.get_or_init(HashMap::new).clone();
        let ctx = default_ctx(&records, &scores);
        let params = MatchParams::default();
        let report = ReportInput {
            report_id: "det",
            species: Species::Dog,
            lat: None,
            lon: None,
            date: base_date(),
            size: Some(Size::Medium),
            colors: &black,
        };

        let r1 = match_report(&report, &params, &ctx).unwrap();
        let r2 = match_report(&report, &params, &ctx).unwrap();

        assert_eq!(r1.candidates.len(), 1);
        assert_eq!(r2.candidates.len(), 1);
        let s1 = r1.candidates[0].score;
        let s2 = r2.candidates[0].score;
        assert!(
            (s1 - s2).abs() < 1e-12,
            "fusion must be deterministic for fixed inputs: {s1} != {s2}"
        );
    }

    #[test]
    fn score_is_in_unit_interval() {
        let records = vec![
            make_record(1, Species::Dog, Availability::InCustody, IntakeType::Stray, None, vec![], Some(0)),
        ];
        let scores = EMPTY_SCORES.get_or_init(HashMap::new).clone();
        let ctx = default_ctx(&records, &scores);
        let params = MatchParams::default();
        let report = simple_report(Species::Dog, &[]);
        let result = match_report(&report, &params, &ctx).unwrap();
        for c in &result.candidates {
            assert!(
                c.score >= 0.0 && c.score <= 1.0,
                "score must be in [0,1], got {}",
                c.score
            );
        }
    }

    #[test]
    fn bucket_precision_reported_on_held_out_set() {
        use crate::calibration::{BucketThresholds, report_calibration};
        let pairs: Vec<(f64, bool)> = vec![
            (0.8, true),
            (0.7, true),
            (0.6, false),
            (0.5, true),
            (0.4, false),
            (0.3, false),
            (0.2, false),
            (0.9, true),
            (0.1, false),
            (0.65, true),
        ];
        let thresholds = BucketThresholds::default();
        let cal = report_calibration(&pairs, thresholds);

        assert_eq!(cal.n_pairs, 10);
        assert!(cal.strong_precision >= 0.0 && cal.strong_precision <= 1.0);
        assert!(cal.possible_precision >= 0.0 && cal.possible_precision <= 1.0);
    }

    #[test]
    fn invalid_params_return_error() {
        let records: Vec<PetRecord> = vec![];
        let scores = EMPTY_SCORES.get_or_init(HashMap::new).clone();
        let ctx = default_ctx(&records, &scores);
        let bad_params = MatchParams {
            radius_km: -1.0,
            ..MatchParams::default()
        };
        let report = simple_report(Species::Dog, &[]);
        let result = match_report(&report, &bad_params, &ctx);
        assert!(result.is_err(), "negative radius should return error");
    }
}
