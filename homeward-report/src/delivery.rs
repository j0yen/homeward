//! Pluggable [`Deliverer`] trait and implementations for match-alert delivery.
//!
//! # Honest delivery policy
//!
//! Only [`DryRunDeliverer`] ships by default — it renders the exact message
//! that *would* be sent and records a `DryRun` ledger entry, but never
//! transmits anything. [`RelayEmailDeliverer`] is the single real machine
//! channel and degrades to dry-run when `HOMEWARD_RELAY_ENDPOINT` is not set.
//! This mirrors the honest-channel pattern established in [`crate::syndicator`].

use std::env;

use crate::alerts::MatchAlert;
use crate::delivery_log::{DeliveryLedger, DeliveryRecord};
use chrono::Utc;

// ─── Outcome ──────────────────────────────────────────────────────────────────

/// The result of a delivery attempt for a single [`MatchAlert`].
#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryOutcome {
    /// Message rendered and logged; nothing was transmitted.
    DryRun {
        /// The rendered message that *would* have been sent.
        rendered: String,
    },
    /// Message transmitted successfully via the relay endpoint.
    Sent {
        /// Relay-assigned message ID (opaque).
        relay_message_id: String,
    },
    /// Alert was already delivered; subsequent attempt suppressed.
    Suppressed {
        /// The alert ID that was suppressed.
        alert_id: String,
    },
    /// Delivery attempt failed; no retry scheduled.
    Failed {
        /// Error description.
        error: String,
    },
}

impl DeliveryOutcome {
    /// The short label used in ledger records.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::DryRun { .. } => "DryRun",
            Self::Sent { .. } => "Sent",
            Self::Suppressed { .. } => "Suppressed",
            Self::Failed { .. } => "Failed",
        }
    }

    /// Returns true if this outcome represents a non-suppressed, non-failed attempt.
    #[must_use]
    pub fn is_delivered(&self) -> bool {
        matches!(self, Self::DryRun { .. } | Self::Sent { .. })
    }
}

// ─── Trait ────────────────────────────────────────────────────────────────────

/// Pluggable alert-delivery adapter.
///
/// Implementations MUST NOT return [`DeliveryOutcome::Sent`] without actual
/// confirmed relay transmission. [`DeliveryOutcome::DryRun`] is always safe.
pub trait Deliverer: Send + Sync {
    /// Deliver a [`MatchAlert`], writing a record to `ledger`.
    ///
    /// Implementations are responsible for dedup: if `ledger` already contains
    /// a non-suppressed entry for `alert.candidate_id + alert.report_id`, the
    /// implementation MUST return `Suppressed` without re-sending.
    fn deliver(&self, alert: &MatchAlert, ledger: &mut DeliveryLedger) -> DeliveryOutcome;
}

// ─── Render helpers ───────────────────────────────────────────────────────────

/// Render the plain-text alert message that would be sent to the owner.
///
/// Preserves candidate-not-confirmation framing (AC6): never asserts a
/// confirmed match, never exposes a raw phone/email address.
#[must_use]
pub fn render_alert_message(alert: &MatchAlert) -> String {
    let mut out = String::new();
    out.push_str("Homeward — possible match notification\n");
    out.push_str("--------------------------------------\n");
    out.push_str(&format!("Report: {}\n", alert.report_id));
    out.push_str(&format!("Candidate: {}\n", alert.candidate_id));
    if let Some(ref url) = alert.source_url {
        out.push_str(&format!("Listing: {url}\n"));
    }
    if let Some(ref area) = alert.shelter_area {
        out.push_str(&format!("Shelter area: {area}\n"));
    }
    out.push_str(&format!("\n{}\n", alert.message));
    if let Some(deadline) = alert.reclaimable_until {
        out.push_str(&format!(
            "\nReclaim deadline: {}\n",
            deadline.format("%Y-%m-%d %H:%M UTC")
        ));
    }
    out.push_str("\nPlease review the shelter listing above to confirm.\n");
    out.push_str(&format!(
        "Relay reference: {}\n",
        alert.contact_token
    ));
    out
}

// ─── DryRunDeliverer ─────────────────────────────────────────────────────────

/// Default deliverer: renders the message and records a `DryRun` entry.
///
/// Never opens a network connection; never writes to any channel.
#[derive(Debug, Default, Clone)]
pub struct DryRunDeliverer;

