use anyhow::Result;
use chrono::{DateTime, Utc};
use owo_colors::OwoColorize;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::Path;

use crate::cli::kill::resolve_service;
use crate::cli::ListArgs;
use crate::config::Config;
use crate::storage::logfile::log_sizes;
use crate::storage::models::RunStatus;
use crate::storage::reconcile::is_wrapper_process;
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
    run_count: usize,
    wrapper_alive: bool,
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
        "restarting" => format!("{}", status.yellow()),
        "completed" => format!("{}", status.dimmed()),
        "failed" => format!("{}", status.red()),
        "killed" => format!("{}", status.yellow()),
        _ => format!("{}", status.dimmed()),
    }
}

/// Determine wrapper-aware status for a service's latest run.
/// Returns "restarting" if the child isn't running but the wrapper PID is alive.
fn wrapper_aware_status(run: &crate::storage::models::Run) -> String {
    if run.status == RunStatus::Running {
        return "running".to_string();
    }
    if let Some(wrapper_pid) = run.wrapper_pid {
        if is_wrapper_process(wrapper_pid) {
            return "restarting".to_string();
        }
    }
    run.status.as_str().to_string()
}

/// Format a duration between two timestamps as a human-readable string.
fn format_duration(started: DateTime<Utc>, ended: Option<DateTime<Utc>>) -> String {
    let end = ended.unwrap_or_else(Utc::now);
    let dur = end.signed_duration_since(started);
    let secs = dur.num_seconds();
    if secs < 0 {
        return "0s".to_string();
    }
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
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
    crate::storage::reconcile_stale_runs(&db)?;

    // Drill-down mode: `brainlog list <id>`
    if let Some(ref target) = args.id {
        return handle_list_drilldown(&db, target, &args);
    }

    if args.group {
        return handle_list_grouped(&db, &args);
    }

    let services = if let Some(ref name) = args.name {
        db.search_services(Some(name), None, &[], None, None, None, 100)?
    } else {
        db.list_services()?
    };

    if args.json {
        return handle_list_json(&db, &services);
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
                let status = wrapper_aware_status(&run);
                let run_count = db.count_runs(&service.id)?;
                println!(
                    "Latest Run:  {} ({}) [{} total runs]",
                    &run.id[..id_prefix_len.min(run.id.len())],
                    status,
                    run_count,
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

    // Fixed-width columns: ID + STATUS(12) + RUNS(6) + LAST_RUN(id_prefix) + CREATED(20) + gaps(12)
    let fixed_cols = id_prefix_len + 12 + 6 + id_prefix_len + 20 + 12;
    let width = term_width();
    let available = width.saturating_sub(fixed_cols);
    // Split remaining space: ~40% name, ~60% command
    let name_max = (available * 2 / 5).max(10);
    let cmd_max = available.saturating_sub(name_max).max(10);

    let tty = stdout_is_tty();
    if tty {
        println!(
            "{:<iw$}  {:<nw$}  {:<12}  {:<6}  {:<rw$}  {:<20}  {}",
            "ID".bold(),
            format!("{:<nw$}", "NAME", nw = name_max).bold(),
            "STATUS".bold(),
            "RUNS".bold(),
            format!("{:<rw$}", "LAST RUN", rw = id_prefix_len).bold(),
            "CREATED".bold(),
            "COMMAND".bold(),
            iw = id_prefix_len,
            nw = name_max,
            rw = id_prefix_len,
        );
    } else {
        println!(
            "{:<iw$}  {:<nw$}  {:<12}  {:<6}  {:<rw$}  {:<20}  COMMAND",
            "ID",
            "NAME",
            "STATUS",
            "RUNS",
            "LAST RUN",
            "CREATED",
            iw = id_prefix_len,
            nw = name_max,
            rw = id_prefix_len,
        );
    }

    for service in &services {
        let name_display = service
            .name
            .as_deref()
            .unwrap_or(&service.id[..id_prefix_len]);
        let (status_raw, last_run_id) = if let Some(run) = db.get_latest_run(&service.id)? {
            let status = wrapper_aware_status(&run);
            let run_prefix = &run.id[..id_prefix_len.min(run.id.len())];
            (status, run_prefix.to_string())
        } else {
            ("no runs".to_string(), "-".to_string())
        };
        let run_count = db.count_runs(&service.id)?;
        let created = service.created_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let cmd = service.command_line.join(" ");

        let status_padded = format!("{:<12}", status_raw);
        let status_display = colour_status(&status_padded, tty);

        println!(
            "{:<iw$}  {:<nw$}  {}  {:<6}  {:<rw$}  {:<20}  {}",
            &service.id[..id_prefix_len],
            truncate(name_display, name_max),
            status_display,
            run_count,
            last_run_id,
            created,
            truncate(&cmd, cmd_max),
            iw = id_prefix_len,
            nw = name_max,
            rw = id_prefix_len,
        );
    }

    eprintln!();
    if stderr_is_tty() {
        eprintln!(
            "{}",
            "Tip: use `brainlog list <id>` to see full run history".dimmed()
        );
    } else {
        eprintln!("Tip: use `brainlog list <id>` to see full run history");
    }

    Ok(())
}

fn handle_list_json(db: &Database, services: &[crate::storage::models::Service]) -> Result<()> {
    let mut json_services = Vec::new();
    for service in services {
        let tags = db.get_tags(&service.id)?;
        let tag_json: Vec<TagJson> = tags
            .iter()
            .map(|t| TagJson {
                key: t.key.clone(),
                value: t.value.clone(),
            })
            .collect();

        let run_count = db.count_runs(&service.id)?;
        let (latest_run, ports, wrapper_alive) =
            if let Some(run) = db.get_latest_run(&service.id)? {
                let run_ports = db.get_ports(&run.id)?;
                let port_nums: Vec<u16> = run_ports.iter().map(|p| p.port).collect();
                let alive = run.wrapper_pid.map(is_wrapper_process).unwrap_or(false);
                (
                    Some(RunJson {
                        id: run.id.clone(),
                        status: run.status.as_str().to_string(),
                        started_at: run.started_at.to_rfc3339(),
                        exit_code: run.exit_code,
                    }),
                    port_nums,
                    alive,
                )
            } else {
                (None, Vec::new(), false)
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
            run_count,
            wrapper_alive,
        });
    }
    println!("{}", serde_json::to_string_pretty(&json_services)?);
    Ok(())
}

/// Drill-down mode: show all runs for a specific service.
fn handle_list_drilldown(db: &Database, target: &str, args: &ListArgs) -> Result<()> {
    let (service, _matched_by) = resolve_service(db, target)?;
    let runs = db.list_runs(&service.id)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(());
    }

    let service_name = service
        .name
        .as_deref()
        .unwrap_or(&service.id[..8.min(service.id.len())]);

    let tty = stdout_is_tty();

    if tty {
        println!(
            "Runs for '{}' ({}):\n",
            service_name.bold(),
            service.id[..12.min(service.id.len())].to_string().dimmed()
        );
    } else {
        println!(
            "Runs for '{}' ({}):\n",
            service_name,
            &service.id[..12.min(service.id.len())]
        );
    }

    if runs.is_empty() {
        println!("  No runs found.");
        return Ok(());
    }

    let run_ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    let run_prefix_len = unique_prefix_len(&run_ids, 8);

    if tty {
        println!(
            "  {:<rw$}  {:<12}  {:<7}  {:<5}  {:<20}  {}",
            "RUN ID".bold(),
            "STATUS".bold(),
            "PID".bold(),
            "EXIT".bold(),
            "STARTED".bold(),
            "DURATION".bold(),
            rw = run_prefix_len,
        );
    } else {
        println!(
            "  {:<rw$}  {:<12}  {:<7}  {:<5}  {:<20}  DURATION",
            "RUN ID",
            "STATUS",
            "PID",
            "EXIT",
            "STARTED",
            rw = run_prefix_len,
        );
    }

    for run in &runs {
        let status_raw = run.status.as_str();
        let status_padded = format!("{:<12}", status_raw);
        let status_display = colour_status(&status_padded, tty);

        let pid_display = run
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());
        let exit_display = run
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string());
        let started = run.started_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let duration = if run.status == RunStatus::Running {
            format_duration(run.started_at, None)
        } else {
            format_duration(run.started_at, run.ended_at)
        };

        println!(
            "  {:<rw$}  {}  {:<7}  {:<5}  {:<20}  {}",
            &run.id[..run_prefix_len.min(run.id.len())],
            status_display,
            pid_display,
            exit_display,
            started,
            duration,
            rw = run_prefix_len,
        );
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

        // Use wrapper-aware status: check if any service in the group has a live wrapper
        let latest_status_raw = group_wrapper_aware_status(db, group);

        let dir = shorten_home(&group.working_dir);

        if tty {
            let status_display = colour_status(&latest_status_raw, true);
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
                    wrapper_aware_status(&run)
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

/// Determine wrapper-aware status for a service group.
/// Checks the latest run across services in the group for wrapper liveness.
fn group_wrapper_aware_status(
    db: &Database,
    group: &crate::storage::models::ServiceGroup,
) -> String {
    // If the group already reports running, keep it
    if let Some(ref status) = group.latest_run_status {
        if *status == RunStatus::Running {
            return "running".to_string();
        }
    }

    // Check each service's latest run for a live wrapper
    for svc in &group.services {
        if let Ok(Some(run)) = db.get_latest_run(&svc.id) {
            if run.status == RunStatus::Running {
                return "running".to_string();
            }
            if let Some(wrapper_pid) = run.wrapper_pid {
                if is_wrapper_process(wrapper_pid) {
                    return "restarting".to_string();
                }
            }
        }
    }

    group
        .latest_run_status
        .as_ref()
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|| "no runs".to_string())
}
