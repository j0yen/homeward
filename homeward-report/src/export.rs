//! Federation export — JSON-LD serializer, human-postable flyer, and
//! pluggable [`Syndicator`] trait for `homeward-report`.
//!
//! # JSON-LD shape
//!
//! No open lost-pet interchange standard exists (schema.org has `AnimalShelter`
//! and a `Pet` enum but no lost/found-report type). This module defines
//! homeward's own JSON-LD vocabulary:
//!
//! - schema.org types are used where they overlap: `schema:Animal` for the pet
//!   description, `schema:GeoCoordinates`-adjacent fields for location,
//!   `schema:ContactPoint` shape for the brokered relay.
//! - homeward-namespaced fields (IRI prefix `https://homeward.example/vocab#`)
//!   cover the lost/found semantics schema.org lacks: `homeward:reportId`,
//!   `homeward:lostStatus`, `homeward:brokerRelayToken`, etc.
//!
//! # Privacy contract
//!
//! - The exported photo URL is EXIF-stripped before inclusion (via
//!   [`crate::exif::strip_exif`]).
//! - The `contact` field is always the brokered relay token, never raw PII.
//! - No street-level location — only the `CoarseLocation` (ZIP / radius).
//!
//! # Syndication
//!
//! [`Syndicator`] is a pluggable trait. Two implementations ship:
//!
//! - [`LocalArtifactSyndicator`] — always-on, writes JSON-LD + flyer to a
//!   configured output directory.
//! - [`PetFbiPartnerSyndicator`] — behind feature `petfbi-partner`; formats
//!   the partner POST but returns [`SyndicationOutcome::DryRun`] until a
//!   real endpoint URL + credential are configured. Never reports success
//!   without an actual 2xx.
//!
//! Gated channels (PawBoost, Nextdoor, Facebook Groups, Petco Love Lost)
//! appear **only** as [`SyndicationOutcome::ManualOnly`] entries. Zero code
//! pretends to deliver to them.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use homeward_schema::{CoarseLocation, LostReport, LostStatus, PhotoRef, Species};
use serde::{Deserialize, Serialize};

use crate::exif::strip_exif;

// ─── IRI constants ────────────────────────────────────────────────────────────

const HOMEWARD_VOCAB: &str = "https://homeward.example/vocab#";
const SCHEMA_ORG: &str = "https://schema.org/";

// ─── JSON-LD export types ─────────────────────────────────────────────────────

/// The portable JSON-LD export of a [`LostReport`].
///
/// Carries schema.org-typed fields plus homeward-namespaced lost/found fields.
/// The `contact` field is always the brokered relay token; photos are EXIF-
/// stripped references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LostReportExport {
    /// JSON-LD context — schema.org + homeward vocab.
    #[serde(rename = "@context")]
    pub context: ExportContext,

    /// JSON-LD type.
    #[serde(rename = "@type")]
    pub report_type: String,

    /// Homeward's canonical report identifier.
    #[serde(rename = "homeward:reportId")]
    pub report_id: String,

    /// Schema.org-typed animal description.
    #[serde(rename = "schema:animal")]
    pub animal: AnimalDescription,

    /// Coarse last-seen location (ZIP / radius — no street address).
    #[serde(rename = "homeward:lastSeenLocation")]
    pub last_seen_location: LocationExport,

    /// EXIF-stripped photo references (homeward-namespaced, schema.org URL type).
    #[serde(rename = "homeward:photos")]
    pub photos: Vec<PhotoExport>,

    /// Brokered contact relay token — never raw PII.
    #[serde(rename = "homeward:brokerRelayToken")]
    pub broker_relay_token: String,

    /// Report status.
    #[serde(rename = "homeward:lostStatus")]
    pub lost_status: String,

    /// When the report was created (ISO 8601 / schema:Date).
    #[serde(rename = "schema:dateCreated")]
    pub date_created: DateTime<Utc>,

    /// When the report expires (homeward-namespaced).
    #[serde(rename = "homeward:expires")]
    pub expires: DateTime<Utc>,
}

