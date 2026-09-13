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

/// Input for the `match_photo` tool. Exactly one of `image_url`/`image_b64`
/// must be given.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct MatchPhotoRequest {
    /// URL of the lost pet's photo (`http`/`https` only). Alternative to `image_b64`.
    #[serde(default)]
    pub image_url: Option<String>,
    /// Base64-encoded JPEG/PNG photo bytes. Alternative to `image_url`.
    #[serde(default)]
    pub image_b64: Option<String>,
    /// Species of the lost pet: "dog" or "cat".
    pub species: String,
    /// Optional latitude to filter candidates by proximity (with `lon`).
    #[serde(default)]
    pub lat: Option<f64>,
    /// Optional longitude, paired with `lat`.
    #[serde(default)]
    pub lon: Option<f64>,
    /// Optional radius in kilometers, used only with `lat`/`lon`.
    #[serde(default)]
    pub radius_km: Option<f64>,
    /// Maximum number of candidates to return (default and hard cap 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// One ranked candidate returned by `match_photo` -- same redacted shape as
/// [`PetSummary`] plus a similarity score. No owner PII: this tool only
/// ever reads the shelter intake store, never a `LostReport`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MatchCandidate {
    /// Homeward's canonical id.
    pub id: String,
    /// "dog" or "cat".
    pub species: String,
    /// Cosine similarity to the submitted photo, in `[0, 1]`.
    pub similarity: f64,
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
    /// Brokered shelter contact route -- never a person's phone/email.
    pub shelter_contact: String,
}

/// Species-level accuracy baseline attached to every `match_photo` response
/// so a caller can calibrate expectations (see `baseline` module).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SpeciesBaseline {
    /// The species this baseline describes.
    pub species: String,
    /// Fraction of eval queries where the correct animal was the top-1 hit.
    pub rank1: f64,
    /// Fraction of eval queries where the correct animal was in the top 5.
    pub rank5: f64,
    /// Fraction of eval queries where the correct animal was in the top 20.
    pub rank20: f64,
    /// Which checked-in eval artifact this baseline was computed from.
    pub source: String,
}

/// Output of the `match_photo` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MatchPhotoResult {
    /// Ranked candidates, most-similar first, already capped to `limit`.
    pub candidates: Vec<MatchCandidate>,
    /// Fixed framing string every response carries: candidates are leads,
    /// never confirmed identifications.
    pub advisory: String,
    /// Species-level baseline so the caller can calibrate expectations.
    pub species_baseline: SpeciesBaseline,
}
