use anyhow::{bail, Result};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use crate::cli::KillArgs;
use crate::config::Config;
use crate::storage::models::RunStatus;
use crate::storage::Database;

pub async fn handle_kill(args: KillArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    let signal = if args.force {
        Signal::SIGKILL
    } else {
        parse_signal(&args.signal)?
    };

    let (service, _matched_by) = resolve_service(&db, &args.target)?;
    let service_name = service
        .name
        .as_deref()
        .unwrap_or(&service.id[..8.min(service.id.len())]);

    let run = db
        .get_latest_run(&service.id)?
        .ok_or_else(|| anyhow::anyhow!("Service '{}' has no runs", service_name))?;

    if run.status != RunStatus::Running {
        bail!(
            "Service '{}' is not running (status: {})",
            service_name,
            run.status.as_str()
        );
    }

    let pid = run
        .pid
        .ok_or_else(|| anyhow::anyhow!("Service '{}' has no PID recorded", service_name))?;

    let tree = collect_process_tree(pid).await;

    // Send signal to children first (reverse order: deepest children first), then parent
    let mut kill_order: Vec<u32> = tree.iter().copied().filter(|&p| p != pid).collect();
    kill_order.reverse();
    kill_order.push(pid);

    let mut success_count = 0;
    let mut fail_count = 0;

    for target_pid in &kill_order {
        let nix_pid = Pid::from_raw(*target_pid as i32);
        match signal::kill(nix_pid, signal) {
            Ok(()) => {
                success_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "brainlog: failed to send {} to PID {}: {}",
                    signal, target_pid, e
                );
                fail_count += 1;
            }
        }
    }

    let signal_name = format!("{}", signal);
    if kill_order.len() == 1 {
        println!("Sent {} to '{}' (PID {})", signal_name, service_name, pid);
    } else {
        println!(
            "Sent {} to '{}' (PID {} + {} child processes)",
            signal_name,
            service_name,
            pid,
            kill_order.len() - 1
        );
    }

    if fail_count > 0 {
        bail!(
            "Failed to signal {} of {} processes",
            fail_count,
            success_count + fail_count
        );
    }

    Ok(())
}

