//! Integration tests for homeward-report federation export, flyer, and syndicator.
//!
//! 7 tests covering:
//!   1. JSON-LD well-formedness (context, type, status fields)
//!   2. Contact is brokered — raw owner email absent from export JSON
//!   3. Photo is a URL reference, not bytes
//!   4. Flyer determinism — same input → byte-identical output
//!   5. Flyer contains required fields (LOST:, Last seen:, Contact:)
//!   6. LocalArtifactSyndicator writes both artifacts to disk
//!   7. GatedChannelSyndicator returns ManualOnly for pawboost/nextdoor/facebook/petco

use chrono::{Duration, Utc};
use homeward_report::{
    GatedChannelSyndicator, LocalArtifactSyndicator, LostReportExport, SyndicationOutcome,
    Syndicator,
};
use homeward_report::flyer::render_flyer;
use homeward_schema::{
    BrokeredContactToken, CoarseLocation, LostReport, LostStatus, PhotoRef, Species,
};

/// Build a fixture [`LostReport`] with a known "owner_email" baked into context.
///
/// LostReport has no raw owner_email field (by design — privacy). The raw contact
/// was "owner@example.com"; it was minted into a brokered token at submit time.
/// We use the raw string as a sentinel to assert it never leaks into the export.
const OWNER_EMAIL: &str = "owner@example.com";
const OWNER_PHONE: &str = "+15551234567";

fn fixture_report() -> LostReport {
    let now = Utc::now();
    LostReport {
        report_id: "test-report-001".to_owned(),
        species: Species::Dog,
        breed_primary: Some("Labrador".to_owned()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec!["black".to_owned(), "white".to_owned()],
        description: Some("Friendly dog with a red collar".to_owned()),
        photos: vec![PhotoRef {
            url: "https://example.com/photos/buddy.jpg".to_owned(),
            attribution: None,
            is_primary: true,
        }],
        last_seen: CoarseLocation {
            zip_code: Some("78701".to_owned()),
            city: Some("Austin".to_owned()),
            state: Some("TX".to_owned()),
            radius_miles: None,
        },
        // BrokeredContactToken — NOT the raw email/phone. The raw strings
        // (OWNER_EMAIL, OWNER_PHONE) exist only as test sentinels to check
        // they never leak into the export JSON.
        contact: BrokeredContactToken::new("tok_0123456789abcdef".to_owned()),
        created: now,
        expires: now + Duration::days(90),
        status: LostStatus::Active,
    }
}

// ─── Test 1 ──────────────────────────────────────────────────────────────────

/// JSON-LD export is well-formed: parses as valid JSON, and @context, @type,
/// homeward:lostStatus (or equivalent status field) are present.
#[test]
fn export_json_ld_well_formed() {
    let report = fixture_report();
    let export = LostReportExport::from_report(&report);
    let json = serde_json::to_string_pretty(&export).expect("serialize ok");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    // @context and @type must be top-level keys
    assert!(
        parsed.get("@context").is_some(),
        "@context must be present in JSON-LD: {json}"
    );
    assert!(
        parsed.get("@type").is_some(),
        "@type must be present in JSON-LD: {json}"
    );

    // homeward:lostStatus (or homeward:reportId) must be present
    assert!(
        json.contains("homeward:"),
        "at least one homeward-namespaced field must be present: {json}"
    );
    assert!(
        json.contains("active") || json.contains("lostStatus"),
        "status field must be serialized: {json}"
    );
}

// ─── Test 2 ──────────────────────────────────────────────────────────────────

/// The export's contact field never contains raw owner email or phone.
/// The owner's raw contact (OWNER_EMAIL, OWNER_PHONE) must not appear anywhere
/// in the serialized export JSON.
#[test]
fn export_contact_brokered_not_raw() {
    let report = fixture_report();
    let export = LostReportExport::from_report(&report);
    let json = serde_json::to_string(&export).expect("serialize ok");

    assert!(
        !json.contains(OWNER_EMAIL),
        "raw owner email must not appear in export JSON: {json}"
    );
    assert!(
        !json.contains(OWNER_PHONE),
        "raw owner phone must not appear in export JSON: {json}"
    );
    // The brokered token must be present
    assert!(
        json.contains("tok_"),
        "brokered token must appear in export JSON: {json}"
    );
}

// ─── Test 3 ──────────────────────────────────────────────────────────────────

/// Export photo is a URL reference (string starting with "http" or "data:"),
/// not raw bytes.
#[test]
fn export_photo_is_reference() {
    let report = fixture_report();
    let export = LostReportExport::from_report(&report);

    // The fixture has one photo with URL "https://example.com/photos/buddy.jpg"
    assert_eq!(export.photos.len(), 1, "export should have one photo");

    let photo = &export.photos[0];
    // Photo content_url must be a string URL, not embedded bytes
    assert!(
        photo.content_url.starts_with("http") || photo.content_url.starts_with("data:"),
        "photo must be a URL reference, got: {}",
        photo.content_url
    );
    // For hotlinks (not data URIs) assert it starts with http
    if !photo.content_url.starts_with("data:") {
        assert!(
            photo.content_url.starts_with("http"),
            "hotlink photo must start with http: {}",
            photo.content_url
        );
    }
}

// ─── Test 4 ──────────────────────────────────────────────────────────────────

/// render_flyer is deterministic: two calls with the same LostReport produce
/// byte-identical output.
#[test]
fn flyer_deterministic() {
    // Use a fixed timestamp to ensure full determinism
    let now = chrono::DateTime::parse_from_rfc3339("2026-06-11T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let report = LostReport {
        report_id: "det-001".to_owned(),
        species: Species::Dog,
        breed_primary: Some("Beagle".to_owned()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec!["brown".to_owned()],
        description: Some("Friendly beagle".to_owned()),
        photos: vec![],
        last_seen: CoarseLocation {
            zip_code: Some("90210".to_owned()),
            city: Some("Beverly Hills".to_owned()),
            state: Some("CA".to_owned()),
            radius_miles: None,
        },
        contact: BrokeredContactToken::new("tok_aabbccdd11223344".to_owned()),
        created: now,
        expires: now + Duration::days(90),
        status: LostStatus::Active,
    };

    let flyer1 = render_flyer(&report);
    let flyer2 = render_flyer(&report);

    assert_eq!(
        flyer1, flyer2,
        "render_flyer must be deterministic (byte-identical output)"
    );
}

// ─── Test 5 ──────────────────────────────────────────────────────────────────

/// render_flyer output contains required structural strings.
#[test]
fn flyer_contains_required_fields() {
    let report = fixture_report();
    let flyer = render_flyer(&report);

    assert!(
        flyer.contains("LOST:"),
        "flyer must contain 'LOST:' header: {flyer}"
    );
    assert!(
        flyer.contains("Last seen:"),
        "flyer must contain 'Last seen:' line: {flyer}"
    );
    assert!(
        flyer.contains("Contact:"),
        "flyer must contain 'Contact:' line: {flyer}"
    );
}

// ─── Test 6 ──────────────────────────────────────────────────────────────────

/// LocalArtifactSyndicator writes both JSON-LD and flyer artifacts to disk
/// and returns Written with 2 paths.
#[test]
fn local_syndicator_writes_both_artifacts() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let syndicator = LocalArtifactSyndicator::new(tmp.path());

    let report = fixture_report();
    let export = LostReportExport::from_report(&report);
    let flyer = render_flyer(&report);

    let outcome = syndicator.syndicate(&export, &flyer);

    match outcome {
        SyndicationOutcome::Written { paths } => {
            assert_eq!(paths.len(), 2, "must write exactly 2 artifacts, got: {paths:?}");
            for p in &paths {
                assert!(p.exists(), "artifact must exist on disk: {p:?}");
            }
            // One must be .jsonld, one must be .flyer.txt
            let has_jsonld = paths.iter().any(|p| {
                p.extension().and_then(|e| e.to_str()) == Some("jsonld")
            });
            let has_flyer = paths.iter().any(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".flyer.txt"))
            });
            assert!(has_jsonld, "one artifact must be .jsonld");
            assert!(has_flyer, "one artifact must be .flyer.txt");
        }
        other => panic!("expected Written, got {other:?}"),
    }
}

