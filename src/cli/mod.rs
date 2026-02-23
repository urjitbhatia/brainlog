pub mod list;
pub mod logs;
pub mod mcp;
pub mod purge;
pub mod run;
pub mod search;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use crate::storage::models::StreamFilter;

#[derive(Parser, Debug)]
#[command(
    name = "brainlog",
    version,
    about = "Transparent process wrapper with log capture and MCP server"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run a command with logging (explicit mode)
    Run(RunArgs),
    /// List tracked services
    List(ListArgs),
    /// View logs for a service or run
    Logs(LogsArgs),
    /// Search logs by pattern
    Search(SearchArgs),
    /// Start the MCP server (stdio transport)
    Mcp,
    /// Purge old services and their logs
    Purge(PurgeArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct RunArgs {
    /// Name for this service (reuses existing service if name matches)
    #[arg(short, long)]
    pub name: Option<String>,

    /// Tags in key:value format
    #[arg(short, long, value_delimiter = ',')]
    pub tag: Vec<String>,

    /// Description for this service
    #[arg(short, long)]
    pub desc: Option<String>,

    /// Command and arguments to run
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<String>,
}

/// Parse a single tag string into (key, value), validating the format.
///
/// Tags must be in `key:value` format. The key must be non-empty.
/// If the string contains multiple colons, the split happens at the first colon
/// (e.g., `a:b:c` becomes key=`a`, value=`b:c`).
pub fn parse_tag(tag: &str) -> Result<(&str, &str)> {
    match tag.split_once(':') {
        None => {
            bail!(
                "invalid tag format '{}': expected key:value (missing ':' separator)",
                tag
            );
        }
        Some((key, value)) => {
            let key = key.trim();
            if key.is_empty() {
                bail!(
                    "invalid tag format '{}': key must not be empty (expected key:value)",
                    tag
                );
            }
            Ok((key, value.trim()))
        }
    }
}

/// Validate all tags in a RunArgs, returning an error on the first invalid tag.
pub fn validate_tags(tags: &[String]) -> Result<()> {
    for tag in tags {
        parse_tag(tag)?;
    }
    Ok(())
}

#[derive(Parser, Debug)]
pub struct ListArgs {
    /// Filter by name
    #[arg(short, long)]
    pub name: Option<String>,

    /// Show detailed info
    #[arg(short, long)]
    pub verbose: bool,

    /// Group services by executable and working directory
    #[arg(short, long)]
    pub group: bool,
}

#[derive(Parser, Debug)]
pub struct LogsArgs {
    /// Service ID, run ID, or service name
    pub id: String,

    /// Number of lines to show from the tail
    #[arg(long)]
    pub tail: Option<usize>,

    /// Number of lines to show from the head
    #[arg(long)]
    pub head: Option<usize>,

    /// Follow log output (like tail -f)
    #[arg(short, long)]
    pub follow: bool,

    /// Stream to view: stdout, stderr, stdin, combined
    #[arg(short, long, value_enum, default_value_t = StreamFilter::Combined)]
    pub stream: StreamFilter,
}

#[derive(Parser, Debug)]
pub struct SearchArgs {
    /// Regex pattern to search for
    pub pattern: String,

    /// Filter by service ID or name
    #[arg(short, long)]
    pub service: Option<String>,

    /// Stream to search: stdout, stderr, stdin, combined
    #[arg(long, value_enum, default_value_t = StreamFilter::Combined)]
    pub stream: StreamFilter,

    /// Maximum number of matches
    #[arg(short, long, default_value = "50")]
    pub max_matches: usize,

    /// Only search log content (skip service metadata matching)
    #[arg(long)]
    pub logs_only: bool,
}

#[derive(Parser, Debug)]
pub struct PurgeArgs {
    /// Duration threshold: delete services older than this (e.g. 10h, 30m, 5d, 3600s)
    #[arg(long)]
    pub before: String,

    /// Show what would be purged without deleting
    #[arg(long)]
    pub dry_run: bool,

    /// Skip confirmation prompt
    #[arg(long)]
    pub force: bool,
}

/// Known subcommand names for direct mode detection
pub const KNOWN_SUBCOMMANDS: &[&str] = &[
    "run",
    "list",
    "logs",
    "search",
    "mcp",
    "purge",
    "help",
    "--help",
    "-h",
    "--version",
    "-V",
];

/// Parse direct mode arguments from argv.
/// Returns (RunArgs, remaining) if argv[1] is not a known subcommand.
pub fn parse_direct_mode(args: &[String]) -> Option<RunArgs> {
    if args.len() < 2 {
        return None;
    }

    let first = &args[1];
    if KNOWN_SUBCOMMANDS.contains(&first.as_str()) {
        return None;
    }

    // Parse brainlog's own flags before the command
    let mut name: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();
    let mut desc: Option<String> = None;
    let mut command_start = 1;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--name" | "-n" => {
                if i + 1 < args.len() {
                    name = Some(args[i + 1].clone());
                    i += 2;
                    command_start = i;
                } else {
                    break;
                }
            }
            "--tag" | "-t" => {
                if i + 1 < args.len() {
                    tags.push(args[i + 1].clone());
                    i += 2;
                    command_start = i;
                } else {
                    break;
                }
            }
            "--desc" | "-d" => {
                if i + 1 < args.len() {
                    desc = Some(args[i + 1].clone());
                    i += 2;
                    command_start = i;
                } else {
                    break;
                }
            }
            _ => break, // First unrecognized arg starts the command
        }
    }

    let command: Vec<String> = args[command_start..].to_vec();
    if command.is_empty() {
        return None;
    }

    Some(RunArgs {
        name,
        tag: tags,
        desc,
        command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn direct_mode_simple_command() {
        let result = parse_direct_mode(&args(&["brainlog", "node", "server.js"]));
        let run_args = result.unwrap();
        assert_eq!(run_args.command, vec!["node", "server.js"]);
        assert!(run_args.name.is_none());
    }

    #[test]
    fn direct_mode_with_name_flag() {
        let result = parse_direct_mode(&args(&["brainlog", "-n", "my-app", "python", "app.py"]));
        let run_args = result.unwrap();
        assert_eq!(run_args.name.as_deref(), Some("my-app"));
        assert_eq!(run_args.command, vec!["python", "app.py"]);
    }

    #[test]
    fn direct_mode_with_all_flags() {
        let result = parse_direct_mode(&args(&[
            "brainlog",
            "--name",
            "svc",
            "--tag",
            "env:prod",
            "--desc",
            "my service",
            "cargo",
            "run",
        ]));
        let run_args = result.unwrap();
        assert_eq!(run_args.name.as_deref(), Some("svc"));
        assert_eq!(run_args.tag, vec!["env:prod"]);
        assert_eq!(run_args.desc.as_deref(), Some("my service"));
        assert_eq!(run_args.command, vec!["cargo", "run"]);
    }

    #[test]
    fn known_subcommand_returns_none() {
        for cmd in KNOWN_SUBCOMMANDS {
            let result = parse_direct_mode(&args(&["brainlog", cmd]));
            assert!(
                result.is_none(),
                "Should return None for subcommand: {}",
                cmd
            );
        }
    }

    #[test]
    fn too_few_args_returns_none() {
        assert!(parse_direct_mode(&args(&["brainlog"])).is_none());
        assert!(parse_direct_mode(&args(&[])).is_none());
    }

    #[test]
    fn flag_without_value_becomes_command() {
        // --name with no value => break out of flag parsing, "--name" treated as the command
        let result = parse_direct_mode(&args(&["brainlog", "--name"]));
        let run_args = result.unwrap();
        assert_eq!(run_args.command, vec!["--name"]);
        assert!(run_args.name.is_none());
    }

    #[test]
    fn multiple_tags() {
        let result = parse_direct_mode(&args(&[
            "brainlog",
            "-t",
            "env:prod",
            "-t",
            "team:backend",
            "echo",
            "hi",
        ]));
        let run_args = result.unwrap();
        assert_eq!(run_args.tag, vec!["env:prod", "team:backend"]);
        assert_eq!(run_args.command, vec!["echo", "hi"]);
    }

    // --- Tag validation tests ---

    #[test]
    fn parse_tag_valid() {
        let (key, value) = parse_tag("env:production").unwrap();
        assert_eq!(key, "env");
        assert_eq!(value, "production");
    }

    #[test]
    fn parse_tag_valid_with_whitespace() {
        let (key, value) = parse_tag("  env : production ").unwrap();
        assert_eq!(key, "env");
        assert_eq!(value, "production");
    }

    #[test]
    fn parse_tag_missing_colon_is_error() {
        let err = parse_tag("invalid").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("missing ':' separator"),
            "Expected error about missing separator, got: {}",
            msg
        );
        assert!(
            msg.contains("key:value"),
            "Expected error to mention expected format, got: {}",
            msg
        );
    }

    #[test]
    fn parse_tag_multiple_colons_splits_on_first() {
        let (key, value) = parse_tag("url:http://example.com:8080").unwrap();
        assert_eq!(key, "url");
        assert_eq!(value, "http://example.com:8080");
    }

    #[test]
    fn parse_tag_empty_key_is_error() {
        let err = parse_tag(":value").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("key must not be empty"),
            "Expected error about empty key, got: {}",
            msg
        );
    }

    #[test]
    fn parse_tag_whitespace_only_key_is_error() {
        let err = parse_tag("  :value").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("key must not be empty"),
            "Expected error about empty key, got: {}",
            msg
        );
    }

    #[test]
    fn parse_tag_empty_value_is_ok() {
        // A tag like "flag:" with an empty value is allowed
        let (key, value) = parse_tag("flag:").unwrap();
        assert_eq!(key, "flag");
        assert_eq!(value, "");
    }

    #[test]
    fn validate_tags_all_valid() {
        let tags = vec!["env:prod".to_string(), "team:backend".to_string()];
        assert!(validate_tags(&tags).is_ok());
    }

    #[test]
    fn validate_tags_empty_list() {
        let tags: Vec<String> = vec![];
        assert!(validate_tags(&tags).is_ok());
    }

    #[test]
    fn validate_tags_first_invalid_stops() {
        let tags = vec![
            "good:tag".to_string(),
            "bad_tag".to_string(),
            "also:good".to_string(),
        ];
        let err = validate_tags(&tags).unwrap_err();
        assert!(
            err.to_string().contains("bad_tag"),
            "Expected error to mention the invalid tag, got: {}",
            err
        );
    }
}
