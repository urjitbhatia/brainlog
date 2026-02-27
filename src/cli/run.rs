use anyhow::Result;
use chrono::Utc;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use owo_colors::OwoColorize;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use signal_hook_tokio::Signals;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::cli::kill::collect_process_tree;
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

    // Check for BRAINLOG_SERVICE_NAME env var as fallback for --name
    let env_name = std::env::var("BRAINLOG_SERVICE_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    let resolved_name = resolve_name(args.name.clone(), env_name);

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
    } else if let Some(ref name) = resolved_name {
        effective_name = Some(name.clone());
        resumed_from = None;
        if let Some(existing) = db.find_service_by_name(name)? {
            existing.id
        } else {
            create_service_with_name(&db, &config, name, &args, &executable, &working_dir)?
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

    // --- Long-lived signal listeners (spawned once, live across loop iterations) ---
    let restart_requested = Arc::new(AtomicBool::new(false));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let current_child_pid = Arc::new(AtomicU32::new(0));

    // SIGUSR1 listener: triggers restart
    let restart_flag = restart_requested.clone();
    let child_pid_ref = current_child_pid.clone();
    let sigusr1_handle = tokio::spawn(async move {
        let mut signals = match Signals::new([SIGUSR1]) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to register SIGUSR1 handler: {e}");
                return;
            }
        };
        while signals.next().await.is_some() {
            restart_flag.store(true, Ordering::SeqCst);
            let pid = child_pid_ref.load(Ordering::SeqCst);
            if pid > 0 {
                // Kill the entire child process tree
                let tree = collect_process_tree(pid).await;
                let mut kill_order: Vec<u32> = tree.iter().copied().filter(|&p| p != pid).collect();
                kill_order.reverse();
                kill_order.push(pid);
                for target_pid in &kill_order {
                    let nix_pid = Pid::from_raw(*target_pid as i32);
                    let _ = signal::kill(nix_pid, Signal::SIGTERM);
                }
            }
        }
    });

    // SIGINT/SIGTERM listener: sets stop flag so wrapper exits between iterations
    let stop_flag = stop_requested.clone();
    let sigterm_handle = tokio::spawn(async move {
        let mut signals = match Signals::new([SIGINT, SIGTERM]) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to register SIGINT/SIGTERM handler: {e}");
                return;
            }
        };
        if signals.next().await.is_some() {
            stop_flag.store(true, Ordering::SeqCst);
        }
    });

    // --- Spawn loop ---
    #[allow(unused_assignments)]
    let mut last_exit_code: i32 = 1;
    let mut iteration = 0u32;
    let mut enrichment_handle: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        iteration += 1;

        // Create run record for this iteration
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
            wrapper_pid: Some(std::process::id()),
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

        // If resuming (first iteration only), inject an artificial log entry
        if iteration == 1 {
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
        } else {
            // Inject restart indicator for subsequent iterations
            let restart_msg = format!(
                "[brainlog] Restarting `{}` (iteration {})\n",
                args.command.join(" "),
                iteration
            );
            let _ = tx
                .send(Frame {
                    timestamp_ns: now_ns(),
                    stream_type: StreamType::Stderr,
                    payload: restart_msg.into_bytes(),
                })
                .await;
        }

        // PID channel
        let (pid_tx, pid_rx) = oneshot::channel::<u32>();

        // Prepare port detection cancellation token
        let port_cancel = CancellationToken::new();
        let db_path = config.db_path();

        // Spawn background task for PID recording, port detection, and enrichment
        let run_id_bg = run_id.clone();
        let service_id_bg = service_id.clone();
        let command_bg = args.command.clone();
        let has_user_name = resolved_name.is_some() || args.resume.is_some();
        let port_cancel_bg = port_cancel.clone();
        let child_pid_ref_bg = current_child_pid.clone();
        let config_bg = config.clone();
        let working_dir_bg = working_dir.clone();
        let tags_bg = args.tag.clone();
        let desc_bg = args.desc.clone();
        let first_iteration = iteration == 1;

        let bg_handle: tokio::task::JoinHandle<Option<tokio::task::JoinHandle<()>>> =
            tokio::spawn(async move {
                let pid = match pid_rx.await {
                    Ok(pid) => pid,
                    Err(_) => return None,
                };

                // Store current child PID for SIGUSR1 handler
                child_pid_ref_bg.store(pid, Ordering::SeqCst);

                // Record PID in database
                if pid > 0 {
                    if let Ok(db) = Database::open(&db_path) {
                        if let Err(e) = db.update_run_pid(&run_id_bg, pid) {
                            tracing::warn!("Failed to record child PID: {e}");
                        }
                    }
                }

                // Start port detection
                if config_bg.port_detection.enabled && pid > 0 {
                    let run_id_port = run_id_bg.clone();
                    let db_path_port = db_path.clone();
                    let poll_interval = config_bg.port_detection.poll_interval_secs;
                    let cancel = port_cancel_bg;
                    tokio::spawn(async move {
                        platform::poll_ports(
                            pid,
                            &run_id_port,
                            &db_path_port,
                            poll_interval,
                            cancel,
                        )
                        .await;
                    });
                }

                // LLM enrichment (first iteration only, awaited before exit)
                if first_iteration && config_bg.enrichment.enabled {
                    Some(tokio::spawn(async move {
                        llm::enrichment::enrich_service(
                            &config_bg,
                            &service_id_bg,
                            &command_bg,
                            &working_dir_bg,
                            &tags_bg,
                            desc_bg.as_deref(),
                            has_user_name,
                        )
                        .await;
                    }))
                } else {
                    None
                }
            });

        // Print startup indicator
        let stderr_tty = std::io::stderr().is_terminal();
        let short_svc_id = &service_id[..8.min(service_id.len())];
        if iteration == 1 {
            if stderr_tty {
                eprintln!(
                    "[brainlog] Capturing output for `{}` ({})",
                    args.command.join(" ").bold(),
                    short_svc_id.dimmed()
                );
            } else {
                eprintln!(
                    "[brainlog] Capturing output for `{}` ({})",
                    args.command.join(" "),
                    short_svc_id
                );
            }
        } else if stderr_tty {
            eprintln!(
                "[brainlog] Restarting `{}` (iteration {})",
                args.command.join(" ").bold(),
                iteration
            );
        } else {
            eprintln!(
                "[brainlog] Restarting `{}` (iteration {})",
                args.command.join(" "),
                iteration
            );
        }

        // Set BRAINLOG_SERVICE_NAME in the environment so child processes inherit it.
        // This allows nested brainlog invocations or scripts to detect they're running
        // under brainlog. We resolve the name from effective_name (user-provided or env var)
        // or fall back to reading it from the DB (derived name case).
        if iteration == 1 {
            let child_env_name = if let Some(ref name) = effective_name {
                name.clone()
            } else {
                db.get_service(&service_id)?
                    .and_then(|s| s.name)
                    .unwrap_or_default()
            };
            if !child_env_name.is_empty() {
                std::env::set_var("BRAINLOG_SERVICE_NAME", &child_env_name);
            }
        }

        // Spawn the wrapped process
        let spawn_result = process::spawn_wrapped(&args.command, tx.clone(), pid_tx).await?;

        // Wait for background setup task to complete; capture enrichment handle
        if let Some(handle) = bg_handle.await.ok().flatten() {
            enrichment_handle = Some(handle);
        }

        // Clear child PID since the child has exited
        current_child_pid.store(0, Ordering::SeqCst);

        // Inject exit log frame
        let exit_code = spawn_result.exit_code.unwrap_or(1);
        let exit_msg = format_exit_message(exit_code);
        let _ = tx
            .send(Frame {
                timestamp_ns: now_ns(),
                stream_type: StreamType::Stderr,
                payload: exit_msg.into_bytes(),
            })
            .await;

        // Drop sender so log writer finishes
        drop(tx);
        if let Err(e) = log_handle.await {
            tracing::error!("Log writer task failed: {e}");
        }

        // Stop port polling
        port_cancel.cancel();

        // --- Restart decision (checked early to influence status/messaging) ---
        let will_restart_manual = restart_requested.load(Ordering::SeqCst);
        let will_restart_auto = args.restart
            && !will_restart_manual
            && exit_code != 130
            && exit_code != 143
            && !stop_requested.load(Ordering::SeqCst);

        // Update run status
        let status = if will_restart_manual {
            // Killed by SIGUSR1-triggered restart — not a failure
            RunStatus::Completed
        } else if exit_code == 0 {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        };
        db.update_run_status(&run_id, &status, Some(exit_code))?;

        // Print completion summary
        let short_id = &run_id[..8.min(run_id.len())];
        if will_restart_manual {
            if stderr_tty {
                eprintln!(
                    "[brainlog] Run {} {} (restarting), logs at {}",
                    short_id,
                    "stopped".yellow().bold(),
                    log_dir.display().to_string().dimmed()
                );
            } else {
                eprintln!(
                    "[brainlog] Run {} stopped (restarting), logs at {}",
                    short_id,
                    log_dir.display()
                );
            }
        } else if stderr_tty {
            if exit_code == 0 {
                eprintln!(
                    "[brainlog] Run {} {} (exit 0), logs at {}",
                    short_id,
                    "completed".green().bold(),
                    log_dir.display().to_string().dimmed()
                );
            } else {
                eprintln!(
                    "[brainlog] Run {} {} (exit {}), logs at {}",
                    short_id,
                    "failed".red().bold(),
                    exit_code,
                    log_dir.display().to_string().dimmed()
                );
            }
        } else if exit_code == 0 {
            eprintln!(
                "[brainlog] Run {} completed (exit 0), logs at {}",
                short_id,
                log_dir.display()
            );
        } else {
            eprintln!(
                "[brainlog] Run {} failed (exit {}), logs at {}",
                short_id,
                exit_code,
                log_dir.display()
            );
        }

        last_exit_code = exit_code;

        // --- Execute restart decision ---
        if will_restart_manual {
            restart_requested.store(false, Ordering::SeqCst);
            eprintln!("[brainlog] Restart requested, respawning in 1s...");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }

        if will_restart_auto {
            eprintln!("[brainlog] Auto-restart enabled, respawning in 1s...");
            // Interruptible sleep: if Ctrl+C arrives during sleep, break immediately
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                () = async {
                    while !stop_requested.load(Ordering::SeqCst) {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                } => {
                    eprintln!("[brainlog] Interrupted during restart delay, exiting");
                    break;
                }
            }
            continue;
        }

        if args.restart
            && (exit_code == 130 || exit_code == 143 || stop_requested.load(Ordering::SeqCst))
        {
            eprintln!("[brainlog] Process terminated by signal, not restarting");
        }

        // No restart — exit the loop
        break;
    }

    // Abort signal listeners
    sigusr1_handle.abort();
    let _ = sigusr1_handle.await;
    sigterm_handle.abort();
    let _ = sigterm_handle.await;

    // Wait for LLM enrichment to finish (so short-lived commands don't kill it)
    if let Some(handle) = enrichment_handle {
        let _ = handle.await;
    }

    // Print resume hint
    let has_user_name = effective_name.is_some();
    let svc_name = if let Some(name) = effective_name {
        name
    } else {
        db.get_service(&service_id)?
            .and_then(|s| s.name)
            .unwrap_or_else(|| service_id.clone())
    };
    print_resume_hint(&svc_name, &args.command, has_user_name);

    Ok(last_exit_code)
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
/// If the name was user-provided (via `-n`), suggest `-n name` for resume.
/// Otherwise suggest `--resume <derived-name>`.
fn print_resume_hint(name: &str, command: &[String], user_provided_name: bool) {
    let cmd_str = command
        .iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ");
    let tty = std::io::stderr().is_terminal();
    eprintln!();
    if tty {
        let label = "To resume under the same name, run:".dimmed();
        if user_provided_name {
            eprintln!(
                "{}\n  {} {} {}",
                label,
                "brainlog -n".cyan(),
                shell_quote(name).bold(),
                cmd_str.bold(),
            );
        } else {
            eprintln!(
                "{}\n  {} {} {}",
                label,
                "brainlog --resume".cyan(),
                shell_quote(name).bold(),
                cmd_str.bold(),
            );
        }
    } else if user_provided_name {
        eprintln!(
            "To resume under the same name, run:\n  brainlog -n {} {}",
            shell_quote(name),
            cmd_str,
        );
    } else {
        eprintln!(
            "To resume under the same name, run:\n  brainlog --resume {} {}",
            shell_quote(name),
            cmd_str,
        );
    }
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

/// Resolve the service name from CLI flag and env var.
///
/// Priority: `--name` flag > `BRAINLOG_SERVICE_NAME` env var > None (caller falls through
/// to `--resume` or derived name).
fn resolve_name(cli_name: Option<String>, env_name: Option<String>) -> Option<String> {
    cli_name.or(env_name)
}

/// Derive a compact service name: `<cwd_basename>/<executable>-<hash>`.
///
/// The hash is a 6-char hex digest of the full command (including all args/flags),
/// so different invocations of the same executable get distinct names.
fn derive_name(working_dir: &str, command: &[String]) -> String {
    let dir_basename = std::path::Path::new(working_dir)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let executable = command
        .first()
        .map(|s| {
            std::path::Path::new(s.as_str())
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| s.clone())
        })
        .unwrap_or_default();

    let mut hasher = DefaultHasher::new();
    command.hash(&mut hasher);
    let hash = hasher.finish();
    let short_hash = format!("{:06x}", hash & 0xFFFFFF);

    if dir_basename.is_empty() {
        format!("{executable}-{short_hash}")
    } else {
        format!("{dir_basename}/{executable}-{short_hash}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_name_basic() {
        let name = derive_name(
            "/Users/urjit/code/pimlico/web",
            &["pnpm".to_string(), "run".to_string(), "dev".to_string()],
        );
        assert!(name.starts_with("web/pnpm-"));
        assert_eq!(name.len(), "web/pnpm-".len() + 6);
    }

    #[test]
    fn test_derive_name_no_dir_basename() {
        let name = derive_name("/", &["ls".to_string()]);
        assert!(name.starts_with("ls-"));
        assert_eq!(name.len(), "ls-".len() + 6);
    }

    #[test]
    fn test_derive_name_full_path_executable() {
        let name = derive_name(
            "/home/user/app",
            &["/usr/bin/python3".to_string(), "server.py".to_string()],
        );
        // Uses just the filename from the executable path
        assert!(name.starts_with("app/python3-"));
    }

    #[test]
    fn test_derive_name_deterministic() {
        let cmd = &["cargo".to_string(), "test".to_string()];
        let a = derive_name("/project", cmd);
        let b = derive_name("/project", cmd);
        assert_eq!(a, b);
    }

    #[test]
    fn test_derive_name_different_args_different_hash() {
        let name_a = derive_name(
            "/app",
            &[
                "rg2".to_string(),
                "--jurisdiction".to_string(),
                "malta".to_string(),
            ],
        );
        let name_b = derive_name(
            "/app",
            &[
                "rg2".to_string(),
                "--jurisdiction".to_string(),
                "cyprus".to_string(),
            ],
        );
        // Same executable prefix, different hashes
        assert!(name_a.starts_with("app/rg2-"));
        assert!(name_b.starts_with("app/rg2-"));
        assert_ne!(name_a, name_b);
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

    // --- resolve_name tests ---

    #[test]
    fn resolve_name_cli_flag_takes_priority_over_env() {
        let result = resolve_name(Some("from-cli".to_string()), Some("from-env".to_string()));
        assert_eq!(result.as_deref(), Some("from-cli"));
    }

    #[test]
    fn resolve_name_falls_back_to_env_var() {
        let result = resolve_name(None, Some("from-env".to_string()));
        assert_eq!(result.as_deref(), Some("from-env"));
    }

    #[test]
    fn resolve_name_returns_none_when_both_absent() {
        let result = resolve_name(None, None);
        assert!(result.is_none());
    }

    #[test]
    fn resolve_name_cli_flag_only() {
        let result = resolve_name(Some("my-service".to_string()), None);
        assert_eq!(result.as_deref(), Some("my-service"));
    }
}
