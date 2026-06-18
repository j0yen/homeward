//! `homeward-geocode` — offline geocoder for [`PetRecord::found_location_text`].
//!
//! Resolves common free-form "found at..." strings to a coarse [`ShelterLocation`]
//! using a bundled public-domain US Census Gazetteer — no network access at runtime.
//!
//! ## Usage
//!
//! ```rust
//! use homeward_geocode::{Gazetteer, enrich_record};
//! use homeward_schema::PetRecord;
//!
//! let gz = Gazetteer::load_default().expect("gazetteer load");
//! // enrich_record fills PetRecord.location from found_location_text
//! // only when location is currently None.
//! ```

pub mod enrichment;
pub mod error;
pub mod gazetteer;
pub mod parser;

pub use enrichment::{enrich_record, EnrichmentResult};
pub use error::GeocodeError;
pub use gazetteer::{Gazetteer, GazetteerEntry, LocationSource};
pub use parser::{parse_found_location, ParsedLocation};
