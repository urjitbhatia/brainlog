pub mod tools;
pub mod types;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use std::path::PathBuf;

use crate::storage::Database;

use types::*;

/// Brainlog MCP server.
#[derive(Clone)]
pub struct BrainlogMcp {
    db_path: PathBuf,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl BrainlogMcp {
    /// Create a new BrainlogMcp server.
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            tool_router: Self::tool_router(),
        }
    }

    fn open_db(&self) -> Result<Database, McpError> {
        Database::open(&self.db_path).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("Database error: {}", e).into(),
            data: None,
        })
    }

    /// Discover tracked services. Filter by name, tags, port, executable, or status.
    #[tool(
        description = "Discover tracked services. Filter by name, tags, port, executable, or status. Returns service metadata, latest run info, and detected ports."
    )]
    async fn discover_services(
        &self,
        params: Parameters<DiscoverServicesParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let response = tools::discover_services(&db, params.0).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        let json = serde_json::to_string_pretty(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Get logs for a service or run.
    #[tool(
        description = "Get logs for a service or run. Supports head/tail/range modes with configurable line count and max bytes."
    )]
    async fn get_logs(
        &self,
        params: Parameters<GetLogsParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let response = tools::get_logs(&db, params.0).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        let json = serde_json::to_string_pretty(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Search logs across services using regex patterns.
    #[tool(
        description = "Search logs across services using regex patterns. Returns matching lines with timestamps and context."
    )]
    async fn search_logs(
        &self,
        params: Parameters<SearchLogsParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let response = tools::search_logs(&db, params.0).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        let json = serde_json::to_string_pretty(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for BrainlogMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Brainlog MCP server. Provides tools to discover tracked services, query their logs, and search across all logs."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            ..Default::default()
        }
    }
}