fn parse_signal(s: &str) -> Result<Signal> {
    // Try parsing as a signal name (with or without SIG prefix)
    let upper = s.to_uppercase();
    let name = if upper.starts_with("SIG") {
        upper.as_str()
    } else {
        // We'll match without the SIG prefix below
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

    // Try parsing as a numeric signal
    if let Ok(num) = s.parse::<i32>() {
        return Signal::try_from(num)
            .map_err(|_| anyhow::anyhow!("Invalid signal number: {}", num));
    }

    bail!(
        "Unknown signal '{}'. Supported: TERM, KILL, INT, HUP, USR1, USR2, QUIT, or a number",
        s
    )
}

fn resolve_service(
    db: &Database,
    target: &str,
) -> Result<(crate::storage::models::Service, &'static str)> {
    // Try exact service ID match
    if let Some(service) = db.get_service(target)? {
        return Ok((service, "id"));
    }

    // Try exact service name match
    if let Some(service) = db.find_service_by_name(target)? {
        return Ok((service, "name"));
    }

    // Try ID prefix match
    let services = db.list_services()?;
    let prefix_matches: Vec<_> = services
        .into_iter()
        .filter(|s| s.id.starts_with(target))
        .collect();

    match prefix_matches.len() {
        0 => bail!("No service found matching '{}'", target),
        1 => Ok((prefix_matches.into_iter().next().unwrap(), "id_prefix")),
        _ => {
            eprintln!("Multiple services match '{}':", target);
            for svc in &prefix_matches {
                let name = svc.name.as_deref().unwrap_or("(unnamed)");
                eprintln!("  {} {}", &svc.id[..8.min(svc.id.len())], name);
            }
            bail!("Please be more specific")
        }
    }
}

async fn collect_process_tree(root_pid: u32) -> Vec<u32> {
    let mut tree = vec![root_pid];
    let mut to_visit = vec![root_pid];

    while let Some(parent) = to_visit.pop() {
        if let Ok(children) = get_child_pids(parent).await {
            for child in children {
                if !tree.contains(&child) {
                    tree.push(child);
                    to_visit.push(child);
                }
            }
        }
    }

    tree
}

async fn get_child_pids(parent_pid: u32) -> Result<Vec<u32>> {
    let output = tokio::process::Command::new("pgrep")
        .args(["-P", &parent_pid.to_string()])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pids: Vec<u32> = stdout
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect();
    Ok(pids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_signal_names() {
        assert_eq!(parse_signal("TERM").unwrap(), Signal::SIGTERM);
        assert_eq!(parse_signal("KILL").unwrap(), Signal::SIGKILL);
        assert_eq!(parse_signal("INT").unwrap(), Signal::SIGINT);
        assert_eq!(parse_signal("HUP").unwrap(), Signal::SIGHUP);
        assert_eq!(parse_signal("USR1").unwrap(), Signal::SIGUSR1);
        assert_eq!(parse_signal("USR2").unwrap(), Signal::SIGUSR2);
        assert_eq!(parse_signal("QUIT").unwrap(), Signal::SIGQUIT);
    }

    #[test]
    fn parse_signal_with_sig_prefix() {
        assert_eq!(parse_signal("SIGTERM").unwrap(), Signal::SIGTERM);
        assert_eq!(parse_signal("SIGKILL").unwrap(), Signal::SIGKILL);
        assert_eq!(parse_signal("SIGINT").unwrap(), Signal::SIGINT);
    }

    #[test]
    fn parse_signal_case_insensitive() {
        assert_eq!(parse_signal("term").unwrap(), Signal::SIGTERM);
        assert_eq!(parse_signal("Kill").unwrap(), Signal::SIGKILL);
        assert_eq!(parse_signal("sigint").unwrap(), Signal::SIGINT);
    }

    #[test]
    fn parse_signal_numeric() {
        assert_eq!(parse_signal("15").unwrap(), Signal::SIGTERM);
        assert_eq!(parse_signal("9").unwrap(), Signal::SIGKILL);
        assert_eq!(parse_signal("2").unwrap(), Signal::SIGINT);
    }

    #[test]
    fn parse_signal_invalid() {
        assert!(parse_signal("BOGUS").is_err());
        assert!(parse_signal("999").is_err());
    }

    #[test]
    fn resolve_service_not_found() {
        let db = Database::open_in_memory().unwrap();
        let result = resolve_service(&db, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No service found"));
    }

    #[test]
    fn resolve_service_by_exact_id() {
        let db = Database::open_in_memory().unwrap();
        let svc = crate::storage::models::Service {
            id: "svc-kill-001".to_string(),
            name: Some("my-app".to_string()),
            description: None,
            executable: "/usr/bin/test".to_string(),
            command_line: vec!["test".to_string()],
            working_dir: "/tmp".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            enrichment_status: crate::storage::models::EnrichmentStatus::Pending,
        };
        db.create_service(&svc).unwrap();

        let (found, matched_by) = resolve_service(&db, "svc-kill-001").unwrap();
        assert_eq!(found.id, "svc-kill-001");
        assert_eq!(matched_by, "id");
    }

    #[test]
    fn resolve_service_by_name() {
        let db = Database::open_in_memory().unwrap();
        let svc = crate::storage::models::Service {
            id: "svc-kill-002".to_string(),
            name: Some("web-server".to_string()),
            description: None,
            executable: "/usr/bin/test".to_string(),
            command_line: vec!["test".to_string()],
            working_dir: "/tmp".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            enrichment_status: crate::storage::models::EnrichmentStatus::Pending,
        };
        db.create_service(&svc).unwrap();

        let (found, matched_by) = resolve_service(&db, "web-server").unwrap();
        assert_eq!(found.id, "svc-kill-002");
        assert_eq!(matched_by, "name");
    }

    #[test]
    fn resolve_service_by_id_prefix() {
        let db = Database::open_in_memory().unwrap();
        let svc = crate::storage::models::Service {
            id: "svc-kill-prefix-unique".to_string(),
            name: Some("prefix-app".to_string()),
            description: None,
            executable: "/usr/bin/test".to_string(),
            command_line: vec!["test".to_string()],
            working_dir: "/tmp".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            enrichment_status: crate::storage::models::EnrichmentStatus::Pending,
        };
        db.create_service(&svc).unwrap();

        let (found, matched_by) = resolve_service(&db, "svc-kill-prefix").unwrap();
        assert_eq!(found.id, "svc-kill-prefix-unique");
        assert_eq!(matched_by, "id_prefix");
    }

    #[test]
    fn resolve_service_ambiguous_prefix() {
        let db = Database::open_in_memory().unwrap();
        for i in 1..=3 {
            let svc = crate::storage::models::Service {
                id: format!("svc-ambig-{}", i),
                name: Some(format!("app-{}", i)),
                description: None,
                executable: "/usr/bin/test".to_string(),
                command_line: vec!["test".to_string()],
                working_dir: "/tmp".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                enrichment_status: crate::storage::models::EnrichmentStatus::Pending,
            };
            db.create_service(&svc).unwrap();
        }

        let result = resolve_service(&db, "svc-ambig");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Please be more specific"));
    }
}
