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

/// PTY capture requires being the foreground job on the controlling terminal:
/// a background process that touches termios is stopped by SIGTTOU, freezing
/// the wrapper before it can record the child PID or exit status. Backgrounded
/// invocations (`brainlog cmd &`) therefore capture through pipes even when
/// stdin is a terminal.
fn stdin_is_foreground_tty() -> bool {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return false;
    }
    matches!(nix::unistd::tcgetpgrp(&stdin), Ok(fg) if fg == nix::unistd::getpgrp())
}

/// Spawn a wrapped child process. Uses PTY if stdin is a foreground terminal,
/// otherwise pipes.
///
/// The child's PID is sent on `pid_tx` immediately after spawning, before waiting
/// for the child to exit. This allows the caller to record the PID and start
/// background tasks (e.g. port detection) while the child is still running.
pub async fn spawn_wrapped(
    command: &[String],
    tx: mpsc::Sender<Frame>,
    pid_tx: oneshot::Sender<u32>,
) -> Result<SpawnResult> {
    let use_pty = stdin_is_foreground_tty();

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
