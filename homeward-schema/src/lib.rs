//! `homeward-schema` — canonical record types for shelter-pet aggregation.
//!
//! Every connector, dedup, embed, and match crate in the homeward fleet
//! imports from this crate. No network code, no storage, no ML — pure
//! types, validation, and JSON serde.

pub mod chip;
pub mod geo;
pub mod intake;
pub mod lost;
pub mod media;
pub mod provenance;

pub use chip::ChipStatus;
pub use geo::ShelterLocation;
pub use intake::{Availability, IntakeType};
pub use lost::{BrokeredContactToken, CoarseLocation, LostReport, LostStatus};
pub use media::PhotoRef;
pub use provenance::{Provenance, SourceId, TosClass};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ulid::Ulid;

// ─── Species ─────────────────────────────────────────────────────────────────

/// The species of a sheltered animal.
///
/// Unknown species are rejected at ingest with [`UnknownSpeciesError`] —
/// no silent coercion to a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Species {
    /// Domestic dog.
    Dog,
    /// Domestic cat.
    Cat,
}

/// Returned when a species string cannot be mapped to a known [`Species`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown species: {0:?}")]
pub struct UnknownSpeciesError(pub String);

impl Species {
    /// Parse a species from a free-form string.
    ///
    /// # Errors
    /// Returns [`UnknownSpeciesError`] if the string is not a recognized species.
    pub fn from_str_strict(s: &str) -> Result<Self, UnknownSpeciesError> {
        match s.trim().to_lowercase().as_str() {
            "dog" | "canine" => Ok(Self::Dog),
            "cat" | "feline" => Ok(Self::Cat),
            other => Err(UnknownSpeciesError(other.to_owned())),
        }
    }
}

// ─── Age / Sex / Size ────────────────────────────────────────────────────────

/// Coarse age bucket — precise age is rarely available from shelters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgeBucket {
    /// Typically < 6 months.
    Baby,
    /// Typically 6 months – 2 years.
    Young,
    /// Typically 2 – 8 years.
    Adult,
    /// Typically 8+ years.
    Senior,
}

/// Biological sex of the animal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sex {
    /// Male (intact).
    Male,
    /// Female (intact).
    Female,
    /// Neutered / castrated male.
    NeuteredMale,
    /// Spayed female.
    SpayedFemale,
    /// Sex not recorded.
    Unknown,
}

/// Approximate size of the animal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    /// < 10 lbs.
    Small,
    /// 10 – 25 lbs.
    Medium,
    /// 25 – 60 lbs.
    Large,
    /// > 60 lbs.
    ExtraLarge,
}

// ─── PetRecord ───────────────────────────────────────────────────────────────

/// The canonical shelter-animal record.
///
/// Fields are optional where sources commonly omit them; callers must not
/// fabricate values for absent fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetRecord {
    /// Homeward's own stable identifier (ULID).
    pub canonical_id: Ulid,

    /// Which data source contributed this record.
    pub source: SourceId,

    /// The source's own stable ID for this animal, when available.
    /// Critical for dedup and departure detection.
    #[serde(default)]
    pub source_animal_id: Option<String>,

    /// Species (dog or cat).
    pub species: Species,

    /// Primary breed string (not yet normalized to vocab).
    #[serde(default)]
    pub breed_primary: Option<String>,

    /// Secondary breed string (for mixed-breed animals).
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

    /// Color descriptions (free-form strings from source).
    #[serde(default)]
    pub colors: Vec<String>,

    /// Free-form markings/description text from source.
    #[serde(default)]
    pub markings_text: Option<String>,

    /// How the animal entered the shelter system.
    pub intake_type: IntakeType,

    /// Current custodial status of the animal.
    pub availability: Availability,

    /// Microchip scan result.
    pub chip_status: ChipStatus,

    /// Where the animal is being held (coarse geo).
    #[serde(default)]
    pub location: Option<ShelterLocation>,

    /// Free-form text describing where the animal was found.
    #[serde(default)]
    pub found_location_text: Option<String>,

    /// Photos (hotlinks only — no bytes).
    #[serde(default)]
    pub photos: Vec<PhotoRef>,

    /// When this animal was first observed in any source.
    pub first_seen: DateTime<Utc>,

    /// When this animal was last observed in any source.
    pub last_seen: DateTime<Utc>,

    /// When we last confirmed the animal is still at this location.
    #[serde(default)]
    pub last_confirmed: Option<DateTime<Utc>>,

    /// Date the animal entered custody.
    #[serde(default)]
    pub intake_date: Option<DateTime<Utc>>,

    /// Date the animal left custody (adopted, euthanized, transferred, etc.).
    #[serde(default)]
    pub outcome_date: Option<DateTime<Utc>>,
}

// ─── Validation ──────────────────────────────────────────────────────────────

/// Errors that can arise when validating a [`PetRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    /// A stray animal is listed as adoptable before its legal hold expires.
    ///
    /// Socrata `Intake_Type == STRAY` and `Availability == Adoptable` are
    /// mutually exclusive during the stray-hold window (Phase 1 §3a).
    #[error(
        "stray-hold conflict: IntakeType::Stray cannot be combined with Availability::Adoptable"
    )]
    StrayHoldConflict,
}

/// Validate a [`PetRecord`] for internal consistency.
///
/// # Errors
/// Returns [`ValidationError::StrayHoldConflict`] if `intake_type` is
/// [`IntakeType::Stray`] and `availability` is [`Availability::Adoptable`].
pub fn validate_pet_record(record: &PetRecord) -> Result<(), ValidationError> {
    if record.intake_type == IntakeType::Stray && record.availability == Availability::Adoptable {
        return Err(ValidationError::StrayHoldConflict);
    }
    Ok(())
}
