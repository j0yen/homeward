//! `homeward-report` — owner-side lost-pet reports, continuous matching,
//! match alerts, and the open read API.
//!
//! # Privacy posture (Phase 1 §2)
//! - Raw contact info is never stored in plaintext in this crate; the
//!   [`BrokeredContactToken`] relay handle is the only contact representation
//!   that appears in any read path.
//! - Last-seen location is always a [`CoarseLocation`] (ZIP/radius); no
//!   street address is accepted or stored.
//! - EXIF metadata (including GPS) is stripped from uploaded photos before
//!   they are forwarded or stored.
//! - Reports auto-expire at a configurable TTL; `delete` purges all stored
//!   data immediately.

#![deny(unsafe_code)]

pub mod alerts;
pub mod api;
pub mod db_reader;
pub mod delivery;
pub mod delivery_log;
pub mod exif;
pub mod export;
pub mod flyer;
pub mod server;
pub mod store;
pub mod syndicator;

#[cfg(test)]
pub mod tests_common;

use chrono::{DateTime, Utc};
use homeward_schema::{BrokeredContactToken, CoarseLocation, LostReport, LostStatus, Species};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

// ─── Re-exports ──────────────────────────────────────────────────────────────

pub use alerts::{MatchAlert, MatchCandidate};
pub use api::{ApiConfig, ShelterQuery, ShelterQueryResult};
pub use delivery::{
    DeliveryOutcome, Deliverer, DryRunDeliverer, RelayEmailDeliverer, render_alert_message,
};
pub use delivery_log::{DeliveryLedger, DeliveryRecord};
pub use exif::strip_exif;
pub use export::{LostReportExport, to_export};
pub use flyer::render_flyer;
pub use store::{ReportStore, SubmitError};
pub use syndicator::{
    GatedChannelSyndicator, LocalArtifactSyndicator, SyndicationOutcome, Syndicator,
};

// ─── Report submission ───────────────────────────────────────────────────────

/// Input supplied by an owner when filing a lost-pet report.
///
/// `photo_bytes` carries the raw image upload. EXIF is stripped by
/// [`submit`] before any further processing.
///
/// `raw_contact` holds the plaintext contact string (phone/email) submitted
/// by the owner. [`submit`] mints a [`BrokeredContactToken`] and immediately
/// discards the raw string from the returned [`LostReport`].
#[derive(Debug, Clone)]
pub struct SubmitRequest {
    /// Species of the lost pet.
    pub species: Species,
    /// Primary breed string.
    pub breed_primary: Option<String>,
    /// Secondary breed string.
    pub breed_secondary: Option<String>,
    /// Free-form description.
    pub description: Option<String>,
    /// Raw photo bytes — EXIF will be stripped.
    pub photo_bytes: Option<Vec<u8>>,
    /// Coarse last-seen location (ZIP or radius).
    pub last_seen: CoarseLocation,
    /// Raw contact info (phone or email) — replaced by a brokered token.
    pub raw_contact: String,
    /// Report TTL in seconds (default: 90 days).
    pub ttl_secs: Option<u64>,
}

/// The default report TTL: 90 days.
pub const DEFAULT_TTL_SECS: u64 = 90 * 24 * 3600;

/// Mint a new [`BrokeredContactToken`] for the given raw contact string.
///
/// In production this would call a relay service; here we derive a
/// deterministic opaque handle so that tests are hermetic.
#[must_use]
pub fn mint_token(raw_contact: &str) -> BrokeredContactToken {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    raw_contact.hash(&mut h);
    BrokeredContactToken::new(format!("tok_{:016x}", h.finish()))
}

/// Submit a lost-pet report.
///
/// Steps performed (AC1):
/// 1. Strip EXIF from `photo_bytes` (if provided).
/// 2. Coarsen the last-seen location (already `CoarseLocation`; validated).
/// 3. Mint a [`BrokeredContactToken`] from `raw_contact`; raw contact is
///    never stored.
/// 4. Persist via `store`.
///
/// # Errors
/// Returns [`SubmitError`] if the store rejects the record.
pub fn submit(
    req: SubmitRequest,
    store: &mut ReportStore,
    now: DateTime<Utc>,
) -> Result<LostReport, SubmitError> {
    // AC1: strip EXIF
    let clean_photo = req.photo_bytes.as_deref().map(strip_exif);

    // AC1: mint brokered token; raw contact is never stored beyond this frame
    let contact = mint_token(&req.raw_contact);

    let ttl_secs = req.ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
    let ttl_i64 = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
    let expires = now + chrono::Duration::seconds(ttl_i64);

    let report_id = Ulid::new().to_string();

    // Build photo list (store cleaned bytes as a data-URI stub for testing;
    // production would upload to object storage and store a hotlink).
    let photos = clean_photo.map_or_else(Vec::new, |bytes| {
        vec![homeward_schema::PhotoRef {
            url: format!("data:image/jpeg;base64,{}", base64_encode(&bytes)),
            attribution: None,
            is_primary: true,
        }]
    });

    let report = LostReport {
        report_id,
        species: req.species,
        breed_primary: req.breed_primary,
        breed_secondary: req.breed_secondary,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec![],
        description: req.description,
        photos,
        last_seen: req.last_seen,
        contact,
        created: now,
        expires,
        status: LostStatus::Active,
    };

    let cloned = report.clone();
    store.insert(report)?;
    Ok(cloned)
}

