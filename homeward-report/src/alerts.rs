//! Match-alert generation and deduplication.
//!
//! When homeward-match produces a candidate that crosses the configured
//! threshold for an active report, this module:
//!
//! 1. Checks the dedup set — the same `(report_id, candidate_id)` pair is
//!    never re-alerted (AC3).
//! 2. Generates a [`MatchAlert`] framed as "a possible match appeared —
//!    please review" (never "we found your pet") (AC3).
//! 3. If the candidate is a stray in its hold window (has a
//!    `reclaimable_until` timestamp), the alert includes that deadline (AC3).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use homeward_schema::{LostReport, LostStatus, PetRecord, Species};
use serde::{Deserialize, Serialize};

// ─── Match candidate ─────────────────────────────────────────────────────────

/// A ranked candidate from homeward-match.
///
/// `score` is normalised 0–1 (higher is more similar). A candidate is
/// considered actionable when `score >= threshold`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchCandidate {
    /// Candidate pet record from the shelter store.
    pub record: PetRecord,
    /// Similarity score (0–1).
    pub score: f32,
    /// Optional stray-hold reclaim deadline.
    #[serde(default)]
    pub reclaimable_until: Option<DateTime<Utc>>,
}

// ─── Match alert ─────────────────────────────────────────────────────────────

/// An alert delivered to an owner when a possible match appears.
///
/// Framing requirements (AC3):
/// - `message` never asserts a confirmed match.
/// - `reclaimable_until` is populated when the candidate is a stray in hold.
/// - `contact_token` is the brokered relay handle — NOT a raw phone/email.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchAlert {
    /// The report this alert is for.
    pub report_id: String,
    /// The candidate's canonical ID.
    pub candidate_id: String,
    /// Link to the source shelter listing.
    pub source_url: Option<String>,
    /// Coarse area description (city/state, never street address).
    pub shelter_area: Option<String>,
    /// When to reclaim a stray before hold expires (AC3).
    pub reclaimable_until: Option<DateTime<Utc>>,
    /// Human-readable message — framed as a possible match, not a confirmation.
    pub message: String,
    /// Brokered relay handle for the owner (NOT raw contact data).
    pub contact_token: String,
    /// When this alert was generated.
    pub generated_at: DateTime<Utc>,
}

impl MatchAlert {
    /// Assert that this alert does NOT contain a confirmed-match assertion.
    ///
    /// Called in tests (AC3). Returns `true` if the message text is
    /// candidates-framed (contains "possible" or "review").
    #[must_use]
    pub fn is_candidate_framed(&self) -> bool {
        let lower = self.message.to_lowercase();
        (lower.contains("possible") || lower.contains("review"))
            && !lower.contains("we found your pet")
            && !lower.contains("confirmed match")
    }
}

// ─── Alert deduplication ─────────────────────────────────────────────────────

/// A deduplication set for `(report_id, candidate_id)` pairs.
///
/// Once a pair has been alerted it is never re-alerted (AC3).
#[derive(Debug, Default)]
pub struct AlertDedup {
    seen: HashSet<(String, String)>,
}

impl AlertDedup {
    /// Create a new, empty dedup set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return `true` if this `(report_id, candidate_id)` has already been alerted.
    #[must_use]
    pub fn already_seen(&self, report_id: &str, candidate_id: &str) -> bool {
        self.seen.contains(&(report_id.to_owned(), candidate_id.to_owned()))
    }

    /// Record that `(report_id, candidate_id)` has been alerted.
    pub fn mark_seen(&mut self, report_id: &str, candidate_id: &str) {
        self.seen
            .insert((report_id.to_owned(), candidate_id.to_owned()));
    }
}

// ─── Alert generation ────────────────────────────────────────────────────────

/// Configuration for match-alert generation.
#[derive(Debug, Clone)]
pub struct AlertConfig {
    /// Minimum score to trigger an alert (default: 0.8).
    pub threshold: f32,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self { threshold: 0.8 }
    }
}

/// Process a new intake (or intake update) against active reports.
///
/// For each active report whose species matches `candidate.record.species`
/// and whose score meets `cfg.threshold`:
/// - Skip if already alerted (dedup).
/// - Generate a [`MatchAlert`] with candidate-framed message (AC3).
/// - Record in `dedup`.
///
/// Returns the list of new alerts generated.
pub fn process_candidate(
    candidate: &MatchCandidate,
    reports: &[&LostReport],
    dedup: &mut AlertDedup,
    cfg: &AlertConfig,
    now: DateTime<Utc>,
) -> Vec<MatchAlert> {
    if candidate.score < cfg.threshold {
        return vec![];
    }

    let candidate_id = candidate.record.canonical_id.to_string();

    // Collect matching reports first (immutable borrow of dedup in filter,
    // then mutable borrow in the loop below — avoids the E0500 overlap).
    let matching: Vec<&&LostReport> = reports
        .iter()
        .filter(|r| r.status == LostStatus::Active && r.species == candidate.record.species)
        .filter(|r| !dedup.already_seen(&r.report_id, &candidate_id))
        .collect();

    let mut alerts = Vec::with_capacity(matching.len());
    for r in matching {
        dedup.mark_seen(&r.report_id, &candidate_id);
        alerts.push(build_alert(r, candidate, &candidate_id, now));
    }
    alerts
}

