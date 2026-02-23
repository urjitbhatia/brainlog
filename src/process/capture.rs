use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

use crate::storage::models::{Frame, StreamType};

pub fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// Reads from `reader`, writes to `writer` (passthrough), and sends Frame copies to `tx`.
pub async fn tee_stream<R, W>(
    mut reader: R,
    mut writer: W,
    stream_type: StreamType,
    tx: mpsc::Sender<Frame>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let payload = buf[..n].to_vec();
        tokio::io::AsyncWriteExt::write_all(&mut writer, &payload).await?;
        if let Err(e) = tx
            .send(Frame {
                timestamp_ns: now_ns(),
                stream_type,
                payload,
            })
            .await
        {
            tracing::warn!("Failed to send {stream_type:?} frame to log channel: {e}");
        }
    }
    Ok(())
}

/// Reads from `reader` and sends Frame copies to `tx` without passthrough writing.
pub async fn capture_stream<R>(
    mut reader: R,
    stream_type: StreamType,
    tx: mpsc::Sender<Frame>,
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let payload = buf[..n].to_vec();
        if let Err(e) = tx
            .send(Frame {
                timestamp_ns: now_ns(),
                stream_type,
                payload,
            })
            .await
        {
            tracing::warn!("Failed to send {stream_type:?} frame to log channel: {e}");
        }
    }
    Ok(())
}
