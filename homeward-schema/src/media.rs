//! Media types for shelter animals.
//!
//! `PhotoRef` stores only a hotlink URL — no raw image bytes.
//! This encodes the copyright posture from Phase 1 §1d: images are
//! referenced, never bulk-copied.

use serde::{Deserialize, Serialize};

/// A reference to a photo of an animal.
///
/// Stores only a URL to hotlink and optional attribution.
/// There is intentionally **no** bytes or data field — this is a
/// type-level guarantee that this crate never holds raw image data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoRef {
    /// URL to hotlink when displaying the photo.
    pub url: String,

    /// Optional attribution string required by the source's `ToS`.
    #[serde(default)]
    pub attribution: Option<String>,

    /// Whether this is the primary/featured photo.
    #[serde(default)]
    pub is_primary: bool,
}

impl PhotoRef {
    /// Create a new `PhotoRef` with just a URL.
    #[must_use]
    pub const fn new(url: String) -> Self {
        Self {
            url,
            attribution: None,
            is_primary: false,
        }
    }

    /// Create a `PhotoRef` with attribution.
    #[must_use]
    pub const fn with_attribution(url: String, attribution: String) -> Self {
        Self {
            url,
            attribution: Some(attribution),
            is_primary: false,
        }
    }
}
