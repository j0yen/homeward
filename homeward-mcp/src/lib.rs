//! `homeward-mcp` -- a read-only MCP server over the homeward shelter
//! database.
//!
//! Exposes `search_pets` and `get_pet` tools plus a `recent_intakes`
//! resource, over stdio and streamable HTTP from the same entry point.
//! Adapter only: no new storage, no writes, and the legal-ethics contract
//! from the original homeward PRDs carries into every tool output --
//! hotlinked photo URLs, coarse locations, brokered shelter contact, no
//! owner PII.

#![deny(unsafe_code)]

pub mod dto;
pub mod filter;
pub mod geo;
pub mod http;
pub mod query;
pub mod server;

pub use server::HomewardMcpServer;