// ─── Test 7 ──────────────────────────────────────────────────────────────────

/// GatedChannelSyndicator returns ManualOnly for all gated social channels.
/// None of them return Written or Failed.
#[test]
fn gated_channels_return_manual_only_not_success() {
    let report = fixture_report();
    let export = LostReportExport::from_report(&report);
    let flyer = render_flyer(&report);

    let gated_channels: &[(&str, &str)] = &[
        ("PawBoost", "No third-party write API; shelter-inbound only."),
        (
            "Nextdoor",
            "Share Content endpoint is partnership-approval-gated.",
        ),
        (
            "Facebook Groups",
            "Facebook Groups API deprecated April 2024.",
        ),
        (
            "Petco Love Lost",
            "Petco Love Lost is closed / partner-only.",
        ),
    ];

    for (channel, reason) in gated_channels {
        let syndicator = GatedChannelSyndicator::new(*channel, *reason);
        let outcome = syndicator.syndicate(&export, &flyer);

        match &outcome {
            SyndicationOutcome::ManualOnly { channel: c, .. } => {
                assert_eq!(
                    c.as_str(),
                    *channel,
                    "ManualOnly channel name must match: {c}"
                );
            }
            SyndicationOutcome::Written { .. } => {
                panic!("gated channel '{channel}' must NOT return Written");
            }
            SyndicationOutcome::Failed { .. } => {
                panic!("gated channel '{channel}' must NOT return Failed (no delivery attempted)");
            }
            SyndicationOutcome::DryRun { .. } => {
                panic!(
                    "gated channel '{channel}' must return ManualOnly, not DryRun"
                );
            }
        }
    }
}
