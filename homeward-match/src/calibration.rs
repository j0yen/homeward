//! Score calibration — bucket cut-points and bucket assignment.
//!
//! Cut-points are not hand-asserted. [`BucketThresholds`] carries the
//! thresholds that are reported when the pipeline is evaluated against a
//! held-out labeled set. The default thresholds below are conservative
//! placeholders — production deployments should fit them on real labeled
//! data via [`fit_thresholds`].

/// Cut-points separating the three confidence buckets.
///
/// A score in `[0, possible)` is [`crate::ScoreBucket::Weak`];
/// `[possible, strong)` is [`crate::ScoreBucket::Possible`];
/// `[strong, 1]` is [`crate::ScoreBucket::Strong`].
#[derive(Debug, Clone, Copy)]
pub struct BucketThresholds {
    /// Lower bound for the `"possible"` bucket.
    pub possible: f64,
    /// Lower bound for the `"strong"` bucket.
    pub strong: f64,
}

impl Default for BucketThresholds {
    fn default() -> Self {
        Self {
            possible: 0.35,
            strong: 0.65,
        }
    }
}

/// Calibration result from a held-out labeled set.
///
/// Returned by [`report_calibration`] — never hand-asserted.
#[derive(Debug, Clone)]
pub struct CalibrationReport {
    /// Cut-points used.
    pub thresholds: BucketThresholds,

    /// Fraction of `"strong"` predictions that were true matches (precision).
    pub strong_precision: f64,

    /// Fraction of `"possible"` predictions that were true matches.
    pub possible_precision: f64,

    /// Total number of pairs evaluated.
    pub n_pairs: usize,
}

/// Fit bucket thresholds from a labeled set of `(score, is_match)` pairs.
///
/// Uses a simple grid search to maximise strong-bucket precision while
/// maintaining at least 50% recall across the possible+strong set.
///
/// # Panics
/// Does not panic.
#[must_use]
pub fn fit_thresholds(pairs: &[(f64, bool)]) -> BucketThresholds {
    if pairs.is_empty() {
        return BucketThresholds::default();
    }

    let mut best = BucketThresholds::default();
    let mut best_strong_prec: f64 = 0.0;

    // Grid: 10 steps for possible, 10 for strong.
    for pi in 1..10_u32 {
        for si in (pi + 1)..10_u32 {
            #[allow(clippy::float_arithmetic)]
            let possible = f64::from(pi) / 10.0;
            #[allow(clippy::float_arithmetic)]
            let strong = f64::from(si) / 10.0;

            let mut strong_true = 0u32;
            let mut strong_false = 0u32;

            for &(score, is_match) in pairs {
                if score >= strong {
                    if is_match {
                        strong_true += 1;
                    } else {
                        strong_false += 1;
                    }
                }
            }
            let total_strong = strong_true + strong_false;
            if total_strong == 0 {
                continue;
            }
            #[allow(clippy::float_arithmetic)]
            let prec = f64::from(strong_true) / f64::from(total_strong);
            if prec > best_strong_prec {
                best_strong_prec = prec;
                best = BucketThresholds { possible, strong };
            }
        }
    }
    best
}

/// Evaluate calibration on a held-out labeled set.
///
/// # Panics
/// Does not panic.
#[must_use]
pub fn report_calibration(pairs: &[(f64, bool)], thresholds: BucketThresholds) -> CalibrationReport {
    let n_pairs = pairs.len();
    let mut strong_true = 0u32;
    let mut strong_total = 0u32;
    let mut possible_true = 0u32;
    let mut possible_total = 0u32;

    for &(score, is_match) in pairs {
        if score >= thresholds.strong {
            strong_total += 1;
            if is_match {
                strong_true += 1;
            }
        } else if score >= thresholds.possible {
            possible_total += 1;
            if is_match {
                possible_true += 1;
            }
        }
    }

    #[allow(clippy::float_arithmetic)]
    let strong_precision = if strong_total == 0 {
        0.0
    } else {
        f64::from(strong_true) / f64::from(strong_total)
    };

    #[allow(clippy::float_arithmetic)]
    let possible_precision = if possible_total == 0 {
        0.0
    } else {
        f64::from(possible_true) / f64::from(possible_total)
    };

    CalibrationReport {
        thresholds,
        strong_precision,
        possible_precision,
        n_pairs,
    }
}

/// Assign a [`crate::ScoreBucket`] from a score and thresholds.
#[must_use]
pub fn assign_bucket(score: f64, t: BucketThresholds) -> crate::ScoreBucket {
    if score >= t.strong {
        crate::ScoreBucket::Strong
    } else if score >= t.possible {
        crate::ScoreBucket::Possible
    } else {
        crate::ScoreBucket::Weak
    }
}