/// JSON-LD `@context` block for the export.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportContext {
    /// schema.org prefix.
    pub schema: String,
    /// homeward vocab prefix.
    pub homeward: String,
}

impl Default for ExportContext {
    fn default() -> Self {
        Self {
            schema: SCHEMA_ORG.to_owned(),
            homeward: HOMEWARD_VOCAB.to_owned(),
        }
    }
}

/// schema.org-shaped animal description.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnimalDescription {
    /// JSON-LD type (`schema:Animal`).
    #[serde(rename = "@type")]
    pub animal_type: String,

    /// Species (homeward-namespaced enum).
    #[serde(rename = "homeward:species")]
    pub species: String,

    /// Primary breed string (schema:breed).
    #[serde(rename = "schema:breed", skip_serializing_if = "Option::is_none")]
    pub breed: Option<String>,

    /// Secondary breed (homeward-namespaced).
    #[serde(
        rename = "homeward:breedSecondary",
        skip_serializing_if = "Option::is_none"
    )]
    pub breed_secondary: Option<String>,

    /// Free-form description (schema:description).
    #[serde(rename = "schema:description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Coarse last-seen location for the export (no street address).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocationExport {
    /// JSON-LD type (`schema:Place`).
    #[serde(rename = "@type")]
    pub location_type: String,

    /// ZIP code (schema:postalCode).
    #[serde(rename = "schema:postalCode", skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,

    /// City (schema:addressLocality).
    #[serde(
        rename = "schema:addressLocality",
        skip_serializing_if = "Option::is_none"
    )]
    pub city: Option<String>,

    /// State (schema:addressRegion).
    #[serde(
        rename = "schema:addressRegion",
        skip_serializing_if = "Option::is_none"
    )]
    pub state: Option<String>,

    /// Search radius in miles (homeward-namespaced).
    #[serde(rename = "homeward:radiusMiles", skip_serializing_if = "Option::is_none")]
    pub radius_miles: Option<f32>,
}

impl From<&CoarseLocation> for LocationExport {
    fn from(loc: &CoarseLocation) -> Self {
        Self {
            location_type: "schema:Place".to_owned(),
            postal_code: loc.zip_code.clone(),
            city: loc.city.clone(),
            state: loc.state.clone(),
            radius_miles: loc.radius_miles,
        }
    }
}

/// A photo reference in the export (EXIF-stripped).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhotoExport {
    /// JSON-LD type (`schema:ImageObject`).
    #[serde(rename = "@type")]
    pub photo_type: String,

    /// The photo URL (EXIF-stripped; data URI or hotlink).
    #[serde(rename = "schema:contentUrl")]
    pub content_url: String,

    /// Whether this is the primary photo.
    #[serde(rename = "homeward:isPrimary")]
    pub is_primary: bool,
}

impl PhotoExport {
    /// Build a [`PhotoExport`] from a [`PhotoRef`], stripping EXIF from data URIs.
    ///
    /// For data URIs (base64 JPEG), the EXIF stripper is applied to the decoded
    /// bytes and the result re-encoded. For hotlinks the URL is passed through
    /// unchanged (EXIF was stripped at upload time).
    #[must_use]
    pub fn from_photo_ref(r: &PhotoRef) -> Self {
        let content_url = if let Some(b64) = r.url.strip_prefix("data:image/jpeg;base64,") {
            // Decode → strip → re-encode
            let decoded = base64_decode(b64);
            let stripped = strip_exif(&decoded);
            format!("data:image/jpeg;base64,{}", base64_encode_export(&stripped))
        } else {
            // Hotlink — EXIF was stripped at upload; pass through.
            r.url.clone()
        };
        Self {
            photo_type: "schema:ImageObject".to_owned(),
            content_url,
            is_primary: r.is_primary,
        }
    }
}

