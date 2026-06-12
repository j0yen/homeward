//! Pluggable [`Syndicator`] trait and implementations for homeward-report.
//!
//! # Honest channel policy
//!
//! Only [`LocalArtifactSyndicator`] claims real delivery (it writes to disk).
//! All gated social/partner channels ([`GatedChannelSyndicator`]) return
//! [`SyndicationOutcome::ManualOnly`] — no code pretends to deliver to them.
//! The optional PetFBI partner syndicator is behind feature `petfbi-partner`
//! and always returns [`SyndicationOutcome::DryRun`] until a real endpoint
//! and credential are configured.

use std::path::PathBuf;

use crate::export::LostReportExport;

// ─── Outcome ──────────────────────────────────────────────────────────────────

/// The result of a syndication attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum SyndicationOutcome {
    /// Artifacts written to disk (JSON-LD + flyer).
    Written {
        /// Paths of the written artifacts.
        paths: Vec<PathBuf>,
    },
    /// Dry-run: no delivery attempted (e.g., no endpoint/credential configured).
    DryRun {
        /// Human-readable reason for dry-run mode.
        reason: String,
    },
    /// Channel exists but only accepts manual posting; no automated path.
    ManualOnly {
        /// The channel that was considered.
        channel: String,
        /// Why no automated path exists.
        reason: String,
    },
    /// A real delivery was attempted and failed.
    Failed {
        /// The channel that failed.
        channel: String,
        /// Error description.
        error: String,
    },
}

// ─── Trait ────────────────────────────────────────────────────────────────────

/// Pluggable syndication adapter.
///
/// Receives a [`LostReportExport`] and the pre-rendered flyer string.
/// Implementations MUST NOT return a "success" variant without actual confirmed
/// delivery. Dry-run or manual-only are always safe defaults.
pub trait Syndicator: Send + Sync {
    /// Attempt to syndicate the export + flyer text.
    fn syndicate(&self, export: &LostReportExport, flyer: &str) -> SyndicationOutcome;
}

// ─── LocalArtifactSyndicator ──────────────────────────────────────────────────

/// Always-on syndicator: writes JSON-LD + flyer to a configured output directory.
///
/// Returns [`SyndicationOutcome::Written`] with both artifact paths on success.
#[derive(Debug, Clone)]
pub struct LocalArtifactSyndicator {
    /// Directory to write artifacts into.
    pub output_dir: PathBuf,
}

impl LocalArtifactSyndicator {
    /// Create a new syndicator that writes to `output_dir`.
    #[must_use]
    pub fn new(output_dir: impl Into<PathBuf>) -> Self {
        Self {
            output_dir: output_dir.into(),
        }
    }
}

impl Syndicator for LocalArtifactSyndicator {
    fn syndicate(&self, export: &LostReportExport, flyer: &str) -> SyndicationOutcome {
        let json_path = self
            .output_dir
            .join(format!("{}.jsonld", export.report_id));
        let flyer_path = self
            .output_dir
            .join(format!("{}.flyer.txt", export.report_id));

        let json_str = match serde_json::to_string_pretty(export) {
            Ok(s) => s,
            Err(e) => {
                return SyndicationOutcome::Failed {
                    channel: "local".to_owned(),
                    error: format!("json serialize: {e}"),
                };
            }
        };

        if let Err(e) = std::fs::write(&json_path, &json_str) {
            return SyndicationOutcome::Failed {
                channel: "local".to_owned(),
                error: format!("write jsonld: {e}"),
            };
        }
        if let Err(e) = std::fs::write(&flyer_path, flyer) {
            return SyndicationOutcome::Failed {
                channel: "local".to_owned(),
                error: format!("write flyer: {e}"),
            };
        }

        SyndicationOutcome::Written {
            paths: vec![json_path, flyer_path],
        }
    }
}

// ─── GatedChannelSyndicator ───────────────────────────────────────────────────

/// Reports as [`SyndicationOutcome::ManualOnly`] for channels with no automated
/// write path (PawBoost, Nextdoor, Facebook Groups, Petco Love Lost, etc.).
///
/// No delivery is ever attempted; this exists only to document the reason
/// and provide a uniform outlet in syndication pipelines.
#[derive(Debug, Clone)]
pub struct GatedChannelSyndicator {
    /// Channel name (e.g., "PawBoost", "Nextdoor").
    pub channel: String,
    /// Documented reason why no automated path exists.
    pub reason: String,
}

impl GatedChannelSyndicator {
    /// Create a gated-channel syndicator.
    #[must_use]
    pub fn new(channel: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            reason: reason.into(),
        }
    }
}

impl Syndicator for GatedChannelSyndicator {
    fn syndicate(&self, _export: &LostReportExport, _flyer: &str) -> SyndicationOutcome {
        SyndicationOutcome::ManualOnly {
            channel: self.channel.clone(),
            reason: self.reason.clone(),
        }
    }
}

// ─── PetFbiPartnerSyndicator (feature-gated) ─────────────────────────────────

/// Dry-run-by-default adapter for a hypothetical Pet FBI partner POST endpoint.
///
/// Without a real endpoint URL and credential, always returns
/// [`SyndicationOutcome::DryRun`]. **Never returns success without a 2xx.**
///
/// Enabled by the `petfbi-partner` Cargo feature (off by default).
#[cfg(feature = "petfbi-partner")]
#[derive(Debug, Clone, Default)]
pub struct PetFbiPartnerSyndicator {
    /// Partner POST endpoint URL. `None` → dry-run.
    pub endpoint_url: Option<String>,
    /// API credential (bearer token or key). `None` → dry-run.
    pub credential: Option<String>,
}

#[cfg(feature = "petfbi-partner")]
impl PetFbiPartnerSyndicator {
    /// Create an unconfigured adapter (dry-run by default).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(feature = "petfbi-partner")]
impl Syndicator for PetFbiPartnerSyndicator {
    fn syndicate(&self, _export: &LostReportExport, _flyer: &str) -> SyndicationOutcome {
        // No real endpoint established yet.
        // Even with config, return DryRun until a live 2xx is confirmed.
        SyndicationOutcome::DryRun {
            reason: "PetFBI partner endpoint not yet established; dry-run only".to_owned(),
        }
    }
}
