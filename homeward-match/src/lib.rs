//! `homeward-match` — fusion matching engine for shelter-pet candidates.
//!
//! Combines structured prefiltering (species, availability, coarse geo,
//! intake-date window) with attribute-agreement scoring and (optionally) a
//! caller-supplied visual similarity score into a single calibrated ranked
//! shortlist.
//!
//! # Candidates, not confirmations
//!
//! The API deliberately returns [`RankedCandidates`] — an ordered list of
//! *possibilities for human review*. There is **no** `confirmed` or `is_match`
//! field anywhere in this crate. The type system enforces the domain's central
//! UX contract: a shelter record is a candidate to investigate, never an
//! automated reunion claim.

pub mod calibration;
pub mod filter;
pub mod fusion;
pub mod hold;
pub mod report;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

// ─── Error ───────────────────────────────────────────────────────────────────

/// Errors from the match pipeline.
#[derive(Debug, thiserror::Error)]
pub enum MatchError {
    /// The supplied radius is not finite or is negative.
    #[error("radius_km must be finite and non-negative, got {0}")]
    InvalidRadius(f64),

    /// The supplied date window is negative.
    #[error("date_window_days must be non-negative, got {0}")]
    InvalidDateWindow(i64),
}

// ─── Score bucket ────────────────────────────────────────────────────────────

/// Calibrated confidence bucket for a candidate match.
///
/// The cut-points are fit on held-out data (see [`calibration`]) and
/// documented in `calibration::BUCKET_THRESHOLDS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreBucket {
    /// Combined score < [`calibration::BUCKET_THRESHOLDS`].possible — low plausibility.
    Weak,
    /// Combined score in [possible, strong) — worth a look.
    Possible,
    /// Combined score ≥ [`calibration::BUCKET_THRESHOLDS`].strong — high plausibility.
    Strong,
}

impl std::fmt::Display for ScoreBucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Weak => f.write_str("weak"),
            Self::Possible => f.write_str("possible"),
            Self::Strong => f.write_str("strong"),
        }
    }
}

// ─── MatchExplanation ────────────────────────────────────────────────────────

/// Names the contributing signals for one candidate match.
///
/// Exposed for transparency and auditability — every candidate must carry
/// an explanation of *why* it was included in the shortlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchExplanation {
    /// Fraction of score contributed by visual similarity (0.0 if not provided).
    pub visual_score: f64,

    /// Fraction of score contributed by attribute agreement (breed/size/color/sex).
    pub attribute_score: f64,

    /// Fraction of score contributed by geographic proximity.
    pub geo_score: f64,

    /// Fraction of score contributed by intake-date recency.
    pub recency_score: f64,

    /// Human-readable narrative of the strongest matching signals.
    pub narrative: String,
}

// ─── Candidate ───────────────────────────────────────────────────────────────

/// A single ranked candidate returned by the match pipeline.
///
/// # No confirmation field
///
/// This type intentionally has **no** `confirmed`, `is_match`, or `found`
/// field. Presenting a candidate to an owner must be framed as "please review
/// this animal" — never "we found your pet."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// Homeward's stable identifier for the shelter record.
    pub canonical_id: Ulid,

    /// Combined calibrated score in [0.0, 1.0].
    pub score: f64,

    /// Calibrated confidence bucket.
    pub bucket: ScoreBucket,

    /// Named contributing signals.
    pub why: MatchExplanation,

    /// Set when the animal is a stray within its legal hold window.
    ///
    /// This candidate should be prioritised — the reclaim window is
    /// time-critical. Value is the deadline by which the owner must
    /// contact the shelter to reclaim.
    pub reclaimable_until: Option<DateTime<Utc>>,
}

// ─── RankedCandidates ────────────────────────────────────────────────────────

/// The result of a match pipeline run: an ordered list of candidates.
///
/// Candidates are sorted descending by score with reclaimable strays
/// promoted over equal-scored non-strays. An empty list means no
/// plausible candidates were found — this is reported clearly to the
/// caller, not treated as an error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedCandidates {
    /// Report ID this result was computed for (for auditability).
    pub report_id: String,

    /// The ordered list of candidates (best first). May be empty.
    pub candidates: Vec<Candidate>,

    /// Parameters used for this run (for reproducibility).
    pub params: MatchParams,
}

impl RankedCandidates {
    /// Returns `true` if no candidates were found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Returns the number of candidates.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }
}

// ─── MatchParams ─────────────────────────────────────────────────────────────

/// Parameters controlling the match pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchParams {
    /// Maximum straight-line distance (km) between shelter location and
    /// report's last-seen location. Default: 80 km (≈ 50 miles).
    pub radius_km: f64,

    /// Accept shelter records whose `intake_date` falls within
    /// `± date_window_days` of the report's `created` date.
    /// Default: 30 days.
    pub date_window_days: i64,

    /// Maximum number of candidates to return. Default: 20.
    pub k: usize,
}

impl Default for MatchParams {
    fn default() -> Self {
        Self {
            radius_km: 80.0,
            date_window_days: 30,
            k: 20,
        }
    }
}

impl MatchParams {
    /// Validate parameters, returning an error if they are out of range.
    ///
    /// # Errors
    /// Returns [`MatchError::InvalidRadius`] if `radius_km` is not finite and
    /// non-negative.
    /// Returns [`MatchError::InvalidDateWindow`] if `date_window_days` is
    /// negative.
    pub fn validate(&self) -> Result<(), MatchError> {
        if !self.radius_km.is_finite() || self.radius_km < 0.0 {
            return Err(MatchError::InvalidRadius(self.radius_km));
        }
        if self.date_window_days < 0 {
            return Err(MatchError::InvalidDateWindow(self.date_window_days));
        }
        Ok(())
    }
}

// ─── Re-exports for convenience ──────────────────────────────────────────────

pub use filter::{prefilter, PrefilterInput, ReportFilter};
pub use fusion::fuse_scores;
pub use hold::stray_hold_deadline;