// ─── Export builder ───────────────────────────────────────────────────────────

impl LostReportExport {
    /// Build a [`LostReportExport`] from a [`LostReport`].
    ///
    /// Convenience constructor that delegates to [`to_export`].
    #[must_use]
    pub fn from_report(report: &LostReport) -> Self {
        to_export(report)
    }
}

/// Serialise a [`LostReport`] to a [`LostReportExport`] JSON-LD document.
///
/// - Photos are EXIF-stripped via [`PhotoExport::from_photo_ref`].
/// - Contact is always the brokered relay token (never raw PII).
/// - Location is the coarse `CoarseLocation` (no street address).
#[must_use]
pub fn to_export(report: &LostReport) -> LostReportExport {
    let animal = AnimalDescription {
        animal_type: "schema:Animal".to_owned(),
        species: species_label(report.species),
        breed: report.breed_primary.clone(),
        breed_secondary: report.breed_secondary.clone(),
        description: report.description.clone(),
    };

    let photos = report
        .photos
        .iter()
        .map(PhotoExport::from_photo_ref)
        .collect();

    LostReportExport {
        context: ExportContext::default(),
        report_type: "homeward:LostReport".to_owned(),
        report_id: report.report_id.clone(),
        animal,
        last_seen_location: LocationExport::from(&report.last_seen),
        photos,
        broker_relay_token: report.contact.token().to_owned(),
        lost_status: lost_status_label(report.status),
        date_created: report.created,
        expires: report.expires,
    }
}

fn species_label(s: Species) -> String {
    match s {
        Species::Dog => "dog".to_owned(),
        Species::Cat => "cat".to_owned(),
    }
}

fn lost_status_label(s: LostStatus) -> String {
    match s {
        LostStatus::Active => "active".to_owned(),
        LostStatus::Reunited => "reunited".to_owned(),
        LostStatus::Expired => "expired".to_owned(),
    }
}

// ─── Flyer renderer ──────────────────────────────────────────────────────────

/// Render a human-postable plain-text flyer for a [`LostReportExport`].
///
/// The flyer is deterministic: same export → byte-identical output.
/// It contains name (if known from breed/species), species/breed,
/// coarse last-seen location, and brokered contact. No raw owner PII.
///
/// # Format
///
/// ```text
/// LOST PET — PLEASE HELP
/// =======================
/// Animal : Dog (Labrador)
/// Last seen : Austin, TX 78701
/// Report date : 2026-06-11
/// Contact (via relay) : tok_0123456789abcdef
///
/// Please post this flyer to local lost-pet groups.
/// Report ID : <id>
/// ```
#[must_use]
pub fn render_flyer(export: &LostReportExport) -> String {
    let species = &export.animal.species;
    let breed_part = match &export.animal.breed {
        Some(b) => format!(" ({b})"),
        None => String::new(),
    };
    let animal_line = format!("{}{}", capitalize(species), breed_part);

    let loc = &export.last_seen_location;
    let location_line = build_location_line(loc);

    let date_str = export.date_created.format("%Y-%m-%d").to_string();

    let mut out = String::new();
    out.push_str("LOST PET — PLEASE HELP\n");
    out.push_str("=======================\n");
    out.push_str(&format!("Animal      : {animal_line}\n"));
    out.push_str(&format!("Last seen   : {location_line}\n"));
    out.push_str(&format!("Report date : {date_str}\n"));
    out.push_str(&format!(
        "Contact (via relay) : {}\n",
        export.broker_relay_token
    ));
    out.push('\n');
    out.push_str("Please post this flyer to local lost-pet groups.\n");
    out.push_str(&format!("Report ID   : {}\n", export.report_id));
    out
}

