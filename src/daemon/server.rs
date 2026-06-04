use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};

use crate::daemon::paths::DaemonPaths;
use crate::daemon::pidfile::PidFile;
use crate::daemon::protocol::{
    read_message, write_message, Request, Response, ServiceInfo, ServiceSpec,
};

/// In-memory record of a child `brainlog run` subprocess managed by the daemon.
/// Keyed by wrapper PID in `DaemonState::children`.
#[derive(Debug, Clone)]
struct TrackedChild {
    name: Option<String>,
    command: Vec<String>,
    started_at: DateTime<Utc>,
    /// Working directory the wrapper was launched in. Echoed in `Status`
    /// responses so `brainlog daemon status` can show where each service runs.
    cwd: String,
}

#[derive(Default)]
struct DaemonState {
    /// Keyed by wrapper PID — the unique handle into our supervised children.
    children: HashMap<u32, TrackedChild>,
}

/// Bind the daemon's Unix Domain Socket and serve requests until shutdown.
///
/// `started_at` is the timestamp the daemon process recorded at boot — used
/// to report uptime in `Status` responses. `_pid_file` keeps the pidfile lock
/// alive for the lifetime of the daemon.
pub async fn serve(
    paths: DaemonPaths,
    _pid_file: PidFile,
    started_at: DateTime<Utc>,
) -> Result<()> {
    let socket_path = paths.socket_path();
    // Remove any stale socket left over from a hard crash. Safe because the
    // pid file lock above guarantees no other live daemon is using it.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding daemon socket at {}", socket_path.display()))?;
    set_socket_permissions(&socket_path);
    tracing::info!(
        socket = %socket_path.display(),
        pid = std::process::id(),
        "brainlog daemon listening"
    );

    let state = Arc::new(Mutex::new(DaemonState::default()));
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // Periodic reaper: remove children whose pids no longer exist so Status
    // doesn't return stale entries.
    let state_for_reaper = state.clone();
    let reaper = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let mut s = state_for_reaper.lock().await;
            s.children.retain(|pid, _| is_process_alive(*pid));
        }
    });

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _addr)) => {
                        let state = state.clone();
                        let shutdown_tx = shutdown_tx.clone();
                        let socket_path = socket_path.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(
                                stream,
                                state,
                                shutdown_tx,
                                started_at,
                                socket_path,
                            )
                            .await
                            {
                                tracing::warn!("client connection error: {e:#}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!("accept failed: {e}");
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("brainlog daemon shutting down");
                break;
            }
        }
    }

    reaper.abort();
    let _ = reaper.await;

    // On shutdown, send SIGTERM to every supervised child so users can rely
    // on `daemon stop` actually stopping their services. Best-effort.
    let children: Vec<u32> = state.lock().await.children.keys().copied().collect();
    for pid in children {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
    }

    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// On Unix, the socket should not be world-accessible: it lets any caller
/// spawn processes as this user. 0o600 restricts it to the owner.
fn set_socket_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

fn is_process_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

async fn handle_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    shutdown_tx: mpsc::Sender<()>,
    started_at: DateTime<Utc>,
    socket_path: std::path::PathBuf,
) -> Result<()> {
    let req: Request = read_message(&mut stream).await?;
    let resp = match req {
        Request::Ping => Response::Pong,
        Request::Status => build_status(&state, started_at, &socket_path).await,
        Request::Shutdown => {
            // Notify the accept loop, then ack.
            let _ = shutdown_tx.send(()).await;
            Response::ShuttingDown
        }
        Request::SpawnService { spec } => match spawn_service(state.clone(), spec).await {
            Ok((service_id, run_id, name)) => Response::Spawned {
                service_id,
                run_id,
                name,
            },
            Err(e) => Response::Error {
                message: format!("{e:#}"),
            },
        },
    };
    write_message(&mut stream, &resp).await?;
    Ok(())
}

async fn build_status(
    state: &Arc<Mutex<DaemonState>>,
    started_at: DateTime<Utc>,
    socket_path: &Path,
) -> Response {
    let snapshot = state.lock().await.children.clone();
    let services: Vec<ServiceInfo> = snapshot
        .iter()
        .map(|(pid, c)| ServiceInfo {
            service_id: String::new(),
            run_id: String::new(),
            name: c.name.clone(),
            command: c.command.clone(),
            cwd: c.cwd.clone(),
            pid: Some(*pid),
            started_at: c.started_at.to_rfc3339(),
            status: if is_process_alive(*pid) {
                "running".into()
            } else {
                "exited".into()
            },
        })
        .collect();
    let uptime = (Utc::now() - started_at).num_seconds().max(0) as u64;
    Response::Status {
        pid: std::process::id(),
        started_at: started_at.to_rfc3339(),
        uptime_secs: uptime,
        socket_path: socket_path.to_string_lossy().into_owned(),
        services,
    }
}

