use anyhow::{bail, Result};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use crate::cli::RestartArgs;
use crate::config::Config;
use crate::storage::models::RunStatus;
use crate::storage::Database;

pub async fn handle_restart(args: RestartArgs) -> Result<()> {
    let config = Config::load()?;
    let db = Database::open(&config.db_path())?;

    let (service, _matched_by) = super::kill::resolve_service(&db, &args.target)?;
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

    let wrapper_pid = run.wrapper_pid.ok_or_else(|| {
        anyhow::anyhow!(
            "Service '{}' has no wrapper PID recorded (was it started with an older brainlog version?)",
            service_name
        )
    })?;

    // Send SIGUSR1 to the wrapper process to trigger restart
    let nix_pid = Pid::from_raw(wrapper_pid as i32);
    signal::kill(nix_pid, Signal::SIGUSR1).map_err(|e| {
        anyhow::anyhow!(
            "Failed to send SIGUSR1 to wrapper PID {}: {}",
            wrapper_pid,
            e
        )
    })?;

    println!(
        "Sent restart signal to '{}' (wrapper PID {})",
        service_name, wrapper_pid
    );

    Ok(())
}
