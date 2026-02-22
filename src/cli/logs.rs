use anyhow::{bail, Result};
use std::path::Path;

use crate::cli::LogsArgs;
use crate::config::Config;
use crate::storage::logfile::{frames_to_text, LogReader};
use crate::storage::Database;

pub async fn handle_logs(args: LogsArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    // Resolve ID to a log directory
    let log_dir = resolve_log_dir(&db, &args.id)?;

    let reader = LogReader::new(Path::new(&log_dir), &args.stream);

    if args.follow {
        follow_logs(&reader, &args.stream).await?;
    } else if let Some(n) = args.tail {
        let frames = reader.read_tail(n)?;
        print!("{}", frames_to_text(&frames));
    } else if let Some(n) = args.head {
        let frames = reader.read_head(n)?;
        print!("{}", frames_to_text(&frames));
    } else {
        // Default: show last 50 lines
        let frames = reader.read_tail(50)?;
        print!("{}", frames_to_text(&frames));
    }

    Ok(())
}

fn resolve_log_dir(db: &Database, id: &str) -> Result<String> {
    // Try as run ID first
    if let Some(run) = db.get_run(id)? {
        return Ok(run.log_dir);
    }

    // Try as service ID — get latest run
    if let Some(run) = db.get_latest_run(id)? {
        return Ok(run.log_dir);
    }

    // Try as service name
    if let Some(service) = db.find_service_by_name(id)? {
        if let Some(run) = db.get_latest_run(&service.id)? {
            return Ok(run.log_dir);
        }
        bail!("Service '{}' has no runs", id);
    }

    // Try partial ID match
    let services = db.list_services()?;
    for service in &services {
        if service.id.starts_with(id) {
            if let Some(run) = db.get_latest_run(&service.id)? {
                return Ok(run.log_dir);
            }
        }
    }

    bail!("No service or run found matching '{}'", id);
}

async fn follow_logs(reader: &LogReader, _stream: &str) -> Result<()> {
    let mut last_size = reader.file_size()?;

    // Show last 10 frames first
    let frames = reader.read_tail(10)?;
    let mut shown_frames = reader.read_frames()?.len();
    print!("{}", frames_to_text(&frames));

    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        let current_size = reader.file_size()?;
        if current_size > last_size {
            let all_frames = reader.read_frames()?;
            if all_frames.len() > shown_frames {
                let new_frames = &all_frames[shown_frames..];
                print!("{}", frames_to_text(new_frames));
                shown_frames = all_frames.len();
            }
            last_size = current_size;
        }
    }
}
