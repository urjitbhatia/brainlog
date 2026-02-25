use anyhow::Result;
use chrono::Utc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::cli::{parse_tag, validate_tags, RunArgs};
use crate::config::Config;
use crate::llm;
use crate::platform;
use crate::process;
use crate::process::capture::now_ns;
use crate::storage::models::*;
use crate::storage::{Database, LogWriter};

pub async fn handle_run(args: RunArgs) -> Result<i32> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    let working_dir = std::env::current_dir()?.to_string_lossy().to_string();
    let executable = args.command[0].clone();

    // Determine the service name for resume hint later
    let effective_name: Option<String>;
    let resumed_from: Option<String>; // old service id if resuming

    // Handle --resume: supersede old service, create new one with same name
    let service_id = if let Some(ref resume_name) = args.resume {
        let old_service = db.supersede_service(resume_name)?;
        let old_service_id = old_service.as_ref().map(|s| s.id.clone());

        if old_service.is_none() {
            eprintln!(
                "brainlog: warning: no existing service named '{}' found, creating new",
                resume_name
            );
        }

        effective_name = Some(resume_name.clone());
        resumed_from = old_service_id;

        // Create a new service with the resume name
        create_service_with_name(&db, &config, resume_name, &args, &executable, &working_dir)?
    } else if let Some(ref name) = args.name {
        effective_name = Some(name.clone());
        resumed_from = None;
        if let Some(existing) = db.find_service_by_name(name)? {
            existing.id
        } else {
            create_service(&db, &config, &args, &executable, &working_dir)?
        }
    } else {
        effective_name = None;
        resumed_from = None;
        create_service(&db, &config, &args, &executable, &working_dir)?
    };

    // Validate and store tags
    validate_tags(&args.tag)?;
    for tag_str in &args.tag {
        let (key, value) = parse_tag(tag_str)?;
        db.add_tag(&service_id, key, value)?;
    }

    // Create run
    let run_id = Uuid::new_v4().to_string();
    let log_dir = config.logs_dir().join(&run_id);
    let run = Run {
        id: run_id.clone(),
        service_id: service_id.clone(),
        pid: None,
        started_at: Utc::now(),
        ended_at: None,
        exit_code: None,
        log_dir: log_dir.to_string_lossy().to_string(),
        status: RunStatus::Running,
    };
    db.create_run(&run)?;

    // Set up log writer channel
    let (tx, rx) = mpsc::channel::<Frame>(1024);
    let log_writer = LogWriter::new(
        log_dir.clone(),
        rx,
        config.capture.flush_interval_ms,
        config.capture.flush_buffer_bytes,
    );
    let log_handle = tokio::spawn(async move { log_writer.run().await });

    // If resuming, inject an artificial log entry noting the restart
    if let Some(ref old_id) = resumed_from {
        let resume_msg = format!(
            "[brainlog] Resumed service (previous service id: {}). Command: {}\n",
            old_id,
            args.command.join(" ")
        );
        let _ = tx
            .send(Frame {
                timestamp_ns: now_ns(),
                stream_type: StreamType::Stderr,
                payload: resume_msg.into_bytes(),
            })
            .await;
    }

    // PID channel — child sends PID immediately after spawn, before exiting
    let (pid_tx, pid_rx) = oneshot::channel::<u32>();

    // Prepare port detection cancellation token
    let port_cancel = CancellationToken::new();
    let db_path = config.db_path();

    // Spawn a task that waits for the PID and starts background work immediately
    let run_id_bg = run_id.clone();
    let service_id_bg = service_id.clone();
    let command_bg = args.command.clone();
    let has_user_name = args.name.is_some() || args.resume.is_some();
    let port_cancel_bg = port_cancel.clone();

    let bg_handle = tokio::spawn(async move {
        let pid = match pid_rx.await {
            Ok(pid) => pid,
            Err(_) => return,
        };

        // Record PID in database while child is still running
        if pid > 0 {
            if let Ok(db) = Database::open(&db_path) {
                if let Err(e) = db.update_run_pid(&run_id_bg, pid) {
                    tracing::warn!("Failed to record child PID: {e}");
                }
            }
        }

        // Start port detection while child is still running
        if config.port_detection.enabled && pid > 0 {
            let run_id_port = run_id_bg.clone();
            let db_path_port = db_path.clone();
            let poll_interval = config.port_detection.poll_interval_secs;
            let cancel = port_cancel_bg;
            tokio::spawn(async move {
                platform::poll_ports(pid, &run_id_port, &db_path_port, poll_interval, cancel).await;
            });
        }

        // LLM enrichment (fire-and-forget)
        if config.enrichment.enabled {
            tokio::spawn(async move {
                llm::enrichment::enrich_service(
                    &config,
                    &service_id_bg,
                    &command_bg,
                    &working_dir,
                    &args.tag,
                    args.desc.as_deref(),
                    has_user_name,
                )
                .await;
            });
        }
    });

    // Print startup indicator
    eprintln!(
        "[brainlog] Capturing output for `{}`",
        args.command.join(" ")
    );

    // Spawn the wrapped process — PID is sent via pid_tx immediately after fork/spawn
    let spawn_result = process::spawn_wrapped(&args.command, tx.clone(), pid_tx).await?;

    // Wait for background setup task to complete
    let _ = bg_handle.await;

    // Inject an artificial exit log frame recording how the process exited
    let exit_code = spawn_result.exit_code.unwrap_or(1);
    let exit_msg = format_exit_message(exit_code);
    let _ = tx
        .send(Frame {
            timestamp_ns: now_ns(),
            stream_type: StreamType::Stderr,
            payload: exit_msg.into_bytes(),
        })
        .await;

    // Drop the sender so the log writer can finish
    drop(tx);

    // Wait for log writer to finish
    if let Err(e) = log_handle.await {
        tracing::error!("Log writer task failed: {e}");
    }

    // Stop port polling now that the child has exited
    port_cancel.cancel();

    // Update run status
    let status = if exit_code == 0 {
        RunStatus::Completed
    } else {
        RunStatus::Failed
    };
    db.update_run_status(&run_id, &status, Some(exit_code))?;

    // Print resume hint to stderr
    let svc_name = if let Some(name) = effective_name {
        name
    } else {
        // Try to get the name from the DB (may have been enriched)
        db.get_service(&service_id)?
            .and_then(|s| s.name)
            .unwrap_or_else(|| service_id.clone())
    };
    print_resume_hint(&svc_name, &args.command);

    Ok(exit_code)
}

