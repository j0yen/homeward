//! Re-export of the embed HTTP client from the shared `homeward-embed-client` crate.
//!
//! All types have moved to [`homeward_embed_client`]; this module re-exports them
//! so that existing `homeward_ingest::embed_client::*` paths continue to resolve.

pub use homeward_embed_client::{
    EmbedClient, EmbedClientConfig, EmbedClientError, EnrollRequest, EnrollResponse, QueryMatch,
    QueryRequest,
};
