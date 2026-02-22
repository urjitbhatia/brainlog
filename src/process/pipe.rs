use anyhow::Result;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::storage::models::{Frame, StreamType};

use super::capture::tee_stream;

pub struct PipeResult {
    pub exit_code: Option<i32>,
    pub pid: u32,
}

pub async fn spawn_piped(
    command: &[String],
    tx: mpsc::Sender<Frame>,
) -> Result<PipeResult> {
    let (program, args) = command.split_first().expect("command must not be empty");

    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let pid = child.id().unwrap_or(0);

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let tx_stdout = tx.clone();
    let tx_stderr = tx.clone();

    let stdout_handle = tokio::spawn(async move {
        tee_stream(
            tokio::io::BufReader::new(stdout),
            tokio::io::stdout(),
            StreamType::Stdout,
            tx_stdout,
        )
        .await
    });

    let stderr_handle = tokio::spawn(async move {
        tee_stream(
            tokio::io::BufReader::new(stderr),
            tokio::io::stderr(),
            StreamType::Stderr,
            tx_stderr,
        )
        .await
    });

    // Forward signals to child
    let signal_handle = tokio::spawn(super::signals::forward_signals(pid));

    let status = child.wait().await?;
    signal_handle.abort();

    // Wait for I/O to drain
    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    drop(tx);

    Ok(PipeResult {
        exit_code: status.code(),
        pid,
    })
}
