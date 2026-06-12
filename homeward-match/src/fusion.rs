//! Score fusion — combines visual, attribute, geo, and recency signals
//! into a single calibrated score in [0.0, 1.0].
//!
//! The fusion is a weighted linear combination (weights sum to 1.0) so the
//! output is inherently in [0.0, 1.0] when each component is in [0.0, 1.0].
//! For fixed inputs the computation is deterministic (AC3).

use chrono::{DateTime, Utc};
use homeward_schema::{PetRecord, Size};

use crate::{MatchExplanation, MatchParams};

// ─── Fusion weights ───────────────────────────────────────────────────────────

/// Component weights for the linear fusion.
///
/// Must sum to 1.0 (verified in the test below).
const W_VISUAL: f64 = 0.40;
const W_ATTRIBUTE: f64 = 0.30;
const W_GEO: f64 = 0.15;
const W_RECENCY: f64 = 0.15;

// ─── Component scorers ────────────────────────────────────────────────────────

/// Compute attribute agreement score in [0.0, 1.0].
///
/// Checks size and color independently and averages the agreement fractions
/// for fields where both sides have data.
#[must_use]
pub fn attribute_score(record: &PetRecord, report_size: Option<Size>, report_colors: &[String]) -> f64 {
    let mut components = Vec::new();

    // Size agreement
    if let (Some(rs), Some(ps)) = (record.size, report_size) {
        components.push(if rs == ps { 1.0_f64 } else { 0.0_f64 });
    }

    // Color agreement — any common token in colour lists
    if !record.colors.is_empty() && !report_colors.is_empty() {
        let record_colors_lower: std::collections::HashSet<String> =
            record.colors.iter().map(|c| c.to_lowercase()).collect();
        let any_match = report_colors
            .iter()
            .any(|c| record_colors_lower.contains(&c.to_lowercase()));
        components.push(if any_match { 1.0_f64 } else { 0.0_f64 });
    }

    if components.is_empty() {
        // No shared attributable fields — neutral
        return 0.5;
    }

    #[allow(clippy::float_arithmetic)]
    let sum: f64 = components.iter().sum();
    let len = components.len();
    // len is bounded by the number of attribute fields (small constant), so precision loss is fine
    #[allow(clippy::float_arithmetic, clippy::cast_precision_loss, clippy::as_conversions)]
    let avg = sum / (len as f64);
    avg
}

/// Compute geographic proximity score in [0.0, 1.0].
///
/// Uses an exponential decay: score = exp(-dist_km / `scale_km`).
/// Returns 0.5 if coordinates are unavailable (neutral).
#[must_use]
#[allow(clippy::float_arithmetic, clippy::option_if_let_else)]
pub fn geo_score(dist_km: Option<f64>, radius_km: f64) -> f64 {
    match dist_km {
        None => 0.5,
        Some(d) => {
            let scale = radius_km / 2.0_f64.ln().recip();
            let score = (-d / scale).exp();
            score.clamp(0.0, 1.0)
        }
    }
}

/// Compute recency score in [0.0, 1.0].
///
/// Uses exponential decay over days since the report date.
/// Returns 0.5 if `intake_date` is unavailable (neutral).
#[must_use]
#[allow(
    clippy::float_arithmetic,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    clippy::option_if_let_else,
    clippy::suboptimal_flops
)]
pub fn recency_score(intake_date: Option<DateTime<Utc>>, report_date: DateTime<Utc>, window_days: i64) -> f64 {
    match intake_date {
        None => 0.5,
        Some(intake) => {
            // num_days() returns i64; unsigned_abs() → u64; cast to f64 is lossy
            // for huge values but date differences are small in practice.
            let days_diff = (intake - report_date).num_days().unsigned_abs() as f64;
            let scale = window_days as f64 / 2.0_f64.ln().recip();
            let score = (-days_diff / scale).exp();
            score.clamp(0.0, 1.0)
        }
    }
}

// ─── Fusion entry point ───────────────────────────────────────────────────────

/// Inputs to the fusion function.
#[derive(Debug, Clone)]
pub struct FusionInput<'a> {
    /// Record being scored.
    pub record: &'a PetRecord,

    /// Visual similarity score from an embedding kNN, in [0.0, 1.0].
    /// Pass `None` when no photo is available.
    pub visual_score: Option<f64>,

    /// Straight-line distance (km) to the report's last-seen location.
    pub dist_km: Option<f64>,

    /// The report's size.
    pub report_size: Option<Size>,

    /// The report's color tokens.
    pub report_colors: &'a [String],

    /// The report's created date.
    pub report_date: DateTime<Utc>,
}

