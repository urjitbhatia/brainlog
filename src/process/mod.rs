pub mod capture;
pub mod pipe;
pub mod pty;
pub mod signals;

use anyhow::Result;
use std::io::IsTerminal;
use tokio::sync::{mpsc, oneshot};

use crate::storage::models::Frame;

pub struct SpawnResult {
    pub exit_code: Option<i32>,
    pub pid: u32,
}

/// Spawn a wrapped child process. Uses PTY if stdin is a terminal, otherwise pipes.
///
/// The child's PID is sent on `pid_tx` immediately after spawning, before waiting
/// for the child to exit. This allows the caller to record the PID and start
/// background tasks (e.g. port detection) while the child is still running.
pub async fn spawn_wrapped(
    command: &[String],
    tx: mpsc::Sender<Frame>,
    pid_tx: oneshot::Sender<u32>,
) -> Result<SpawnResult> {
    let use_pty = std::io::stdin().is_terminal();

    if use_pty {
        let result = pty::spawn_pty(command, tx, pid_tx).await?;
        Ok(SpawnResult {
            exit_code: result.exit_code,
            pid: result.pid,
        })
    } else {
        let result = pipe::spawn_piped(command, tx, pid_tx).await?;
        Ok(SpawnResult {
            exit_code: result.exit_code,
            pid: result.pid,
        })
    }
}
