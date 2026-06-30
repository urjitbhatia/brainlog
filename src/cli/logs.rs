use anyhow::Result;
use std::io::IsTerminal;
use std::path::Path;

use owo_colors::OwoColorize;

use crate::cli::LogsArgs;
use crate::config::Config;
use crate::storage::logfile::{frames_to_text, JsonFrame, LogReader};
use crate::storage::models::{Frame, Run, StreamFilter};
use crate::storage::{Database, FollowTarget};

fn frames_to_json_pretty(frames: &[Frame]) -> Result<String> {
    let json_frames: Vec<JsonFrame> = frames.iter().map(JsonFrame::from_frame).collect();
    Ok(serde_json::to_string_pretty(&json_frames)?)
}

pub async fn handle_logs(args: LogsArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    if args.follow {
        // Resolve to a follow target so that following a *service* can switch
        // to a new run's log directory when the service is restarted.
        let (service_id, current_run) = match db.resolve_follow_target(&args.id)? {
            FollowTarget::Run(run) => (None, run),
            FollowTarget::Service {
                service_id,
                current,
            } => (Some(service_id), current),
        };
        if args.json {
            follow_logs_json(&db, args.stream, service_id, current_run).await?;
        } else {
            follow_logs(&db, args.stream, service_id, current_run).await?;
        }
        return Ok(());
    }

    // Resolve ID to a log directory
    let log_dir = db.resolve_log_dir(&args.id)?;

    let reader = LogReader::new(Path::new(&log_dir), args.stream);

    if let Some(n) = args.tail {
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

/// Read frames that have appeared since the last poll, advancing the relevant
/// cursor: `last_ts` for combined mode (merge-sort across stream files) or the
/// byte `offset` for a single stream.
fn read_new_frames(reader: &LogReader, last_ts: &mut u64, offset: &mut u64) -> Result<Vec<Frame>> {
    if reader.is_combined() {
        // Combined mode: poll by timestamp across all stream files
        let frames = reader.read_frames_since(*last_ts)?;
        if let Some(max_ts) = frames.iter().map(|f| f.timestamp_ns).max() {
            *last_ts = max_ts;
        }
        Ok(frames)
    } else {
        // Single-stream: poll by byte offset
        let current_size = reader.file_size()?;
        if current_size > *offset {
            let (frames, new_offset) = reader.read_frames_from_offset(*offset)?;
            *offset = new_offset;
            Ok(frames)
        } else {
            Ok(Vec::new())
        }
    }
}

/// When following a service, return a newer run to switch to if the service has
/// been restarted (i.e. its latest run differs from the one currently followed).
/// Returns `None` for a single-run follow target or when no newer run exists.
fn next_run(db: &Database, service_id: &Option<String>, current_run_id: &str) -> Option<Run> {
    let sid = service_id.as_ref()?;
    match db.get_latest_run(sid) {
        Ok(Some(run)) if run.id != current_run_id => Some(run),
        _ => None,
    }
}

async fn follow_logs(
    db: &Database,
    stream: StreamFilter,
    service_id: Option<String>,
    mut current_run: Run,
) -> Result<()> {
    let tty = std::io::stderr().is_terminal();

    if tty {
        eprintln!(
            "{} Following output... (Ctrl+C to stop)",
            "[brainlog]".dimmed()
        );
    } else {
        eprintln!("[brainlog] Following output... (Ctrl+C to stop)");
    }

    let mut reader = LogReader::new(Path::new(&current_run.log_dir), stream);

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
                let new_frames = read_new_frames(&reader, &mut last_ts, &mut offset)?;
                if !new_frames.is_empty() {
                    print!("{}", frames_to_text(&new_frames));
                }

                // If following a service, switch to its new run on restart so
                // following continues seamlessly across `brainlog restart`.
                if let Some(new_run) = next_run(db, &service_id, &current_run.id) {
                    // Drain any final frames the old run flushed before exiting.
                    let tail = read_new_frames(&reader, &mut last_ts, &mut offset)?;
                    if !tail.is_empty() {
                        print!("{}", frames_to_text(&tail));
                    }
                    print_restart_notice(tty, &new_run.id);
                    reader = LogReader::new(Path::new(&new_run.log_dir), stream);
                    last_ts = 0;
                    offset = 0;
                    current_run = new_run;
                }
            }
        }
    }
}

/// Follow logs in NDJSON format: one compact JSON object per frame per line.
async fn follow_logs_json(
    db: &Database,
    stream: StreamFilter,
    service_id: Option<String>,
    mut current_run: Run,
) -> Result<()> {
    let tty = std::io::stderr().is_terminal();
    let mut reader = LogReader::new(Path::new(&current_run.log_dir), stream);

    // Show last 10 frames first as NDJSON
    let frames = reader.read_tail(10)?;
    for frame in &frames {
        let json_frame = JsonFrame::from_frame(frame);
        println!("{}", serde_json::to_string(&json_frame)?);
    }

    let mut last_ts = frames.iter().map(|f| f.timestamp_ns).max().unwrap_or(0);
    let mut offset = reader.file_size()?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                return Ok(());
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_millis(200)) => {
                let new_frames = read_new_frames(&reader, &mut last_ts, &mut offset)?;
                for frame in &new_frames {
                    let json_frame = JsonFrame::from_frame(frame);
                    println!("{}", serde_json::to_string(&json_frame)?);
                }

                // Switch to the service's new run on restart (see follow_logs).
                if let Some(new_run) = next_run(db, &service_id, &current_run.id) {
                    let tail = read_new_frames(&reader, &mut last_ts, &mut offset)?;
                    for frame in &tail {
                        let json_frame = JsonFrame::from_frame(frame);
                        println!("{}", serde_json::to_string(&json_frame)?);
                    }
                    print_restart_notice(tty, &new_run.id);
                    reader = LogReader::new(Path::new(&new_run.log_dir), stream);
                    last_ts = 0;
                    offset = 0;
                    current_run = new_run;
                }
            }
        }
    }
}

/// Emit a notice to stderr that following has switched to a restarted run.
/// Goes to stderr so it never corrupts stdout (text or NDJSON) output.
fn print_restart_notice(tty: bool, new_run_id: &str) {
    let short = &new_run_id[..8.min(new_run_id.len())];
    if tty {
        eprintln!(
            "{} Service restarted — following new run {}...",
            "[brainlog]".dimmed(),
            short
        );
    } else {
        eprintln!("[brainlog] Service restarted — following new run {short}...");
    }
}
