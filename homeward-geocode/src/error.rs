//! Error types for homeward-geocode.

use thiserror::Error;

/// Errors that can arise during geocoding operations.
#[derive(Debug, Error)]
pub enum GeocodeError {
    /// The gazetteer data file could not be read.
    #[error("failed to read gazetteer data: {0}")]
    DataRead(String),

    /// A row in the gazetteer CSV was malformed.
    #[error("malformed gazetteer row at line {line}: {reason}")]
    MalformedRow {
        /// 1-based line number in the CSV.
        line: usize,
        /// Description of the parse failure.
        reason: String,
    },
}