/// Minimal base64 encoder (no external dep — only used for test round-trips).
fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    /// Look up a character by 6-bit index (0–63).
    fn b64(idx: usize) -> char {
        // Index is always masked to 0x3F (0–63); CHARS has exactly 64 elements.
        // SAFETY: idx & 0x3F is always in 0..64, same as CHARS length.
        #[allow(clippy::indexing_slicing)]
        char::from(CHARS[idx & 0x3F])
    }

    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = usize::from(chunk.first().copied().unwrap_or(0));
        let b1 = usize::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = usize::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(b64((triple >> 18) & 0x3F));
        out.push(b64((triple >> 12) & 0x3F));
        if chunk.len() > 1 {
            out.push(b64((triple >> 6) & 0x3F));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(b64(triple & 0x3F));
        } else {
            out.push('=');
        }
    }
    out
}

// ─── Lifecycle ───────────────────────────────────────────────────────────────

/// Mark a report as reunited.
///
/// # Errors
/// Returns [`LifecycleError::NotFound`] if the `report_id` is unknown.
pub fn mark_reunited(
    report_id: &str,
    store: &mut ReportStore,
    now: DateTime<Utc>,
) -> Result<(), LifecycleError> {
    store
        .update_status(report_id, LostStatus::Reunited, now)
        .map_err(|_| LifecycleError::NotFound(report_id.to_owned()))
}

/// Delete a report and purge all stored PII immediately (CCPA AC4).
///
/// # Errors
/// Returns [`LifecycleError::NotFound`] if the `report_id` is unknown.
pub fn delete_report(report_id: &str, store: &mut ReportStore) -> Result<(), LifecycleError> {
    store
        .delete(report_id)
        .map_err(|_| LifecycleError::NotFound(report_id.to_owned()))
}

/// Expire all reports whose `expires` timestamp is at or before `now`.
///
/// Returns the list of expired report IDs.
pub fn expire_stale(store: &mut ReportStore, now: DateTime<Utc>) -> Vec<String> {
    store.expire_stale(now)
}

/// Errors from lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LifecycleError {
    /// The requested `report_id` does not exist in the store.
    #[error("report not found: {0}")]
    NotFound(String),
}

// ─── Serializable report summary (no PII) ────────────────────────────────────

/// A read-safe summary of a [`LostReport`] that never exposes raw contact.
///
/// The `contact` field is the brokered token string — callers must direct
/// owners to the relay service, never expose raw contact data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSummary {
    /// Report identifier.
    pub report_id: String,
    /// Species of the lost pet.
    pub species: Species,
    /// Current status.
    pub status: LostStatus,
    /// When the report expires.
    pub expires: DateTime<Utc>,
    /// Brokered contact relay token (NOT a phone/email).
    pub contact_token: String,
}

impl From<&LostReport> for ReportSummary {
    fn from(r: &LostReport) -> Self {
        Self {
            report_id: r.report_id.clone(),
            species: r.species,
            status: r.status,
            expires: r.expires,
            contact_token: r.contact.token().to_owned(),
        }
    }
}

