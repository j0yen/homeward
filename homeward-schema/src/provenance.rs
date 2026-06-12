//! Source identity and provenance for data records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Terms-of-service class for a data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TosClass {
    /// Official API with published `ToS`.
    Api,
    /// Open-data portal (e.g. Socrata).
    OpenData,
    /// Scraped (no explicit API permission).
    Scraped,
    /// Nonprofit community source requiring attribution on display.
    NonprofitAttributionRequired,
}

/// Identifies a data source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceId {
    /// Short slug for the source (e.g. `"rescuegroups"`, `"seattle_socrata"`).
    pub name: String,
    /// Terms-of-service class for this source.
    pub tos_class: TosClass,
}

impl SourceId {
    /// Create a new [`SourceId`].
    #[must_use]
    pub fn new(name: impl Into<String>, tos_class: TosClass) -> Self {
        Self {
            name: name.into(),
            tos_class,
        }
    }
}

/// Provenance metadata attached to a record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    /// The source that produced this record.
    pub source: SourceId,

    /// When this record was fetched from the source.
    pub fetched_at: DateTime<Utc>,

    /// The canonical URL of the source record, if available.
    #[serde(default)]
    pub source_url: Option<String>,

    /// Opaque checksum/ETag from the source for change detection.
    #[serde(default)]
    pub source_etag: Option<String>,
}
