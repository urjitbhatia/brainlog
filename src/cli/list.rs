use anyhow::Result;
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::path::Path;

use crate::cli::ListArgs;
use crate::config::Config;
use crate::storage::logfile::log_sizes;
use crate::storage::Database;

/// Whether stdout is a terminal (cached once per call).
fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Whether stderr is a terminal (cached once per call).
fn stderr_is_tty() -> bool {
    std::io::stderr().is_terminal()
}

/// Colour a status string based on its value, only when outputting to a TTY.
/// The input may be padded (e.g. "running     "); we match on the trimmed value
/// but colour the full (padded) string to preserve column alignment.
fn colour_status(status: &str, is_tty: bool) -> String {
    if !is_tty {
        return status.to_string();
    }
    match status.trim() {
        "running" => format!("{}", status.green()),
        "completed" => format!("{}", status.yellow()),
        "failed" => format!("{}", status.red()),
        _ => format!("{}", status.dimmed()),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Get terminal width, defaulting to 120 if unavailable.
fn term_width() -> usize {
    // Try the COLUMNS env var first, then fall back to a reasonable default
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
}

/// Truncate a string to fit within `max_len`, appending `..` if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if max_len < 3 {
        return s.chars().take(max_len).collect();
    }
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}..", &s[..max_len - 2])
    }
}

pub async fn handle_list(args: ListArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    if args.group {
        return handle_list_grouped(&db, &args);
    }

    let services = if let Some(ref name) = args.name {
        db.search_services(Some(name), None, &[], None, None, None, 100)?
    } else {
        db.list_services()?
    };

    if services.is_empty() {
        println!("No services found.");
        return Ok(());
    }

    if args.verbose {
        for service in &services {
            let name_display = service.name.as_deref().unwrap_or(&service.id[..8]);
            let desc_display = service.description.as_deref().unwrap_or("(no description)");

            println!("ID:          {}", service.id);
            println!("Name:        {}", name_display);
            println!("Description: {}", desc_display);
            println!("Executable:  {}", service.executable);
            println!("Command:     {}", service.command_line.join(" "));
            println!("Working Dir: {}", service.working_dir);
            println!("Created:     {}", service.created_at);
            println!("Updated:     {}", service.updated_at);

            let tags = db.get_tags(&service.id)?;
            if !tags.is_empty() {
                let tag_strs: Vec<String> = tags
                    .iter()
                    .map(|t| format!("{}:{}", t.key, t.value))
                    .collect();
                println!("Tags:        {}", tag_strs.join(", "));
            }

            if let Some(run) = db.get_latest_run(&service.id)? {
                println!("Latest Run:  {} ({})", &run.id[..8], run.status.as_str());
                println!("Started At:  {}", run.started_at);
                if let Some(exit_code) = run.exit_code {
                    println!("Exit Code:   {}", exit_code);
                }
                let ports = db.get_ports(&run.id)?;
                if !ports.is_empty() {
                    let port_strs: Vec<String> = ports.iter().map(|p| p.port.to_string()).collect();
                    println!("Ports:       {}", port_strs.join(", "));
                }
                let (stdout_sz, stderr_sz, stdin_sz, combined_sz) =
                    log_sizes(Path::new(&run.log_dir));
                println!(
                    "Log Sizes:   stdout={} stderr={} stdin={} combined={}",
                    format_bytes(stdout_sz),
                    format_bytes(stderr_sz),
                    format_bytes(stdin_sz),
                    format_bytes(combined_sz),
                );
            }
            println!("---");
        }
        return Ok(());
    }

    // Fixed-width columns: ID(8) + STATUS(12) + CREATED(20) + gaps(8) = 48
    let fixed_cols = 48;
    let width = term_width();
    let available = width.saturating_sub(fixed_cols);
    // Split remaining space: ~40% name, ~60% command
    let name_max = (available * 2 / 5).max(10);
    let cmd_max = available.saturating_sub(name_max).max(10);

    let tty = stdout_is_tty();
    if tty {
        println!(
            "{:<8}  {:<nw$}  {:<12}  {:<20}  {}",
            "ID".bold(),
            format!("{:<nw$}", "NAME", nw = name_max).bold(),
            "STATUS".bold(),
            "CREATED".bold(),
            "COMMAND".bold(),
            nw = name_max
        );
    } else {
        println!(
            "{:<8}  {:<nw$}  {:<12}  {:<20}  COMMAND",
            "ID",
            "NAME",
            "STATUS",
            "CREATED",
            nw = name_max
        );
    }

    for service in &services {
        let name_display = service.name.as_deref().unwrap_or(&service.id[..8]);
        let status_raw = if let Some(run) = db.get_latest_run(&service.id)? {
            run.status.as_str().to_string()
        } else {
            "no runs".to_string()
        };
        let created = service.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let cmd = service.command_line.join(" ");

        // The coloured status string may contain ANSI escapes, so we pad the raw
        // string to the column width first, then colourise that padded string.
        let status_padded = format!("{:<12}", status_raw);
        let status_display = colour_status(&status_padded, tty);

        println!(
            "{:<8}  {:<nw$}  {}  {:<20}  {}",
            &service.id[..8],
            truncate(name_display, name_max),
            status_display,
            created,
            truncate(&cmd, cmd_max),
            nw = name_max
        );
    }

    eprintln!();
    if stderr_is_tty() {
        eprintln!(
            "{}",
            "Tip: resume a service with: brainlog --resume <name> <command>".dimmed()
        );
    } else {
        eprintln!("Tip: resume a service with: brainlog --resume <name> <command>");
    }

    Ok(())
}