fn create_service(
    db: &Database,
    config: &Config,
    args: &RunArgs,
    executable: &str,
    working_dir: &str,
) -> Result<String> {
    let service_id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let name = args
        .name
        .clone()
        .or_else(|| Some(derive_name(working_dir, &args.command)));
    let service = Service {
        id: service_id.clone(),
        name,
        description: args.desc.clone(),
        executable: executable.to_string(),
        command_line: args.command.clone(),
        working_dir: working_dir.to_string(),
        created_at: now,
        updated_at: now,
        enrichment_status: if config.enrichment.enabled {
            EnrichmentStatus::Pending
        } else {
            EnrichmentStatus::Skipped
        },
    };
    db.create_service(&service)?;
    Ok(service_id)
}

fn create_service_with_name(
    db: &Database,
    config: &Config,
    name: &str,
    args: &RunArgs,
    executable: &str,
    working_dir: &str,
) -> Result<String> {
    let service_id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let service = Service {
        id: service_id.clone(),
        name: Some(name.to_string()),
        description: args.desc.clone(),
        executable: executable.to_string(),
        command_line: args.command.clone(),
        working_dir: working_dir.to_string(),
        created_at: now,
        updated_at: now,
        enrichment_status: if config.enrichment.enabled {
            EnrichmentStatus::Pending
        } else {
            EnrichmentStatus::Skipped
        },
    };
    db.create_service(&service)?;
    Ok(service_id)
}

/// Format a human-readable exit message for the artificial log entry.
fn format_exit_message(exit_code: i32) -> String {
    if exit_code == 0 {
        "[brainlog] Process exited normally (exit code: 0)\n".to_string()
    } else if exit_code > 128 {
        let signal = exit_code - 128;
        let signal_name = match signal {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            6 => "SIGABRT",
            9 => "SIGKILL",
            11 => "SIGSEGV",
            13 => "SIGPIPE",
            14 => "SIGALRM",
            15 => "SIGTERM",
            _ => "unknown",
        };
        format!(
            "[brainlog] Process killed by signal {} ({}, exit code: {})\n",
            signal, signal_name, exit_code
        )
    } else {
        format!(
            "[brainlog] Process exited with error (exit code: {})\n",
            exit_code
        )
    }
}

