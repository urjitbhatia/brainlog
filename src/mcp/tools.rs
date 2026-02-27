use anyhow::Result;
use regex::Regex;
use std::path::Path;
use std::time::Instant;

use crate::storage::logfile::{frames_to_text, LogReader};
use crate::storage::models::{LogMode, StreamFilter};
use crate::storage::Database;

use super::types::*;

/// Strip ANSI/terminal escape sequences using a proper VT parser.
fn strip_ansi_codes(s: &str) -> String {
    let stripped = strip_ansi_escapes::strip(s);
    String::from_utf8_lossy(&stripped).into_owned()
}

pub fn discover_services(
    db: &Database,
    params: DiscoverServicesParams,
) -> Result<serde_json::Value> {
    let group = params.group.unwrap_or(true);

    if group {
        discover_services_grouped(db, params)
    } else {
        discover_services_flat(db, params)
    }
}

fn discover_services_grouped(
    db: &Database,
    params: DiscoverServicesParams,
) -> Result<serde_json::Value> {
    let limit = params.limit.unwrap_or(20);
    let tail_lines = params.tail_lines.unwrap_or(0);
    let groups = db.list_services_grouped()?;

    let name_filter = params.name.map(|n| n.to_lowercase());
    let exe_filter = params.executable.map(|e| e.to_lowercase());
    let cwd_filter = params.cwd.map(|c| c.to_lowercase());

    let mut result = Vec::new();
    for group in groups {
        if let Some(ref needle) = name_filter {
            let matches = group.services.iter().any(|s| {
                s.name
                    .as_deref()
                    .map(|n| n.to_lowercase().contains(needle))
                    .unwrap_or(false)
                    || s.executable.to_lowercase().contains(needle)
            });
            if !matches {
                continue;
            }
        }
        if let Some(ref needle) = exe_filter {
            if !group.executable.to_lowercase().contains(needle) {
                continue;
            }
        }
        if let Some(ref needle) = cwd_filter {
            if !group.working_dir.to_lowercase().contains(needle) {
                continue;
            }
        }
        if let Some(ref status_filter) = params.status {
            match &group.latest_run_status {
                Some(s) if s.as_str() == status_filter => {}
                _ => continue,
            }
        }

        // Collect unique names and commands
        let mut names: Vec<String> = group
            .services
            .iter()
            .filter_map(|s| s.name.clone())
            .collect();
        names.sort();
        names.dedup();

        let mut commands: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for svc in &group.services {
            commands.insert(svc.command_line.join(" "));
        }

        // Get ports from the latest run of any service in the group
        let mut ports = Vec::new();
        for svc in &group.services {
            if let Some(run) = db.get_latest_run(&svc.id)? {
                let svc_ports: Vec<u16> = db.get_ports(&run.id)?.iter().map(|p| p.port).collect();
                ports.extend(svc_ports);
            }
        }
        ports.sort();
        ports.dedup();

        if let Some(port_filter) = params.port {
            if !ports.contains(&port_filter) {
                continue;
            }
        }

        let latest_run_row = group
            .services
            .iter()
            .filter_map(|svc| db.get_latest_run(&svc.id).ok().flatten())
            .max_by_key(|r| r.started_at);

        let log_preview = if tail_lines > 0 {
            latest_run_row
                .as_ref()
                .and_then(|r| read_log_preview(&r.log_dir, tail_lines))
        } else {
            None
        };

        let latest_run = latest_run_row.map(|r| RunInfo {
            id: r.id,
            status: r.status.as_str().to_string(),
            started_at: r.started_at.to_rfc3339(),
            ended_at: r.ended_at.map(|t| t.to_rfc3339()),
            exit_code: r.exit_code,
            pid: r.pid,
            wrapper_pid: r.wrapper_pid,
        });

        result.push(GroupedServiceInfo {
            executable: group.executable,
            working_dir: group.working_dir,
            run_count: group.run_count,
            names,
            latest_run,
            commands: commands.into_iter().collect(),
            ports,
            log_preview,
        });

        if result.len() >= limit {
            break;
        }
    }

    Ok(serde_json::to_value(DiscoverServicesGroupedResponse {
        groups: result,
    })?)
}

