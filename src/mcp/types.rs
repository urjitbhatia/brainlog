use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::storage::models::{LogMode, StreamFilter};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiscoverServicesParams {
    /// Filter by service name (substring match)
    pub name: Option<String>,
    /// Filter by tags in "key:value" format
    pub tags: Option<Vec<String>>,
    /// Filter by detected port number
    pub port: Option<u16>,
    /// Filter by executable name (substring match)
    pub executable: Option<String>,
    /// Filter by run status: running, completed, failed
    pub status: Option<String>,
    /// Semantic search query (requires LLM)
    pub query: Option<String>,
    /// Maximum number of results (default 20)
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct DiscoverServicesResponse {
    pub services: Vec<ServiceInfo>,
}

#[derive(Debug, Serialize)]
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
}

#[derive(Debug, Serialize)]
pub struct TagInfo {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct RunInfo {
    pub id: String,
    pub status: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetLogsParams {
    /// Service ID or run ID
    pub id: String,
    /// Stream to read: stdout, stderr, stdin, combined (default: combined)
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
    /// Regex pattern to search for
    pub pattern: String,
    /// Filter by service ID
    pub service_id: Option<String>,
    /// Stream to search: stdout, stderr, stdin, combined (default: combined)
    pub stream: Option<StreamFilter>,
    /// Start time filter (ns since epoch)
    pub start_time: Option<u64>,
    /// End time filter (ns since epoch)
    pub end_time: Option<u64>,
    /// Number of context lines around each match
    pub context_lines: Option<usize>,
    /// Maximum number of matches (default: 50)
    pub max_matches: Option<usize>,
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
