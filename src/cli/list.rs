use anyhow::Result;
use owo_colors::OwoColorize;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::Path;

use crate::cli::ListArgs;
use crate::config::Config;
use crate::storage::logfile::log_sizes;
use crate::storage::Database;

/// JSON output struct for a service in the list command.
#[derive(Serialize)]
struct ServiceJson {
    id: String,
    name: Option<String>,
    description: Option<String>,
    executable: String,
    command_line: Vec<String>,
    working_dir: String,
    created_at: String,
    tags: Vec<TagJson>,
    latest_run: Option<RunJson>,
    ports: Vec<u16>,
}

#[derive(Serialize)]
struct TagJson {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct RunJson {
    id: String,
    status: String,
    started_at: String,
    exit_code: Option<i32>,
}

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

/// Calculate the minimum prefix length (at least `min`) so that every ID in
/// `ids` is uniquely identified by its prefix.  If there are collisions at
/// `min` characters, the length is increased one character at a time until
/// all prefixes are distinct (up to the full ID length).
fn unique_prefix_len(ids: &[&str], min: usize) -> usize {
    use std::collections::HashSet;
    if ids.is_empty() {
        return min;
    }
    let max_possible = ids.iter().map(|id| id.len()).min().unwrap_or(min);
    let mut len = min.min(max_possible);
    loop {
        let mut seen = HashSet::with_capacity(ids.len());
        let unique = ids.iter().all(|id| {
            let prefix = &id[..len.min(id.len())];
            seen.insert(prefix)
        });
        if unique || len >= max_possible {
            break;
        }
        len += 1;
    }
    len
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

    if args.json {
        let mut json_services = Vec::new();
        for service in &services {
            let tags = db.get_tags(&service.id)?;
            let tag_json: Vec<TagJson> = tags
                .iter()
                .map(|t| TagJson {
                    key: t.key.clone(),
                    value: t.value.clone(),
                })
                .collect();

            let (latest_run, ports) = if let Some(run) = db.get_latest_run(&service.id)? {
                let run_ports = db.get_ports(&run.id)?;
                let port_nums: Vec<u16> = run_ports.iter().map(|p| p.port).collect();
                (
                    Some(RunJson {
                        id: run.id.clone(),
                        status: run.status.as_str().to_string(),
                        started_at: run.started_at.to_rfc3339(),
                        exit_code: run.exit_code,
                    }),
                    port_nums,
                )
            } else {
                (None, Vec::new())
            };

            json_services.push(ServiceJson {
                id: service.id.clone(),
                name: service.name.clone(),
                description: service.description.clone(),
                executable: service.executable.clone(),
                command_line: service.command_line.clone(),
                working_dir: service.working_dir.clone(),
                created_at: service.created_at.to_rfc3339(),
                tags: tag_json,
                latest_run,
                ports,
            });
        }
        println!("{}", serde_json::to_string_pretty(&json_services)?);
        return Ok(());
    }

    if services.is_empty() {
        println!("No services found.");
        return Ok(());
    }

    let svc_ids: Vec<&str> = services.iter().map(|s| s.id.as_str()).collect();
    let id_prefix_len = unique_prefix_len(&svc_ids, 8);

    if args.verbose {
        for service in &services {
            let name_display = service
                .name
                .as_deref()
                .unwrap_or(&service.id[..id_prefix_len]);
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
                println!(
                    "Latest Run:  {} ({})",
                    &run.id[..id_prefix_len.min(run.id.len())],
                    run.status.as_str()
                );
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

    // Fixed-width columns: ID(dynamic) + STATUS(12) + CREATED(20) + gaps(8)
    let fixed_cols = id_prefix_len + 12 + 20 + 8;
    let width = term_width();
    let available = width.saturating_sub(fixed_cols);
    // Split remaining space: ~40% name, ~60% command
    let name_max = (available * 2 / 5).max(10);
    let cmd_max = available.saturating_sub(name_max).max(10);

    let tty = stdout_is_tty();
    if tty {
        println!(
            "{:<iw$}  {:<nw$}  {:<12}  {:<20}  {}",
            "ID".bold(),
            format!("{:<nw$}", "NAME", nw = name_max).bold(),
            "STATUS".bold(),
            "CREATED".bold(),
            "COMMAND".bold(),
            iw = id_prefix_len,
            nw = name_max
        );
    } else {
        println!(
            "{:<iw$}  {:<nw$}  {:<12}  {:<20}  COMMAND",
            "ID",
            "NAME",
            "STATUS",
            "CREATED",
            iw = id_prefix_len,
            nw = name_max
        );
    }

    for service in &services {
        let name_display = service
            .name
            .as_deref()
            .unwrap_or(&service.id[..id_prefix_len]);
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
            "{:<iw$}  {:<nw$}  {}  {:<20}  {}",
            &service.id[..id_prefix_len],
            truncate(name_display, name_max),
            status_display,
            created,
            truncate(&cmd, cmd_max),
            iw = id_prefix_len,
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

    if args.json {
        println!("{}", serde_json::to_string_pretty(&groups)?);
        return Ok(());
    }

    if groups.is_empty() {
        println!("No services found.");
        return Ok(());
    }

    // Compute dynamic ID prefix length across all services in all groups.
    let all_ids: Vec<&str> = groups
        .iter()
        .flat_map(|g| g.services.iter().map(|s| s.id.as_str()))
        .collect();
    let id_prefix_len = unique_prefix_len(&all_ids, 8);

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
                    "    {:<iw$} {:<20} {} {}",
                    &svc.id[..id_prefix_len],
                    name,
                    status_display,
                    svc.command_line.join(" "),
                    iw = id_prefix_len
                );
            }
        }

        println!();
    }

    Ok(())
}