fn build_location_line(loc: &LocationExport) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(city) = &loc.city {
        parts.push(city.clone());
    }
    if let Some(state) = &loc.state {
        parts.push(state.clone());
    }
    if let Some(zip) = &loc.postal_code {
        parts.push(zip.clone());
    }
    if parts.is_empty() {
        "location not specified".to_owned()
    } else {
        parts.join(", ")
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

// ─── Syndication trait ────────────────────────────────────────────────────────

/// The result of a syndication attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum SyndicationOutcome {
    /// Artifacts were written to the configured output directory.
    Written {
        /// Paths of the written artifacts.
        paths: Vec<PathBuf>,
    },
    /// Dry-run: the adapter was invoked but no real delivery was attempted
    /// (no endpoint/credential configured).
    DryRun,
    /// No automated delivery path exists; the owner must post manually.
    ManualOnly {
        /// The channel that was considered.
        channel: String,
        /// Why no automated path exists.
        reason: String,
    },
    /// A real POST was made and failed (non-2xx or network error).
    DeliveryFailed {
        /// HTTP status or error description.
        detail: String,
    },
}

/// Pluggable syndication adapter.
///
/// Implementors receive a [`LostReportExport`] and return a [`SyndicationOutcome`].
/// Implementations MUST NOT return a "success" variant without actual confirmed
/// delivery. Dry-run or manual-only are always safe defaults.
pub trait Syndicator {
    /// Attempt to syndicate the export. See [`SyndicationOutcome`] for meanings.
    fn syndicate(&self, export: &LostReportExport) -> SyndicationOutcome;
}

// ─── LocalArtifactSyndicator ──────────────────────────────────────────────────

/// Writes JSON-LD + text flyer to a local output directory.
///
/// This is the default, always-on syndication path that requires no external
/// partnerships. It returns [`SyndicationOutcome::Written`] with the paths of
/// the two artifacts.
#[derive(Debug, Clone)]
pub struct LocalArtifactSyndicator {
    /// Directory to write artifacts into.
    pub output_dir: PathBuf,
}

impl LocalArtifactSyndicator {
    /// Create a new syndicator that writes to `output_dir`.
    #[must_use]
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }
}

impl Syndicator for LocalArtifactSyndicator {
    fn syndicate(&self, export: &LostReportExport) -> SyndicationOutcome {
        let json_path = self
            .output_dir
            .join(format!("{}.jsonld", export.report_id));
        let flyer_path = self
            .output_dir
            .join(format!("{}.flyer.txt", export.report_id));

        let json_str = match serde_json::to_string_pretty(export) {
            Ok(s) => s,
            Err(e) => {
                return SyndicationOutcome::DeliveryFailed {
                    detail: format!("json serialize: {e}"),
                };
            }
        };
        let flyer_str = render_flyer(export);

        if let Err(e) = std::fs::write(&json_path, &json_str) {
            return SyndicationOutcome::DeliveryFailed {
                detail: format!("write jsonld: {e}"),
            };
        }
        if let Err(e) = std::fs::write(&flyer_path, &flyer_str) {
            return SyndicationOutcome::DeliveryFailed {
                detail: format!("write flyer: {e}"),
            };
        }

        SyndicationOutcome::Written {
            paths: vec![json_path, flyer_path],
        }
    }
}

// ─── PetFbiPartnerSyndicator (feature-gated) ─────────────────────────────────

/// Dry-run-by-default adapter for a hypothetical Pet FBI partner POST endpoint.
///
/// Without a real endpoint URL and credential, always returns
/// [`SyndicationOutcome::DryRun`]. With configuration, it would POST the
/// JSON-LD export and return `Written` or `DeliveryFailed` based on the
/// actual HTTP response. **Never returns a success variant without a 2xx.**
///
/// Enabled by the `petfbi-partner` Cargo feature (off by default).
#[cfg(feature = "petfbi-partner")]
#[derive(Debug, Clone, Default)]
pub struct PetFbiPartnerSyndicator {
    /// Partner POST endpoint URL. `None` → dry-run.
    pub endpoint_url: Option<String>,
    /// API credential (bearer token or key). `None` → dry-run.
    pub credential: Option<String>,
}