/// Print a resume hint to stderr so the user knows how to restart.
fn print_resume_hint(name: &str, command: &[String]) {
    let cmd_str = command
        .iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!();
    eprintln!(
        "To resume under the same name, run:\n  brainlog --resume {} {}",
        shell_quote(name),
        cmd_str,
    );
}

/// Simple shell quoting: wrap in single quotes if the string contains special characters.
fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().all(|c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/' || c == ':'
    }) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Noise subcommands that are filtered out of derived names.
/// These are common "do nothing" verbs used by package managers and task runners.
const NOISE_SUBCOMMANDS: &[&str] = &["run", "exec"];

/// Derive a human-readable service name from the working directory and command.
///
/// Format: `<cwd_basename>/<executable>-<arg1>-<arg2>-...`
///
/// Noise subcommands like `run` (e.g. `pnpm run dev`) are stripped so the
/// derived name stays concise (`pnpm-dev` instead of `pnpm-run-dev`).
/// Arguments preserve colons (e.g. `dev:with-binding`) and are joined with `-`.
/// The working directory basename is separated from the command part by `/`.
fn derive_name(working_dir: &str, command: &[String]) -> String {
    let dir_basename = std::path::Path::new(working_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let filtered: Vec<&str> = command
        .iter()
        .enumerate()
        .filter(|(i, arg)| {
            // Only filter noise subcommands in non-first position
            if *i == 0 {
                return true;
            }
            !NOISE_SUBCOMMANDS.contains(&arg.as_str())
        })
        .map(|(_, arg)| arg.as_str())
        .collect();

    let cmd_part = filtered.join("-");

    if dir_basename.is_empty() {
        cmd_part
    } else {
        format!("{dir_basename}/{cmd_part}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_name_pnpm_dev_with_binding() {
        let name = derive_name(
            "/Users/urjit/code/pimlico/web",
            &[
                "pnpm".to_string(),
                "run".to_string(),
                "dev:with-binding".to_string(),
            ],
        );
        assert_eq!(name, "web/pnpm-dev:with-binding");
    }

    #[test]
    fn test_derive_name_make_dev() {
        let name = derive_name(
            "/Users/urjit/code/pimlico/api",
            &["make".to_string(), "dev".to_string()],
        );
        assert_eq!(name, "api/make-dev");
    }

    #[test]
    fn test_derive_name_cargo_test() {
        let name = derive_name(
            "/home/user/project",
            &["cargo".to_string(), "test".to_string()],
        );
        assert_eq!(name, "project/cargo-test");
    }

    #[test]
    fn test_derive_name_single_command() {
        let name = derive_name("/home/user/myapp", &["node".to_string()]);
        assert_eq!(name, "myapp/node");
    }

    #[test]
    fn test_derive_name_root_dir() {
        let name = derive_name("/", &["ls".to_string()]);
        // Root has no basename, so just the command part
        assert_eq!(name, "ls");
    }

    #[test]
    fn format_exit_message_normal() {
        let msg = format_exit_message(0);
        assert!(msg.contains("exit code: 0"));
        assert!(msg.contains("exited normally"));
    }

    #[test]
    fn format_exit_message_error() {
        let msg = format_exit_message(1);
        assert!(msg.contains("exit code: 1"));
        assert!(msg.contains("exited with error"));
    }

    #[test]
    fn format_exit_message_sigint() {
        let msg = format_exit_message(130);
        assert!(msg.contains("SIGINT"));
        assert!(msg.contains("signal 2"));
        assert!(msg.contains("exit code: 130"));
    }

    #[test]
    fn format_exit_message_sigterm() {
        let msg = format_exit_message(143);
        assert!(msg.contains("SIGTERM"));
        assert!(msg.contains("signal 15"));
    }

    #[test]
    fn format_exit_message_sigkill() {
        let msg = format_exit_message(137);
        assert!(msg.contains("SIGKILL"));
        assert!(msg.contains("signal 9"));
    }

    #[test]
    fn format_exit_message_unknown_signal() {
        let msg = format_exit_message(159); // 128 + 31
        assert!(msg.contains("unknown"));
        assert!(msg.contains("signal 31"));
    }
}
