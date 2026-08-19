//! `homeward-wall` — "Paws & Petals": a public teaser wall of live shelter
//! dog/cat photos, read straight from the homeward ingest database.
//!
//! # Read-only posture
//! This crate never writes to the ingest DB. Every connection is opened
//! read-only via [`wall_db::open_readonly`] (`mode=ro&immutable=0`, plus a
//! busy-timeout) so the wall can run safely alongside `homeward-ingestd`'s
//! live WAL writer on the same file.
//!
//! # Surface
//! - [`server`] — the Axum router and HTTP handlers (`GET /`, `/health`,
//!   `/api/buddies`, `/api/stream`, `/api/stats`).
//! - [`stream`] — the background poller that watches the ingest DB for new
//!   photo-bearing records and broadcasts them to connected SSE clients.
//! - [`wall_db`] — read-only SQLite queries: cursor pagination, the
//!   photo-bearing filter, and aggregate stats.

#![deny(unsafe_code)]

pub mod server;
pub mod stream;
pub mod wall_db;

pub use server::{AppState, build_router};
pub use wall_db::{Buddy, BuddyPage, WallDbError, WallStats};
