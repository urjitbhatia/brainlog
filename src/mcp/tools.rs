use anyhow::Result;
use regex::Regex;
use std::path::Path;

use crate::storage::logfile::{frames_to_text, LogReader};
use crate::storage::models::LogMode;
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
            db.get_ports(&run.id)?.iter().map(|p| p.port).collect()
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
    let stream = params.stream.unwrap_or_default();
    let mode = params.mode.unwrap_or_default();
    let lines = params.lines.unwrap_or(100);
    let max_bytes = params.max_bytes.unwrap_or(51200);

    // Resolve ID to log dir
    let log_dir = resolve_log_dir(db, &params.id)?;
    let reader = LogReader::new(Path::new(&log_dir), stream);

    let frames = match mode {
        LogMode::Head => reader.read_head(lines)?,
        LogMode::Range => reader.read_range(params.start_time, params.end_time)?,
        LogMode::Tail => reader.read_tail(lines)?,
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
        stream: stream.as_str().to_string(),
    })
}

pub fn search_logs(db: &Database, params: SearchLogsParams) -> Result<SearchLogsResponse> {
    let pattern = Regex::new(&params.pattern)?;
    let stream = params.stream.unwrap_or_default();
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

    // ── discover_services ────────────────────────────────────────────

    #[tokio::test]
    async fn discover_all_services() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = DiscoverServicesParams {
            name: None,
            tags: None,
            port: None,
            executable: None,
            status: None,
            query: None,
            limit: None,
        };
        let resp = discover_services(&db, params).unwrap();
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
    async fn discover_filter_by_name() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = DiscoverServicesParams {
            name: Some("web".to_string()),
            tags: None,
            port: None,
            executable: None,
            status: None,
            query: None,
            limit: None,
        };
        let resp = discover_services(&db, params).unwrap();
        assert_eq!(resp.services.len(), 1);

        // Non-matching name
        let params = DiscoverServicesParams {
            name: Some("nonexistent".to_string()),
            tags: None,
            port: None,
            executable: None,
            status: None,
            query: None,
            limit: None,
        };
        let resp = discover_services(&db, params).unwrap();
        assert_eq!(resp.services.len(), 0);
    }

    #[tokio::test]
    async fn discover_filter_by_tag() {
        let (db, _, _, _dir) = setup_test_env().await;
        let params = DiscoverServicesParams {
            name: None,
            tags: Some(vec!["env:dev".to_string()]),
            port: None,
            executable: None,
            status: None,
            query: None,
            limit: None,
        };
        let resp = discover_services(&db, params).unwrap();
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
        };
        let resp = get_logs(&db, params).unwrap();
        assert_eq!(resp.stream, "stderr");
        assert_eq!(resp.frame_count, 1);
        assert!(resp.content.contains("deprecated API"));
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
        };
        let resp = search_logs(&db, params).unwrap();
        assert_eq!(resp.total_matches, 0);
    }

    // ── resolve_log_dir ──────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_by_run_id() {
        let (db, _, run_id, dir) = setup_test_env().await;
        let log_dir = resolve_log_dir(&db, &run_id).unwrap();
        assert_eq!(log_dir, dir.path().to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_by_service_name() {
        let (db, _, _, dir) = setup_test_env().await;
        let log_dir = resolve_log_dir(&db, "test-web").unwrap();
        assert_eq!(log_dir, dir.path().to_string_lossy());
    }

    #[tokio::test]
    async fn resolve_unknown_id_fails() {
        let (db, _, _, _dir) = setup_test_env().await;
        assert!(resolve_log_dir(&db, "no-such-thing").is_err());
    }
}