fn shorten_home(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = path.strip_prefix(&home) {
            return format!("~{}", rest);
        }
    }
    path.to_string()
}

fn handle_list_grouped(db: &Database, args: &ListArgs) -> Result<()> {
    let tty = stdout_is_tty();

    let groups = db.list_services_grouped()?;

    let groups: Vec<_> = if let Some(ref name_filter) = args.name {
        let needle = name_filter.to_lowercase();
        groups
            .into_iter()
            .filter(|g| {
                g.services.iter().any(|s| {
                    s.name
                        .as_deref()
                        .map(|n| n.to_lowercase().contains(&needle))
                        .unwrap_or(false)
                        || s.executable.to_lowercase().contains(&needle)
                })
            })
            .collect()
    } else {
        groups
    };

    if groups.is_empty() {
        println!("No services found.");
        return Ok(());
    }

    for group in &groups {
        let latest_ts = group
            .latest_run_at
            .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "never".to_string());

        let latest_status_raw = group
            .latest_run_status
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("no runs");

        let dir = shorten_home(&group.working_dir);

        if tty {
            let status_display = colour_status(latest_status_raw, true);
            println!(
                "[ {} ] in {}  ({} runs, latest: {} {})",
                group.executable.bold(),
                dir.dimmed(),
                group.run_count,
                status_display,
                latest_ts
            );
        } else {
            println!(
                "[ {} ] in {}  ({} runs, latest: {} {})",
                group.executable, dir, group.run_count, latest_status_raw, latest_ts
            );
        }

        let mut commands: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for svc in &group.services {
            commands.insert(svc.command_line.join(" "));
        }
        for cmd in &commands {
            println!("  $ {}", cmd);
        }

        if args.verbose {
            for svc in &group.services {
                let name = svc.name.as_deref().unwrap_or("-");
                let status_raw = if let Some(run) = db.get_latest_run(&svc.id)? {
                    run.status.as_str().to_string()
                } else {
                    "no runs".to_string()
                };
                let status_padded = format!("{:<12}", status_raw);
                let status_display = colour_status(&status_padded, tty);
                println!(
                    "    {} {:<20} {} {}",
                    &svc.id[..8],
                    name,
                    status_display,
                    svc.command_line.join(" ")
                );
            }
        }

        println!();
    }

    Ok(())
}
