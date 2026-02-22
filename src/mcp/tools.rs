use anyhow::Result;
use regex::Regex;
use std::path::Path;

use crate::storage::logfile::{frames_to_text, LogReader};
use crate::storage::Database;

use super::types::*;

pub fn discover_services(
    db: &Database,
    params: DiscoverServicesParams,
) -> Result<DiscoverServicesResponse> {
    let limit = params.limit.unwrap_or(20);

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
            db.get_ports(&run.id)?
                .iter()
                .map(|p| p.port)
                .collect()
        } else {
            Vec::new()
        };

        let run_info = latest_run.map(|r| RunInfo {
            id: r.id,
            status: r.status.as_str().to_string(),
            started_at: r.started_at.to_rfc3339(),
            ended_at: r.ended_at.map(|t| t.to_rfc3339()),
            exit_code: r.exit_code,
            pid: r.pid,
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
        });
    }

    Ok(DiscoverServicesResponse { services: result })
}

pub fn get_logs(db: &Database, params: GetLogsParams) -> Result<GetLogsResponse> {
    let stream = params.stream.as_deref().unwrap_or("combined");
    let mode = params.mode.as_deref().unwrap_or("tail");
    let lines = params.lines.unwrap_or(100);
    let max_bytes = params.max_bytes.unwrap_or(51200);

    // Resolve ID to log dir
    let log_dir = resolve_log_dir(db, &params.id)?;
    let reader = LogReader::new(Path::new(&log_dir), stream);

    let frames = match mode {
        "head" => reader.read_head(lines)?,
        "range" => reader.read_range(params.start_time, params.end_time)?,
        _ => reader.read_tail(lines)?,
    };

    let frame_count = frames.len();
    let text = frames_to_text(&frames);
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
        stream: stream.to_string(),
    })
}

pub fn search_logs(db: &Database, params: SearchLogsParams) -> Result<SearchLogsResponse> {
    let pattern = Regex::new(&params.pattern)?;
    let stream = params.stream.as_deref().unwrap_or("combined");
    let max_matches = params.max_matches.unwrap_or(50);

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
                all_matches.push(SearchMatch {
                    service_id: service.id.clone(),
                    service_name: service.name.clone(),
                    run_id: run.id.clone(),
                    stream: m.stream_type.as_str().to_string(),
                    timestamp_ns: m.timestamp_ns,
                    line: m.line,
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

fn resolve_log_dir(db: &Database, id: &str) -> Result<String> {
    if let Some(run) = db.get_run(id)? {
        return Ok(run.log_dir);
    }
    if let Some(run) = db.get_latest_run(id)? {
        return Ok(run.log_dir);
    }
    if let Some(service) = db.find_service_by_name(id)? {
        if let Some(run) = db.get_latest_run(&service.id)? {
            return Ok(run.log_dir);
        }
    }
    anyhow::bail!("No service or run found matching '{}'", id);
}
