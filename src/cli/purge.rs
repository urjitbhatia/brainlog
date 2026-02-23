use anyhow::{bail, Result};
use chrono::Utc;
use std::io::{self, Write};
use std::time::Duration;

use crate::cli::PurgeArgs;
use crate::config::Config;
use crate::storage::Database;

/// Parse a duration string like "10h", "30m", "5d", "3600s" into a `Duration`.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Duration string is empty");
    }

    let (num_str, suffix) = match s.bytes().rposition(|b| b.is_ascii_digit()) {
        Some(pos) => {
            let num_part = &s[..=pos];
            let suffix_part = &s[pos + 1..];
            (num_part, suffix_part)
        }
        None => bail!("Invalid duration '{}': no numeric part found", s),
    };

    let value: u64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid duration '{}': '{}' is not a number", s, num_str))?;

    if value == 0 {
        bail!("Invalid duration '{}': value must be greater than 0", s);
    }

    let seconds = match suffix {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        "d" => value * 86400,
        other => bail!(
            "Invalid duration '{}': unknown suffix '{}' (expected s, m, h, or d)",
            s,
            other
        ),
    };

    Ok(Duration::from_secs(seconds))
}

pub async fn handle_purge(args: PurgeArgs) -> Result<()> {
    let duration = parse_duration(&args.before)?;
    let cutoff = Utc::now() - chrono::Duration::from_std(duration)?;

    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    let candidates = db.find_purgeable_services(&cutoff)?;

    if candidates.is_empty() {
        println!("No services found older than {}.", args.before);
        return Ok(());
    }

    // Display what will be purged
    println!(
        "Found {} service(s) to purge (older than {}):",
        candidates.len(),
        args.before
    );
    println!();
    for candidate in &candidates {
        let display_name = candidate
            .name
            .as_deref()
            .unwrap_or(&candidate.service_id[..8.min(candidate.service_id.len())]);
        println!(
            "  {} ({})",
            display_name,
            &candidate.service_id[..8.min(candidate.service_id.len())]
        );
    }
    println!();

    if args.dry_run {
        println!("Dry run: no changes made.");
        return Ok(());
    }

    // Confirm unless --force
    if !args.force {
        print!("Continue? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();
        if input != "y" && input != "yes" {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Perform the purge
    let mut total_services = 0;
    let mut total_runs = 0;

    for candidate in &candidates {
        // Collect log dirs before deleting DB records
        let log_dirs = db.get_run_log_dirs(&candidate.service_id)?;

        // Delete from DB
        let runs_deleted = db.delete_service_cascade(&candidate.service_id)?;

        // Delete log directories from disk
        for log_dir in &log_dirs {
            let path = std::path::Path::new(log_dir);
            if path.exists() {
                if let Err(e) = std::fs::remove_dir_all(path) {
                    eprintln!("Warning: failed to remove log directory {}: {}", log_dir, e);
                }
            }
        }

        total_services += 1;
        total_runs += runs_deleted;
    }

    println!(
        "Purged {} service(s) and {} run(s).",
        total_services, total_runs
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_seconds() {
        let d = parse_duration("3600s").unwrap();
        assert_eq!(d, Duration::from_secs(3600));
    }

    #[test]
    fn parse_minutes() {
        let d = parse_duration("30m").unwrap();
        assert_eq!(d, Duration::from_secs(30 * 60));
    }

    #[test]
    fn parse_hours() {
        let d = parse_duration("10h").unwrap();
        assert_eq!(d, Duration::from_secs(10 * 3600));
    }

    #[test]
    fn parse_days() {
        let d = parse_duration("5d").unwrap();
        assert_eq!(d, Duration::from_secs(5 * 86400));
    }

    #[test]
    fn parse_single_unit() {
        let d = parse_duration("1s").unwrap();
        assert_eq!(d, Duration::from_secs(1));
    }

    #[test]
    fn parse_with_whitespace() {
        let d = parse_duration("  10h  ").unwrap();
        assert_eq!(d, Duration::from_secs(10 * 3600));
    }

    #[test]
    fn parse_empty_string() {
        let err = parse_duration("").unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn parse_no_suffix() {
        let err = parse_duration("100").unwrap_err();
        assert!(err.to_string().contains("unknown suffix"));
    }

    #[test]
    fn parse_no_number() {
        let err = parse_duration("h").unwrap_err();
        assert!(err.to_string().contains("no numeric part"));
    }

    #[test]
    fn parse_invalid_suffix() {
        let err = parse_duration("10x").unwrap_err();
        assert!(err.to_string().contains("unknown suffix"));
    }

    #[test]
    fn parse_zero_value() {
        let err = parse_duration("0h").unwrap_err();
        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn parse_non_numeric() {
        let err = parse_duration("abch").unwrap_err();
        assert!(err.to_string().contains("no numeric part"));
    }

    #[test]
    fn parse_mixed_non_numeric() {
        let err = parse_duration("12a3h").unwrap_err();
        assert!(err.to_string().contains("not a number"));
    }
}
