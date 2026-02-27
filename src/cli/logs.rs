use anyhow::Result;
use std::io::IsTerminal;
use std::path::Path;

use owo_colors::OwoColorize;

use crate::cli::LogsArgs;
use crate::config::Config;
use crate::storage::logfile::{frames_to_text, JsonFrame, LogReader};
use crate::storage::models::Frame;
use crate::storage::Database;

fn frames_to_json_pretty(frames: &[Frame]) -> Result<String> {
    let json_frames: Vec<JsonFrame> = frames.iter().map(JsonFrame::from_frame).collect();
    Ok(serde_json::to_string_pretty(&json_frames)?)
}

pub async fn handle_logs(args: LogsArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    // Resolve ID to a log directory
    let log_dir = db.resolve_log_dir(&args.id)?;

    let reader = LogReader::new(Path::new(&log_dir), args.stream);

    if args.follow {
        if args.json {
            follow_logs_json(&reader).await?;
        } else {
            follow_logs(&reader).await?;
        }
    } else if let Some(n) = args.tail {
        let frames = reader.read_tail(n)?;
        if args.json {
            println!("{}", frames_to_json_pretty(&frames)?);
        } else {
            print!("{}", frames_to_text(&frames));
        }
    } else if let Some(n) = args.head {
        let frames = reader.read_head(n)?;
        if args.json {
            println!("{}", frames_to_json_pretty(&frames)?);
        } else {
            print!("{}", frames_to_text(&frames));
        }
    } else {
        // Default: show last 50 lines
        let frames = reader.read_tail(50)?;
        if args.json {
            println!("{}", frames_to_json_pretty(&frames)?);
        } else {
            print!("{}", frames_to_text(&frames));
        }
    }

    Ok(())
}

async fn follow_logs(reader: &LogReader) -> Result<()> {
    let tty = std::io::stderr().is_terminal();

    if tty {
        eprintln!(
            "{} Following output... (Ctrl+C to stop)",
            "[brainlog]".dimmed()
        );
    } else {
        eprintln!("[brainlog] Following output... (Ctrl+C to stop)");
    }

    // Show last 10 frames first
    let frames = reader.read_tail(10)?;
    print!("{}", frames_to_text(&frames));

    // Track the latest timestamp from the initial tail for timestamp-based polling
    let mut last_ts = frames.iter().map(|f| f.timestamp_ns).max().unwrap_or(0);
    // Track byte offset for offset-based polling (single-stream)
    let mut offset = reader.file_size()?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if tty {
                    eprintln!("\n{} Stopped following.", "[brainlog]".dimmed());
                } else {
                    eprintln!("\n[brainlog] Stopped following.");
                }
                return Ok(());
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(200)) => {
                if reader.is_combined() {
                    // Combined mode: poll by timestamp across all stream files
                    let new_frames = reader.read_frames_since(last_ts)?;
                    if !new_frames.is_empty() {
                        if let Some(max_ts) = new_frames.iter().map(|f| f.timestamp_ns).max() {
                            last_ts = max_ts;
                        }
                        print!("{}", frames_to_text(&new_frames));
                    }
                } else {
                    // Single-stream: poll by byte offset
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
        }
    }
}

/// Follow logs in NDJSON format: one compact JSON object per frame per line.
async fn follow_logs_json(reader: &LogReader) -> Result<()> {
    // Show last 10 frames first as NDJSON
    let frames = reader.read_tail(10)?;
    for frame in &frames {
        let json_frame = JsonFrame::from_frame(frame);
        println!("{}", serde_json::to_string(&json_frame)?);
    }

    // Start incremental reads from end of file
    let mut offset = reader.file_size()?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                return Ok(());
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(200)) => {
                let current_size = reader.file_size()?;
                if current_size > offset {
                    let (new_frames, new_offset) = reader.read_frames_from_offset(offset)?;
                    for frame in &new_frames {
                        let json_frame = JsonFrame::from_frame(frame);
                        println!("{}", serde_json::to_string(&json_frame)?);
                    }
                    offset = new_offset;
                }
            }
        }
    }
}
