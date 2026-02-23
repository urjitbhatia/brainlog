use anyhow::Result;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use signal_hook::consts::*;
use signal_hook_tokio::Signals;
use tokio_stream::StreamExt;

const FORWARDED_SIGNALS: &[i32] = &[SIGINT, SIGTERM, SIGQUIT, SIGHUP, SIGUSR1, SIGUSR2, SIGCONT];

pub async fn forward_signals(child_pid: u32) -> Result<()> {
    let mut signals = Signals::new(FORWARDED_SIGNALS)?;
    let pid = Pid::from_raw(child_pid as i32);

    while let Some(sig) = signals.next().await {
        let nix_signal = match sig {
            SIGINT => Signal::SIGINT,
            SIGTERM => Signal::SIGTERM,
            SIGQUIT => Signal::SIGQUIT,
            SIGHUP => Signal::SIGHUP,
            SIGUSR1 => Signal::SIGUSR1,
            SIGUSR2 => Signal::SIGUSR2,
            SIGCONT => Signal::SIGCONT,
            _ => continue,
        };
        if let Err(e) = signal::kill(pid, nix_signal) {
            tracing::warn!("Failed to forward signal {nix_signal} to child pid {child_pid}: {e}");
        }
    }

    Ok(())
}
