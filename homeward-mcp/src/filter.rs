//! In-memory filtering for `search_pets` / `recent_intakes`, plus the
//! redaction step into [`PetSummary`] output DTOs.
//!
//! `homeward_report::api::query_shelter` (the existing query layer) only
//! filters by species/zip/state/since and its `ShelterRecord` output drops
//! colors and lat/lon -- it predates this PRD's radius/breed/color
//! requirements and can't satisfy them without filtering the raw
//! [`PetRecord`]s twice (once through it, once more for what it can't
//! express). This module does one unified pass directly over
//! [`PetRecord`] instead, reusing the same [`homeward_schema::Species`] /
//! [`PetRecord`] types (still shared, not duplicated) rather than
//! force-fitting the narrower existing DTO.

use chrono::{DateTime, Utc};
use homeward_schema::{PetRecord, Species};

use crate::dto::PetSummary;
use crate::geo::haversine_km;

/// Where to center a `search_pets` radius search (or no geo filter at all).
pub enum LocationFilter {
    /// Center + radius (km), matched via great-circle distance.
    LatLon {
        /// Center latitude.
        lat: f64,
        /// Center longitude.
        lon: f64,
        /// Radius in kilometers.
        radius_km: f64,
    },
    /// Best-effort postal/ZIP match against the shelter's `city_county`
    /// string (same heuristic as `homeward_report::api::query_shelter`'s
    /// zip filter -- the DB stores city/county text, not a ZIP column).
    Postal(String),
    /// No geographic filter.
    None,
}

/// Filters for one `search_pets` call.
pub struct SearchFilters<'a> {
    /// Required species match.
    pub species: Species,
    /// Geographic filter.
    pub location: LocationFilter,
    /// Only records first seen on/after this instant.
    pub since: Option<DateTime<Utc>>,
    /// Case-insensitive breed substring filter.
    pub breed: Option<&'a str>,
    /// Case-insensitive color substring filter.
    pub color: Option<&'a str>,
}

/// Whether `record` satisfies every filter in `f`.
#[must_use]
pub fn matches(record: &PetRecord, f: &SearchFilters<'_>) -> bool {
    if record.species != f.species {
        return false;
    }
    if let Some(since) = f.since
        && record.first_seen < since
    {
        return false;
    }
    if let Some(breed) = f.breed {
        let hay = record
            .breed_primary
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        if !hay.contains(&breed.to_lowercase()) {
            return false;
        }
    }
    if let Some(color) = f.color {
        let color_lc = color.to_lowercase();
        if !record
            .colors
            .iter()
            .any(|c| c.to_lowercase().contains(&color_lc))
        {
            return false;
        }
    }
    match &f.location {
        LocationFilter::LatLon {
            lat,
            lon,
            radius_km,
        } => match record.location.as_ref().and_then(|l| l.lat.zip(l.lon)) {
            Some((rlat, rlon)) => haversine_km(*lat, *lon, rlat, rlon) <= *radius_km,
            None => false,
        },
        LocationFilter::Postal(zip) => record
            .location
            .as_ref()
            .is_some_and(|l| l.city_county.contains(zip.as_str())),
        LocationFilter::None => true,
    }
}

/// Redact a [`PetRecord`] into the agent-facing [`PetSummary`] shape.
///
/// This is the legal-ethics contract boundary: no field here can carry
/// owner PII (the caller only ever passes shelter-intake records, never a
/// `LostReport`), photo fields are hotlinks, and `shelter_contact` is a
/// brokered attribution slug, not a person's phone/email.
#[must_use]
pub fn to_summary(record: &PetRecord) -> PetSummary {
    PetSummary {
        id: record.canonical_id.to_string(),
        species: match record.species {
            Species::Dog => "dog".to_owned(),
            Species::Cat => "cat".to_owned(),
        },
        breed: record.breed_primary.clone(),
        colors: record.colors.clone(),
        city_county: record.location.as_ref().map(|l| l.city_county.clone()),
        state: record.location.as_ref().and_then(|l| l.state.clone()),
        lat: record.location.as_ref().and_then(|l| l.lat),
        lon: record.location.as_ref().and_then(|l| l.lon),
        photo_urls: record.photos.iter().map(|p| p.url.clone()).collect(),
        shelter_contact: format!("brokered-via:{}", record.source.name),
        first_seen: record.first_seen.to_rfc3339(),
    }
}
