use anyhow::{Context, Result};
use nix::libc;
use nix::pty::ForkptyResult;
use nix::sys::termios;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{read, write, Pid};
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::storage::models::{Frame, StreamType};

use super::capture::now_ns;

pub struct PtyResult {
    pub exit_code: Option<i32>,
    pub pid: u32,
}

pub async fn spawn_pty(
    command: &[String],
    tx: mpsc::Sender<Frame>,
    pid_tx: oneshot::Sender<u32>,
) -> Result<PtyResult> {
    let (program, _args) = command.split_first().expect("command must not be empty");

    // Save terminal state before forkpty
    let saved_termios = if nix::unistd::isatty(libc::STDIN_FILENO).unwrap_or(false) {
        termios::tcgetattr(std::io::stdin()).ok()
    } else {
        None
    };

    // forkpty - returns an enum in nix 0.29
    let fork_result = unsafe { nix::pty::forkpty(None, None)? };

    match fork_result {
        ForkptyResult::Child => {
            // In child: exec the target command
            let c_program = CString::new(program.as_str()).context("Invalid program name")?;
            let c_args: Vec<CString> = command
                .iter()
                .map(|a| CString::new(a.as_str()).unwrap())
                .collect();
            let c_args_refs: Vec<&std::ffi::CStr> = c_args.iter().map(|a| a.as_c_str()).collect();

            nix::unistd::execvp(&c_program, &c_args_refs)?;
            unreachable!();
        }
        ForkptyResult::Parent { child, master } => {
            let child_pid = child.as_raw() as u32;

            // Notify caller of PID immediately, before waiting for child to exit
            let _ = pid_tx.send(child_pid);

            // Set stdin to raw mode
            if saved_termios.is_some() {
                if let Ok(mut raw) = termios::tcgetattr(std::io::stdin()) {
                    termios::cfmakeraw(&mut raw);
                    if let Err(e) =
                        termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &raw)
                    {
                        tracing::warn!("Failed to set terminal to raw mode: {e}");
                    }
                }
            }

            // Run I/O pump in blocking threads since PTY fd uses nix read/write
            let result = run_pty_pump(master, child, tx).await;

            // Restore terminal
            if let Some(saved) = saved_termios {
                if let Err(e) =
                    termios::tcsetattr(std::io::stdin(), termios::SetArg::TCSANOW, &saved)
                {
                    tracing::warn!("Failed to restore terminal settings: {e}");
                }
            }

            result.map(|exit_code| PtyResult {
                exit_code,
                pid: child_pid,
            })
        }
    }
}

async fn run_pty_pump(master: OwnedFd, child: Pid, tx: mpsc::Sender<Frame>) -> Result<Option<i32>> {
    let master_raw = master.as_raw_fd();

    // We need to dup the fd for the write side
    let master_raw_dup = nix::unistd::dup(master_raw)?;

    // Shared flag to signal stdin reader to stop
    let done = Arc::new(AtomicBool::new(false));

    let tx_read = tx.clone();
    let tx_write = tx.clone();

    // Spawn a signal forwarder
    let child_pid = child.as_raw() as u32;
    let signal_handle = tokio::spawn(super::signals::forward_signals(child_pid));

    // Read from PTY master -> stdout + capture
    let read_handle = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut buf = [0u8; 8192];
        loop {
            match read(master_raw, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let payload = buf[..n].to_vec();
                    if let Err(e) =
                        std::io::Write::write_all(&mut std::io::stdout().lock(), &payload)
                    {
                        tracing::warn!("Failed to write PTY output to stdout: {e}");
                    }
                    if let Err(e) = tx_read.blocking_send(Frame {
                        timestamp_ns: now_ns(),
                        stream_type: StreamType::Stdout,
                        payload,
                    }) {
                        tracing::warn!("Failed to send PTY stdout frame to log channel: {e}");
                    }
                }
                Err(nix::errno::Errno::EIO) => break,
                Err(nix::errno::Errno::EAGAIN) => continue,
                Err(e) => {
                    tracing::debug!("PTY read error: {}", e);
                    break;
                }
            }
        }
        Ok(())
    });

    // Read from stdin -> PTY master + capture
    // Uses poll() to make stdin reads interruptible via the `done` flag
    let done_write = done.clone();
    let write_handle = tokio::task::spawn_blocking(move || -> Result<()> {
        let fd = master_raw_dup;
        let stdin_fd = libc::STDIN_FILENO;
        let mut buf = [0u8; 8192];

        loop {
            if done_write.load(Ordering::Relaxed) {
                break;
            }

            // Use poll with a timeout so we can check the done flag periodically
            let mut pollfd = libc::pollfd {
                fd: stdin_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let poll_result = unsafe { libc::poll(&mut pollfd, 1, 100) }; // 100ms timeout

            if poll_result <= 0 {
                continue; // timeout or error, loop back to check done flag
            }

            if pollfd.revents & libc::POLLIN == 0 {
                continue;
            }

            match std::io::Read::read(&mut std::io::stdin().lock(), &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let payload = buf[..n].to_vec();
                    match write(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }, &payload) {
                        Ok(_) => {}
                        Err(nix::errno::Errno::EIO) => break,
                        Err(_) => break,
                    }
                    if let Err(e) = tx_write.blocking_send(Frame {
                        timestamp_ns: now_ns(),
                        stream_type: StreamType::Stdin,
                        payload,
                    }) {
                        tracing::warn!("Failed to send PTY stdin frame to log channel: {e}");
                    }
                }
                Err(_) => break,
            }
        }
        if let Err(e) = nix::unistd::close(fd) {
            tracing::warn!("Failed to close PTY master fd: {e}");
        }
        Ok(())
    });

    // Wait for child to exit
    let exit_code = tokio::task::spawn_blocking(move || -> Option<i32> {
        loop {
            match waitpid(child, Some(WaitPidFlag::WUNTRACED)) {
                Ok(WaitStatus::Exited(_, code)) => return Some(code),
                Ok(WaitStatus::Signaled(_, sig, _)) => return Some(128 + sig as i32),
                Ok(WaitStatus::StillAlive) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    })
    .await?;

    // Signal stdin reader to stop
    done.store(true, Ordering::Relaxed);

    signal_handle.abort();
    let _ = signal_handle.await;
    read_handle.abort();
    let _ = read_handle.await;
    // write_handle should exit on its own due to the done flag, but give it a moment
    match tokio::time::timeout(tokio::time::Duration::from_millis(200), write_handle).await {
        Ok(Err(e)) => tracing::warn!("PTY stdin writer task failed: {e}"),
        Err(_) => tracing::debug!("PTY stdin writer did not finish within timeout, proceeding"),
        _ => {}
    }

    drop(tx);

    Ok(exit_code)
}