fn discover_services_flat(
    db: &Database,
    params: DiscoverServicesParams,
) -> Result<serde_json::Value> {
    let limit = params.limit.unwrap_or(20);
    let tail_lines = params.tail_lines.unwrap_or(0);

    let tag_filters: Vec<(String, String)> = params
        .tags
        .unwrap_or_default()
        .iter()
        .filter_map(|t| {
            t.split_once(':')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect();

    let services = db.search_services(
        params.name.as_deref(),
        params.executable.as_deref(),
        &tag_filters,
        params.status.as_deref(),
        params.port,
        params.cwd.as_deref(),
        limit,
    )?;

    let mut result = Vec::new();
    for service in services {
        let tags = db.get_tags(&service.id)?;
        let tag_infos: Vec<TagInfo> = tags
            .iter()
            .map(|t| TagInfo {
                key: t.key.clone(),
                value: t.value.clone(),
            })
            .collect();

        let latest_run = db.get_latest_run(&service.id)?;
        let ports = if let Some(ref run) = latest_run {
            db.get_ports(&run.id)?.iter().map(|p| p.port).collect()
        } else {
            Vec::new()
        };

        let log_preview = if tail_lines > 0 {
            latest_run
                .as_ref()
                .and_then(|r| read_log_preview(&r.log_dir, tail_lines))
        } else {
            None
        };

        let run_info = latest_run.map(|r| RunInfo {
            id: r.id,
            status: r.status.as_str().to_string(),
            started_at: r.started_at.to_rfc3339(),
            ended_at: r.ended_at.map(|t| t.to_rfc3339()),
            exit_code: r.exit_code,
            pid: r.pid,
            wrapper_pid: r.wrapper_pid,
        });

        result.push(ServiceInfo {
            id: service.id,
            name: service.name,
            description: service.description,
            executable: service.executable,
            command_line: service.command_line,
            working_dir: service.working_dir,
            tags: tag_infos,
            latest_run: run_info,
            ports,
            log_preview,
        });
    }

    Ok(serde_json::to_value(DiscoverServicesResponse {
        services: result,
    })?)
}

pub fn get_logs(db: &Database, params: GetLogsParams) -> Result<GetLogsResponse> {
    let stream = params.stream.unwrap_or_default();
    let mode = params.mode.unwrap_or_default();
    let lines = params.lines.unwrap_or(100);
    let max_bytes = params.max_bytes.unwrap_or(51200);
    let strip_ansi = params.strip_ansi.unwrap_or(true);

    // Resolve ID to log dir
    let log_dir = db.resolve_log_dir(&params.id)?;
    let reader = LogReader::new(Path::new(&log_dir), stream);

    let frames = match (mode, params.since) {
        // When `since` is provided, read only frames with timestamp >= since,
        // then apply head/tail/range semantics on top of that subset.
        (LogMode::Head, Some(since)) => {
            let ranged = reader.read_range(Some(since), None)?;
            ranged.into_iter().take(lines).collect()
        }
        (LogMode::Tail, Some(since)) => {
            let ranged = reader.read_range(Some(since), None)?;
            let skip = ranged.len().saturating_sub(lines);
            ranged.into_iter().skip(skip).collect()
        }
        (LogMode::Range, Some(since)) => {
            let effective_start = Some(std::cmp::max(since, params.start_time.unwrap_or(0)));
            reader.read_range(effective_start, params.end_time)?
        }
        // No `since` — original behavior
        (LogMode::Head, None) => reader.read_head(lines)?,
        (LogMode::Range, None) => reader.read_range(params.start_time, params.end_time)?,
        (LogMode::Tail, None) => reader.read_tail(lines)?,
    };

    let frame_count = frames.len();
    let text = frames_to_text(&frames);
    let text = if strip_ansi {
        strip_ansi_codes(&text)
    } else {
        text
    };
    let has_more = text.len() > max_bytes;
    let content = if has_more {
        text[..max_bytes].to_string()
    } else {
        text
    };

    Ok(GetLogsResponse {
        content,
        frame_count,
        has_more,
        stream: stream.as_str().to_string(),
    })
}

pub fn search_logs(db: &Database, params: SearchLogsParams) -> Result<SearchLogsResponse> {
    let pattern = Regex::new(&params.pattern)?;
    let stream = params.stream.unwrap_or_default();
    let max_matches = params.max_matches.unwrap_or(50);
    let strip_ansi = params.strip_ansi.unwrap_or(true);

    let services = if let Some(ref sid) = params.service_id {
        if let Some(s) = db.get_service(sid)? {
            vec![s]
        } else {
            Vec::new()
        }
    } else {
        db.list_services()?
    };

    let mut all_matches = Vec::new();

    for service in &services {
        let runs = db.list_runs(&service.id)?;
        for run in &runs {
            let reader = LogReader::new(Path::new(&run.log_dir), stream);
            let remaining = max_matches.saturating_sub(all_matches.len());
            if remaining == 0 {
                break;
            }
            let matches = reader.search(&pattern, remaining)?;
            for m in matches {
                // Apply time filters
                if let Some(start) = params.start_time {
                    if m.timestamp_ns < start {
                        continue;
                    }
                }
                if let Some(end) = params.end_time {
                    if m.timestamp_ns > end {
                        continue;
                    }
                }
                let line = if strip_ansi {
                    strip_ansi_codes(&m.line)
                } else {
                    m.line
                };
                all_matches.push(SearchMatch {
                    service_id: service.id.clone(),
                    service_name: service.name.clone(),
                    run_id: run.id.clone(),
                    stream: m.stream_type.as_str().to_string(),
                    timestamp_ns: m.timestamp_ns,
                    line,
                });
            }
        }
    }

    let total = all_matches.len();
    Ok(SearchLogsResponse {
        matches: all_matches,
        total_matches: total,
    })
}

/// Resolve the log directory from the database (synchronous), then delegate
/// to the async polling loop. This two-phase design avoids holding a `&Database`
/// reference across `.await` points (Database is not Sync).
pub fn wait_for_pattern_resolve(db: &Database, params: &WaitForPatternParams) -> Result<String> {
    db.resolve_log_dir(&params.id)
}

pub async fn wait_for_pattern(
    log_dir: &str,
    params: WaitForPatternParams,
) -> Result<WaitForPatternResponse> {
    let pattern = Regex::new(&params.pattern)?;
    let stream = params.stream.unwrap_or_default();
    let timeout_secs = params.timeout.unwrap_or(30);
    let poll_interval_ms = params.poll_interval_ms.unwrap_or(500);
    let should_strip_ansi = params.strip_ansi.unwrap_or(true);

    let reader = LogReader::new(Path::new(log_dir), stream);

    let start = Instant::now();
    let timeout_dur = std::time::Duration::from_secs(timeout_secs);
    let poll_dur = std::time::Duration::from_millis(poll_interval_ms);

    // Default to "now" so we only match new lines, unless caller explicitly
    // passes since=0 to search the full history.
    let since = params.since.unwrap_or_else(crate::process::capture::now_ns);
    let mut last_seen_ts: u64 = since;

    loop {
        // Read frames newer than what we have already checked.
        // read_range uses >=, so we add 1 to skip already-seen frames.
        let start_time = Some(last_seen_ts + 1);
        let frames = reader.read_range(start_time, None)?;

        for frame in &frames {
            if frame.timestamp_ns > last_seen_ts {
                last_seen_ts = frame.timestamp_ns;
            }
            if let Ok(text) = std::str::from_utf8(&frame.payload) {
                for line in text.lines() {
                    let matchable = if should_strip_ansi {
                        strip_ansi_codes(line)
                    } else {
                        line.to_string()
                    };
                    if pattern.is_match(&matchable) {
                        let elapsed_ms = start.elapsed().as_millis() as u64;
                        return Ok(WaitForPatternResponse {
                            matched: true,
                            line: Some(matchable),
                            timestamp_ns: Some(frame.timestamp_ns),
                            elapsed_ms,
                            timed_out: false,
                        });
                    }
                }
            }
        }

        if start.elapsed() >= timeout_dur {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            return Ok(WaitForPatternResponse {
                matched: false,
                line: None,
                timestamp_ns: None,
                elapsed_ms,
                timed_out: true,
            });
        }

        tokio::time::sleep(poll_dur).await;
    }
}

fn read_log_preview(log_dir: &str, tail_lines: usize) -> Option<String> {
    let reader = LogReader::new(Path::new(log_dir), StreamFilter::Combined);
    let frames = reader.read_tail(tail_lines).ok()?;
    if frames.is_empty() {
        return None;
    }
    Some(frames_to_text(&frames))
}

/// Resolved info from the sync phase of kill_service.
pub struct KillServiceResolved {
    pub service_name: String,
    pub service_id: String,
    pub signal: nix::sys::signal::Signal,
    /// Set when fully handled in the resolve phase (no async work needed).
    pub early_response: Option<KillServiceResponse>,
    /// Child PID for direct kill fallback (SIGKILL or no wrapper).
    pub child_pid: Option<u32>,
    /// Wrapper PID for SIGKILL cleanup.
    pub wrapper_pid: Option<u32>,
}

/// Phase 1: synchronously resolve service, run, and PID info (Database is not Sync).
///
/// For catchable signals, sends the signal to the wrapper (which kills the child
/// tree and exits cleanly). For SIGKILL or when no wrapper is available, returns
/// the child PID so the async phase can kill the tree directly.
pub fn kill_service_resolve(
    db: &Database,
    params: &KillServiceParams,
) -> Result<KillServiceResolved> {
    use crate::cli::kill::{is_process_alive, resolve_service};
    use crate::storage::models::RunStatus;
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    let sig = parse_signal(params.signal.as_deref().unwrap_or("TERM"))?;

    let (service, _) = resolve_service(db, &params.id)?;
    let service_name = service
        .name
        .clone()
        .unwrap_or_else(|| service.id[..8.min(service.id.len())].to_string());

    let run = db
        .get_latest_run(&service.id)?
        .ok_or_else(|| anyhow::anyhow!("Service '{}' has no runs", service_name))?;

    // If child isn't running, try the wrapper PID
    if run.status != RunStatus::Running {
        if let Some(wrapper_pid) = run.wrapper_pid {
            if is_process_alive(wrapper_pid) {
                let nix_pid = Pid::from_raw(wrapper_pid as i32);
                signal::kill(nix_pid, sig).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to send {} to wrapper PID {}: {}",
                        sig,
                        wrapper_pid,
                        e
                    )
                })?;
                return Ok(KillServiceResolved {
                    service_name: service_name.clone(),
                    service_id: service.id.clone(),
                    signal: sig,
                    early_response: Some(KillServiceResponse {
                        success: true,
                        service_name,
                        service_id: service.id,
                        signal: format!("{}", sig),
                        pids: vec![wrapper_pid],
                        message: format!(
                            "Sent {} to wrapper PID {} (child not running)",
                            sig, wrapper_pid
                        ),
                    }),
                    child_pid: None,
                    wrapper_pid: None,
                });
            }
        }
        anyhow::bail!(
            "Service '{}' is not running (status: {})",
            service_name,
            run.status.as_str()
        );
    }

    let pid = run
        .pid
        .ok_or_else(|| anyhow::anyhow!("Service '{}' has no PID recorded", service_name))?;

    // For catchable signals, send to wrapper and let it handle the child tree.
    if sig != Signal::SIGKILL {
        if let Some(wrapper_pid) = run.wrapper_pid {
            if is_process_alive(wrapper_pid) {
                let nix_pid = Pid::from_raw(wrapper_pid as i32);
                signal::kill(nix_pid, sig).map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to send {} to wrapper PID {}: {}",
                        sig,
                        wrapper_pid,
                        e
                    )
                })?;
                let message = format!(
                    "Sent {} to '{}' (wrapper PID {})",
                    sig, service_name, wrapper_pid
                );
                return Ok(KillServiceResolved {
                    service_name: service_name.clone(),
                    service_id: service.id.clone(),
                    signal: sig,
                    early_response: Some(KillServiceResponse {
                        success: true,
                        service_name,
                        service_id: service.id,
                        signal: format!("{}", sig),
                        pids: vec![wrapper_pid],
                        message,
                    }),
                    child_pid: None,
                    wrapper_pid: None,
                });
            }
        }
    }

    // Fallback: SIGKILL or no wrapper — async phase will kill child tree directly
    Ok(KillServiceResolved {
        service_name,
        service_id: service.id,
        signal: sig,
        early_response: None,
        child_pid: Some(pid),
        wrapper_pid: run.wrapper_pid,
    })
}

