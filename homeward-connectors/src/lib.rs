//! `homeward-connectors` — source connectors for the homeward fleet.
//!
//! This crate provides:
//! - The [`Connector`] async trait (poll, provenance, `cadence_hint`)
//! - A polite HTTP core (conditional requests, rate limiting, robots.txt)
//! - [`ArcGisConnector`] — ArcGIS REST Feature Service connector
//! - [`RescueGroupsConnector`] — JSON:API v5 paging from RescueGroups.org
//! - [`SocrataConnector`] — generic SODA client pre-configured for Austin,
//!   Dallas, Sonoma, and Long Beach municipal shelter datasets
//! - [`ConnectorRegistry`] for looking up connectors by name

pub mod catalog;
pub mod connector;
pub mod connectors;
pub mod coverage;
pub mod error;
pub mod http;
pub mod probe;
pub mod registry;

pub use catalog::{ArcGisConfig, OpenDataSoftConfig, SourceCatalog, SourceFamily, load_catalog};
pub use connector::{Connector, Cursor};
pub use connectors::arcgis::ArcGisConnector;
pub use connectors::petfbi::{PetFbiConfig, PetFbiConnector};
pub use connectors::rescuegroups::RescueGroupsConnector;
pub use connectors::socrata::{SocrataColumnMap, SocrataConfig, SocrataConnector};
pub use error::ConnectorError;
pub use registry::ConnectorRegistry;