/// Combine component scores into a single calibrated score and explanation.
///
/// The combination is a deterministic weighted linear blend. When
/// `visual_score` is `None`, the visual weight is redistributed to
/// attribute (keeps weights summing to 1.0).
///
/// # Returns
/// `(combined_score, MatchExplanation)` where score ∈ [0.0, 1.0].
#[must_use]
#[allow(clippy::float_arithmetic, clippy::suboptimal_flops)]
pub fn fuse_scores(input: &FusionInput<'_>, params: &MatchParams) -> (f64, MatchExplanation) {
    let visual = input.visual_score.unwrap_or(0.5);
    let attr = attribute_score(input.record, input.report_size, input.report_colors);
    let geo = geo_score(input.dist_km, params.radius_km);
    let rec = recency_score(input.record.intake_date, input.report_date, params.date_window_days);

    // When no visual score, redistribute visual weight to attribute
    let (w_vis, w_attr) = if input.visual_score.is_some() {
        (W_VISUAL, W_ATTRIBUTE)
    } else {
        (0.0, W_VISUAL + W_ATTRIBUTE)
    };

    let combined = (w_vis * visual + w_attr * attr + W_GEO * geo + W_RECENCY * rec).clamp(0.0, 1.0);

    let mut signals = Vec::new();
    if input.visual_score.is_some() {
        signals.push(format!("visual similarity {:.0}%", visual * 100.0));
    }
    if attr > 0.6 {
        signals.push("attribute match (size/color)".to_owned());
    }
    if geo > 0.6 {
        signals.push("nearby shelter location".to_owned());
    }
    if rec > 0.6 {
        signals.push("intake close to lost date".to_owned());
    }
    if signals.is_empty() {
        signals.push("no strong individual signal".to_owned());
    }
    let narrative = signals.join("; ");

    let explanation = MatchExplanation {
        visual_score: visual,
        attribute_score: attr,
        geo_score: geo,
        recency_score: rec,
        narrative,
    };
    (combined, explanation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one() {
        #[allow(clippy::float_arithmetic)]
        let sum = W_VISUAL + W_ATTRIBUTE + W_GEO + W_RECENCY;
        assert!((sum - 1.0).abs() < 1e-10, "weights must sum to 1.0, got {sum}");
    }

    #[test]
    fn geo_score_zero_distance_is_one() {
        let s = geo_score(Some(0.0), 80.0);
        assert!((s - 1.0).abs() < 1e-6, "distance=0 should score ~1.0");
    }

    #[test]
    fn geo_score_none_is_neutral() {
        let s = geo_score(None, 80.0);
        assert_eq!(s, 0.5);
    }

    #[test]
    fn recency_score_zero_days_is_one() {
        use chrono::TimeZone;
        let d = Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap();
        let s = recency_score(Some(d), d, 30);
        assert!((s - 1.0).abs() < 1e-6, "same day should score ~1.0");
    }

    #[test]
    fn attribute_score_no_shared_data_is_neutral() {
        let mut record = make_pet_record();
        record.colors = vec![];
        record.size = None;
        let s = attribute_score(&record, None, &[]);
        assert_eq!(s, 0.5, "no shared attribute data should be neutral 0.5");
    }

    #[test]
    fn attribute_score_size_match_boosts() {
        let mut record = make_pet_record();
        record.size = Some(Size::Medium);
        let s = attribute_score(&record, Some(Size::Medium), &[]);
        assert!(s > 0.5, "matching size should boost attribute score");
    }

    fn make_pet_record() -> PetRecord {
        use chrono::TimeZone;
        use homeward_schema::{Availability, ChipStatus, IntakeType, Species};
        use homeward_schema::provenance::{SourceId, TosClass};
        PetRecord {
            canonical_id: ulid::Ulid::nil(),
            source: SourceId::new("test", TosClass::OpenData),
            source_animal_id: None,
            species: Species::Dog,
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
            first_seen: Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap(),
            last_seen: Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap(),
            last_confirmed: None,
            intake_date: None,
            outcome_date: None,
            secondary_provenances: vec![],
        }
    }
}
