use anyhow::Result;
use regex::Regex;
use serde::Serialize;
use std::path::Path;

use crate::cli::SearchArgs;
use crate::config::Config;
use crate::storage::logfile::LogReader;
use crate::storage::Database;

/// JSON output for the search command.
#[derive(Serialize)]
struct SearchResultJson {
    metadata_matches: Vec<crate::storage::db::ServiceMetadataMatch>,
    log_matches: Vec<crate::storage::logfile::LogMatch>,
    total_metadata_matches: usize,
    total_log_matches: usize,
}

pub async fn handle_search(args: SearchArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;
    crate::storage::reconcile_stale_runs(&db)?;
    let pattern = Regex::new(&args.pattern)?;

    // --- Metadata search (unless --logs-only) ---
    let mut metadata_matches = Vec::new();
    if !args.logs_only {
        let all_matches = db.search_services_by_pattern(&pattern)?;
        // If a service filter is specified, narrow down metadata matches too
        if let Some(ref service_filter) = args.service {
            for m in all_matches {
                let name_match = m.service.name.as_ref().is_some_and(|n| n == service_filter);
                let id_match = m.service.id.starts_with(service_filter);
                if name_match || id_match {
                    metadata_matches.push(m);
                }
            }
        } else {
            metadata_matches = all_matches;
        }
    }

    // --- Log content search ---
    let services = if let Some(ref service_filter) = args.service {
        if let Some(s) = db.find_service_by_name(service_filter)? {
            vec![s]
        } else {
            db.list_services()?
                .into_iter()
                .filter(|s| s.id.starts_with(service_filter))
                .collect()
        }
    } else {
        db.list_services()?
    };

    let mut total_log_matches = 0;
    let mut all_log_matches = Vec::new();

    for service in &services {
        let runs = db.list_runs(&service.id)?;
        for run in &runs {
            let reader = LogReader::new(Path::new(&run.log_dir), args.stream);
            let remaining = args.max_matches.saturating_sub(total_log_matches);
            if remaining == 0 {
                break;
            }
            let matches = reader.search(&pattern, remaining)?;
            total_log_matches += matches.len();
            all_log_matches.extend(matches);
        }
    }

    if args.json {
        let result = SearchResultJson {
            total_metadata_matches: metadata_matches.len(),
            total_log_matches,
            metadata_matches,
            log_matches: all_log_matches,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    // --- Text output (existing behaviour) ---

    // Print metadata section if we have metadata matches
    let has_metadata = !metadata_matches.is_empty();
    if !args.logs_only && has_metadata {
        println!("Services matching '{}':", args.pattern);
        for m in &metadata_matches {
            let name_display = m.service.name.as_deref().unwrap_or(&m.service.id[..8]);
            let matched_summary = m.matched_fields.join(", ");
            println!(
                "  {}  {}  ({})  [{}]",
                &m.service.id[..8],
                name_display,
                m.status,
                matched_summary,
            );
        }
        println!();
    }

    // Build text output for log matches
    let mut log_output = Vec::new();
    for service in &services {
        let runs = db.list_runs(&service.id)?;
        for run in &runs {
            let reader = LogReader::new(Path::new(&run.log_dir), args.stream);
            let remaining = args.max_matches.saturating_sub(log_output.len());
            if remaining == 0 {
                break;
            }
            let matches = reader.search(&pattern, remaining)?;
            for m in &matches {
                let service_name = service.name.as_deref().unwrap_or(&service.id[..8]);
                let ts_secs = m.timestamp_ns / 1_000_000_000;
                let dt = chrono::DateTime::from_timestamp(ts_secs as i64, 0).unwrap_or_default();
                log_output.push(format!(
                    "[{}] [{}] [{}] {}",
                    service_name,
                    m.stream_type.as_str(),
                    dt.format("%H:%M:%S UTC"),
                    m.line
                ));
            }
        }
    }

    // Print log section
    if !args.logs_only && has_metadata {
        // We showed the metadata header, now show log section header
        if total_log_matches > 0 {
            println!("Log matches:");
            for line in &log_output {
                println!("  {}", line);
            }
            println!("\n{} log match(es) found.", total_log_matches);
        } else {
            println!("Log matches:");
            println!("  No log matches found.");
        }
    } else if total_log_matches > 0 {
        // No metadata section (either --logs-only or no metadata matches)
        for line in &log_output {
            println!("{}", line);
        }
        println!("\n{} match(es) found.", total_log_matches);
    } else if has_metadata {
        // Had metadata matches but no log matches, and we already printed metadata
        // This case is handled above
    } else {
        println!("No matches found.");
    }

    Ok(())
}
