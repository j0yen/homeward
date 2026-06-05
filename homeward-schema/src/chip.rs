//! Microchip scan status for shelter animals.

use serde::{Deserialize, Serialize};

/// Result of a microchip scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ChipStatus {
    /// Animal was scanned; chip ID present if scanner read one.
    Scanned {
        /// The chip number, if the scanner returned one.
        #[serde(default)]
        chip: Option<String>,
    },
    /// Animal was scanned; no chip detected.
    ScanNoChip,
    /// Animal has not been scanned yet.
    NotScanned,
    /// Chip status unknown / not recorded.
    Unknown,
}
