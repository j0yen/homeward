//! Lost-pet report types (owner-side records).
//!
//! Privacy posture (Phase 1 §2):
//! - Contact info is stored as an opaque [`BrokeredContactToken`] relay handle.
//!   There is no public accessor that returns a raw phone/email string.
//! - Last-seen location is a [`CoarseLocation`] with ZIP or radius precision —
//!   no street address field.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{AgeBucket, PhotoRef, Sex, Size, Species};

// ─── Privacy types ───────────────────────────────────────────────────────────

/// An opaque relay handle for owner contact.
///
/// This type intentionally has no public accessor that exposes a raw
/// phone number or email address. The relay implementation lives in a
/// separate service crate. Callers may compare tokens for equality or
/// forward them to the relay service but cannot extract raw contact data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokeredContactToken(
    /// Opaque token string — treat as an identifier, not as contact data.
    String,
);

impl BrokeredContactToken {
    /// Create a new [`BrokeredContactToken`] from a relay-issued opaque string.
    ///
    /// Typically called by the relay service when registering a new owner.
    #[must_use]
    pub const fn new(token: String) -> Self {
        Self(token)
    }

    /// Return the opaque token string for forwarding to the relay service.
    ///
    /// This returns the relay handle, NOT a phone number or email address.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.0
    }
}

/// A coarse location (ZIP code / radius) for lost-pet reports.
///
/// No street address field — the finest granularity is ZIP code or a
/// radius around a city, preserving owner privacy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoarseLocation {
    /// ZIP code or postal code where the pet was last seen.
    #[serde(default)]
    pub zip_code: Option<String>,

    /// City name where the pet was last seen.
    #[serde(default)]
    pub city: Option<String>,

    /// State code (two-letter, US).
    #[serde(default)]
    pub state: Option<String>,

    /// Radius in miles around the city center (when ZIP is unavailable).
    #[serde(default)]
    pub radius_miles: Option<f32>,
}

// ─── LostReport ──────────────────────────────────────────────────────────────

/// Status of a lost-pet report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LostStatus {
    /// Report is active — pet is still missing.
    Active,
    /// Pet has been reunited with owner.
    Reunited,
    /// Report has expired (typically 30 days).
    Expired,
}

/// An owner-side lost-pet report.
///
/// Mirrors [`crate::PetRecord`]'s descriptive fields so they can be compared
/// during matching, but carries privacy-safe contact and location types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LostReport {
    /// Homeward's ID for this report (ULID as string for portability).
    pub report_id: String,

    /// Species of the lost pet.
    pub species: Species,

    /// Primary breed string.
    #[serde(default)]
    pub breed_primary: Option<String>,

    /// Secondary breed string.
    #[serde(default)]
    pub breed_secondary: Option<String>,

    /// Biological sex.
    #[serde(default)]
    pub sex: Option<Sex>,

    /// Coarse age bucket.
    #[serde(default)]
    pub age_bucket: Option<AgeBucket>,

    /// Approximate size.
    #[serde(default)]
    pub size: Option<Size>,

    /// Color descriptions.
    #[serde(default)]
    pub colors: Vec<String>,

    /// Free-form description.
    #[serde(default)]
    pub description: Option<String>,

    /// Photos of the lost pet (hotlinks only).
    #[serde(default)]
    pub photos: Vec<PhotoRef>,

    /// Where the pet was last seen (coarse, no street address).
    pub last_seen: CoarseLocation,

    /// Relay handle for contacting the owner — never a raw phone/email.
    pub contact: BrokeredContactToken,

    /// When the report was created.
    pub created: DateTime<Utc>,

    /// When the report expires (typically 30 days after creation).
    pub expires: DateTime<Utc>,

    /// Current status of the report.
    pub status: LostStatus,
}
