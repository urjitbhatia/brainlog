use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

use super::models::{Frame, StreamFilter, StreamType};

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
        // Restrict the parent logs/ directory as well as the per-run log directory
        if let Some(parent) = self.log_dir.parent() {
            super::permissions::create_dir_restricted(parent)?;
        }
        super::permissions::create_dir_restricted(&self.log_dir)?;

        let stdout_path = self.log_dir.join("stdout.log");
        let stderr_path = self.log_dir.join("stderr.log");
        let stdin_path = self.log_dir.join("stdin.log");
        let combined_path = self.log_dir.join("combined.log");

        let mut stdout_file = std::fs::File::create(&stdout_path)?;
        let mut stderr_file = std::fs::File::create(&stderr_path)?;
        let mut stdin_file = std::fs::File::create(&stdin_path)?;
        let mut combined_file = std::fs::File::create(&combined_path)?;

        super::permissions::set_file_restricted(&stdout_path);
        super::permissions::set_file_restricted(&stderr_path);
        super::permissions::set_file_restricted(&stdin_path);
        super::permissions::set_file_restricted(&combined_path);

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
    pub fn new(log_dir: &Path, stream: StreamFilter) -> Self {
        Self {
            path: log_dir.join(stream.log_filename()),
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

    pub fn read_range(&self, start_time: Option<u64>, end_time: Option<u64>) -> Result<Vec<Frame>> {
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

/// Returns (stdout, stderr, stdin, combined) file sizes in bytes for a log directory.
pub fn log_sizes(log_dir: &Path) -> (u64, u64, u64, u64) {
    let size = |name: &str| -> u64 {
        std::fs::metadata(log_dir.join(name))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    (
        size("stdout.log"),
        size("stderr.log"),
        size("stdin.log"),
        size("combined.log"),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_frame(stream: StreamType, payload: &[u8], ts: u64) -> Frame {
        Frame {
            timestamp_ns: ts,
            stream_type: stream,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let frame = make_frame(StreamType::Stdout, b"hello world", 123456789);
        let encoded = encode_frame(&frame);

        let mut cursor = std::io::Cursor::new(encoded);
        // Cursor implements Read, but read_one_frame needs File. Use manual decode.
        let mut header = [0u8; FRAME_HEADER_SIZE];
        std::io::Read::read_exact(&mut cursor, &mut header).unwrap();

        let ts = u64::from_le_bytes(header[0..8].try_into().unwrap());
        let st = StreamType::from_u8(header[8]).unwrap();
        let len = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;

        let mut payload = vec![0u8; len];
        std::io::Read::read_exact(&mut cursor, &mut payload).unwrap();

        assert_eq!(ts, 123456789);
        assert_eq!(st, StreamType::Stdout);
        assert_eq!(payload, b"hello world");
    }

    #[tokio::test]
    async fn writer_reader_roundtrip() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        tx.send(make_frame(StreamType::Stdout, b"out1\n", 1000))
            .await
            .unwrap();
        tx.send(make_frame(StreamType::Stderr, b"err1\n", 2000))
            .await
            .unwrap();
        tx.send(make_frame(StreamType::Stdin, b"in1\n", 3000))
            .await
            .unwrap();
        tx.send(make_frame(StreamType::Stdout, b"out2\n", 4000))
            .await
            .unwrap();
        drop(tx);

        writer.run().await.unwrap();

        // Read stdout
        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let frames = reader.read_frames().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].payload, b"out1\n");
        assert_eq!(frames[1].payload, b"out2\n");

        // Read stderr
        let reader = LogReader::new(dir.path(), StreamFilter::Stderr);
        let frames = reader.read_frames().unwrap();
        assert_eq!(frames.len(), 1);

        // Read combined
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let frames = reader.read_frames().unwrap();
        assert_eq!(frames.len(), 4);
    }

    #[tokio::test]
    async fn head_and_tail() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        for i in 0..5 {
            tx.send(make_frame(
                StreamType::Stdout,
                format!("line{}\n", i).as_bytes(),
                i * 1000,
            ))
            .await
            .unwrap();
        }
        drop(tx);
        writer.run().await.unwrap();

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);

        let head = reader.read_head(2).unwrap();
        assert_eq!(head.len(), 2);
        assert_eq!(head[0].payload, b"line0\n");
        assert_eq!(head[1].payload, b"line1\n");

        let tail = reader.read_tail(2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].payload, b"line3\n");
        assert_eq!(tail[1].payload, b"line4\n");
    }

    #[tokio::test]
    async fn read_range_filters_by_timestamp() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        for i in 0..5u64 {
            tx.send(make_frame(
                StreamType::Stdout,
                format!("f{}", i).as_bytes(),
                i * 1000,
            ))
            .await
            .unwrap();
        }
        drop(tx);
        writer.run().await.unwrap();

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let range = reader.read_range(Some(1000), Some(3000)).unwrap();
        assert_eq!(range.len(), 3); // timestamps 1000, 2000, 3000
    }

    #[tokio::test]
    async fn search_matches_pattern() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        tx.send(make_frame(StreamType::Stdout, b"INFO: started\n", 1000))
            .await
            .unwrap();
        tx.send(make_frame(StreamType::Stdout, b"ERROR: failed\n", 2000))
            .await
            .unwrap();
        tx.send(make_frame(StreamType::Stdout, b"INFO: done\n", 3000))
            .await
            .unwrap();
        drop(tx);
        writer.run().await.unwrap();

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let pattern = regex::Regex::new("ERROR").unwrap();
        let matches = reader.search(&pattern, 10).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].line.contains("ERROR"));
    }

    #[tokio::test]
    async fn log_sizes_returns_file_sizes() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        tx.send(make_frame(StreamType::Stdout, b"hello", 1000))
            .await
            .unwrap();
        tx.send(make_frame(StreamType::Stderr, b"world", 2000))
            .await
            .unwrap();
        drop(tx);
        writer.run().await.unwrap();

        let (stdout_sz, stderr_sz, stdin_sz, combined_sz) = log_sizes(dir.path());
        assert!(stdout_sz > 0);
        assert!(stderr_sz > 0);
        assert_eq!(stdin_sz, 0); // file created but no stdin frames written
        assert!(combined_sz > 0);
    }

    #[test]
    fn log_sizes_missing_dir() {
        let (a, b, c, d) = log_sizes(Path::new("/nonexistent/path"));
        assert_eq!((a, b, c, d), (0, 0, 0, 0));
    }

    #[test]
    fn file_size_missing_file() {
        let reader = LogReader::new(Path::new("/nonexistent"), StreamFilter::Stdout);
        assert_eq!(reader.file_size().unwrap(), 0);
    }

    #[test]
    fn frames_to_text_concatenates() {
        let frames = vec![
            make_frame(StreamType::Stdout, b"hello ", 1),
            make_frame(StreamType::Stdout, b"world", 2),
        ];
        assert_eq!(frames_to_text(&frames), "hello world");
    }

    #[test]
    fn frames_to_bytes_respects_limit() {
        let frames = vec![
            make_frame(StreamType::Stdout, b"aaaa", 1),
            make_frame(StreamType::Stdout, b"bbbb", 2),
            make_frame(StreamType::Stdout, b"cccc", 3),
        ];
        let (bytes, has_more) = frames_to_bytes(&frames, 6);
        assert_eq!(bytes, b"aaaa");
        assert!(has_more);
    }
}
