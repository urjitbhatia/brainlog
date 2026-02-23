use anyhow::Result;
use std::path::Path;

use crate::cli::LogsArgs;
use crate::config::Config;
use crate::storage::logfile::{frames_to_text, LogReader};
use crate::storage::Database;

pub async fn handle_logs(args: LogsArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    // Resolve ID to a log directory
    let log_dir = db.resolve_log_dir(&args.id)?;

    let reader = LogReader::new(Path::new(&log_dir), args.stream);

    if args.follow {
        follow_logs(&reader).await?;
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

async fn follow_logs(reader: &LogReader) -> Result<()> {
    // Show last 10 frames first
    let frames = reader.read_tail(10)?;
    print!("{}", frames_to_text(&frames));

    // Start incremental reads from end of file
    let mut offset = reader.file_size()?;

    loop {
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        let current_size = reader.file_size()?;
        if current_size > offset {
            let (new_frames, new_offset) = reader.read_frames_from_offset(offset)?;
            if !new_frames.is_empty() {
                print!("{}", frames_to_text(&new_frames));
            }
            offset = new_offset;
        }
    }
}