fn build_alert(
    report: &LostReport,
    candidate: &MatchCandidate,
    candidate_id: &str,
    now: DateTime<Utc>,
) -> MatchAlert {
    let shelter_area = candidate.record.location.as_ref().map(|loc| {
        loc.state.as_ref().map_or_else(
            || loc.city_county.clone(),
            |state| format!("{}, {}", loc.city_county, state),
        )
    });

    let source_url = None::<String>; // PhotoRef has no source_url field; link provided separately

    let mut message = format!(
        "A possible match for your {} appeared",
        species_label(report.species),
    );
    if let Some(ref area) = shelter_area {
        message.push_str(&format!(" at {area}"));
    }
    message.push_str(" — please review the animal at the shelter listing to confirm.");

    let reclaimable_until = candidate.reclaimable_until;
    if let Some(deadline) = reclaimable_until {
        message.push_str(&format!(
            " This animal is a stray in hold; reclaim deadline: {}.",
            deadline.format("%Y-%m-%d %H:%M UTC")
        ));
    }

    MatchAlert {
        report_id: report.report_id.clone(),
        candidate_id: candidate_id.to_owned(),
        source_url,
        shelter_area,
        reclaimable_until,
        message,
        contact_token: report.contact.token().to_owned(),
        generated_at: now,
    }
}

const fn species_label(species: Species) -> &'static str {
    match species {
        Species::Dog => "dog",
        Species::Cat => "cat",
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_common::{make_candidate, make_report};

    #[test]
    fn alert_is_candidate_framed() {
        let now = Utc::now();
        let report = make_report("r1");
        let candidate = make_candidate(0.9, false);
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();
        let alerts =
            process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert_eq!(alerts.len(), 1);
        let alert = &alerts[0];
        assert!(
            alert.is_candidate_framed(),
            "alert must be candidate-framed: {}", alert.message
        );
        assert!(
            !alert.message.to_lowercase().contains("we found your pet"),
            "must not assert confirmed match"
        );
    }

    #[test]
    fn stray_in_hold_includes_reclaim_deadline() {
        let now = Utc::now();
        let report = make_report("r1");
        let candidate = make_candidate(0.9, true); // stray in hold
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();
        let alerts =
            process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert_eq!(alerts.len(), 1);
        assert!(
            alerts[0].reclaimable_until.is_some(),
            "stray-in-hold alert must include reclaimable_until"
        );
        assert!(
            alerts[0].message.contains("reclaim deadline"),
            "message must mention reclaim deadline"
        );
    }

    #[test]
    fn dedup_prevents_re_alert() {
        let now = Utc::now();
        let report = make_report("r1");
        let candidate = make_candidate(0.9, false);
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();
        // First call — should generate 1 alert
        let first =
            process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert_eq!(first.len(), 1);
        // Second call — same candidate, should be deduped
        let second =
            process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert!(second.is_empty(), "same candidate must not be re-alerted");
    }

    #[test]
    fn below_threshold_no_alert() {
        let now = Utc::now();
        let report = make_report("r1");
        let candidate = make_candidate(0.5, false); // below 0.8 threshold
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();
        let alerts =
            process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert!(alerts.is_empty(), "below-threshold candidate must not alert");
    }

    #[test]
    fn species_mismatch_no_alert() {
        use homeward_schema::Species;
        let now = Utc::now();
        let mut report = make_report("r1");
        report.species = Species::Cat; // report is for a cat
        let candidate = make_candidate(0.95, false); // candidate is a dog (see make_candidate)
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();
        let alerts =
            process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert!(
            alerts.is_empty(),
            "species mismatch must not generate alert"
        );
    }

    #[test]
    fn contact_token_not_raw_contact() {
        let now = Utc::now();
        let report = make_report("r1");
        let candidate = make_candidate(0.9, false);
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();
        let alerts =
            process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert_eq!(alerts.len(), 1);
        let token = &alerts[0].contact_token;
        // Must not look like a raw phone or email
        assert!(
            !token.contains('@') && !token.starts_with('+'),
            "contact_token must be opaque relay handle, not raw contact: {token}"
        );
    }
}
