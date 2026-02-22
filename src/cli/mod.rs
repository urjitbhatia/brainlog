pub mod list;
pub mod logs;
pub mod mcp;
pub mod run;
pub mod search;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "brainlog", version, about = "Transparent process wrapper with log capture and MCP server")]
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

#[derive(Parser, Debug)]
pub struct ListArgs {
    /// Filter by name
    #[arg(short, long)]
    pub name: Option<String>,

    /// Show detailed info
    #[arg(short, long)]
    pub verbose: bool,
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
    #[arg(short, long, default_value = "combined")]
    pub stream: String,
}

#[derive(Parser, Debug)]
pub struct SearchArgs {
    /// Regex pattern to search for
    pub pattern: String,

    /// Filter by service ID or name
    #[arg(short, long)]
    pub service: Option<String>,

    /// Stream to search: stdout, stderr, stdin, combined
    #[arg(long, default_value = "combined")]
    pub stream: String,

    /// Maximum number of matches
    #[arg(short, long, default_value = "50")]
    pub max_matches: usize,
}

/// Known subcommand names for direct mode detection
pub const KNOWN_SUBCOMMANDS: &[&str] = &[
    "run", "list", "logs", "search", "mcp", "help", "--help", "-h", "--version", "-V",
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
            assert!(result.is_none(), "Should return None for subcommand: {}", cmd);
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
            "brainlog", "-t", "env:prod", "-t", "team:backend", "echo", "hi",
        ]));
        let run_args = result.unwrap();
        assert_eq!(run_args.tag, vec!["env:prod", "team:backend"]);
        assert_eq!(run_args.command, vec!["echo", "hi"]);
    }
}