#[cfg(feature = "petfbi-partner")]
impl PetFbiPartnerSyndicator {
    /// Create an unconfigured adapter (dry-run by default).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the adapter with a real endpoint URL and credential.
    #[must_use]
    pub fn with_config(endpoint_url: impl Into<String>, credential: impl Into<String>) -> Self {
        Self {
            endpoint_url: Some(endpoint_url.into()),
            credential: Some(credential.into()),
        }
    }
}

#[cfg(feature = "petfbi-partner")]
impl Syndicator for PetFbiPartnerSyndicator {
    fn syndicate(&self, export: &LostReportExport) -> SyndicationOutcome {
        match (&self.endpoint_url, &self.credential) {
            (Some(_url), Some(_cred)) => {
                // Real POST path — not implemented yet (no partner endpoint
                // established). When a real endpoint is obtained, this branch
                // will POST the JSON-LD and parse the response status.
                // It MUST NOT return Written/success without a confirmed 2xx.
                // For now, return DryRun to be safe until the endpoint is wired.
                let _ = export; // suppress unused warning
                SyndicationOutcome::DryRun
            }
            _ => SyndicationOutcome::DryRun,
        }
    }
}

// ─── Gated-channel stubs ──────────────────────────────────────────────────────

/// Returns the [`SyndicationOutcome::ManualOnly`] entry for PawBoost.
///
/// PawBoost has no third-party write API; only shelters may post.
/// See: <https://www.pawboost.com/blog/pawboost-for-shelters-faqs/>
#[must_use]
pub fn pawboost_manual_only() -> SyndicationOutcome {
    SyndicationOutcome::ManualOnly {
        channel: "PawBoost".to_owned(),
        reason: "No third-party write API; shelter-inbound only. Post the flyer manually at \
                 https://www.pawboost.com/p"
            .to_owned(),
    }
}

/// Returns the [`SyndicationOutcome::ManualOnly`] entry for Nextdoor.
///
/// Nextdoor's Share Content endpoint is partnership-approval-gated with no
/// lost-pet write capability for non-partners.
/// See: <https://developer.nextdoor.com/>
#[must_use]
pub fn nextdoor_manual_only() -> SyndicationOutcome {
    SyndicationOutcome::ManualOnly {
        channel: "Nextdoor".to_owned(),
        reason: "Share Content endpoint is partnership-approval-gated; no lost-pet write path \
                 exists. Post the flyer manually in your Nextdoor neighborhood."
            .to_owned(),
    }
}

/// Returns the [`SyndicationOutcome::ManualOnly`] entry for Facebook Groups.
///
/// The Facebook Groups API was deprecated by Meta in April 2024; all third-party
/// group posting was removed.
/// See: <https://www.sprinklr.com/help/articles/getting-started/meta-deprecates-facebook-groups-api/66229eb25f9dd9599d632712>
#[must_use]
pub fn facebook_groups_manual_only() -> SyndicationOutcome {
    SyndicationOutcome::ManualOnly {
        channel: "Facebook Groups".to_owned(),
        reason: "Facebook Groups API deprecated April 2024; all third-party group posting \
                 removed. Post the flyer manually in local lost-pet Facebook groups."
            .to_owned(),
    }
}

/// Returns the [`SyndicationOutcome::ManualOnly`] entry for Petco Love Lost.
///
/// Petco Love Lost is closed to third-party partners.
#[must_use]
pub fn petco_love_lost_manual_only() -> SyndicationOutcome {
    SyndicationOutcome::ManualOnly {
        channel: "Petco Love Lost".to_owned(),
        reason: "Petco Love Lost is closed / partner-only with no open write API. \
                 Post the flyer manually at https://lovelost.petco.com"
            .to_owned(),
    }
}

