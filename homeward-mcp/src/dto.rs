//! Request/response DTOs for the read-only MCP surface.
//!
//! Output types are the legal-ethics contract boundary: photo fields are
//! hotlink URLs (never re-hosted bytes), locations are already-coarsened
//! (city/state, or lat/lon already rounded at ingest by
//! [`homeward_schema::ShelterLocation`]), and `shelter_contact` is a
//! brokered route string (the data source's own attribution slug) --
//! never a person's phone/email. No field here carries owner PII: this
//! server only ever reads the shelter intake store, never a `LostReport`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Input for the `search_pets` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SearchPetsRequest {
    /// Species to search for: "dog" or "cat".
    pub species: String,
    /// Latitude of the search center. Provide with `lon` (alternative to `postal_code`).
    #[serde(default)]
    pub lat: Option<f64>,
    /// Longitude of the search center. Provide with `lat` (alternative to `postal_code`).
    #[serde(default)]
    pub lon: Option<f64>,
    /// Postal/ZIP code search center (alternative to `lat`/`lon`).
    #[serde(default)]
    pub postal_code: Option<String>,
    /// Search radius in kilometers, used only with `lat`/`lon` (default 50).
    #[serde(default)]
    pub radius_km: Option<f64>,
    /// Only return intakes first seen on/after this RFC3339 timestamp.
    #[serde(default)]
    pub since: Option<String>,
    /// Substring filter on breed (case-insensitive).
    #[serde(default)]
    pub breed: Option<String>,
    /// Substring filter on color (case-insensitive).
    #[serde(default)]
    pub color: Option<String>,
    /// Maximum rows to return (default 20, hard cap 50).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Input for the `get_pet` tool.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetPetRequest {
    /// The canonical shelter-intake id (as returned by `search_pets`).
    pub id: String,
}

/// A redacted, agent-facing view of one shelter intake.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PetSummary {
    /// Homeward's canonical id.
    pub id: String,
    /// "dog" or "cat".
    pub species: String,
    /// Primary breed description, if known.
    pub breed: Option<String>,
    /// Free-form color descriptions.
    pub colors: Vec<String>,
    /// Coarse shelter city/county.
    pub city_county: Option<String>,
    /// Shelter state code.
    pub state: Option<String>,
    /// Coarse (already-rounded) latitude, if known.
    pub lat: Option<f64>,
    /// Coarse (already-rounded) longitude, if known.
    pub lon: Option<f64>,
    /// Hotlinked photo URLs (never re-hosted bytes).
    pub photo_urls: Vec<String>,
    /// Brokered shelter contact route -- the data source's own attribution
    /// slug, never a person's phone/email/address.
    pub shelter_contact: String,
    /// When this animal was first observed, RFC3339.
    pub first_seen: String,
}

/// Output of the `search_pets` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchPetsResult {
    /// Matching records, already capped to the requested/default limit.
    pub pets: Vec<PetSummary>,
    /// True if more records matched than were returned.
    pub truncated: bool,
}
