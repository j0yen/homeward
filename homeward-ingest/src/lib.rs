//! `homeward-ingest` — freshness engine for the homeward shelter-pet aggregator.
//!
//! Turns connector streams of [`PetRecord`]s into a single, fresh,
//! de-duplicated, self-expiring canonical sqlite store.  Ships:
//!
//! - [`store::Store`] — embedded sqlite store of canonical records.
//! - [`dedup`] — entity-resolution by `source_animal_id` and attribute key.
//! - [`orchestrator::Orchestrator`] — AIMD-cadence multi-connector loop.
//! - [`departure`] — explicit/implicit/TTL departure detection.
//! - [`events`] — change-event types (`intake.new`, `intake.updated`,
//!   `intake.departed`).
//! - [`embed_client`] — async HTTP client for the homeward-embed Python sidecar.

pub mod departure;
pub mod dedup;
pub mod embed_client;
pub mod events;
pub mod orchestrator;
pub mod store;
