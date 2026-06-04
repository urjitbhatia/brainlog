use clap::Parser;
use std::process::ExitCode;

use brainlog::cli::{self, Cli, Commands};

/// Convert an i32 process exit code to a u8 suitable for ExitCode.
///
/// - Negative values map to 1 (general failure)
/// - Values 0-255 pass through unchanged
/// - Values > 255 clamp to 255
fn exit_code_to_u8(code: i32) -> u8 {
    if code < 0 {
        1
    } else if code > 255 {
        255
    } else {
        code as u8
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();

    // Direct mode: if argv[1] is not a known subcommand, treat as a command to wrap
    if let Some(run_args) = cli::parse_direct_mode(&args) {
        match cli::run::handle_run(run_args).await {
            Ok(code) => return ExitCode::from(exit_code_to_u8(code)),
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                return ExitCode::FAILURE;
            }
        }
    }

    // Standard clap parsing for known subcommands
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run(run_args)) => match cli::run::handle_run(run_args).await {
            Ok(code) => ExitCode::from(exit_code_to_u8(code)),
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::List(args)) => match cli::list::handle_list(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Logs(args)) => match cli::logs::handle_logs(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Search(args)) => match cli::search::handle_search(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Kill(args)) => match cli::kill::handle_kill(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Mcp) => match cli::mcp::handle_mcp().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Purge(args)) => match cli::purge::handle_purge(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Restart(args)) => match cli::restart::handle_restart(args).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        Some(Commands::Daemon(args)) => match cli::daemon::handle_daemon(args).await {
            Ok(code) => ExitCode::from(exit_code_to_u8(code)),
            Err(e) => {
                eprintln!("brainlog: {:#}", e);
                ExitCode::FAILURE
            }
        },
        None => {
            // No command provided, print help
            use clap::CommandFactory;
            Cli::command().print_help().ok();
            println!();
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_code_zero() {
        assert_eq!(exit_code_to_u8(0), 0);
    }

    #[test]
    fn test_exit_code_one() {
        assert_eq!(exit_code_to_u8(1), 1);
    }

    #[test]
    fn test_exit_code_general_values() {
        assert_eq!(exit_code_to_u8(42), 42);
        assert_eq!(exit_code_to_u8(127), 127);
        assert_eq!(exit_code_to_u8(128), 128);
    }

    #[test]
    fn test_exit_code_sigint() {
        // 128 + 2 (SIGINT) = 130
        assert_eq!(exit_code_to_u8(130), 130);
    }

    #[test]
    fn test_exit_code_max_u8() {
        assert_eq!(exit_code_to_u8(255), 255);
    }

    #[test]
    fn test_exit_code_overflow_clamps_to_255() {
        assert_eq!(exit_code_to_u8(256), 255);
        assert_eq!(exit_code_to_u8(i32::MAX), 255);
    }

    #[test]
    fn test_exit_code_negative_maps_to_one() {
        assert_eq!(exit_code_to_u8(-1), 1);
        assert_eq!(exit_code_to_u8(i32::MIN), 1);
    }
}
