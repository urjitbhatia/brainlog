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

    /// Discover tracked commands.
    #[tool(
        description = "List commands tracked by brainlog. Brainlog wraps any command (build tools, dev servers, scripts, long-running services) and captures their stdout, stderr, and stdin. Use this tool to discover what commands are being tracked so you can read their output.\n\nReturns: command name, executable, working directory, full command line, latest run info (status, PID, exit code, timestamps), detected TCP ports, and optional log preview.\n\nFilters: name (substring), tags (key:value format, AND logic), port (exact u16), executable (substring), cwd (substring of working directory), status (running|completed|failed), query (semantic search via LLM).\n\nResults are grouped by executable+working_dir by default (set group=false for flat list). Use tail_lines to include a preview of recent stdout/stderr output. Use limit to cap results (default 20)."
    )]
    async fn discover_services(
        &self,
        params: Parameters<DiscoverServicesParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let value = tools::discover_services(&db, params.0).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        let json = serde_json::to_string(&value).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Read stdout/stderr output of a tracked command.
    #[tool(
        description = "Read the stdout, stderr, or stdin output of a command tracked by brainlog. Use this to see what a command has printed — build output, server logs, error messages, etc. The id parameter accepts a command name, command ID, or run ID (prefix match supported).\n\nModes: tail (default, last N lines), head (first N lines), range (by timestamp). Stream: combined (default), stdout, stderr, stdin.\n\nUse since (nanoseconds since epoch) for incremental polling — pass the timestamp of your last-seen frame to get only newer output. Use max_bytes to limit response size (default 51200). ANSI escape codes are stripped by default (set strip_ansi=false to preserve)."
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
        let json = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Search stdout/stderr output across commands using regex.
    #[tool(
        description = "Search the captured stdout/stderr output of commands tracked by brainlog using regex patterns. Use this to find error messages, specific log lines, or any text across all tracked commands (or a specific one). Returns matching lines with timestamps, stream type (stdout/stderr/stdin), command ID, and run ID.\n\nSupports Rust regex syntax including alternation (error|warn), character classes, and quantifiers. Filter by service_id, stream (stdout/stderr/stdin/combined), and time range (start_time/end_time in nanoseconds since epoch). Use max_matches to limit results (default 50). ANSI codes are stripped by default."
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
        let json = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Wait for a pattern to appear in a command's output.
    #[tool(
        description = "Block until a regex pattern appears in a command's stdout/stderr output, or timeout. Similar to Playwright's wait_for_text. Ideal for verifying async behavior: start a build or server, then wait_for_pattern to confirm it printed expected output (e.g. 'listening on port', 'build succeeded', 'error').\n\nThe id parameter accepts a command name, command ID, or run ID. By default only matches NEW output (since=now). Set since=0 to search full history. Supports Rust regex with alternation (started|error). Configurable timeout (default 30s) and poll_interval_ms (default 500ms). Returns the matching line, its timestamp, and elapsed wait time."
    )]
    async fn wait_for_pattern(
        &self,
        params: Parameters<WaitForPatternParams>,
    ) -> Result<CallToolResult, McpError> {
        // Phase 1: resolve the log directory synchronously (Database is not Sync).
        let db = self.open_db()?;
        let log_dir = tools::wait_for_pattern_resolve(&db, &params.0).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        drop(db);

        // Phase 2: async polling loop (no &Database across await points).
        let response = tools::wait_for_pattern(&log_dir, params.0)
            .await
            .map_err(|e| McpError {
                code: ErrorCode::INTERNAL_ERROR,
                message: format!("{}", e).into(),
                data: None,
            })?;
        let json = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for BrainlogMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Brainlog MCP server. Brainlog wraps commands (build tools, dev servers, scripts, etc.) and captures their stdout, stderr, and stdin. Use these tools to discover tracked commands, read their output, and search across all captured output."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            ..Default::default()
        }
    }
}