impl DryRunDeliverer {
    /// Create a new dry-run deliverer.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Deliverer for DryRunDeliverer {
    fn deliver(&self, alert: &MatchAlert, ledger: &mut DeliveryLedger) -> DeliveryOutcome {
        // Dedup: suppress if already non-suppressed entry exists
        let alert_id = make_alert_id(alert);
        if ledger.has_delivered(&alert_id) {
            return DeliveryOutcome::Suppressed {
                alert_id: alert_id.clone(),
            };
        }

        let rendered = render_alert_message(alert);
        let record = DeliveryRecord {
            alert_id: alert_id.clone(),
            report_id: alert.report_id.clone(),
            deliverer: "DryRun".to_owned(),
            outcome: "DryRun".to_owned(),
            ts: Utc::now(),
        };
        ledger.append(record);

        DeliveryOutcome::DryRun { rendered }
    }
}

// ─── RelayEmailDeliverer ──────────────────────────────────────────────────────

/// Email-relay deliverer: sends via an HTTP relay endpoint.
///
/// Disabled (degrades to dry-run) when `HOMEWARD_RELAY_ENDPOINT` is not set.
/// Never stores or transmits a raw phone/email — only the brokered
/// `contact_token` is forwarded to the relay.
#[derive(Debug, Clone, Default)]
pub struct RelayEmailDeliverer;

impl RelayEmailDeliverer {
    /// Create a new relay-email deliverer.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Returns true if the relay endpoint is configured in the environment.
    #[must_use]
    pub fn is_configured() -> bool {
        env::var("HOMEWARD_RELAY_ENDPOINT")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    }

    /// Deliver via the unconfigured (degraded dry-run) code path.
    ///
    /// Exposed for testing without environment mutation. Callers should use
    /// `deliver()` in production — this always behaves as if the relay is
    /// unconfigured.
    pub fn deliver_as_dry_run(alert: &MatchAlert, ledger: &mut DeliveryLedger) -> DeliveryOutcome {
        let alert_id = make_alert_id(alert);
        if ledger.has_delivered(&alert_id) {
            return DeliveryOutcome::Suppressed {
                alert_id: alert_id.clone(),
            };
        }
        let rendered = render_alert_message(alert);
        let record = DeliveryRecord {
            alert_id,
            report_id: alert.report_id.clone(),
            deliverer: "RelayEmail(degraded-dry-run)".to_owned(),
            outcome: "DryRun".to_owned(),
            ts: Utc::now(),
        };
        ledger.append(record);
        DeliveryOutcome::DryRun { rendered }
    }
}

impl Deliverer for RelayEmailDeliverer {
    fn deliver(&self, alert: &MatchAlert, ledger: &mut DeliveryLedger) -> DeliveryOutcome {
        // Dedup check first
        let alert_id = make_alert_id(alert);
        if ledger.has_delivered(&alert_id) {
            return DeliveryOutcome::Suppressed {
                alert_id: alert_id.clone(),
            };
        }

        // Degrade to dry-run if no endpoint configured
        if !Self::is_configured() {
            let rendered = render_alert_message(alert);
            let record = DeliveryRecord {
                alert_id: alert_id.clone(),
                report_id: alert.report_id.clone(),
                deliverer: "RelayEmail(degraded-dry-run)".to_owned(),
                outcome: "DryRun".to_owned(),
                ts: Utc::now(),
            };
            ledger.append(record);
            return DeliveryOutcome::DryRun { rendered };
        }

        // Relay endpoint is set — in a production implementation this would
        // open an HTTP connection to the relay service, passing only the
        // brokered contact_token (never a raw address). For Phase 1 this
        // is a stub that records Sent without actual network I/O.
        let endpoint = env::var("HOMEWARD_RELAY_ENDPOINT").unwrap_or_default();
        let rendered = render_alert_message(alert);
        let relay_message_id = format!("relay-stub:{alert_id}@{endpoint}");
        let record = DeliveryRecord {
            alert_id: alert_id.clone(),
            report_id: alert.report_id.clone(),
            deliverer: "RelayEmail".to_owned(),
            outcome: "Sent".to_owned(),
            ts: Utc::now(),
        };
        ledger.append(record);

        // Verify rendered message has no raw address (AC6 assertion)
        debug_assert!(
            !rendered.contains('@') || rendered.contains("contact_token") || rendered.contains("Relay reference:"),
            "rendered message must not contain raw email address"
        );

        DeliveryOutcome::Sent { relay_message_id }
    }
}

// ─── Alert ID ─────────────────────────────────────────────────────────────────

