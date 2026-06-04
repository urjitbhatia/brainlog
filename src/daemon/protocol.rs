use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Magic bytes identifying a brainlog daemon framed message. Bumped if the
/// wire format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// Cap each message at 1 MiB. The protocol carries small JSON payloads only —
/// log contents flow through SQLite + log files, never through the socket.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// Request sent from CLI client to the daemon over its Unix Domain Socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Liveness probe — daemon should respond with `Response::Pong`.
    Ping,
    /// Status of the running daemon (pid, uptime, services).
    Status,
    /// Request a clean shutdown — daemon stops accepting new requests, waits
    /// for in-flight services to drain (best-effort), then exits.
    Shutdown,
    /// Spawn a new wrapped service inside the daemon.
    SpawnService { spec: ServiceSpec },
}

/// Service spec sent to the daemon when launching a service. The daemon
/// performs the actual `spawn_wrapped` and writes to the shared SQLite DB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceSpec {
    /// The command + args to run.
    pub command: Vec<String>,
    /// Working directory the daemon should `chdir` into for the child.
    pub cwd: String,
    /// Optional service name (clap `--name`).
    pub name: Option<String>,
    /// Optional `--resume` target name.
    pub resume: Option<String>,
    /// `key:value` tag strings.
    pub tags: Vec<String>,
    /// Optional human-readable description.
    pub desc: Option<String>,
    /// If true, auto-restart on non-signal exit.
    pub restart: bool,
}

/// Response from the daemon back to a CLI client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Pong,
    Status {
        pid: u32,
        started_at: String,
        uptime_secs: u64,
        socket_path: String,
        services: Vec<ServiceInfo>,
    },
    /// Daemon accepted the shutdown request and will exit shortly.
    ShuttingDown,
    /// Daemon accepted a spawn request and started the service.
    Spawned {
        service_id: String,
        run_id: String,
        name: Option<String>,
    },
    /// Daemon rejected the request with a human-readable error.
    Error {
        message: String,
    },
}

/// Lightweight summary of a single service running under the daemon.
/// Returned by `Response::Status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceInfo {
    pub service_id: String,
    pub run_id: String,
    pub name: Option<String>,
    pub command: Vec<String>,
    pub cwd: String,
    pub pid: Option<u32>,
    pub started_at: String,
    pub status: String,
}

/// Frame on the wire: `[u32 LE length][JSON bytes]`. The length excludes its
/// own 4 bytes. We bound the length at `MAX_MESSAGE_BYTES` so a malformed
/// peer can't trigger an unbounded allocation.
pub async fn write_message<W: AsyncWriteExt + Unpin, T: Serialize>(
    writer: &mut W,
    msg: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(msg).context("serializing message")?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        anyhow::bail!(
            "message of {} bytes exceeds max {} bytes",
            bytes.len(),
            MAX_MESSAGE_BYTES
        );
    }
    let len: u32 = bytes.len() as u32;
    writer.write_all(&len.to_le_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_message<R: AsyncReadExt + Unpin, T: for<'de> Deserialize<'de>>(
    reader: &mut R,
) -> Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_BYTES {
        anyhow::bail!("message length {} exceeds max {}", len, MAX_MESSAGE_BYTES);
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).context("deserializing message")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn roundtrip_request_ping() {
        let req = Request::Ping;
        let (mut client, mut server) = duplex(4096);
        write_message(&mut client, &req).await.unwrap();
        let got: Request = read_message(&mut server).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn roundtrip_spawn_service() {
        let req = Request::SpawnService {
            spec: ServiceSpec {
                command: vec!["echo".into(), "hi".into()],
                cwd: "/tmp".into(),
                name: Some("test".into()),
                resume: None,
                tags: vec!["env:prod".into()],
                desc: Some("a service".into()),
                restart: false,
            },
        };
        let (mut a, mut b) = duplex(4096);
        write_message(&mut a, &req).await.unwrap();
        let got: Request = read_message(&mut b).await.unwrap();
        assert_eq!(got, req);
    }

    #[tokio::test]
    async fn roundtrip_response_status() {
        let resp = Response::Status {
            pid: 1234,
            started_at: "2026-06-03T00:00:00Z".into(),
            uptime_secs: 42,
            socket_path: "/tmp/daemon.sock".into(),
            services: vec![ServiceInfo {
                service_id: "svc-1".into(),
                run_id: "run-1".into(),
                name: Some("api".into()),
                command: vec!["node".into(), "server.js".into()],
                cwd: "/srv".into(),
                pid: Some(9999),
                started_at: "2026-06-03T00:00:00Z".into(),
                status: "running".into(),
            }],
        };
        let (mut a, mut b) = duplex(4096);
        write_message(&mut a, &resp).await.unwrap();
        let got: Response = read_message(&mut b).await.unwrap();
        assert_eq!(got, resp);
    }

    #[tokio::test]
    async fn oversized_message_rejected_on_read() {
        // Hand-craft a frame claiming size > MAX
        let (mut a, mut b) = duplex(64);
        let oversized = (MAX_MESSAGE_BYTES + 1) as u32;
        let write_task = tokio::spawn(async move {
            // Write the length only; reader should refuse before reading payload.
            let _ = a.write_all(&oversized.to_le_bytes()).await;
        });
        let result: Result<Request> = read_message(&mut b).await;
        write_task.await.unwrap();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds max"));
    }

    #[test]
    fn protocol_version_constant() {
        // Sanity: bump this when the wire format changes incompatibly.
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