/// Phase 2: async kill (collect process tree, send signals). No &Database needed.
///
/// Only runs when the resolve phase couldn't handle it (SIGKILL or no wrapper).
pub async fn kill_service(resolved: KillServiceResolved) -> Result<KillServiceResponse> {
    use crate::cli::kill::{collect_process_tree, is_process_alive};
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    // If resolve phase already handled it (wrapper-mediated kill), return early.
    if let Some(resp) = resolved.early_response {
        return Ok(resp);
    }

    let pid = resolved
        .child_pid
        .ok_or_else(|| anyhow::anyhow!("No target PID to kill"))?;

    let tree = collect_process_tree(pid).await;

    // Send signal to children first (deepest first), then parent
    let mut kill_order: Vec<u32> = tree.iter().copied().filter(|&p| p != pid).collect();
    kill_order.reverse();
    kill_order.push(pid);

    let mut signaled = Vec::new();
    for target_pid in &kill_order {
        let nix_pid = Pid::from_raw(*target_pid as i32);
        if signal::kill(nix_pid, resolved.signal).is_ok() {
            signaled.push(*target_pid);
        }
    }

    // For SIGKILL, also kill the wrapper since it can't catch the signal itself
    if resolved.signal == Signal::SIGKILL {
        if let Some(wrapper_pid) = resolved.wrapper_pid {
            if is_process_alive(wrapper_pid) {
                let nix_pid = Pid::from_raw(wrapper_pid as i32);
                let _ = signal::kill(nix_pid, Signal::SIGKILL);
            }
        }
    }

    let message = if kill_order.len() == 1 {
        format!(
            "Sent {} to '{}' (PID {})",
            resolved.signal, resolved.service_name, pid
        )
    } else {
        format!(
            "Sent {} to '{}' (PID {} + {} child processes)",
            resolved.signal,
            resolved.service_name,
            pid,
            kill_order.len() - 1
        )
    };

    Ok(KillServiceResponse {
        success: true,
        service_name: resolved.service_name,
        service_id: resolved.service_id,
        signal: format!("{}", resolved.signal),
        pids: signaled,
        message,
    })
}