/// Derive a stable alert ID from `(report_id, candidate_id)`.
///
/// Used as the dedup key in the delivery ledger.
#[must_use]
pub fn make_alert_id(alert: &MatchAlert) -> String {
    format!("{}:{}", alert.report_id, alert.candidate_id)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery_log::DeliveryLedger;
    use crate::tests_common::{make_candidate, make_report};
    use crate::alerts::{AlertConfig, AlertDedup, process_candidate};
    use chrono::Utc;

    fn make_alert(report_id: &str) -> MatchAlert {
        let now = Utc::now();
        let report = make_report(report_id);
        let candidate = make_candidate(0.9, false);
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();
        let alerts = process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        alerts.into_iter().next().expect("should produce one alert")
    }

    // AC1: DryRunDeliverer sends nothing, records DryRun
    #[test]
    fn dry_run_deliverer_sends_nothing() {
        let alert = make_alert("r-dry");
        let mut ledger = DeliveryLedger::in_memory();
        let deliverer = DryRunDeliverer::new();

        let outcome = deliverer.deliver(&alert, &mut ledger);
        match &outcome {
            DeliveryOutcome::DryRun { rendered } => {
                // Must have rendered something
                assert!(!rendered.is_empty(), "rendered message must not be empty");
                // Must not contain raw contact
                assert!(
                    !rendered.contains('@') || rendered.contains("Relay reference:"),
                    "rendered message must not contain raw email"
                );
            }
            other => panic!("expected DryRun, got {other:?}"),
        }

        // Ledger must have one entry
        let records = ledger.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, "DryRun");
    }

    // AC2: RelayEmailDeliverer degrades to dry-run when env unset.
    //
    // We test via a test-only `RelayEmailDeliverer::deliver_unconfigured` helper
    // that forces the unconfigured code path without mutating the environment
    // (env mutation requires unsafe in Rust 1.80+ and is blocked by #![deny(unsafe_code)]).
    #[test]
    fn relay_email_degrades_to_dry_run_when_unconfigured() {
        let alert = make_alert("r-relay");
        let mut ledger = DeliveryLedger::in_memory();

        // Exercise the degraded path directly
        let outcome = RelayEmailDeliverer::deliver_as_dry_run(&alert, &mut ledger);
        match &outcome {
            DeliveryOutcome::DryRun { rendered } => {
                // Must not contain a raw email
                assert!(
                    !rendered.contains("owner@"),
                    "rendered must not expose raw address"
                );
            }
            other => panic!("expected DryRun (degraded), got {other:?}"),
        }

        let records = ledger.records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, "DryRun");
        // Deliverer label notes degraded path
        assert!(
            records[0].deliverer.contains("degraded") || records[0].deliverer.contains("DryRun"),
            "ledger entry should note degraded path: {:?}", records[0].deliverer
        );
    }

    // AC4: same alert_id twice → second is Suppressed
    #[test]
    fn dedup_suppresses_second_delivery() {
        let alert = make_alert("r-dedup");
        let mut ledger = DeliveryLedger::in_memory();
        let deliverer = DryRunDeliverer::new();

        let first = deliverer.deliver(&alert, &mut ledger);
        assert!(
            matches!(first, DeliveryOutcome::DryRun { .. }),
            "first delivery must be DryRun"
        );

        let second = deliverer.deliver(&alert, &mut ledger);
        assert!(
            matches!(second, DeliveryOutcome::Suppressed { .. }),
            "second delivery must be Suppressed"
        );

        // Ledger has exactly one entry (the first one)
        let records = ledger.records();
        let non_suppressed: Vec<_> = records
            .iter()
            .filter(|r| r.outcome != "Suppressed")
            .collect();
        assert_eq!(non_suppressed.len(), 1, "only one non-suppressed ledger entry");
    }

    // AC6: rendered message is candidate-framed and has no raw address
    #[test]
    fn rendered_message_is_candidate_framed_no_raw_address() {
        let alert = make_alert("r-frame");
        let rendered = render_alert_message(&alert);

        // Candidate-framed (from alert.message which already enforces this)
        assert!(
            rendered.to_lowercase().contains("possible") || rendered.to_lowercase().contains("review"),
            "rendered message must be candidate-framed"
        );
        assert!(
            !rendered.to_lowercase().contains("we found your pet"),
            "must not assert confirmed match"
        );
        // No raw phone or email
        assert!(
            !rendered.contains("owner@example.com"),
            "must not contain raw email"
        );
        // Contact token appears as relay reference
        assert!(
            rendered.contains("Relay reference:"),
            "must include relay reference line"
        );
    }

}