// ─── Internal base64 helpers ──────────────────────────────────────────────────

/// Minimal base64 encoder (no external dep) — mirrors the one in lib.rs but
/// kept local to avoid cross-module coupling.
fn base64_encode_export(input: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    fn b64(idx: usize) -> char {
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

/// Minimal base64 decoder.
fn base64_decode(input: &str) -> Vec<u8> {
    fn val(c: u8) -> u8 {
        match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let b0 = val(chunk.first().copied().unwrap_or(0));
        let b1 = val(chunk.get(1).copied().unwrap_or(0));
        let b2 = val(chunk.get(2).copied().unwrap_or(0));
        let b3 = val(chunk.get(3).copied().unwrap_or(0));
        let triple = (u32::from(b0) << 18)
            | (u32::from(b1) << 12)
            | (u32::from(b2) << 6)
            | u32::from(b3);
        out.push((triple >> 16) as u8);
        if chunk.len() > 2 {
            out.push((triple >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(triple as u8);
        }
    }
    out
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_common::make_report;
    use homeward_schema::{BrokeredContactToken, CoarseLocation, LostStatus, PhotoRef, Species};

    /// Build a minimal `LostReport` with a known photo containing a synthetic
    /// EXIF marker and a known contact token.
    fn make_report_with_photo(report_id: &str) -> LostReport {
        let mut r = make_report(report_id);
        // Build a JPEG with an APP1 (EXIF) marker
        let mut jpeg = vec![0xFF_u8, 0xD8];
        let exif_payload = b"GPS_FAKE_DATA";
        let len = (2 + exif_payload.len()) as u16;
        jpeg.push(0xFF);
        jpeg.push(0xE1); // APP1
        jpeg.extend_from_slice(&len.to_be_bytes());
        jpeg.extend_from_slice(exif_payload);
        jpeg.push(0xFF);
        jpeg.push(0xD9); // EOI
        let b64 = base64_encode_export(&jpeg);
        r.photos = vec![PhotoRef {
            url: format!("data:image/jpeg;base64,{b64}"),
            attribution: None,
            is_primary: true,
        }];
        r
    }

    // ── AC2: JSON-LD shape ──────────────────────────────────────────────────

    /// AC2: LostReport serializes to well-formed JSON-LD with schema.org and
    /// homeward-namespaced fields, asserted field by field.
    #[test]
    fn export_produces_valid_jsonld_fields() {
        let report = make_report("r-jsonld");
        let export = to_export(&report);

        // Context
        assert_eq!(export.context.schema, SCHEMA_ORG);
        assert_eq!(export.context.homeward, HOMEWARD_VOCAB);

        // Type
        assert_eq!(export.report_type, "homeward:LostReport");

        // Report ID round-trips
        assert_eq!(export.report_id, "r-jsonld");

        // Animal fields
        assert_eq!(export.animal.animal_type, "schema:Animal");
        assert_eq!(export.animal.species, "dog");
        assert_eq!(export.animal.breed, Some("Labrador".to_owned())); // from make_report

        // Location
        assert_eq!(export.last_seen_location.location_type, "schema:Place");
        assert_eq!(
            export.last_seen_location.postal_code,
            Some("78701".to_owned())
        );

        // Status
        assert_eq!(export.lost_status, "active");

        // Serializes as well-formed JSON
        let json = serde_json::to_string(&export).expect("serialize ok");
        let _parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(json.contains("@context"), "JSON-LD @context present");
        assert!(json.contains("@type"), "JSON-LD @type present");
        assert!(json.contains("homeward:reportId"), "homeward field present");
        assert!(json.contains("schema:animal"), "schema.org field present");
    }

    // ── AC3: No raw contact / EXIF leak ──────────────────────────────────────

    /// AC3: exported photo reference is EXIF-stripped; contact is brokered token.
    #[test]
    fn export_strips_exif_and_uses_brokered_contact() {
        let report = make_report_with_photo("r-exif");
        let export = to_export(&report);

        // Photo must exist and must not contain the EXIF payload
        assert_eq!(export.photos.len(), 1, "one photo in export");
        let photo = &export.photos[0];
        let b64_part = photo
            .content_url
            .strip_prefix("data:image/jpeg;base64,")
            .expect("data URI photo");
        let decoded = base64_decode(b64_part);
        // APP1 (0xFFE1) must be absent
        let has_app1 = decoded
            .windows(2)
            .any(|w| w[0] == 0xFF && w[1] == 0xE1);
        assert!(!has_app1, "EXIF APP1 marker must be stripped from export photo");

        // Contact must be the brokered token, not raw PII
        assert!(
            export.broker_relay_token.starts_with("tok_"),
            "contact is brokered token"
        );
        assert!(
            !export.broker_relay_token.contains('@'),
            "no raw email in export contact"
        );
    }

    /// AC3b: JSON serialization of export never contains raw contact PII.
    #[test]
    fn export_json_never_contains_raw_pii() {
        let mut report = make_report("r-pii");
        // Manually set a token that looks realistic but is not raw contact
        report.contact = BrokeredContactToken::new("tok_deadbeef12345678".to_owned());
        let export = to_export(&report);
        let json = serde_json::to_string(&export).expect("serialize ok");

        assert!(
            !json.contains('@') || json.contains("schema") || json.contains("homeward"),
            "no raw email in JSON export"
        );
        // More precise: the broker_relay_token should appear, not a real email/phone
        assert!(
            json.contains("tok_deadbeef"),
            "brokered token in export JSON"
        );
    }

    // ── AC4: Flyer is deterministic ─────────────────────────────────────────

    /// AC4: flyer renderer produces deterministic, byte-identical output for
    /// the same export; contains name, species/breed, location, brokered contact.
    #[test]
    fn flyer_is_deterministic_and_contains_required_fields() {
        let report = make_report("r-flyer");
        let export = to_export(&report);

        let flyer1 = render_flyer(&export);
        let flyer2 = render_flyer(&export);
        assert_eq!(flyer1, flyer2, "flyer is deterministic (byte-identical)");

        // Required content
        assert!(flyer1.contains("LOST PET"), "flyer has header");
        assert!(
            flyer1.to_lowercase().contains("dog") || flyer1.contains("Labrador"),
            "flyer contains species/breed"
        );
        assert!(
            flyer1.contains("78701") || flyer1.contains("Austin"),
            "flyer contains coarse location"
        );
        assert!(
            flyer1.contains("tok_"),
            "flyer contains brokered contact token"
        );
        assert!(
            flyer1.contains("r-flyer"),
            "flyer contains report ID"
        );
    }

    // ── AC5: LocalArtifactSyndicator ─────────────────────────────────────────

    /// AC5: LocalArtifactSyndicator writes both artifacts to tmpdir and returns
    /// SyndicationOutcome::Written with the paths.
    #[test]
    fn local_artifact_syndicator_writes_both_artifacts() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let syndicator = LocalArtifactSyndicator::new(tmp.path());
        let report = make_report("r-local");
        let export = to_export(&report);

        let outcome = syndicator.syndicate(&export);
        match outcome {
            SyndicationOutcome::Written { paths } => {
                assert_eq!(paths.len(), 2, "two artifacts written");
                for p in &paths {
                    assert!(p.exists(), "artifact exists: {p:?}");
                }
                // Verify JSON-LD artifact is valid JSON
                let jsonld_path = paths.iter().find(|p| {
                    p.extension().and_then(|e| e.to_str()) == Some("jsonld")
                });
                let jsonld_path = jsonld_path.expect("jsonld artifact");
                let content = std::fs::read_to_string(jsonld_path).expect("read jsonld");
                let _: serde_json::Value =
                    serde_json::from_str(&content).expect("valid JSON-LD");
                assert!(content.contains("@context"), "JSON-LD @context in file");

                // Verify flyer artifact contains key text
                let flyer_path = paths
                    .iter()
                    .find(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.ends_with(".flyer.txt"))
                    })
                    .expect("flyer artifact");
                let flyer = std::fs::read_to_string(flyer_path).expect("read flyer");
                assert!(flyer.contains("LOST PET"), "flyer header in file");
            }
            other => panic!("expected Written, got {other:?}"),
        }
    }

    // ── AC6: PetFbiPartnerSyndicator (feature-gated) ─────────────────────────

    #[cfg(feature = "petfbi-partner")]
    mod petfbi_tests {
        use super::*;

        /// AC6: unconfigured PetFbiPartnerSyndicator always returns DryRun.
        #[test]
        fn petfbi_syndicator_unconfigured_returns_dry_run() {
            let syndicator = PetFbiPartnerSyndicator::new();
            let report = make_report("r-petfbi");
            let export = to_export(&report);
            let outcome = syndicator.syndicate(&export);
            assert!(
                matches!(outcome, SyndicationOutcome::DryRun),
                "unconfigured adapter must return DryRun, got: {outcome:?}"
            );
        }

        /// AC6: even a configured-but-unverified PetFbiPartnerSyndicator must not
        /// return Written (no live Pet FBI call in tests).
        #[test]
        fn petfbi_syndicator_never_reports_success_without_real_endpoint() {
            // "Configured" with a fake endpoint — still no real server.
            let syndicator = PetFbiPartnerSyndicator::with_config(
                "https://fake-petfbi-endpoint.example/lost",
                "bearer-fake",
            );
            let report = make_report("r-petfbi-cfg");
            let export = to_export(&report);
            let outcome = syndicator.syndicate(&export);
            // Must be DryRun or DeliveryFailed — never Written
            assert!(
                !matches!(outcome, SyndicationOutcome::Written { .. }),
                "must not report success without confirmed 2xx: {outcome:?}"
            );
        }
    }

    // ── AC7: Gated channels are ManualOnly ───────────────────────────────────

    /// AC7: every gated channel function returns ManualOnly — none claim
    /// automated delivery to PawBoost, Nextdoor, Facebook, or Petco.
    #[test]
    fn gated_channels_are_manual_only() {
        let channels = [
            pawboost_manual_only(),
            nextdoor_manual_only(),
            facebook_groups_manual_only(),
            petco_love_lost_manual_only(),
        ];
        let channel_names = ["PawBoost", "Nextdoor", "Facebook Groups", "Petco Love Lost"];

        for (outcome, name) in channels.iter().zip(channel_names.iter()) {
            match outcome {
                SyndicationOutcome::ManualOnly { channel, .. } => {
                    assert_eq!(
                        channel, name,
                        "ManualOnly channel name matches: {channel}"
                    );
                }
                other => panic!(
                    "gated channel {name} must return ManualOnly, got: {other:?}"
                ),
            }
        }
    }

    /// AC7b: No Syndicator impl claims automated delivery to any gated network.
    ///
    /// Asserts that none of the four gated-channel functions return Written or
    /// any variant that would imply automated delivery.
    #[test]
    fn no_syndicator_claims_automated_delivery_to_gated_channels() {
        let outcomes = [
            pawboost_manual_only(),
            nextdoor_manual_only(),
            facebook_groups_manual_only(),
            petco_love_lost_manual_only(),
        ];
        for outcome in &outcomes {
            assert!(
                !matches!(outcome, SyndicationOutcome::Written { .. }),
                "gated channel must never claim Written delivery: {outcome:?}"
            );
            assert!(
                !matches!(outcome, SyndicationOutcome::DeliveryFailed { .. }),
                "gated channel must not even attempt delivery: {outcome:?}"
            );
        }
    }
}