pub fn restart_service(
    db: &Database,
    params: RestartServiceParams,
) -> Result<RestartServiceResponse> {
    use crate::cli::kill::resolve_service;
    use crate::storage::models::RunStatus;
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;

    let (service, _) = resolve_service(db, &params.id)?;
    let service_name = service
        .name
        .clone()
        .unwrap_or_else(|| service.id[..8.min(service.id.len())].to_string());

    let run = db
        .get_latest_run(&service.id)?
        .ok_or_else(|| anyhow::anyhow!("Service '{}' has no runs", service_name))?;

    if run.status != RunStatus::Running {
        anyhow::bail!(
            "Service '{}' is not running (status: {})",
            service_name,
            run.status.as_str()
        );
    }

    let wrapper_pid = run.wrapper_pid.ok_or_else(|| {
        anyhow::anyhow!(
            "Service '{}' has no wrapper PID (was it started with an older brainlog version?)",
            service_name
        )
    })?;

    let nix_pid = Pid::from_raw(wrapper_pid as i32);
    signal::kill(nix_pid, Signal::SIGUSR1).map_err(|e| {
        anyhow::anyhow!(
            "Failed to send SIGUSR1 to wrapper PID {}: {}",
            wrapper_pid,
            e
        )
    })?;

    Ok(RestartServiceResponse {
        success: true,
        service_name: service_name.clone(),
        service_id: service.id,
        wrapper_pid,
        message: format!(
            "Sent restart signal to '{}' (wrapper PID {})",
            service_name, wrapper_pid
        ),
    })
}

