use anyhow::Result;
use regex::Regex;
use std::path::Path;

use crate::cli::SearchArgs;
use crate::config::Config;
use crate::storage::logfile::LogReader;
use crate::storage::Database;

pub async fn handle_search(args: SearchArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;
    let pattern = Regex::new(&args.pattern)?;

    let services = if let Some(ref service_filter) = args.service {
        // Try by name first, then by ID prefix
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

    let mut total_matches = 0;

    for service in &services {
        let runs = db.list_runs(&service.id)?;
        for run in &runs {
            let reader = LogReader::new(Path::new(&run.log_dir), &args.stream);
            let remaining = args.max_matches.saturating_sub(total_matches);
            if remaining == 0 {
                break;
            }
            let matches = reader.search(&pattern, remaining)?;
            for m in &matches {
                let service_name = service.name.as_deref().unwrap_or(&service.id[..8]);
                let ts_secs = m.timestamp_ns / 1_000_000_000;
                let dt = chrono::DateTime::from_timestamp(ts_secs as i64, 0)
                    .unwrap_or_default();
                println!(
                    "[{}] [{}] [{}] {}",
                    service_name,
                    m.stream_type.as_str(),
                    dt.format("%H:%M:%S"),
                    m.line
                );
                total_matches += 1;
            }
        }
    }

    if total_matches == 0 {
        println!("No matches found.");
    } else {
        println!("\n{} match(es) found.", total_matches);
    }

    Ok(())
}
