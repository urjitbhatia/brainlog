use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use super::models::{Frame, StreamType};

const FRAME_HEADER_SIZE: usize = 13; // u64 + u8 + u32

pub struct LogWriter {
    log_dir: PathBuf,
    rx: mpsc::Receiver<Frame>,
    flush_interval_ms: u64,
    flush_buffer_bytes: usize,
}

impl LogWriter {
    pub fn new(
        log_dir: PathBuf,
        rx: mpsc::Receiver<Frame>,
        flush_interval_ms: u64,
        flush_buffer_bytes: usize,
    ) -> Self {
        Self {
            log_dir,
            rx,
            flush_interval_ms,
            flush_buffer_bytes,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        std::fs::create_dir_all(&self.log_dir)?;

        let mut stdout_file = std::fs::File::create(self.log_dir.join("stdout.log"))?;
        let mut stderr_file = std::fs::File::create(self.log_dir.join("stderr.log"))?;
        let mut stdin_file = std::fs::File::create(self.log_dir.join("stdin.log"))?;
        let mut combined_file = std::fs::File::create(self.log_dir.join("combined.log"))?;

        let mut buffer_size: usize = 0;
        let flush_interval = tokio::time::Duration::from_millis(self.flush_interval_ms);
        let mut flush_timer = tokio::time::interval(flush_interval);
        flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                frame = self.rx.recv() => {
                    match frame {
                        Some(frame) => {
                            let encoded = encode_frame(&frame);
                            let stream_file = match frame.stream_type {
                                StreamType::Stdout => &mut stdout_file,
                                StreamType::Stderr => &mut stderr_file,
                                StreamType::Stdin => &mut stdin_file,
                            };
                            stream_file.write_all(&encoded)?;
                            combined_file.write_all(&encoded)?;
                            buffer_size += encoded.len() * 2;

                            if buffer_size >= self.flush_buffer_bytes {
                                stdout_file.flush()?;
                                stderr_file.flush()?;
                                stdin_file.flush()?;
                                combined_file.flush()?;
                                buffer_size = 0;
                            }
                        }
                        None => {
                            stdout_file.flush()?;
                            stderr_file.flush()?;
                            stdin_file.flush()?;
                            combined_file.flush()?;
                            break;
                        }
                    }
                }
                _ = flush_timer.tick() => {
                    if buffer_size > 0 {
                        stdout_file.flush()?;
                        stderr_file.flush()?;
                        stdin_file.flush()?;
                        combined_file.flush()?;
                        buffer_size = 0;
                    }
                }
            }
        }

        Ok(())
    }
}

fn encode_frame(frame: &Frame) -> Vec<u8> {
    let mut buf = Vec::with_capacity(FRAME_HEADER_SIZE + frame.payload.len());
    buf.extend_from_slice(&frame.timestamp_ns.to_le_bytes());
    buf.push(frame.stream_type as u8);
    buf.extend_from_slice(&(frame.payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&frame.payload);
    buf
}

pub struct LogReader {
    path: PathBuf,
}

impl LogReader {
    pub fn new(log_dir: &Path, stream: &str) -> Self {
        let filename = match stream {
            "stdout" => "stdout.log",
            "stderr" => "stderr.log",
            "stdin" => "stdin.log",
            _ => "combined.log",
        };
        Self {
            path: log_dir.join(filename),
        }
    }

    pub fn read_frames(&self) -> Result<Vec<Frame>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut file = std::fs::File::open(&self.path)?;
        let mut frames = Vec::new();
        loop {
            match read_one_frame(&mut file) {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading frame: {}", e);
                    break;
                }
            }
        }
        Ok(frames)
    }

    pub fn read_head(&self, n: usize) -> Result<Vec<Frame>> {
        let frames = self.read_frames()?;
        Ok(frames.into_iter().take(n).collect())
    }

    pub fn read_tail(&self, n: usize) -> Result<Vec<Frame>> {
        let frames = self.read_frames()?;
        let skip = frames.len().saturating_sub(n);
        Ok(frames.into_iter().skip(skip).collect())
    }

    pub fn read_range(
        &self,
        start_time: Option<u64>,
        end_time: Option<u64>,
    ) -> Result<Vec<Frame>> {
        let frames = self.read_frames()?;
        Ok(frames
            .into_iter()
            .filter(|f| {
                if let Some(start) = start_time {
                    if f.timestamp_ns < start {
                        return false;
                    }
                }
                if let Some(end) = end_time {
                    if f.timestamp_ns > end {
                        return false;
                    }
                }
                true
            })
            .collect())
    }

    pub fn search(&self, pattern: &regex::Regex, max_matches: usize) -> Result<Vec<LogMatch>> {
        let frames = self.read_frames()?;
        let mut matches = Vec::new();
        for (idx, frame) in frames.iter().enumerate() {
            if let Ok(text) = std::str::from_utf8(&frame.payload) {
                for line in text.lines() {
                    if pattern.is_match(line) {
                        matches.push(LogMatch {
                            frame_index: idx,
                            timestamp_ns: frame.timestamp_ns,
                            stream_type: frame.stream_type,
                            line: line.to_string(),
                        });
                        if matches.len() >= max_matches {
                            return Ok(matches);
                        }
                    }
                }
            }
        }
        Ok(matches)
    }

    pub fn file_size(&self) -> Result<u64> {
        if !self.path.exists() {
            return Ok(0);
        }
        Ok(std::fs::metadata(&self.path)?.len())
    }
}

#[derive(Debug, Clone)]
pub struct LogMatch {
    pub frame_index: usize,
    pub timestamp_ns: u64,
    pub stream_type: StreamType,
    pub line: String,
}

fn read_one_frame(file: &mut std::fs::File) -> Result<Option<Frame>> {
    let mut header = [0u8; FRAME_HEADER_SIZE];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }

    let timestamp_ns = u64::from_le_bytes(header[0..8].try_into().unwrap());
    let stream_type = StreamType::from_u8(header[8]).context("Invalid stream type byte")?;
    let length = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;

    let mut payload = vec![0u8; length];
    file.read_exact(&mut payload)?;

    Ok(Some(Frame {
        timestamp_ns,
        stream_type,
        payload,
    }))
}

pub fn frames_to_text(frames: &[Frame]) -> String {
    let mut output = String::new();
    for frame in frames {
        if let Ok(text) = std::str::from_utf8(&frame.payload) {
            output.push_str(text);
        }
    }
    output
}

pub fn frames_to_bytes(frames: &[Frame], max_bytes: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::new();
    let mut has_more = false;
    for frame in frames {
        if output.len() + frame.payload.len() > max_bytes {
            has_more = true;
            break;
        }
        output.extend_from_slice(&frame.payload);
    }
    (output, has_more)
}
