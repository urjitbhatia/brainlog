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

    /// List recent runs across all tracked commands, newest first.
    #[tool(
        description = "List the most recent runs across ALL tracked commands, sorted newest first. Unlike discover_services (which groups by service), this returns individual runs with service metadata inlined — ideal for 'what just happened?' queries.\n\nReturns: run_id, service_id, service_name, executable, command_line, working_dir, status, started_at, ended_at, exit_code, pid, and optional log_preview.\n\nFilters: cwd (working directory substring), command (command line substring), exit_code (exact match, e.g. 0 or 1), status (running|completed|failed|crashed|killed). Use tail_lines to include a preview of recent output. Use limit to cap results (default 20)."
    )]
    fn list_recent_runs(
        &self,
        params: Parameters<ListRecentRunsParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let response = tools::list_recent_runs(&db, params.0).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        let json = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Discover tracked commands.
    #[tool(
        description = "List commands tracked by brainlog. Brainlog wraps any command (build tools, dev servers, scripts, long-running services) and captures their stdout, stderr, and stdin. Use this tool to discover what commands are being tracked so you can read their output.\n\nReturns: command name, executable, working directory, full command line, latest run info (status, PID, exit code, timestamps), detected TCP ports, and optional log preview.\n\nFilters: name (substring), tags (key:value format, AND logic), port (exact u16), executable (substring), cwd (substring of working directory), status (running|completed|failed), exit_code (exact match on latest run's exit code, e.g. 0 for success, 1 for failure), query (semantic search via LLM).\n\nResults are grouped by executable+working_dir by default (set group=false for flat list). Use tail_lines to include a preview of recent stdout/stderr output. Use limit to cap results (default 20)."
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
        description = "Read the stdout, stderr, or stdin output of a command tracked by brainlog. Use this to see what a command has printed — build output, server logs, error messages, etc.\n\nIdentification: Provide EITHER id (command name, command ID, or run ID with prefix match) OR cwd (working directory substring to find the most recent run in that directory). Using cwd avoids a discover_services round-trip when you know the project path.\n\nModes: tail (default, last N lines), head (first N lines), range (by timestamp). Stream: combined (default), stdout, stderr, stdin.\n\nUse since (nanoseconds since epoch) for incremental polling — pass the timestamp of your last-seen frame to get only newer output. Use max_bytes to limit response size (default 51200). ANSI escape codes are stripped by default (set strip_ansi=false to preserve)."
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

    /// Send a signal to stop a running command.
    #[tool(
        description = "Send a signal (SIGTERM by default) to a running command tracked by brainlog. Kills the entire process tree (child processes first, then parent). Use this to stop a dev server, build process, or any running command.\n\nThe id parameter accepts a command name, command ID, or ID prefix. Supported signals: TERM (default, graceful), KILL (force), INT, HUP, USR1, USR2, QUIT, or a numeric signal.\n\nIf the child process has exited but the brainlog wrapper is still alive (e.g. in a restart loop), the signal is sent to the wrapper instead."
    )]
    async fn kill_service(
        &self,
        params: Parameters<KillServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        // Phase 1: resolve synchronously (Database is not Sync).
        let db = self.open_db()?;
        let resolved = tools::kill_service_resolve(&db, &params.0).map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        drop(db);

        // Phase 2: async kill (no &Database across await points).
        let response = tools::kill_service(resolved).await.map_err(|e| McpError {
            code: ErrorCode::INTERNAL_ERROR,
            message: format!("{}", e).into(),
            data: None,
        })?;
        let json = serde_json::to_string(&response).unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Restart a running command by sending SIGUSR1 to its wrapper.
    #[tool(
        description = "Restart a running command tracked by brainlog. Sends SIGUSR1 to the brainlog wrapper process, which gracefully stops the current child and respawns it. The command must have been started with brainlog (so it has a wrapper PID).\n\nThe id parameter accepts a command name, command ID, or ID prefix. The command must be currently running. After restart, use wait_for_pattern to confirm the new instance started successfully."
    )]
    fn restart_service(
        &self,
        params: Parameters<RestartServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.open_db()?;
        let response = tools::restart_service(&db, params.0).map_err(|e| McpError {
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
                concat!(
                    "Brainlog MCP server. Brainlog wraps commands (build tools, dev servers, scripts, etc.) ",
                    "and captures their stdout, stderr, and stdin in real time. Use these tools to see what ",
                    "commands have printed without asking the user to copy-paste terminal output.\n\n",
                    "IMPORTANT: Brainlog tracks commands started by ANYONE — you, the user, or other processes. ",
                    "The user may be running a dev server, database, build tool, or other commands in separate ",
                    "terminals. You can observe their output too. Use discover_services to see everything that's ",
                    "running in the environment, not just commands you started.\n\n",
                    "WHEN TO USE:\n",
                    "- A build, test, or script is running and you need to check its output or errors\n",
                    "- You started a server/process and need to confirm it's ready (use wait_for_pattern)\n",
                    "- The user mentions something is running or failing — check brainlog before asking them to paste logs\n",
                    "- You need context about the dev environment — discover what's running, read their output\n",
                    "- You need to search across multiple commands for a pattern (e.g. 'error', 'panic', a port number)\n\n",
                    "WORKFLOW:\n",
                    "1. list_recent_runs — see the last N runs across all commands, newest first (best for 'what just happened?')\n",
                    "2. discover_services — find what commands are tracked, grouped by executable+directory\n",
                    "3. get_logs — read stdout/stderr of a specific command (by id or cwd shorthand)\n",
                    "4. search_logs — grep across all commands with regex\n",
                    "5. wait_for_pattern — block until expected output appears (e.g. 'listening on port 3000')\n",
                    "6. kill_service — send a signal (TERM, KILL, etc.) to stop a running command\n",
                    "7. restart_service — restart a running command (sends SIGUSR1 to the wrapper)\n\n",
                    "TIPS:\n",
                    "- Use get_logs with stream='stderr' to focus on errors\n",
                    "- Use since parameter for incremental polling (only new output since last check)\n",
                    "- After running a command via Bash, use wait_for_pattern to confirm it started successfully\n",
                    "- When the user says 'the server is crashing' or 'the build failed', check brainlog first\n",
                    "- If the user is running a command you can't see (not tracked by brainlog), suggest they ",
                    "wrap it with brainlog so you can inspect its output: 'brainlog <command>' instead of '<command>'",
                )
                    .into(),
            ),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            ..Default::default()
        }
    }
}