/// Spawn a `brainlog run` subprocess for the requested service.
///
/// The wrapper's stdin/stdout/stderr go to `/dev/null`: the wrapped child's
/// real output flows through brainlog's PTY/pipe capture into the log files
/// under `~/.brainlog/logs/<run-id>/`. The daemon survives, the wrapper runs
/// independently, and the user reads logs via `brainlog logs` later.
async fn spawn_service(
    state: Arc<Mutex<DaemonState>>,
    spec: ServiceSpec,
) -> Result<(String, String, Option<String>)> {
    if spec.command.is_empty() {
        anyhow::bail!("empty command");
    }

    // Validate tags up front so the client sees a useful error.
    crate::cli::validate_tags(&spec.tags).context("validating tags")?;

    let brainlog_bin = current_exe_path()?;

    let mut args: Vec<String> = vec!["run".into()];
    if let Some(ref name) = spec.name {
        args.push("--name".into());
        args.push(name.clone());
    }
    if let Some(ref resume) = spec.resume {
        args.push("--resume".into());
        args.push(resume.clone());
    }
    for tag in &spec.tags {
        args.push("--tag".into());
        args.push(tag.clone());
    }
    if let Some(ref desc) = spec.desc {
        args.push("--desc".into());
        args.push(desc.clone());
    }
    if spec.restart {
        args.push("--restart".into());
    }
    args.push("--".into());
    args.extend(spec.command.iter().cloned());

    // Spawn detached: own session, /dev/null streams, inherit env so HOME etc.
    // point at the same brainlog data dir the daemon uses.
    let cwd = PathBuf::from(&spec.cwd);
    let mut cmd = tokio::process::Command::new(&brainlog_bin);
    cmd.args(&args)
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detach from the daemon's process group so signals to the daemon don't
    // automatically propagate (the daemon explicitly signals children on
    // shutdown).
    unsafe {
        cmd.pre_exec(|| {
            // Become session leader; orphan the child from the daemon's pgrp.
            let rc = libc::setsid();
            if rc == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawning brainlog wrapper for {:?}", spec.command))?;
    let wrapper_pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("spawned child has no pid"))?;

    // Detach the child handle from tokio (we don't await it). The daemon
    // tracks liveness via the periodic reaper.
    tokio::spawn(async move {
        let _ = child.wait_with_output().await;
    });

    state.lock().await.children.insert(
        wrapper_pid,
        TrackedChild {
            name: spec.name.clone(),
            command: spec.command.clone(),
            started_at: Utc::now(),
            cwd: spec.cwd.clone(),
        },
    );

    // We don't know the brainlog-allocated service_id/run_id from here without
    // an IPC handshake with the spawned wrapper. For now, return empty IDs and
    // let the user discover the new service via `brainlog list`. The
    // human-readable name is the canonical identifier.
    Ok((String::new(), String::new(), spec.name.clone()))
}

fn current_exe_path() -> Result<PathBuf> {
    // The current binary is the daemon — we re-invoke it as `brainlog run …`
    // for each spawned service. Tests can override with BRAINLOG_BIN.
    if let Ok(p) = std::env::var("BRAINLOG_BIN") {
        return Ok(PathBuf::from(p));
    }
    std::env::current_exe().context("resolving brainlog binary path")
}

/// Send a single request and read a single response. Used by CLI commands.
pub async fn round_trip(socket: &Path, req: Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to daemon at {}", socket.display()))?;
    write_message(&mut stream, &req).await?;
    let resp: Response = read_message(&mut stream).await?;
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn serve_and_ping_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let paths = DaemonPaths::new(tmp.path().to_path_buf());
        let pid_file = PidFile::acquire(paths.pid_file()).unwrap();
        let socket = paths.socket_path();
        let started = Utc::now();

        let server_handle = tokio::spawn(serve(paths.clone(), pid_file, started));

        // Wait for the socket to appear (server binds asynchronously).
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(socket.exists(), "socket should exist after server starts");

        let resp = round_trip(&socket, Request::Ping).await.unwrap();
        assert_eq!(resp, Response::Pong);

        let resp = round_trip(&socket, Request::Status).await.unwrap();
        match resp {
            Response::Status { pid, services, .. } => {
                assert_eq!(pid, std::process::id());
                assert!(services.is_empty(), "no spawned services initially");
            }
            other => panic!("expected Status, got {other:?}"),
        }

        let resp = round_trip(&socket, Request::Shutdown).await.unwrap();
        assert_eq!(resp, Response::ShuttingDown);

        server_handle.await.unwrap().unwrap();
        assert!(!socket.exists(), "socket cleaned up after shutdown");
    }

    #[tokio::test]
    async fn spawn_service_via_socket_runs_real_binary() {
        // Skip if no brainlog binary built yet — tests can't bootstrap without
        // an existing binary to invoke.
        let bin = std::env::var("CARGO_BIN_EXE_brainlog").ok();
        let Some(bin) = bin else {
            eprintln!("skipping: CARGO_BIN_EXE_brainlog not set");
            return;
        };

        let tmp = TempDir::new().unwrap();
        let paths = DaemonPaths::new(tmp.path().join(".brainlog"));
        let pid_file = PidFile::acquire(paths.pid_file()).unwrap();
        let socket = paths.socket_path();
        let started = Utc::now();

        std::env::set_var("BRAINLOG_BIN", &bin);
        std::env::set_var("HOME", tmp.path());

        let server_handle = tokio::spawn(serve(paths.clone(), pid_file, started));
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let req = Request::SpawnService {
            spec: ServiceSpec {
                command: vec!["echo".into(), "from-daemon".into()],
                cwd: tmp.path().to_string_lossy().into_owned(),
                name: Some("daemon-test".into()),
                resume: None,
                tags: vec![],
                desc: None,
                restart: false,
            },
        };
        let resp = round_trip(&socket, req).await.unwrap();
        match resp {
            Response::Spawned { name, .. } => {
                assert_eq!(name.as_deref(), Some("daemon-test"));
            }
            other => panic!("expected Spawned, got {other:?}"),
        }

        // Give the wrapper a moment, then shut down.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let _ = round_trip(&socket, Request::Shutdown).await;
        let _ = server_handle.await;
    }
}
