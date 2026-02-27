use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::storage::models::{LogMode, StreamFilter};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiscoverServicesParams {
    /// Filter by command name (substring match)
    pub name: Option<String>,
    /// Filter by tags in "key:value" format
    pub tags: Option<Vec<String>>,
    /// Filter by detected port number
    pub port: Option<u16>,
    /// Filter by executable name (substring match)
    pub executable: Option<String>,
    /// Filter by working directory of the tracked command (substring match)
    pub cwd: Option<String>,
    /// Filter by run status: running, completed, failed
    pub status: Option<String>,
    /// Semantic search query (requires LLM)
    pub query: Option<String>,
    /// Maximum number of results (default 20)
    pub limit: Option<usize>,
    /// Group commands by executable and working directory, showing only the latest run per group. Defaults to true.
    pub group: Option<bool>,
    /// Include a preview of the last N lines of stdout/stderr output from the latest run. Omit or set to 0 to skip.
    pub tail_lines: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoverServicesResponse {
    pub services: Vec<ServiceInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GroupedServiceInfo {
    pub executable: String,
    pub working_dir: String,
    pub run_count: usize,
    pub names: Vec<String>,
    pub latest_run: Option<RunInfo>,
    pub commands: Vec<String>,
    pub ports: Vec<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_preview: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoverServicesGroupedResponse {
    pub groups: Vec<GroupedServiceInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub executable: String,
    pub command_line: Vec<String>,
    pub working_dir: String,
    pub tags: Vec<TagInfo>,
    pub latest_run: Option<RunInfo>,
    pub ports: Vec<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_preview: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TagInfo {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunInfo {
    pub id: String,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
    pub wrapper_pid: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetLogsParams {
    /// Command name, command ID, or run ID (prefix match supported)
    pub id: String,
    /// Which output stream to read: stdout, stderr, stdin, combined (default: combined)
    pub stream: Option<StreamFilter>,
    /// Read mode: head, tail, range (default: tail)
    pub mode: Option<LogMode>,
    /// Number of lines for head/tail mode (default: 100)
    pub lines: Option<usize>,
    /// Start time (ns since epoch) for range mode
    pub start_time: Option<u64>,
    /// End time (ns since epoch) for range mode
    pub end_time: Option<u64>,
    /// Maximum bytes to return (default: 51200)
    pub max_bytes: Option<usize>,
    /// Strip ANSI escape codes from output. Useful for programmatic consumers. Defaults to true.
    pub strip_ansi: Option<bool>,
    /// Only return output with timestamps >= this value (nanoseconds since epoch).
    /// Useful for incremental polling — pass the timestamp of the last frame you received
    /// to get only newer output. Omit to get all output matching the mode/lines criteria.
    pub since: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct GetLogsResponse {
    pub content: String,
    pub frame_count: usize,
    pub has_more: bool,
    pub stream: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchLogsParams {
    /// Regex pattern to search for in command output
    pub pattern: String,
    /// Filter by command ID (to search a specific command's output)
    pub service_id: Option<String>,
    /// Which output stream to search: stdout, stderr, stdin, combined (default: combined)
    pub stream: Option<StreamFilter>,
    /// Start time filter (ns since epoch)
    pub start_time: Option<u64>,
    /// End time filter (ns since epoch)
    pub end_time: Option<u64>,
    /// Number of context lines around each match
    pub context_lines: Option<usize>,
    /// Maximum number of matches (default: 50)
    pub max_matches: Option<usize>,
    /// Strip ANSI escape codes from output. Useful for programmatic consumers. Defaults to true.
    pub strip_ansi: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct SearchLogsResponse {
    pub matches: Vec<SearchMatch>,
    pub total_matches: usize,
}

#[derive(Debug, Serialize)]
pub struct SearchMatch {
    pub service_id: String,
    pub service_name: Option<String>,
    pub run_id: String,
    pub stream: String,
    pub timestamp_ns: u64,
    pub line: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitForPatternParams {
    /// Command name, command ID, or run ID
    pub id: String,
    /// Regex pattern to match against stdout/stderr output lines
    pub pattern: String,
    /// Which output stream to watch: stdout, stderr, stdin, combined (default: combined)
    pub stream: Option<StreamFilter>,
    /// Timeout in seconds (default: 30)
    pub timeout: Option<u64>,
    /// How often to check for new logs in milliseconds (default: 500)
    pub poll_interval_ms: Option<u64>,
    /// Strip ANSI escape codes before matching (default: true)
    pub strip_ansi: Option<bool>,
    /// Only match output lines with timestamps >= this value (nanoseconds since epoch).
    /// Defaults to the current time, so only new output is matched.
    /// Set to 0 to match against the full output history.
    pub since: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct WaitForPatternResponse {
    /// Whether the pattern was found
    pub matched: bool,
    /// The matching line (if found)
    pub line: Option<String>,
    /// Timestamp of the matching frame in nanoseconds since epoch
    pub timestamp_ns: Option<u64>,
    /// How long the wait took in milliseconds
    pub elapsed_ms: u64,
    /// Whether the timeout was reached without finding a match
    pub timed_out: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KillServiceParams {
    /// Command name, command ID, or ID prefix
    pub id: String,
    /// Signal to send: TERM (default), KILL, INT, HUP, USR1, USR2, QUIT, or a numeric signal
    pub signal: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KillServiceResponse {
    /// Whether the signal was sent successfully
    pub success: bool,
    /// Service name
    pub service_name: String,
    /// Service ID
    pub service_id: String,
    /// Signal that was sent
    pub signal: String,
    /// PIDs that received the signal
    pub pids: Vec<u32>,
    /// Human-readable summary
    pub message: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RestartServiceParams {
    /// Command name, command ID, or ID prefix
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct RestartServiceResponse {
    /// Whether the restart signal was sent successfully
    pub success: bool,
    /// Service name
    pub service_name: String,
    /// Service ID
    pub service_id: String,
    /// Wrapper PID that received SIGUSR1
    pub wrapper_pid: u32,
    /// Human-readable summary
    pub message: String,
}
