//! Intake type and availability enums for shelter animals.

use serde::{Deserialize, Serialize};

/// How the animal entered the shelter system.
///
/// Distinct from [`Availability`] — a stray in its legal hold is
/// `IntakeType::Stray` but `Availability::InCustody`, not `Adoptable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeType {
    /// Animal was picked up as a stray.
    Stray,
    /// Animal was found and reported by a member of the public.
    FoundReport,
    /// Animal was surrendered by its owner.
    OwnerSurrender,
    /// Animal was transferred from another facility.
    Transfer,
    /// Animal is listed as available for adoption (origin unspecified).
    Adoptable,
    /// An owner-filed report of a lost pet (community network, not a shelter intake).
    LostReport,
    /// Intake type not recorded.
    Unknown,
}

/// Current custodial status of the animal.
///
/// Kept distinct from [`IntakeType`] per PRD §AC3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Animal is in custody but not yet available for adoption.
    InCustody,
    /// Animal is available for adoption.
    Adoptable,
    /// Animal has left the shelter (adopted, transferred, deceased, etc.).
    Departed,
    /// Animal is available for pickup / claiming (community found reports).
    Available,
    /// Pet is actively missing (from a community lost report).
    Missing,
    /// Availability status not recorded.
    Unknown,
}