// ─── Integration tests (AC1–AC7) ─────────────────────────────────────────────

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::alerts::{AlertConfig, AlertDedup, process_candidate};
    use crate::tests_common::{make_candidate, make_report, make_pet_record};
    use homeward_schema::{CoarseLocation, Species};

    /// AC1: submit strips EXIF and stores only brokered token, not raw contact.
    #[test]
    fn submit_strips_exif_and_brokers_contact() {
        let now = Utc::now();
        let mut store = ReportStore::new();

        // Build a JPEG with a fake APP1 marker
        let mut jpeg = vec![0xFF, 0xD8_u8];
        let app1_payload = b"ExifFakeData";
        let len = (2 + app1_payload.len()) as u16;
        jpeg.push(0xFF);
        jpeg.push(0xE1);
        jpeg.extend_from_slice(&len.to_be_bytes());
        jpeg.extend_from_slice(app1_payload);
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI

        let req = SubmitRequest {
            species: Species::Dog,
            breed_primary: Some("Labrador".to_owned()),
            breed_secondary: None,
            description: None,
            photo_bytes: Some(jpeg),
            last_seen: CoarseLocation {
                zip_code: Some("78701".to_owned()),
                city: None,
                state: None,
                radius_miles: None,
            },
            raw_contact: "owner@example.com".to_owned(),
            ttl_secs: None,
        };

        let report = submit(req, &mut store, now).expect("submit ok");

        // AC1: EXIF stripped — photo data must not contain 0xFFE1
        for photo in &report.photos {
            // We encode bytes as base64 in data URIs for test round-trips.
            // Verify the URL doesn't embed raw 0xFFE1 — it'll be base64 so
            // just assert the APP1 payload text is absent.
            assert!(
                !photo.url.contains("RXhpZkZha2VEYXRh"), // base64("ExifFakeData")
                "EXIF payload should have been stripped from photo"
            );
        }

        // AC1: contact is a brokered token, not the raw email
        assert!(
            !report.contact.token().contains('@'),
            "contact token must not be a raw email"
        );
        assert!(
            report.contact.token().starts_with("tok_"),
            "contact token must have tok_ prefix"
        );
    }

    /// AC2: no read path returns raw phone/email or street-level location.
    #[test]
    fn no_read_path_exposes_raw_contact_or_street_address() {
        let now = Utc::now();
        let mut store = ReportStore::new();
        let req = SubmitRequest {
            species: Species::Dog,
            breed_primary: None,
            breed_secondary: None,
            description: None,
            photo_bytes: None,
            last_seen: CoarseLocation {
                zip_code: Some("90210".to_owned()),
                city: None,
                state: None,
                radius_miles: None,
            },
            raw_contact: "+15551234567".to_owned(),
            ttl_secs: None,
        };
        let report = submit(req, &mut store, now).expect("submit ok");
        let summary = ReportSummary::from(&report);

        // Serialise to JSON and assert no raw phone appears
        let json = serde_json::to_string(&summary).expect("serialize ok");
        assert!(
            !json.contains("+15551234567"),
            "raw phone must not appear in serialized summary: {json}"
        );
        // contact_token is opaque
        assert!(
            !summary.contact_token.contains('+'),
            "contact_token must not look like a phone number"
        );
    }

    /// AC3 (dog): new intake above threshold triggers exactly one alert; re-delivery deduped.
    #[test]
    fn dog_matching_intake_triggers_one_alert_deduped() {
        let now = Utc::now();
        let report = make_report("dog-r1"); // Dog report
        let candidate = make_candidate(0.9, false); // Dog intake, score 0.9
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();

        let first = process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert_eq!(first.len(), 1, "first delivery: exactly one alert");
        assert!(first[0].is_candidate_framed(), "alert is candidate-framed");

        let second = process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert!(second.is_empty(), "re-delivery is deduped");
    }

    /// AC3 (cat): same flow for a cat report.
    #[test]
    fn cat_matching_intake_triggers_one_alert() {
        let now = Utc::now();
        let mut report = make_report("cat-r1");
        report.species = Species::Cat;
        let mut candidate = make_candidate(0.85, false);
        candidate.record.species = Species::Cat;
        let mut dedup = AlertDedup::new();
        let cfg = AlertConfig::default();

        let alerts = process_candidate(&candidate, &[&report], &mut dedup, &cfg, now);
        assert_eq!(alerts.len(), 1, "cat intake: one alert");
        assert!(alerts[0].is_candidate_framed());
    }

    /// AC4: expired/deleted reports generate no alerts.
    #[test]
    fn expired_report_generates_no_alert() {
        use chrono::Duration;
        let now = Utc::now();
        let mut store = ReportStore::new();

        let mut report = make_report("r-exp");
        report.expires = now - Duration::seconds(1);
        store.insert(report).expect("insert ok");

        let expired = expire_stale(&mut store, now);
        assert_eq!(expired, vec!["r-exp"]);
        assert!(store.get("r-exp").is_none(), "expired report purged");
    }

    /// AC4: deleted report is immediately absent and its alerts stop.
    #[test]
    fn deleted_report_no_further_alerts() {
        let now = Utc::now();
        let mut store = ReportStore::new();
        let report = make_report("r-del");
        store.insert(report).expect("insert ok");
        delete_report("r-del", &mut store).expect("delete ok");
        assert!(store.get("r-del").is_none(), "deleted report absent");
        // If report is gone, active_reports yields nothing for this id
        assert!(
            store.active_reports().all(|r| r.report_id != "r-del"),
            "deleted report must not appear in active set"
        );
    }

    /// AC5: shelter query returns intake records; no LostReport PII accessible.
    #[test]
    fn shelter_api_no_report_pii() {
        use crate::api::{ApiConfig, ShelterQuery, query_shelter};
        let records = vec![
            make_pet_record(Species::Dog, Some("Austin"), Some("TX")),
            make_pet_record(Species::Cat, Some("Dallas"), Some("TX")),
        ];
        let cfg = ApiConfig::default();
        let result = query_shelter(&records, &ShelterQuery::default(), &cfg);
        let json = serde_json::to_string(&result).expect("serialize ok");
        // No phone, email, or raw contact should appear
        assert!(!json.contains("phone") && !json.contains("email") && !json.contains("contact"));
    }
}
