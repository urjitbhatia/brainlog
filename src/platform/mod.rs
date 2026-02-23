pub mod macos;

use crate::storage::Database;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Poll for ports opened by a child process, storing them in the database.
///
/// The loop exits when `cancel` is triggered, which should happen after the
/// child process terminates. Without cancellation the task would poll forever.
pub async fn poll_ports(
    pid: u32,
    run_id: &str,
    db_path: &Path,
    interval_secs: u64,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
    // Skip the first immediate tick
    interval.tick().await;

    loop {
        // Wait for the next tick OR cancellation, whichever comes first.
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancel.cancelled() => {
                tracing::debug!("port polling cancelled for pid {pid}");
                break;
            }
        }

        let ports = detect_ports(pid).await;
        if !ports.is_empty() {
            if let Ok(db) = Database::open(db_path) {
                for port in &ports {
                    if let Err(e) = db.add_port(run_id, *port, "tcp") {
                        tracing::warn!(
                            "Failed to record detected port {port} for run {run_id}: {e}"
                        );
                    }
                }
            }
        }
    }
}

pub async fn detect_ports(pid: u32) -> Vec<u16> {
    #[cfg(target_os = "macos")]
    {
        macos::detect_ports(pid).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        Vec::new()
    }
}
