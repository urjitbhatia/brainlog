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
use crate::storage::models::*;
use crate::storage::{Database, LogWriter};

pub async fn handle_run(args: RunArgs) -> Result<i32> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    let working_dir = std::env::current_dir()?.to_string_lossy().to_string();
    let executable = args.command[0].clone();

    // Find or create service
    let service_id = if let Some(ref name) = args.name {
        if let Some(existing) = db.find_service_by_name(name)? {
            existing.id
        } else {
            create_service(&db, &config, &args, &executable, &working_dir)?
        }
    } else {
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

    // PID channel — child sends PID immediately after spawn, before exiting
    let (pid_tx, pid_rx) = oneshot::channel::<u32>();

    // Prepare port detection cancellation token
    let port_cancel = CancellationToken::new();
    let db_path = config.db_path();

    // Spawn a task that waits for the PID and starts background work immediately
    let run_id_bg = run_id.clone();
    let service_id_bg = service_id.clone();
    let config_bg = config.clone();
    let working_dir_bg = working_dir.clone();
    let command_bg = args.command.clone();
    let tags_bg = args.tag.clone();
    let desc_bg = args.desc.clone();
    let has_user_name = args.name.is_some();
    let port_cancel_bg = port_cancel.clone();
    let db_path_bg = db_path.clone();

    let bg_handle = tokio::spawn(async move {
        let pid = match pid_rx.await {
            Ok(pid) => pid,
            Err(_) => return,
        };

        // Record PID in database while child is still running
        if pid > 0 {
            if let Ok(db) = Database::open(&db_path_bg) {
                if let Err(e) = db.update_run_pid(&run_id_bg, pid) {
                    tracing::warn!("Failed to record child PID: {e}");
                }
            }
        }

        // Start port detection while child is still running
        if config_bg.port_detection.enabled && pid > 0 {
            let run_id_port = run_id_bg.clone();
            let db_path_port = db_path_bg.clone();
            let poll_interval = config_bg.port_detection.poll_interval_secs;
            let cancel = port_cancel_bg;
            tokio::spawn(async move {
                platform::poll_ports(pid, &run_id_port, &db_path_port, poll_interval, cancel).await;
            });
        }

        // LLM enrichment (fire-and-forget)
        if config_bg.enrichment.enabled {
            tokio::spawn(async move {
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
            });
        }
    });

    // Spawn the wrapped process — PID is sent via pid_tx immediately after fork/spawn
    let spawn_result = process::spawn_wrapped(&args.command, tx, pid_tx).await?;

    // Wait for background setup task to complete
    let _ = bg_handle.await;

    // Wait for log writer to finish
    if let Err(e) = log_handle.await {
        tracing::error!("Log writer task failed: {e}");
    }

    // Stop port polling now that the child has exited
    port_cancel.cancel();

    // Update run status
    let exit_code = spawn_result.exit_code.unwrap_or(1);
    let status = if exit_code == 0 {
        RunStatus::Completed
    } else {
        RunStatus::Failed
    };
    db.update_run_status(&run_id, &status, Some(exit_code))?;

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
        .or_else(|| Some(derive_name(&args.command)));
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

/// Derive a short human-readable name from the command line.
///
/// Examples:
///   ["make", "dev"]              -> "make-dev"
///   ["pnpm", "run", "dev:build"] -> "pnpm-dev:build"
///   ["python3", "-m", "http.server", "9876"] -> "python3-http.server"
///   ["node", "server.js"]        -> "node-server.js"
fn derive_name(command: &[String]) -> String {
    if command.is_empty() {
        return "unknown".to_string();
    }

    // Start with the base executable name (strip path)
    let exe = std::path::Path::new(&command[0])
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(&command[0]);

    // Collect meaningful args (skip flags/options, stop at 2 meaningful args)
    let meaningful: Vec<&str> = command[1..]
        .iter()
        .filter(|a| !a.starts_with('-'))
        .filter(|a| {
            // Skip common subcommand noise like "run", "exec", "start"
            !matches!(a.as_str(), "run" | "exec" | "start" | "--")
        })
        .map(|s| s.as_str())
        .take(2)
        .collect();

    if meaningful.is_empty() {
        exe.to_string()
    } else {
        format!("{}-{}", exe, meaningful.join("-"))
    }
}
