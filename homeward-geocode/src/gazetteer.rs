//! Offline gazetteer: load and index the bundled Census Gazetteer CSV.
//!
//! The gazetteer CSV (`data/gazetteer.csv`) is a trimmed public-domain subset
//! of the US Census Bureau TIGER/Line Gazetteer Files. No network access occurs
//! at load or query time.

use std::collections::HashMap;

use homeward_schema::ShelterLocation;
use serde::{Deserialize, Serialize};

use crate::error::GeocodeError;

/// The privacy precision used throughout homeward (±~1.1 km at precision 2).
pub const HOMEWARD_PRECISION: u8 = 2;

/// Indicates how a [`ShelterLocation`] was populated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationSource {
    /// Reported by the shelter directly.
    Shelter,
    /// Derived from [`PetRecord::found_location_text`] via offline geocoding.
    FoundText,
}

/// A single entry from the gazetteer.
#[derive(Debug, Clone)]
pub struct GazetteerEntry {
    /// Normalized place name (lowercase).
    pub name_lower: String,
    /// Two-letter state abbreviation (uppercase).
    pub state: String,
    /// Centroid latitude (raw, before homeward coarsening).
    pub lat: f64,
    /// Centroid longitude (raw, before homeward coarsening).
    pub lon: f64,
    /// ZIP code string when entry is a ZCTA.
    pub zip: Option<String>,
}

impl GazetteerEntry {
    /// Convert this entry into a coarse [`ShelterLocation`] at homeward's
    /// standard precision.
    #[must_use]
    pub fn to_shelter_location(&self) -> ShelterLocation {
        ShelterLocation::new(
            Some(self.lat),
            Some(self.lon),
            HOMEWARD_PRECISION,
            self.name_lower.clone(),
            Some(self.state.clone()),
        )
    }
}

/// In-memory index built from the bundled Census Gazetteer.
///
/// Build once at startup with [`Gazetteer::load_default`] or
/// [`Gazetteer::load_csv`]; thereafter all lookups are O(1) hash probes.
#[derive(Debug, Default)]
pub struct Gazetteer {
    /// `(name_lower, state_upper)` → entry
    by_name_state: HashMap<(String, String), GazetteerEntry>,
    /// `zip` → entry
    by_zip: HashMap<String, GazetteerEntry>,
}

impl Gazetteer {
    /// Load the default bundled gazetteer.
    ///
    /// The data file is embedded at compile time from `data/gazetteer.csv`
    /// in the crate root (no filesystem access at runtime).
    ///
    /// # Errors
    /// Returns [`GeocodeError::MalformedRow`] if any data row is malformed.
    pub fn load_default() -> Result<Self, GeocodeError> {
        // Embed the file at compile time — no network, no filesystem at runtime.
        let csv_bytes = include_str!("../data/gazetteer.csv");
        Self::parse_csv(csv_bytes)
    }

    /// Load a gazetteer from arbitrary CSV text (useful in tests).
    ///
    /// # Errors
    /// Returns [`GeocodeError::MalformedRow`] if any data row is malformed.
    pub fn load_csv(csv_text: &str) -> Result<Self, GeocodeError> {
        Self::parse_csv(csv_text)
    }

    fn parse_csv(text: &str) -> Result<Self, GeocodeError> {
        let mut gz = Self::default();
        for (lineno, raw_line) in text.lines().enumerate() {
            let line_num = lineno + 1;
            let line = raw_line.trim();
            // Skip blank lines and comment lines.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.splitn(6, ',').collect();
            if parts.len() < 5 {
                return Err(GeocodeError::MalformedRow {
                    line: line_num,
                    reason: format!("expected ≥5 columns, got {}", parts.len()),
                });
            }
            let kind = parts[0].trim();
            let name = parts[1].trim();
            let state = parts[2].trim().to_uppercase();
            let lat_str = parts[3].trim().replace('−', "-"); // handle Unicode minus
            let lon_str = parts[4].trim().replace('−', "-");
            let zip_str = if parts.len() == 6 {
                let z = parts[5].trim();
                if z.is_empty() { None } else { Some(z.to_owned()) }
            } else {
                None
            };

            let lat = lat_str.parse::<f64>().map_err(|_| GeocodeError::MalformedRow {
                line: line_num,
                reason: format!("invalid lat {lat_str:?}"),
            })?;
            let lon = lon_str.parse::<f64>().map_err(|_| GeocodeError::MalformedRow {
                line: line_num,
                reason: format!("invalid lon {lon_str:?}"),
            })?;

            let entry = GazetteerEntry {
                name_lower: name.to_lowercase(),
                state: state.clone(),
                lat,
                lon,
                zip: if kind == "Z" {
                    Some(name.to_owned())
                } else {
                    zip_str
                },
            };

            // Index by (name, state) for place lookups.
            gz.by_name_state
                .insert((entry.name_lower.clone(), state), entry.clone());

            // Index by ZIP for ZCTA lookups.
            if let Some(ref zip) = entry.zip {
                gz.by_zip.insert(zip.clone(), entry);
            }
        }
        Ok(gz)
    }

    /// Look up a 5-digit ZIP code.
    ///
    /// Returns `None` if the ZIP is not in the gazetteer.
    #[must_use]
    pub fn lookup_zip(&self, zip: &str) -> Option<&GazetteerEntry> {
        self.by_zip.get(zip)
    }

    /// Look up by place name and state abbreviation.
    ///
    /// Both are case-insensitive.
    #[must_use]
    pub fn lookup_name_state(&self, name: &str, state: &str) -> Option<&GazetteerEntry> {
        self.by_name_state
            .get(&(name.to_lowercase(), state.to_uppercase()))
    }

    /// Look up by place name and state, or by name alone when `state` is `None`.
    ///
    /// When `state` is `None`, returns `None` (ambiguous without state context).
    #[must_use]
    pub fn lookup(&self, name: &str, state: Option<&str>) -> Option<&GazetteerEntry> {
        let s = state?;
        self.lookup_name_state(name, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI_CSV: &str = "P,Austin,TX,30.2672,-97.7431,\nZ,78701,TX,30.2707,-97.7424\n";

    #[test]
    fn parse_place_row() {
        let gz = Gazetteer::load_csv(MINI_CSV).expect("parse");
        let entry = gz.lookup_name_state("austin", "TX").expect("found");
        assert_eq!(entry.state, "TX");
        // lat within reasonable range of Austin
        assert!((entry.lat - 30.2672_f64).abs() < 0.001);
    }

    #[test]
    fn parse_zip_row() {
        let gz = Gazetteer::load_csv(MINI_CSV).expect("parse");
        let entry = gz.lookup_zip("78701").expect("found");
        assert_eq!(entry.state, "TX");
    }

    #[test]
    fn unknown_zip_returns_none() {
        let gz = Gazetteer::load_csv(MINI_CSV).expect("parse");
        assert!(gz.lookup_zip("00000").is_none());
    }

    #[test]
    fn case_insensitive_name() {
        let gz = Gazetteer::load_csv(MINI_CSV).expect("parse");
        assert!(gz.lookup_name_state("AUSTIN", "TX").is_some());
        assert!(gz.lookup_name_state("austin", "tx").is_some());
    }
}
