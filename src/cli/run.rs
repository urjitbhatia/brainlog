use anyhow::Result;
use chrono::Utc;
use tokio::sync::mpsc;
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

    // Spawn the wrapped process
    let spawn_result = process::spawn_wrapped(&args.command, tx).await?;

    // Update run with pid
    if spawn_result.pid > 0 {
        db.update_run_pid(&run_id, spawn_result.pid)?;
    }

    // Start background tasks
    let db_path = config.db_path();
    let run_id_port = run_id.clone();
    let pid_for_port = spawn_result.pid;

    // Port detection (fire-and-forget for the duration of the process)
    if config.port_detection.enabled && spawn_result.pid > 0 {
        let poll_interval = config.port_detection.poll_interval_secs;
        let db_path_port = db_path.clone();
        tokio::spawn(async move {
            platform::poll_ports(pid_for_port, &run_id_port, &db_path_port, poll_interval).await;
        });
    }

    // LLM enrichment (fire-and-forget)
    if config.enrichment.enabled {
        let service_id_enrich = service_id.clone();
        let config_enrich = config.clone();
        let working_dir_enrich = working_dir.clone();
        let command_enrich = args.command.clone();
        let tags_enrich = args.tag.clone();
        let desc_enrich = args.desc.clone();
        let has_user_name = args.name.is_some();
        tokio::spawn(async move {
            llm::enrichment::enrich_service(
                &config_enrich,
                &service_id_enrich,
                &command_enrich,
                &working_dir_enrich,
                &tags_enrich,
                desc_enrich.as_deref(),
                has_user_name,
            )
            .await;
        });
    }

    // Wait for log writer to finish
    let _ = log_handle.await;

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
    let service = Service {
        id: service_id.clone(),
        name: args.name.clone(),
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
