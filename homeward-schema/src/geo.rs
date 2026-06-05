//! Geographic types with coarsening for privacy.

use serde::{Deserialize, Serialize};

/// A shelter's location, stored at coarse precision.
///
/// Lat/lon are rounded to `precision` decimal places on construction to
/// prevent inadvertent precision leakage. Default precision is 2
/// (roughly ±1.1 km).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShelterLocation {
    /// Human-readable city or county.
    pub city_county: String,

    /// Two-letter state code, if available.
    #[serde(default)]
    pub state: Option<String>,

    /// Latitude rounded to `precision` decimal places.
    #[serde(default)]
    pub lat: Option<f64>,

    /// Longitude rounded to `precision` decimal places.
    #[serde(default)]
    pub lon: Option<f64>,

    /// Number of decimal places lat/lon are rounded to.
    pub precision: u8,
}

impl ShelterLocation {
    /// Construct a [`ShelterLocation`], rounding lat/lon to `precision` decimal places.
    ///
    /// `precision = 0` stores only integer degrees; `precision = 2` is roughly ±1.1 km.
    #[must_use]
    pub fn new(
        lat: Option<f64>,
        lon: Option<f64>,
        precision: u8,
        city_county: String,
        state: Option<String>,
    ) -> Self {
        #[allow(clippy::float_arithmetic)]
        let factor = 10_f64.powi(i32::from(precision));
        #[allow(clippy::float_arithmetic)]
        let coarse_lat = lat.map(|v| (v * factor).round() / factor);
        #[allow(clippy::float_arithmetic)]
        let coarse_lon = lon.map(|v| (v * factor).round() / factor);
        Self {
            city_county,
            state,
            lat: coarse_lat,
            lon: coarse_lon,
            precision,
        }
    }
}
