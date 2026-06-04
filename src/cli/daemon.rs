use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use crate::config::Config;
use crate::daemon::pidfile::{read_locked_pid, PidFile};
use crate::daemon::protocol::{Request, Response};
use crate::daemon::server::{round_trip, serve};
use crate::daemon::DaemonPaths;

#[derive(Parser, Debug)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub action: DaemonAction,
}

#[derive(Subcommand, Debug)]
pub enum DaemonAction {
    /// Start the brainlog daemon (detaches from the terminal).
    Start(StartArgs),
    /// Stop a running brainlog daemon.
    Stop,
    /// Restart the daemon.
    Restart(StartArgs),
    /// Show daemon status and supervised services.
    Status(StatusArgs),
    /// Internal: serve the daemon loop in the foreground. Used by `start` after detach.
    #[command(hide = true)]
    Serve,
}

#[derive(Parser, Debug)]
pub struct StartArgs {
    /// Run the daemon in the foreground instead of detaching.
    #[arg(long)]
    pub foreground: bool,
}

#[derive(Parser, Debug)]
pub struct StatusArgs {
    /// Emit JSON instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

pub async fn handle_daemon(args: DaemonArgs) -> Result<i32> {
    let paths = daemon_paths()?;

    match args.action {
        DaemonAction::Start(start) => handle_start(paths, start).await,
        DaemonAction::Stop => handle_stop(paths).await,
        DaemonAction::Restart(start) => {
            // Best-effort stop, ignore "not running" so users can `restart`
            // without checking state first.
            let _ = handle_stop(paths.clone()).await;
            handle_start(paths, start).await
        }
        DaemonAction::Status(s) => handle_status(paths, s).await,
        DaemonAction::Serve => handle_serve(paths).await,
    }
}

fn daemon_paths() -> Result<DaemonPaths> {
    let config = Config::load()?;
    Ok(DaemonPaths::new(config.base_dir().to_path_buf()))
}

async fn handle_start(paths: DaemonPaths, args: StartArgs) -> Result<i32> {
    if let Some(pid) = read_locked_pid(paths.pid_file())? {
        let tty = std::io::stderr().is_terminal();
        if tty {
            eprintln!(
                "{} brainlog daemon is already running (pid {})",
                "ok".green(),
                pid.bold()
            );
        } else {
            eprintln!("brainlog daemon is already running (pid {pid})");
        }
        return Ok(0);
    }

    if args.foreground {
        // Run the daemon loop directly in this process.
        return handle_serve(paths).await;
    }

    let child_pid = spawn_detached_daemon(&paths).await?;
    let socket = paths.socket_path();
    let tty = std::io::stdout().is_terminal();
    if tty {
        println!(
            "{} brainlog daemon started (pid {}, socket {})",
            "ok".green(),
            child_pid.bold(),
            socket.display().to_string().dimmed()
        );
    } else {
        println!(
            "brainlog daemon started (pid {child_pid}, socket {})",
            socket.display()
        );
    }
    Ok(0)
}

/// Fork a detached daemon serve process and block until its socket is bound.
/// Returns the child PID. The caller should not have a daemon already running —
/// guard with `read_locked_pid` first.
async fn spawn_detached_daemon(paths: &DaemonPaths) -> Result<u32> {
    let bin = std::env::current_exe().context("resolving brainlog binary path")?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.args(["daemon", "serve"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            // setsid: detach from the parent's controlling terminal and pgrp,
            // so closing the terminal won't deliver SIGHUP to the daemon.
            let rc = libc::setsid();
            if rc == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().context("spawning detached daemon process")?;
    let child_pid = child.id();

    let socket = paths.socket_path();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if socket.exists() {
            return Ok(child_pid);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!(
        "daemon did not start within 5s (no socket at {})",
        socket.display()
    );
}

async fn handle_stop(paths: DaemonPaths) -> Result<i32> {
    let socket = paths.socket_path();
    if read_locked_pid(paths.pid_file())?.is_none() {
        let tty = std::io::stderr().is_terminal();
        if tty {
            eprintln!("{} brainlog daemon is not running", "note".yellow().bold());
        } else {
            eprintln!("brainlog daemon is not running");
        }
        return Ok(0);
    }

    let resp = round_trip(&socket, Request::Shutdown).await?;
    match resp {
        Response::ShuttingDown => {}
        Response::Error { message } => anyhow::bail!("daemon refused shutdown: {message}"),
        other => anyhow::bail!("unexpected response from daemon: {other:?}"),
    }

    // Wait for the daemon process to actually release the pid file lock.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if read_locked_pid(paths.pid_file())?.is_none() {
            let tty = std::io::stdout().is_terminal();
            if tty {
                println!("{} brainlog daemon stopped", "ok".green());
            } else {
                println!("brainlog daemon stopped");
            }
            return Ok(0);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("daemon did not exit within 5s of shutdown request");
}

async fn handle_status(paths: DaemonPaths, args: StatusArgs) -> Result<i32> {
    let locked = read_locked_pid(paths.pid_file())?;
    if locked.is_none() {
        if args.json {
            println!("{{\"running\":false}}");
        } else {
            let tty = std::io::stdout().is_terminal();
            if tty {
                println!(
                    "brainlog daemon: {} (no pid file lock)",
                    "stopped".red().bold()
                );
            } else {
                println!("brainlog daemon: stopped");
            }
        }
        return Ok(0);
    }

    let socket = paths.socket_path();
    let resp = round_trip(&socket, Request::Status).await?;
    let status = match resp {
        Response::Status {
            pid,
            started_at,
            uptime_secs,
            socket_path,
            services,
        } => (pid, started_at, uptime_secs, socket_path, services),
        Response::Error { message } => anyhow::bail!("daemon error: {message}"),
        other => anyhow::bail!("unexpected response from daemon: {other:?}"),
    };
    let (pid, started_at, uptime_secs, socket_path, services) = status;

    if args.json {
        let value = serde_json::json!({
            "running": true,
            "pid": pid,
            "started_at": started_at,
            "uptime_secs": uptime_secs,
            "socket_path": socket_path,
            "services": services,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(0);
    }

    let tty = std::io::stdout().is_terminal();
    if tty {
        println!("brainlog daemon: {}", "running".green().bold());
        println!("  pid:        {}", pid.bold());
        println!("  started_at: {}", started_at);
        println!("  uptime:     {}s", uptime_secs);
        println!("  socket:     {}", socket_path.dimmed());
    } else {
        println!("brainlog daemon: running");
        println!("  pid:        {pid}");
        println!("  started_at: {started_at}");
        println!("  uptime:     {uptime_secs}s");
        println!("  socket:     {socket_path}");
    }
    if services.is_empty() {
        println!("  services:   (none)");
    } else {
        println!("  services:");
        for s in &services {
            let name = s.name.as_deref().unwrap_or("(unnamed)");
            let pid_str = s
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "    - {} pid={} status={} cwd={} cmd=`{}`",
                name,
                pid_str,
                s.status,
                s.cwd,
                s.command.join(" "),
            );
        }
    }
    Ok(0)
}

async fn handle_serve(paths: DaemonPaths) -> Result<i32> {
    let pid_file = PidFile::acquire(paths.pid_file())
        .context("acquiring daemon pid file (another daemon may be running)")?;
    let started_at = Utc::now();
    serve(paths, pid_file, started_at).await?;
    Ok(0)
}

/// Send a `SpawnService` request to the daemon, autostarting it if needed.
/// Used by `brainlog run --daemon` so the user never has to bootstrap state
/// before launching a service.
pub async fn spawn_via_daemon(
    paths: &DaemonPaths,
    spec: crate::daemon::protocol::ServiceSpec,
) -> Result<Response> {
    if read_locked_pid(paths.pid_file())?.is_none() {
        // No daemon running — bring one up transparently. Mirrors the work
        // `brainlog daemon start` does, minus the chatty success print.
        let tty = std::io::stderr().is_terminal();
        if tty {
            eprintln!(
                "{} brainlog daemon not running, starting it...",
                "note".yellow().bold()
            );
        } else {
            eprintln!("brainlog daemon not running, starting it...");
        }
        spawn_detached_daemon(paths).await?;
    }
    let socket = paths.socket_path();
    round_trip(&socket, Request::SpawnService { spec }).await
}

/// Resolve the daemon paths from the current brainlog config (which honours
/// the `HOME` environment variable). Useful for callers that need the same
/// paths the CLI subcommand will use.
pub fn resolve_daemon_paths() -> Result<DaemonPaths> {
    let config = Config::load()?;
    Ok(DaemonPaths::new(config.base_dir().to_path_buf()))
}

// Suppress unused-import warning when this file is compiled standalone in tests.
#[allow(dead_code)]
fn _phantom_pathbuf() -> PathBuf {
    PathBuf::new()
}
