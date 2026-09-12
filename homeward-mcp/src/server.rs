//! The `rmcp::ServerHandler` implementation: `search_pets` and `get_pet`
//! tools, plus the `recent_intakes` resource.

use std::path::PathBuf;
use std::sync::Arc;

use homeward_schema::Species;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    ContentBlock, ErrorData as McpError, ListResourcesResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, tool, tool_handler, tool_router};

pub use rmcp::model::CallToolResult;

use crate::dto::{GetPetRequest, SearchPetsRequest, SearchPetsResult};
use crate::filter::{self, LocationFilter, SearchFilters};
use crate::query;

/// Default `search_pets`/`recent_intakes` page size when `limit` is omitted.
const DEFAULT_LIMIT: usize = 20;
/// Hard cap on rows returned per call, reusing `homeward-report`'s own
/// open-API result cap rather than re-declaring the constant.
fn max_limit() -> usize {
    homeward_report::api::ApiConfig::default().max_results_per_query
}
/// Default `search_pets` radius (km) when `radius_km` is omitted but `lat`/`lon` are given.
const DEFAULT_RADIUS_KM: f64 = 50.0;
/// URI for the `recent_intakes` resource.
const RECENT_INTAKES_URI: &str = "homeward://recent-intakes";

/// The read-only homeward MCP server: `search_pets`, `get_pet`, and the
/// `recent_intakes` resource, backed by the ingest `SQLite` DB.
#[derive(Clone)]
pub struct HomewardMcpServer {
    db_path: Arc<PathBuf>,
    // Read by the `#[tool_handler]`-generated `call_tool`/`list_tools`
    // dispatch (rustc's dead-code pass doesn't see through that macro
    // expansion, hence the allow).
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl HomewardMcpServer {
    /// Build a server reading from the given ingest DB path.
    #[must_use]
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path: Arc::new(db_path),
            tool_router: Self::tool_router(),
        }
    }

    /// Build a server reading the DB path from `HOMEWARD_INGEST_DB` (or the
    /// default under `$HOME/.local/share/homeward/`).
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(query::resolve_db_path())
    }

    /// Load every shelter intake record, off the async runtime's worker
    /// thread (the DB read is synchronous rusqlite I/O).
    async fn load_records(&self) -> Result<Vec<homeward_schema::PetRecord>, String> {
        let path = self.db_path.as_ref().clone();
        match tokio::task::spawn_blocking(move || query::load_records(&path)).await {
            Ok(result) => result,
            Err(e) => Err(format!("worker task failed: {e}")),
        }
    }

    fn unavailable() -> CallToolResult {
        CallToolResult::error(vec![ContentBlock::text(
            "shelter database is currently unavailable",
        )])
    }
}

#[tool_router]
impl HomewardMcpServer {
    /// Search shelter intakes by species and location.
    #[tool(
        name = "search_pets",
        description = "Search shelter intakes by species (dog|cat) and location (lat+lon+radius_km, or postal_code), optionally filtered by since (RFC3339), breed, and color. Returns up to 50 records with a hotlinked photo URL, coarse location, and a brokered shelter contact route -- never owner PII."
    )]
    pub async fn search_pets(
        &self,
        Parameters(req): Parameters<SearchPetsRequest>,
    ) -> Result<CallToolResult, McpError> {
        if !query::db_reachable(&self.db_path) {
            return Ok(Self::unavailable());
        }

        let species = match Species::from_str_strict(&req.species) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "invalid species: {e}"
                ))]));
            }
        };

        let location = match (req.lat, req.lon, req.postal_code.as_deref()) {
            (Some(lat), Some(lon), _) => LocationFilter::LatLon {
                lat,
                lon,
                radius_km: req.radius_km.unwrap_or(DEFAULT_RADIUS_KM),
            },
            (_, _, Some(postal)) => LocationFilter::Postal(postal.to_owned()),
            _ => LocationFilter::None,
        };

        let since = match req.since.as_deref() {
            Some(s) => match chrono::DateTime::parse_from_rfc3339(s) {
                Ok(dt) => Some(dt.with_timezone(&chrono::Utc)),
                Err(e) => {
                    return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "invalid since timestamp: {e}"
                    ))]));
                }
            },
            None => None,
        };

        let filters = SearchFilters {
            species,
            location,
            since,
            breed: req.breed.as_deref(),
            color: req.color.as_deref(),
        };

        let records = match self.load_records().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "shelter database read failed: {e}"
                ))]));
            }
        };

        let limit = req
            .limit
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, max_limit());

        let matched: Vec<&homeward_schema::PetRecord> = records
            .iter()
            .filter(|r| filter::matches(r, &filters))
            .collect();
        let truncated = matched.len() > limit;
        let pets = matched
            .into_iter()
            .take(limit)
            .map(filter::to_summary)
            .collect();

        Ok(to_json_result(&SearchPetsResult { pets, truncated }))
    }

    /// Fetch a single shelter intake by its canonical id.
    #[tool(
        name = "get_pet",
        description = "Fetch a single shelter intake by its canonical id (as returned by search_pets). Returns the same redacted shape as search_pets."
    )]
    pub async fn get_pet(
        &self,
        Parameters(req): Parameters<GetPetRequest>,
    ) -> Result<CallToolResult, McpError> {
        if !query::db_reachable(&self.db_path) {
            return Ok(Self::unavailable());
        }

        let records = match self.load_records().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "shelter database read failed: {e}"
                ))]));
            }
        };

        match records.iter().find(|r| r.canonical_id.to_string() == req.id) {
            Some(record) => Ok(to_json_result(&filter::to_summary(record))),
            None => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "no pet found with id {}",
                req.id
            ))])),
        }
    }
}

/// Serialize `value` into both the human-readable `content` field and the
/// `structured_content` field of a successful [`CallToolResult`].
fn to_json_result<T: serde::Serialize>(value: &T) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("serialize error: {e}")),
    )]);
    result.structured_content = serde_json::to_value(value).ok();
    result
}

#[tool_handler]
impl ServerHandler for HomewardMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(RECENT_INTAKES_URI, "recent_intakes")
                .with_description(
                    "Latest shelter intakes, newest first. Paged via ?limit=N (default 20, max 50).",
                )
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, McpError> {
        if !request.uri.starts_with(RECENT_INTAKES_URI) {
            return Err(McpError::resource_not_found(request.uri, None));
        }

        if !query::db_reachable(&self.db_path) {
            return Err(McpError::internal_error(
                "shelter database is currently unavailable",
                None,
            ));
        }

        let limit = parse_limit_query(&request.uri).clamp(1, max_limit());

        let records = self
            .load_records()
            .await
            .map_err(|e| McpError::internal_error(format!("shelter database read failed: {e}"), None))?;

        let pets: Vec<_> = records.iter().take(limit).map(filter::to_summary).collect();
        let body = serde_json::to_string_pretty(&pets)
            .map_err(|e| McpError::internal_error(format!("serialize result: {e}"), None))?;

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            body,
            RECENT_INTAKES_URI,
        )
        .with_mime_type("application/json")])
        .into())
    }
}

/// Parse a `?limit=N` query param off a `homeward://recent-intakes` URI.
/// Any parse failure (missing scheme, missing/invalid `limit`) falls back
/// to [`DEFAULT_LIMIT`] rather than erroring -- this is a paging hint, not
/// a validated tool argument.
fn parse_limit_query(uri: &str) -> usize {
    url::Url::parse(uri)
        .ok()
        .and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "limit")
                .and_then(|(_, v)| v.parse::<usize>().ok())
        })
        .unwrap_or(DEFAULT_LIMIT)
}
