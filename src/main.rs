use clap::Parser;
use std::process::ExitCode;

use brainlog::cli::{self, Cli, Commands};

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
            Ok(code) => return ExitCode::from(code as u8),
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
            Ok(code) => ExitCode::from(code as u8),
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
        Some(Commands::Mcp) => match cli::mcp::handle_mcp().await {
            Ok(()) => ExitCode::SUCCESS,
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