/// Parse a signal name or number into a nix Signal.
fn parse_signal(s: &str) -> Result<nix::sys::signal::Signal> {
    use nix::sys::signal::Signal;
    let upper = s.to_uppercase();
    let name = if upper.starts_with("SIG") {
        upper.as_str()
    } else {
        &upper
    };
    match name {
        "TERM" | "SIGTERM" => return Ok(Signal::SIGTERM),
        "KILL" | "SIGKILL" => return Ok(Signal::SIGKILL),
        "INT" | "SIGINT" => return Ok(Signal::SIGINT),
        "HUP" | "SIGHUP" => return Ok(Signal::SIGHUP),
        "USR1" | "SIGUSR1" => return Ok(Signal::SIGUSR1),
        "USR2" | "SIGUSR2" => return Ok(Signal::SIGUSR2),
        "QUIT" | "SIGQUIT" => return Ok(Signal::SIGQUIT),
        _ => {}
    }
    if let Ok(num) = s.parse::<i32>() {
        return Signal::try_from(num)
            .map_err(|_| anyhow::anyhow!("Invalid signal number: {}", num));
    }
    anyhow::bail!(
        "Unknown signal '{}'. Supported: TERM, KILL, INT, HUP, USR1, USR2, QUIT, or a number",
        s
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::*;
    use crate::storage::LogWriter;
    use chrono::Utc;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    /// Create an in-memory DB with a service, a run, and log files in a temp dir.
    /// Returns (Database, service_id, run_id, TempDir).
    async fn setup_test_env() -> (Database, String, String, TempDir) {
        let db = Database::open_in_memory().unwrap();
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        let svc = Service {
            id: "svc-mcp-001".to_string(),
            name: Some("test-web".to_string()),
            description: Some("A test web server".to_string()),
            executable: "node".to_string(),
            command_line: vec!["node".to_string(), "server.js".to_string()],
            working_dir: "/tmp/project".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            enrichment_status: EnrichmentStatus::Skipped,
        };
        db.create_service(&svc).unwrap();
        db.add_tag("svc-mcp-001", "env", "dev").unwrap();

        let run = Run {
            id: "run-mcp-001".to_string(),
            service_id: "svc-mcp-001".to_string(),
            pid: Some(9999),
            started_at: Utc::now(),
            ended_at: None,
            exit_code: Some(0),
            log_dir: log_dir.to_string_lossy().to_string(),
            status: RunStatus::Completed,
            wrapper_pid: None,
        };
        db.create_run(&run).unwrap();

        // Write some log frames
        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        tx.send(Frame {
            timestamp_ns: 1_000_000_000,
            stream_type: StreamType::Stdout,
            payload: b"INFO: server started on port 3000\n".to_vec(),
        })
        .await
        .unwrap();
        tx.send(Frame {
            timestamp_ns: 2_000_000_000,
            stream_type: StreamType::Stderr,
            payload: b"WARN: deprecated API used\n".to_vec(),
        })
        .await
        .unwrap();
        tx.send(Frame {
            timestamp_ns: 3_000_000_000,
            stream_type: StreamType::Stdout,
            payload: b"ERROR: connection refused\n".to_vec(),
        })
        .await
        .unwrap();
        tx.send(Frame {
            timestamp_ns: 4_000_000_000,
            stream_type: StreamType::Stdout,
            payload: b"INFO: retrying...\n".to_vec(),
        })
        .await
        .unwrap();
        drop(tx);
        writer.run().await.unwrap();

        (
            db,
            "svc-mcp-001".to_string(),
            "run-mcp-001".to_string(),
            dir,
        )
    }

    // ── discover_services (grouped, default) ──────────────────────────

    fn default_params() -> DiscoverServicesParams {
        DiscoverServicesParams {
            name: None,
            tags: None,
            port: None,
            executable: None,
            cwd: None,
            status: None,
            query: None,
            limit: None,
            group: None,
            tail_lines: None,
        }
    }

    #[tokio::test]
    async fn discover_grouped_default() {
        let (db, _, _, _dir) = setup_test_env().await;
        let value = discover_services(&db, default_params()).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 1);

        let g = &resp.groups[0];
        assert_eq!(g.executable, "node");
        assert_eq!(g.working_dir, "/tmp/project");
        assert_eq!(g.run_count, 1);
        assert_eq!(g.names, vec!["test-web"]);
        assert!(g.latest_run.is_some());
        assert_eq!(g.commands, vec!["node server.js"]);
        let run = g.latest_run.as_ref().unwrap();
        assert_eq!(run.status, "completed");
    }

    #[tokio::test]
    async fn discover_grouped_filter_by_name() {
        let (db, _, _, _dir) = setup_test_env().await;

        let mut params = default_params();
        params.name = Some("web".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 1);

        let mut params = default_params();
        params.name = Some("nonexistent".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 0);
    }

    #[tokio::test]
    async fn discover_grouped_filter_by_executable() {
        let (db, _, _, _dir) = setup_test_env().await;

        let mut params = default_params();
        params.executable = Some("node".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 1);

        let mut params = default_params();
        params.executable = Some("python".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 0);
    }

    // ── discover_services (tail_lines preview) ────────────────────

    #[tokio::test]
    async fn discover_grouped_with_log_preview() {
        let (db, _, _, _dir) = setup_test_env().await;
        let mut params = default_params();
        params.tail_lines = Some(2);
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 1);
        let preview = resp.groups[0].log_preview.as_ref().unwrap();
        assert!(preview.contains("retrying"), "should have last line");
    }

    #[tokio::test]
    async fn discover_grouped_without_log_preview() {
        let (db, _, _, _dir) = setup_test_env().await;
        let value = discover_services(&db, default_params()).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert!(resp.groups[0].log_preview.is_none());
    }

    #[tokio::test]
    async fn discover_flat_with_log_preview() {
        let (db, _, _, _dir) = setup_test_env().await;
        let mut params = default_params();
        params.group = Some(false);
        params.tail_lines = Some(3);
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        let preview = resp.services[0].log_preview.as_ref().unwrap();
        assert!(preview.contains("retrying"), "should have last stdout line");
    }

    #[tokio::test]
    async fn discover_grouped_filter_by_cwd() {
        let (db, _, _, _dir) = setup_test_env().await;

        let mut params = default_params();
        params.cwd = Some("project".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 1);

        let mut params = default_params();
        params.cwd = Some("/home/nonexistent".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesGroupedResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.groups.len(), 0);
    }

    // ── discover_services (flat, group=false) ───────────────────────

    #[tokio::test]
    async fn discover_flat_all_services() {
        let (db, _, _, _dir) = setup_test_env().await;
        let mut params = default_params();
        params.group = Some(false);
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.services.len(), 1);

        let svc = &resp.services[0];
        assert_eq!(svc.name.as_deref(), Some("test-web"));
        assert_eq!(svc.executable, "node");
        assert_eq!(svc.tags.len(), 1);
        assert_eq!(svc.tags[0].key, "env");
        assert_eq!(svc.tags[0].value, "dev");
        assert!(svc.latest_run.is_some());
        let run = svc.latest_run.as_ref().unwrap();
        assert_eq!(run.status, "completed");
        assert_eq!(run.exit_code, Some(0));
    }

    #[tokio::test]
    async fn discover_flat_filter_by_name() {
        let (db, _, _, _dir) = setup_test_env().await;
        let mut params = default_params();
        params.group = Some(false);
        params.name = Some("web".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.services.len(), 1);

        let mut params = default_params();
        params.group = Some(false);
        params.name = Some("nonexistent".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.services.len(), 0);
    }

    #[tokio::test]
    async fn discover_flat_filter_by_tag() {
        let (db, _, _, _dir) = setup_test_env().await;
        let mut params = default_params();
        params.group = Some(false);
        params.tags = Some(vec!["env:dev".to_string()]);
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.services.len(), 1);
    }

    #[tokio::test]
    async fn discover_flat_filter_by_cwd() {
        let (db, _, _, _dir) = setup_test_env().await;
        let mut params = default_params();
        params.group = Some(false);
        params.cwd = Some("project".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.services.len(), 1);

        let mut params = default_params();
        params.group = Some(false);
        params.cwd = Some("/home/nonexistent".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.services.len(), 0);
    }

    #[tokio::test]
    async fn discover_flat_filter_by_cwd_case_insensitive() {
        let (db, _, _, _dir) = setup_test_env().await;
        let mut params = default_params();
        params.group = Some(false);
        params.cwd = Some("PROJECT".to_string());
        let value = discover_services(&db, params).unwrap();
        let resp: DiscoverServicesResponse = serde_json::from_value(value).unwrap();
        assert_eq!(resp.services.len(), 1);
    }

    // ── get_logs ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn get_logs_by_service_name() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: None,
            lines: None,
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.stream, "combined");
        assert_eq!(resp.frame_count, 4);
        assert!(resp.content.contains("server started"));
        assert!(resp.content.contains("deprecated API"));
        assert!(resp.content.contains("ERROR"));
    }

    #[tokio::test]
    async fn get_logs_by_run_id() {
        let (db, _, run_id, _dir) = setup_test_env().await;
        let params = GetLogsParams {
            id: run_id,
            stream: Some(StreamFilter::Stdout),
            mode: Some(LogMode::Head),
            lines: Some(2),
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.stream, "stdout");
        assert_eq!(resp.frame_count, 2);
        assert!(resp.content.contains("server started"));
        assert!(resp.content.contains("ERROR: connection refused"));
        assert!(!resp.content.contains("retrying"));
    }

    #[tokio::test]
    async fn get_logs_tail_mode() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: Some(StreamFilter::Stdout),
            mode: Some(LogMode::Tail),
            lines: Some(1),
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.frame_count, 1);
        assert!(resp.content.contains("retrying"));
    }

    #[tokio::test]
    async fn get_logs_max_bytes_truncation() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: None,
            lines: None,
            start_time: None,
            end_time: None,
            max_bytes: Some(20),
            strip_ansi: None,
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert!(resp.has_more, "should indicate truncation");
        assert!(resp.content.len() <= 20);
    }

    #[tokio::test]
    async fn get_logs_stderr_stream() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: Some(StreamFilter::Stderr),
            mode: None,
            lines: None,
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.stream, "stderr");
        assert_eq!(resp.frame_count, 1);
        assert!(resp.content.contains("deprecated API"));
    }

    // ── get_logs since ─────────────────────────────────────────────

    #[tokio::test]
    async fn get_logs_since_filters_old_frames_head_mode() {
        let (db, _, _, _dir) = setup_test_env().await;
        // Frames have timestamps 1e9, 2e9, 3e9, 4e9.
        // Setting since=3_000_000_000 should only return frames at 3e9 and 4e9.
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: Some(LogMode::Head),
            lines: Some(100),
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: Some(3_000_000_000),
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.frame_count, 2);
        assert!(resp.content.contains("ERROR: connection refused"));
        assert!(resp.content.contains("retrying"));
        assert!(!resp.content.contains("server started"));
        assert!(!resp.content.contains("deprecated API"));
    }

    #[tokio::test]
    async fn get_logs_since_head_mode_limits_lines() {
        let (db, _, _, _dir) = setup_test_env().await;
        // since=2e9 gives frames at 2e9, 3e9, 4e9 (3 frames).
        // Head with lines=1 should only return the first of those.
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: Some(LogMode::Head),
            lines: Some(1),
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: Some(2_000_000_000),
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.frame_count, 1);
        assert!(resp.content.contains("deprecated API"));
    }

    #[tokio::test]
    async fn get_logs_since_tail_mode() {
        let (db, _, _, _dir) = setup_test_env().await;
        // since=2e9 gives frames at 2e9, 3e9, 4e9 (3 frames).
        // Tail with lines=1 should only return the last of those.
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: Some(LogMode::Tail),
            lines: Some(1),
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: Some(2_000_000_000),
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.frame_count, 1);
        assert!(resp.content.contains("retrying"));
    }

    #[tokio::test]
    async fn get_logs_since_range_mode() {
        let (db, _, _, _dir) = setup_test_env().await;
        // range start_time=1e9 but since=3e9 => effective start = max(3e9, 1e9) = 3e9
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: Some(LogMode::Range),
            lines: None,
            start_time: Some(1_000_000_000),
            end_time: Some(4_000_000_000),
            max_bytes: None,
            strip_ansi: None,
            since: Some(3_000_000_000),
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.frame_count, 2);
        assert!(resp.content.contains("ERROR: connection refused"));
        assert!(resp.content.contains("retrying"));
        assert!(!resp.content.contains("server started"));
    }

    #[tokio::test]
    async fn get_logs_since_none_returns_all() {
        let (db, _, _, _dir) = setup_test_env().await;
        // Without since, all 4 frames should be returned
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: Some(LogMode::Head),
            lines: Some(100),
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.frame_count, 4);
    }

    #[tokio::test]
    async fn get_logs_since_future_returns_nothing() {
        let (db, _, _, _dir) = setup_test_env().await;
        // since far in the future should return no frames
        let params = GetLogsParams {
            id: "test-web".to_string(),
            stream: None,
            mode: Some(LogMode::Head),
            lines: Some(100),
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: Some(999_000_000_000),
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.frame_count, 0);
    }

    // ── search_logs ──────────────────────────────────────────────────

    #[tokio::test]
    async fn search_finds_matching_lines() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = SearchLogsParams {
            pattern: "ERROR".to_string(),
            service_id: None,
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: None,
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 1);
        assert!(resp.matches[0].line.contains("connection refused"));
    }

    #[tokio::test]
    async fn search_multiple_matches() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = SearchLogsParams {
            pattern: "INFO".to_string(),
            service_id: None,
            stream: Some(StreamFilter::Stdout),
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: None,
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 2);
    }

    #[tokio::test]
    async fn search_no_matches() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = SearchLogsParams {
            pattern: "FATAL_CRASH".to_string(),
            service_id: None,
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: None,
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 0);
    }

    #[tokio::test]
    async fn search_respects_max_matches() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = SearchLogsParams {
            pattern: ".*".to_string(), // matches everything
            service_id: None,
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: Some(2),
            strip_ansi: None,
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 2);
    }

    #[tokio::test]
    async fn search_filters_by_service_id() {
        let (db, svc_id, _, _dir) = setup_test_env().await;
        let params = SearchLogsParams {
            pattern: "INFO".to_string(),
            service_id: Some(svc_id),
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: None,
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 2);

        // Non-existent service
        let params = SearchLogsParams {
            pattern: "INFO".to_string(),
            service_id: Some("nonexistent-id".to_string()),
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: None,
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 0);
    }

    // ── resolve_log_dir ──────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_by_run_id() {
        let (db, _, run_id, dir) = setup_test_env().await;
        let log_dir = db.resolve_log_dir(&run_id).unwrap();
        assert_eq!(log_dir, dir.path().to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_by_service_name() {
        let (db, _, _, dir) = setup_test_env().await;
        let log_dir = db.resolve_log_dir("test-web").unwrap();
        assert_eq!(log_dir, dir.path().to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_unknown_id_fails() {
        let (db, _, _, _dir) = setup_test_env().await;
        assert!(db.resolve_log_dir("no-such-thing").is_err());
    }

    // ── strip_ansi ──────────────────────────────────────────────────

    #[test]
    fn strip_ansi_codes_removes_csi_sequences() {
        let input = "\x1b[32mINFO\x1b[0m: server started";
        assert_eq!(strip_ansi_codes(input), "INFO: server started");
    }

    #[test]
    fn strip_ansi_codes_removes_osc_sequences() {
        let input = "\x1b]0;window title\x07some text";
        assert_eq!(strip_ansi_codes(input), "some text");
    }

    #[test]
    fn strip_ansi_codes_preserves_plain_text() {
        let input = "plain text with no escape codes";
        assert_eq!(strip_ansi_codes(input), input);
    }

    #[test]
    fn strip_ansi_codes_handles_multiple_sequences() {
        let input = "\x1b[1m\x1b[31mERROR\x1b[0m: \x1b[33mconnection\x1b[0m refused";
        assert_eq!(strip_ansi_codes(input), "ERROR: connection refused");
    }

    #[test]
    fn strip_ansi_codes_handles_dec_private_mode() {
        let input = "\x1b[?2026hsome text\x1b[?2026l\x1b[Omore\x1b[I";
        assert_eq!(strip_ansi_codes(input), "some textmore");
    }

    /// Create a test env with ANSI escape codes in log output.
    async fn setup_ansi_test_env() -> (Database, String, String, TempDir) {
        let db = Database::open_in_memory().unwrap();
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        let svc = Service {
            id: "svc-ansi-001".to_string(),
            name: Some("ansi-test".to_string()),
            description: Some("Service with ANSI logs".to_string()),
            executable: "node".to_string(),
            command_line: vec!["node".to_string(), "app.js".to_string()],
            working_dir: "/tmp/project".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            enrichment_status: EnrichmentStatus::Skipped,
        };
        db.create_service(&svc).unwrap();

        let run = Run {
            id: "run-ansi-001".to_string(),
            service_id: "svc-ansi-001".to_string(),
            pid: Some(8888),
            started_at: Utc::now(),
            ended_at: None,
            exit_code: Some(0),
            log_dir: log_dir.to_string_lossy().to_string(),
            status: RunStatus::Completed,
            wrapper_pid: None,
        };
        db.create_run(&run).unwrap();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        tx.send(Frame {
            timestamp_ns: 1_000_000_000,
            stream_type: StreamType::Stdout,
            payload: b"\x1b[32mINFO\x1b[0m: server started on port 3000\n".to_vec(),
        })
        .await
        .unwrap();
        tx.send(Frame {
            timestamp_ns: 2_000_000_000,
            stream_type: StreamType::Stderr,
            payload: b"\x1b[1m\x1b[31mERROR\x1b[0m: connection refused\n".to_vec(),
        })
        .await
        .unwrap();
        tx.send(Frame {
            timestamp_ns: 3_000_000_000,
            stream_type: StreamType::Stdout,
            payload: b"\x1b]0;my terminal title\x07plain line\n".to_vec(),
        })
        .await
        .unwrap();
        drop(tx);
        writer.run().await.unwrap();

        (
            db,
            "svc-ansi-001".to_string(),
            "run-ansi-001".to_string(),
            dir,
        )
    }

    #[tokio::test]
    async fn get_logs_strip_ansi_removes_escape_codes() {
        let (db, _, run_id, _dir) = setup_ansi_test_env().await;
        let params = GetLogsParams {
            id: run_id,
            stream: None,
            mode: None,
            lines: None,
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: Some(true),
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert!(
            !resp.content.contains("\x1b["),
            "content should not contain ANSI CSI sequences"
        );
        assert!(
            !resp.content.contains("\x1b]"),
            "content should not contain ANSI OSC sequences"
        );
        assert!(resp.content.contains("INFO: server started on port 3000"));
        assert!(resp.content.contains("ERROR: connection refused"));
        assert!(resp.content.contains("plain line"));
    }

    #[tokio::test]
    async fn get_logs_strip_ansi_false_preserves_codes() {
        let (db, _, run_id, _dir) = setup_ansi_test_env().await;
        let params = GetLogsParams {
            id: run_id,
            stream: None,
            mode: None,
            lines: None,
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: Some(false),
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert!(
            resp.content.contains("\x1b[32m"),
            "content should preserve ANSI codes when strip_ansi is false"
        );
    }

    #[tokio::test]
    async fn get_logs_default_strips_ansi() {
        let (db, _, run_id, _dir) = setup_ansi_test_env().await;
        let params = GetLogsParams {
            id: run_id,
            stream: None,
            mode: None,
            lines: None,
            start_time: None,
            end_time: None,
            max_bytes: None,
            strip_ansi: None,
            since: None,
        };
        let resp = get_logs(&db, params).unwrap();
        assert!(
            !resp.content.contains("\x1b["),
            "content should strip ANSI codes by default"
        );
    }

    #[tokio::test]
    async fn search_logs_strip_ansi_removes_escape_codes() {
        let (db, _, _, _dir) = setup_ansi_test_env().await;
        let params = SearchLogsParams {
            pattern: "ERROR".to_string(),
            service_id: Some("svc-ansi-001".to_string()),
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: Some(true),
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 1);
        assert!(
            !resp.matches[0].line.contains("\x1b["),
            "match line should not contain ANSI codes"
        );
        assert!(resp.matches[0].line.contains("ERROR: connection refused"));
    }

    #[tokio::test]
    async fn search_logs_strip_ansi_false_preserves_codes() {
        let (db, _, _, _dir) = setup_ansi_test_env().await;
        let params = SearchLogsParams {
            pattern: "ERROR".to_string(),
            service_id: Some("svc-ansi-001".to_string()),
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: Some(false),
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 1);
        assert!(
            resp.matches[0].line.contains("\x1b["),
            "match line should contain ANSI codes when strip_ansi is false"
        );
    }

    #[tokio::test]
    async fn search_logs_default_strips_ansi() {
        let (db, _, _, _dir) = setup_ansi_test_env().await;
        let params = SearchLogsParams {
            pattern: "ERROR".to_string(),
            service_id: Some("svc-ansi-001".to_string()),
            stream: None,
            start_time: None,
            end_time: None,
            context_lines: None,
            max_matches: None,
            strip_ansi: None,
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 1);
        assert!(
            !resp.matches[0].line.contains("\x1b["),
            "match line should strip ANSI codes by default"
        );
    }

    // ── wait_for_pattern ────────────────────────────────────────────

    /// Test helper: resolve + wait_for_pattern in one call.
    async fn test_wait_for_pattern(
        db: &Database,
        params: WaitForPatternParams,
    ) -> Result<WaitForPatternResponse> {
        let log_dir = wait_for_pattern_resolve(db, &params)?;
        wait_for_pattern(&log_dir, params).await
    }

    #[tokio::test]
    async fn wait_for_pattern_finds_existing_match() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = WaitForPatternParams {
            id: "test-web".to_string(),
            pattern: "ERROR".to_string(),
            stream: None,
            timeout: Some(2),
            poll_interval_ms: Some(100),
            strip_ansi: None,
            since: Some(0), // search full history
        };
        let resp = test_wait_for_pattern(&db, params).await.unwrap();
        assert!(resp.matched);
        assert!(!resp.timed_out);
        assert!(resp.line.unwrap().contains("connection refused"));
        assert_eq!(resp.timestamp_ns, Some(3_000_000_000));
    }

    #[tokio::test]
    async fn wait_for_pattern_times_out_when_no_match() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = WaitForPatternParams {
            id: "test-web".to_string(),
            pattern: "NEVER_APPEARS_IN_LOGS".to_string(),
            stream: None,
            timeout: Some(1),
            poll_interval_ms: Some(200),
            strip_ansi: None,
            since: Some(0), // search full history
        };
        let resp = test_wait_for_pattern(&db, params).await.unwrap();
        assert!(!resp.matched);
        assert!(resp.timed_out);
        assert!(resp.line.is_none());
        assert!(resp.timestamp_ns.is_none());
        assert!(resp.elapsed_ms >= 1000);
    }

    #[tokio::test]
    async fn wait_for_pattern_uses_alternation_regex() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = WaitForPatternParams {
            id: "test-web".to_string(),
            pattern: "server started|error".to_string(),
            stream: None,
            timeout: Some(2),
            poll_interval_ms: Some(100),
            strip_ansi: None,
            since: Some(0), // search full history
        };
        let resp = test_wait_for_pattern(&db, params).await.unwrap();
        assert!(resp.matched);
        assert!(resp.line.unwrap().contains("server started"));
    }

    #[tokio::test]
    async fn wait_for_pattern_filters_by_stream() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = WaitForPatternParams {
            id: "test-web".to_string(),
            pattern: "ERROR".to_string(),
            stream: Some(StreamFilter::Stderr),
            timeout: Some(1),
            poll_interval_ms: Some(200),
            strip_ansi: None,
            since: Some(0), // search full history — ERROR is on stdout, so stderr filter should miss it
        };
        let resp = test_wait_for_pattern(&db, params).await.unwrap();
        assert!(!resp.matched);
        assert!(resp.timed_out);
    }

    #[tokio::test]
    async fn wait_for_pattern_strips_ansi_codes() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();
        let log_dir_str = log_dir.to_string_lossy().to_string();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);
        let ansi_payload = b"\x1b[31mERROR\x1b[0m: something failed\n";
        tx.send(Frame {
            timestamp_ns: 1_000_000,
            stream_type: StreamType::Stdout,
            payload: ansi_payload.to_vec(),
        })
        .await
        .unwrap();
        drop(tx);
        writer.run().await.unwrap();

        let params = WaitForPatternParams {
            id: String::new(),
            pattern: "^ERROR: something".to_string(),
            stream: None,
            timeout: Some(2),
            poll_interval_ms: Some(100),
            strip_ansi: Some(true),
            since: Some(0), // search full history
        };
        let resp = wait_for_pattern(&log_dir_str, params).await.unwrap();
        assert!(resp.matched);
        let line = resp.line.unwrap();
        assert!(!line.contains("\x1b["));
        assert!(line.contains("ERROR: something failed"));
    }

    #[tokio::test]
    async fn wait_for_pattern_no_strip_ansi() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();
        let log_dir_str = log_dir.to_string_lossy().to_string();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);
        let ansi_payload = b"\x1b[31mERROR\x1b[0m: something failed\n";
        tx.send(Frame {
            timestamp_ns: 1_000_000,
            stream_type: StreamType::Stdout,
            payload: ansi_payload.to_vec(),
        })
        .await
        .unwrap();
        drop(tx);
        writer.run().await.unwrap();

        let params = WaitForPatternParams {
            id: String::new(),
            pattern: "^ERROR".to_string(),
            stream: None,
            timeout: Some(1),
            poll_interval_ms: Some(200),
            strip_ansi: Some(false),
            since: Some(0), // search full history — ^ERROR won't match ANSI-prefixed line
        };
        let resp = wait_for_pattern(&log_dir_str, params).await.unwrap();
        assert!(!resp.matched);
        assert!(resp.timed_out);
    }

    #[tokio::test]
    async fn wait_for_pattern_resolves_by_run_id() {
        let (db, _, run_id, _dir) = setup_test_env().await;
        let params = WaitForPatternParams {
            id: run_id,
            pattern: "retrying".to_string(),
            stream: None,
            timeout: Some(2),
            poll_interval_ms: Some(100),
            strip_ansi: None,
            since: Some(0), // search full history
        };
        let resp = test_wait_for_pattern(&db, params).await.unwrap();
        assert!(resp.matched);
        assert!(resp.line.unwrap().contains("retrying"));
    }

    #[tokio::test]
    async fn wait_for_pattern_invalid_regex() {
        let dir = TempDir::new().unwrap();
        let log_dir_str = dir.path().to_string_lossy().to_string();
        let params = WaitForPatternParams {
            id: String::new(),
            pattern: "[invalid".to_string(),
            stream: None,
            timeout: Some(1),
            poll_interval_ms: Some(100),
            strip_ansi: None,
            since: None,
        };
        let result = wait_for_pattern(&log_dir_str, params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn wait_for_pattern_unknown_id_fails() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = WaitForPatternParams {
            id: "nonexistent-service".to_string(),
            pattern: "test".to_string(),
            stream: None,
            timeout: Some(1),
            poll_interval_ms: Some(100),
            strip_ansi: None,
            since: None,
        };
        let result = wait_for_pattern_resolve(&db, &params);
        assert!(result.is_err());
    }
}
