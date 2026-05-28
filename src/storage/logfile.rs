use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
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

        let mut stdout_file = std::fs::File::create(&stdout_path)?;
        let mut stderr_file = std::fs::File::create(&stderr_path)?;
        let mut stdin_file = std::fs::File::create(&stdin_path)?;

        super::permissions::set_file_restricted(&stdout_path);
        super::permissions::set_file_restricted(&stderr_path);
        super::permissions::set_file_restricted(&stdin_path);

        let mut buffer_size: usize = 0;
        let mut dirty_stdout = false;
        let mut dirty_stderr = false;
        let mut dirty_stdin = false;
        let flush_interval = tokio::time::Duration::from_millis(self.flush_interval_ms);
        let mut flush_timer = tokio::time::interval(flush_interval);
        flush_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let flush_all = |stdout: &mut std::fs::File,
                         stderr: &mut std::fs::File,
                         stdin: &mut std::fs::File,
                         d_out: &mut bool,
                         d_err: &mut bool,
                         d_in: &mut bool|
         -> std::io::Result<()> {
            if *d_out {
                stdout.flush()?;
                *d_out = false;
            }
            if *d_err {
                stderr.flush()?;
                *d_err = false;
            }
            if *d_in {
                stdin.flush()?;
                *d_in = false;
            }
            Ok(())
        };

        loop {
            tokio::select! {
                frame = self.rx.recv() => {
                    match frame {
                        Some(frame) => {
                            let encoded = encode_frame(&frame);
                            let (stream_file, dirty_flag) = match frame.stream_type {
                                StreamType::Stdout => (&mut stdout_file, &mut dirty_stdout),
                                StreamType::Stderr => (&mut stderr_file, &mut dirty_stderr),
                                StreamType::Stdin => (&mut stdin_file, &mut dirty_stdin),
                            };
                            stream_file.write_all(&encoded)?;
                            *dirty_flag = true;
                            buffer_size += encoded.len();

                            if buffer_size >= self.flush_buffer_bytes {
                                flush_all(
                                    &mut stdout_file, &mut stderr_file, &mut stdin_file,
                                    &mut dirty_stdout, &mut dirty_stderr, &mut dirty_stdin,
                                )?;
                                buffer_size = 0;
                            }
                        }
                        None => {
                            flush_all(
                                &mut stdout_file, &mut stderr_file, &mut stdin_file,
                                &mut dirty_stdout, &mut dirty_stderr, &mut dirty_stdin,
                            )?;
                            break;
                        }
                    }
                }
                _ = flush_timer.tick() => {
                    if buffer_size > 0 {
                        flush_all(
                            &mut stdout_file, &mut stderr_file, &mut stdin_file,
                            &mut dirty_stdout, &mut dirty_stderr, &mut dirty_stdin,
                        )?;
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
    log_dir: PathBuf,
    stream: StreamFilter,
}

/// The three stream file paths used to reconstruct the combined view.
const STREAM_FILES: [(&str, StreamFilter); 3] = [
    ("stdout.log", StreamFilter::Stdout),
    ("stderr.log", StreamFilter::Stderr),
    ("stdin.log", StreamFilter::Stdin),
];

impl LogReader {
    pub fn new(log_dir: &Path, stream: StreamFilter) -> Self {
        Self {
            log_dir: log_dir.to_path_buf(),
            stream,
        }
    }

    /// Returns the path for a single-stream file. Panics if called for Combined.
    fn single_stream_path(&self) -> PathBuf {
        assert_ne!(
            self.stream,
            StreamFilter::Combined,
            "single_stream_path called for Combined; use merge methods instead"
        );
        self.log_dir.join(self.stream.log_filename())
    }

    /// Whether this reader uses merge-sort across stream files.
    pub fn is_combined(&self) -> bool {
        self.stream == StreamFilter::Combined
    }

    pub fn read_frames(&self) -> Result<Vec<Frame>> {
        if self.is_combined() {
            return self.merge_read_frames();
        }
        let path = self.single_stream_path();
        read_all_frames_from_file(&path)
    }

    pub fn read_head(&self, n: usize) -> Result<Vec<Frame>> {
        if self.is_combined() {
            return self.merge_read_head(n);
        }
        let path = self.single_stream_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&path)?;
        let mut reader = BufReader::new(file);
        let mut frames = Vec::with_capacity(n);
        for _ in 0..n {
            match read_one_frame(&mut reader) {
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

    pub fn read_tail(&self, n: usize) -> Result<Vec<Frame>> {
        if self.is_combined() {
            return self.merge_read_tail(n);
        }
        let path = self.single_stream_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&path)?;
        let mut reader = BufReader::new(file);
        let mut ring = VecDeque::with_capacity(n + 1);
        loop {
            match read_one_frame(&mut reader) {
                Ok(Some(frame)) => {
                    ring.push_back(frame);
                    if ring.len() > n {
                        ring.pop_front();
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading frame: {}", e);
                    break;
                }
            }
        }
        Ok(ring.into())
    }

    /// Read frames within a time range, streaming frame-by-frame.
    /// Skips frames before `start_time` and stops early after `end_time`.
    pub fn read_range(&self, start_time: Option<u64>, end_time: Option<u64>) -> Result<Vec<Frame>> {
        if self.is_combined() {
            return self.merge_read_range(start_time, end_time);
        }
        let path = self.single_stream_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&path)?;
        let mut reader = BufReader::new(file);
        let mut frames = Vec::new();
        loop {
            match read_one_frame(&mut reader) {
                Ok(Some(frame)) => {
                    if let Some(end) = end_time {
                        if frame.timestamp_ns > end {
                            break;
                        }
                    }
                    if let Some(start) = start_time {
                        if frame.timestamp_ns < start {
                            continue;
                        }
                    }
                    frames.push(frame);
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading frame: {}", e);
                    break;
                }
            }
        }
        Ok(frames)
    }

    /// Search log frames for a pattern, streaming frame-by-frame.
    /// Stops as soon as `max_matches` are found without loading the entire file.
    pub fn search(&self, pattern: &regex::Regex, max_matches: usize) -> Result<Vec<LogMatch>> {
        if self.is_combined() {
            return self.merge_search(pattern, max_matches);
        }
        let path = self.single_stream_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&path)?;
        let mut reader = BufReader::new(file);
        let mut matches = Vec::new();
        let mut idx = 0;
        loop {
            match read_one_frame(&mut reader) {
                Ok(Some(frame)) => {
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
                    idx += 1;
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading frame: {}", e);
                    break;
                }
            }
        }
        Ok(matches)
    }

    /// Read frames starting from a byte offset. Returns the frames read and the
    /// new byte offset (i.e. the position after the last successfully read frame).
    /// This enables incremental reads for follow/tail -f style use cases.
    ///
    /// Read frames starting from a byte offset for a single-stream reader.
    /// Not supported for Combined mode — use `read_frames_since` instead.
    pub fn read_frames_from_offset(&self, offset: u64) -> Result<(Vec<Frame>, u64)> {
        if self.is_combined() {
            return Ok((Vec::new(), offset));
        }
        let path = self.single_stream_path();
        if !path.exists() {
            return Ok((Vec::new(), offset));
        }
        let mut file = std::fs::File::open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file);
        let mut frames = Vec::new();
        loop {
            match read_one_frame(&mut reader) {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Error reading frame at offset: {}", e);
                    break;
                }
            }
        }
        let new_offset = reader.stream_position()?;
        Ok((frames, new_offset))
    }

    /// Read frames with timestamp strictly greater than `after_ns` from the
    /// appropriate stream files. For combined mode this reads from all three
    /// stream files and merge-sorts. For single-stream mode it reads from just
    /// that stream file. This is the preferred method for follow/poll use cases
    /// where combined mode is needed.
    pub fn read_frames_since(&self, after_ns: u64) -> Result<Vec<Frame>> {
        self.read_range(Some(after_ns + 1), None)
    }

    pub fn file_size(&self) -> Result<u64> {
        if self.is_combined() {
            let mut total = 0u64;
            for (filename, _) in &STREAM_FILES {
                let p = self.log_dir.join(filename);
                if p.exists() {
                    total += std::fs::metadata(&p)?.len();
                }
            }
            return Ok(total);
        }
        let path = self.single_stream_path();
        if !path.exists() {
            return Ok(0);
        }
        Ok(std::fs::metadata(&path)?.len())
    }

    // ── Merge-sort implementations for Combined mode ──────────────────

    /// Read all frames from all three stream files and merge-sort by timestamp.
    fn merge_read_frames(&self) -> Result<Vec<Frame>> {
        let mut all_frames = Vec::new();
        for (filename, _) in &STREAM_FILES {
            let path = self.log_dir.join(filename);
            all_frames.extend(read_all_frames_from_file(&path)?);
        }
        all_frames.sort_by_key(|f| f.timestamp_ns);
        Ok(all_frames)
    }

    /// 3-way merge taking only the first `n` frames by timestamp.
    fn merge_read_head(&self, n: usize) -> Result<Vec<Frame>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        // Open iterators for each stream file
        let mut iters: Vec<std::iter::Peekable<std::vec::IntoIter<Frame>>> = Vec::new();
        for (filename, _) in &STREAM_FILES {
            let path = self.log_dir.join(filename);
            let frames = read_all_frames_from_file(&path)?;
            iters.push(frames.into_iter().peekable());
        }

        let mut result = Vec::with_capacity(n);
        for _ in 0..n {
            // Find the iterator with the smallest next timestamp
            let mut best_idx: Option<usize> = None;
            let mut best_ts = u64::MAX;
            for (i, iter) in iters.iter_mut().enumerate() {
                if let Some(frame) = iter.peek() {
                    if frame.timestamp_ns < best_ts {
                        best_ts = frame.timestamp_ns;
                        best_idx = Some(i);
                    }
                }
            }
            match best_idx {
                Some(idx) => result.push(iters[idx].next().unwrap()),
                None => break, // All iterators exhausted
            }
        }
        Ok(result)
    }

    /// Read all frames from all three files, merge-sort, take last `n`.
    fn merge_read_tail(&self, n: usize) -> Result<Vec<Frame>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let mut all_frames = self.merge_read_frames()?;
        let skip = all_frames.len().saturating_sub(n);
        Ok(all_frames.split_off(skip))
    }

    /// Read all three stream files, merge-sort, then filter by time range.
    fn merge_read_range(
        &self,
        start_time: Option<u64>,
        end_time: Option<u64>,
    ) -> Result<Vec<Frame>> {
        // Each stream file is already sorted by timestamp, so we can do a
        // 3-way merge with early termination on end_time.
        let mut iters: Vec<std::iter::Peekable<std::vec::IntoIter<Frame>>> = Vec::new();
        for (filename, _) in &STREAM_FILES {
            let path = self.log_dir.join(filename);
            let frames = read_all_frames_from_file(&path)?;
            iters.push(frames.into_iter().peekable());
        }

        let mut result = Vec::new();
        loop {
            let mut best_idx: Option<usize> = None;
            let mut best_ts = u64::MAX;
            for (i, iter) in iters.iter_mut().enumerate() {
                if let Some(frame) = iter.peek() {
                    if frame.timestamp_ns < best_ts {
                        best_ts = frame.timestamp_ns;
                        best_idx = Some(i);
                    }
                }
            }
            match best_idx {
                Some(idx) => {
                    let frame = iters[idx].next().unwrap();
                    if let Some(end) = end_time {
                        if frame.timestamp_ns > end {
                            break;
                        }
                    }
                    if let Some(start) = start_time {
                        if frame.timestamp_ns < start {
                            continue;
                        }
                    }
                    result.push(frame);
                }
                None => break,
            }
        }
        Ok(result)
    }

    /// Search all three stream files, merge results by timestamp, limit to max_matches.
    fn merge_search(&self, pattern: &regex::Regex, max_matches: usize) -> Result<Vec<LogMatch>> {
        // Collect matches from each stream, then merge by timestamp.
        let mut all_matches = Vec::new();
        for (filename, _) in &STREAM_FILES {
            let path = self.log_dir.join(filename);
            let frames = read_all_frames_from_file(&path)?;
            for (idx, frame) in frames.iter().enumerate() {
                if let Ok(text) = std::str::from_utf8(&frame.payload) {
                    for line in text.lines() {
                        if pattern.is_match(line) {
                            all_matches.push(LogMatch {
                                frame_index: idx,
                                timestamp_ns: frame.timestamp_ns,
                                stream_type: frame.stream_type,
                                line: line.to_string(),
                            });
                        }
                    }
                }
            }
        }
        all_matches.sort_by_key(|m| m.timestamp_ns);
        all_matches.truncate(max_matches);
        Ok(all_matches)
    }
}

#[derive(Debug, Clone)]
pub struct LogMatch {
    pub frame_index: usize,
    pub timestamp_ns: u64,
    pub stream_type: StreamType,
    pub line: String,
}

impl serde::Serialize for LogMatch {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("LogMatch", 4)?;
        state.serialize_field("frame_index", &self.frame_index)?;
        state.serialize_field("timestamp_ns", &self.timestamp_ns)?;
        state.serialize_field("stream", self.stream_type.as_str())?;
        state.serialize_field("line", &self.line)?;
        state.end()
    }
}

/// A JSON-serializable representation of a log frame.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JsonFrame {
    pub timestamp_ns: u64,
    pub stream: String,
    pub text: String,
}

impl JsonFrame {
    /// Convert a `Frame` to a `JsonFrame`, decoding the payload as UTF-8 (lossy).
    pub fn from_frame(frame: &Frame) -> Self {
        Self {
            timestamp_ns: frame.timestamp_ns,
            stream: frame.stream_type.as_str().to_string(),
            text: String::from_utf8_lossy(&frame.payload).into_owned(),
        }
    }
}

fn read_one_frame(reader: &mut impl Read) -> Result<Option<Frame>> {
    let mut header = [0u8; FRAME_HEADER_SIZE];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }

    let timestamp_ns = u64::from_le_bytes(header[0..8].try_into().unwrap());
    let stream_type = StreamType::from_u8(header[8]).context("Invalid stream type byte")?;
    let length = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;

    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;

    Ok(Some(Frame {
        timestamp_ns,
        stream_type,
        payload,
    }))
}

/// Read all frames from a single file. Returns an empty vec if the file does not exist.
fn read_all_frames_from_file(path: &Path) -> Result<Vec<Frame>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();
    loop {
        match read_one_frame(&mut reader) {
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

/// Returns (stdout, stderr, stdin, combined) file sizes in bytes for a log directory.
/// Combined size is the sum of all three stream files.
pub fn log_sizes(log_dir: &Path) -> (u64, u64, u64, u64) {
    let size = |name: &str| -> u64 {
        std::fs::metadata(log_dir.join(name))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    let stdout_sz = size("stdout.log");
    let stderr_sz = size("stderr.log");
    let stdin_sz = size("stdin.log");
    let combined_sz = stdout_sz + stderr_sz + stdin_sz;
    (stdout_sz, stderr_sz, stdin_sz, combined_sz)
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

        // Read combined (merge-sorted from three stream files)
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let frames = reader.read_frames().unwrap();
        assert_eq!(frames.len(), 4);
        // Verify merge-sort order: timestamps 1000, 2000, 3000, 4000
        assert_eq!(frames[0].timestamp_ns, 1000);
        assert_eq!(frames[0].payload, b"out1\n");
        assert_eq!(frames[1].timestamp_ns, 2000);
        assert_eq!(frames[1].payload, b"err1\n");
        assert_eq!(frames[2].timestamp_ns, 3000);
        assert_eq!(frames[2].payload, b"in1\n");
        assert_eq!(frames[3].timestamp_ns, 4000);
        assert_eq!(frames[3].payload, b"out2\n");
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

        assert_eq!(combined_sz, stdout_sz + stderr_sz + stdin_sz);
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

    #[tokio::test]
    async fn read_frames_from_offset_reads_only_new_frames() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        // Write 3 initial frames
        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        for i in 0..3u64 {
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

        // Read all frames to get the end offset
        let (all_frames, offset) = reader.read_frames_from_offset(0).unwrap();
        assert_eq!(all_frames.len(), 3);
        assert!(offset > 0);

        // Reading from end offset should return no new frames
        let (new_frames, same_offset) = reader.read_frames_from_offset(offset).unwrap();
        assert_eq!(new_frames.len(), 0);
        assert_eq!(same_offset, offset);

        // Append more frames by writing directly to the file
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(dir.path().join("stdout.log"))
                .unwrap();
            let frame = make_frame(StreamType::Stdout, b"line3\n", 3000);
            file.write_all(&encode_frame(&frame)).unwrap();
            let frame = make_frame(StreamType::Stdout, b"line4\n", 4000);
            file.write_all(&encode_frame(&frame)).unwrap();
        }

        // Read from previous offset should only return the 2 new frames
        let (new_frames, new_offset) = reader.read_frames_from_offset(offset).unwrap();
        assert_eq!(new_frames.len(), 2);
        assert_eq!(new_frames[0].payload, b"line3\n");
        assert_eq!(new_frames[1].payload, b"line4\n");
        assert!(new_offset > offset);
    }

    #[test]
    fn read_frames_from_offset_missing_file() {
        let reader = LogReader::new(Path::new("/nonexistent"), StreamFilter::Stdout);
        let (frames, offset) = reader.read_frames_from_offset(0).unwrap();
        assert_eq!(frames.len(), 0);
        assert_eq!(offset, 0);
    }

    #[tokio::test]
    async fn read_frames_from_offset_mid_file() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().to_path_buf();

        // Write 5 frames
        let (tx, rx) = mpsc::channel(64);
        let writer = LogWriter::new(log_dir.clone(), rx, 50, 4096);

        for i in 0..5u64 {
            tx.send(make_frame(
                StreamType::Stdout,
                format!("msg{}\n", i).as_bytes(),
                i * 1000,
            ))
            .await
            .unwrap();
        }
        drop(tx);
        writer.run().await.unwrap();

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);

        // Read first 2 frames, then continue from offset
        let (batch1, offset1) = reader.read_frames_from_offset(0).unwrap();
        assert_eq!(batch1.len(), 5);

        // Compute offset of frame 2 by reading incrementally
        // Each frame has FRAME_HEADER_SIZE (13) + payload length
        // "msg0\n" = 5 bytes, so frame size = 13 + 5 = 18 bytes
        let frame_size = FRAME_HEADER_SIZE + 5; // "msgN\n" is 5 bytes
        let mid_offset = (frame_size * 2) as u64;

        let (batch2, offset2) = reader.read_frames_from_offset(mid_offset).unwrap();
        assert_eq!(batch2.len(), 3);
        assert_eq!(batch2[0].payload, b"msg2\n");
        assert_eq!(batch2[1].payload, b"msg3\n");
        assert_eq!(batch2[2].payload, b"msg4\n");
        assert_eq!(offset2, offset1);
    }

    /// Helper: write frames directly to a file for unit tests that don't need LogWriter.
    fn write_frames_to_file(path: &Path, frames: &[Frame]) {
        let mut file = std::fs::File::create(path).unwrap();
        for frame in frames {
            file.write_all(&encode_frame(frame)).unwrap();
        }
        file.flush().unwrap();
    }

    #[test]
    fn read_head_stops_early() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        let frames: Vec<Frame> = (0..100)
            .map(|i| {
                make_frame(
                    StreamType::Stdout,
                    format!("line{}\n", i).as_bytes(),
                    i * 1000,
                )
            })
            .collect();
        write_frames_to_file(&log_path, &frames);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);

        // read_head(1) should return exactly the first frame
        let head1 = reader.read_head(1).unwrap();
        assert_eq!(head1.len(), 1);
        assert_eq!(head1[0].payload, b"line0\n");
        assert_eq!(head1[0].timestamp_ns, 0);

        // read_head(3) should return exactly the first 3 frames
        let head3 = reader.read_head(3).unwrap();
        assert_eq!(head3.len(), 3);
        for (i, frame) in head3.iter().enumerate() {
            assert_eq!(frame.payload, format!("line{}\n", i).as_bytes());
            assert_eq!(frame.timestamp_ns, i as u64 * 1000);
        }
    }

    #[test]
    fn read_head_with_n_greater_than_total_frames() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        let frames: Vec<Frame> = (0..3)
            .map(|i| make_frame(StreamType::Stdout, format!("f{}", i).as_bytes(), i * 1000))
            .collect();
        write_frames_to_file(&log_path, &frames);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let head = reader.read_head(100).unwrap();
        assert_eq!(head.len(), 3);
    }

    #[test]
    fn read_head_zero() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        let frames = vec![make_frame(StreamType::Stdout, b"data", 1000)];
        write_frames_to_file(&log_path, &frames);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let head = reader.read_head(0).unwrap();
        assert_eq!(head.len(), 0);
    }

    #[test]
    fn read_head_empty_file() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        write_frames_to_file(&log_path, &[]);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let head = reader.read_head(5).unwrap();
        assert_eq!(head.len(), 0);
    }

    #[test]
    fn read_head_missing_file() {
        let reader = LogReader::new(Path::new("/nonexistent"), StreamFilter::Stdout);
        let head = reader.read_head(5).unwrap();
        assert_eq!(head.len(), 0);
    }

    #[test]
    fn read_tail_uses_ring_buffer() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        let frames: Vec<Frame> = (0..100)
            .map(|i| {
                make_frame(
                    StreamType::Stdout,
                    format!("line{}\n", i).as_bytes(),
                    i * 1000,
                )
            })
            .collect();
        write_frames_to_file(&log_path, &frames);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);

        // read_tail(1) should return only the last frame
        let tail1 = reader.read_tail(1).unwrap();
        assert_eq!(tail1.len(), 1);
        assert_eq!(tail1[0].payload, b"line99\n");
        assert_eq!(tail1[0].timestamp_ns, 99 * 1000);

        // read_tail(3) should return the last 3 frames
        let tail3 = reader.read_tail(3).unwrap();
        assert_eq!(tail3.len(), 3);
        for (j, i) in (97..100).enumerate() {
            assert_eq!(tail3[j].payload, format!("line{}\n", i).as_bytes());
            assert_eq!(tail3[j].timestamp_ns, i as u64 * 1000);
        }
    }

    #[test]
    fn read_tail_with_n_greater_than_total_frames() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        let frames: Vec<Frame> = (0..3)
            .map(|i| make_frame(StreamType::Stdout, format!("f{}", i).as_bytes(), i * 1000))
            .collect();
        write_frames_to_file(&log_path, &frames);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let tail = reader.read_tail(100).unwrap();
        assert_eq!(tail.len(), 3);
        // Verify order is preserved
        for (i, frame) in tail.iter().enumerate() {
            assert_eq!(frame.payload, format!("f{}", i).as_bytes());
        }
    }

    #[test]
    fn read_tail_zero() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        let frames = vec![make_frame(StreamType::Stdout, b"data", 1000)];
        write_frames_to_file(&log_path, &frames);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let tail = reader.read_tail(0).unwrap();
        assert_eq!(tail.len(), 0);
    }

    #[test]
    fn read_tail_empty_file() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        write_frames_to_file(&log_path, &[]);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let tail = reader.read_tail(5).unwrap();
        assert_eq!(tail.len(), 0);
    }

    #[test]
    fn read_tail_missing_file() {
        let reader = LogReader::new(Path::new("/nonexistent"), StreamFilter::Stdout);
        let tail = reader.read_tail(5).unwrap();
        assert_eq!(tail.len(), 0);
    }

    #[test]
    fn read_head_and_tail_match_read_frames() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("stdout.log");
        let frames: Vec<Frame> = (0..20)
            .map(|i| {
                make_frame(
                    StreamType::Stdout,
                    format!("msg{}\n", i).as_bytes(),
                    i * 500,
                )
            })
            .collect();
        write_frames_to_file(&log_path, &frames);

        let reader = LogReader::new(dir.path(), StreamFilter::Stdout);
        let all_frames = reader.read_frames().unwrap();
        assert_eq!(all_frames.len(), 20);

        // read_head should match first N of read_frames
        for n in [1, 5, 10, 20, 30] {
            let head = reader.read_head(n).unwrap();
            let expected: Vec<_> = all_frames.iter().take(n).collect();
            assert_eq!(
                head.len(),
                expected.len(),
                "read_head({}) length mismatch",
                n
            );
            for (h, e) in head.iter().zip(expected.iter()) {
                assert_eq!(h.payload, e.payload);
                assert_eq!(h.timestamp_ns, e.timestamp_ns);
                assert_eq!(h.stream_type, e.stream_type);
            }
        }

        // read_tail should match last N of read_frames
        for n in [1, 5, 10, 20, 30] {
            let tail = reader.read_tail(n).unwrap();
            let skip = all_frames.len().saturating_sub(n);
            let expected: Vec<_> = all_frames.iter().skip(skip).collect();
            assert_eq!(
                tail.len(),
                expected.len(),
                "read_tail({}) length mismatch",
                n
            );
            for (t, e) in tail.iter().zip(expected.iter()) {
                assert_eq!(t.payload, e.payload);
                assert_eq!(t.timestamp_ns, e.timestamp_ns);
                assert_eq!(t.stream_type, e.stream_type);
            }
        }
    }

    // ── Combined merge-sort tests ─────────────────────────────────────

    /// Helper: set up a directory with separate stream files for merge-sort tests
    /// to test merge-sort behaviour.
    fn setup_merge_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        // stdout: ts 100, 300, 500
        write_frames_to_file(
            &dir.path().join("stdout.log"),
            &[
                make_frame(StreamType::Stdout, b"out1\n", 100),
                make_frame(StreamType::Stdout, b"out2\n", 300),
                make_frame(StreamType::Stdout, b"out3\n", 500),
            ],
        );
        // stderr: ts 200, 400
        write_frames_to_file(
            &dir.path().join("stderr.log"),
            &[
                make_frame(StreamType::Stderr, b"err1\n", 200),
                make_frame(StreamType::Stderr, b"err2\n", 400),
            ],
        );
        // stdin: ts 150
        write_frames_to_file(
            &dir.path().join("stdin.log"),
            &[make_frame(StreamType::Stdin, b"in1\n", 150)],
        );
        dir
    }

    #[test]
    fn combined_merge_read_frames() {
        let dir = setup_merge_dir();
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        assert!(reader.is_combined());
        let frames = reader.read_frames().unwrap();
        assert_eq!(frames.len(), 6);
        // Should be sorted by timestamp: 100, 150, 200, 300, 400, 500
        let timestamps: Vec<u64> = frames.iter().map(|f| f.timestamp_ns).collect();
        assert_eq!(timestamps, vec![100, 150, 200, 300, 400, 500]);
    }

    #[test]
    fn combined_merge_read_head() {
        let dir = setup_merge_dir();
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let head = reader.read_head(3).unwrap();
        assert_eq!(head.len(), 3);
        assert_eq!(head[0].timestamp_ns, 100);
        assert_eq!(head[0].payload, b"out1\n");
        assert_eq!(head[1].timestamp_ns, 150);
        assert_eq!(head[1].payload, b"in1\n");
        assert_eq!(head[2].timestamp_ns, 200);
        assert_eq!(head[2].payload, b"err1\n");
    }

    #[test]
    fn combined_merge_read_tail() {
        let dir = setup_merge_dir();
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let tail = reader.read_tail(2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].timestamp_ns, 400);
        assert_eq!(tail[1].timestamp_ns, 500);
    }

    #[test]
    fn combined_merge_read_range() {
        let dir = setup_merge_dir();
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let range = reader.read_range(Some(150), Some(400)).unwrap();
        assert_eq!(range.len(), 4); // ts 150, 200, 300, 400
        let timestamps: Vec<u64> = range.iter().map(|f| f.timestamp_ns).collect();
        assert_eq!(timestamps, vec![150, 200, 300, 400]);
    }

    #[test]
    fn combined_merge_search() {
        let dir = setup_merge_dir();
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let pattern = regex::Regex::new("err").unwrap();
        let matches = reader.search(&pattern, 10).unwrap();
        assert_eq!(matches.len(), 2);
        // Results should be sorted by timestamp
        assert_eq!(matches[0].timestamp_ns, 200);
        assert_eq!(matches[1].timestamp_ns, 400);
    }

    #[test]
    fn combined_merge_file_size() {
        let dir = setup_merge_dir();
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let stdout_sz = LogReader::new(dir.path(), StreamFilter::Stdout)
            .file_size()
            .unwrap();
        let stderr_sz = LogReader::new(dir.path(), StreamFilter::Stderr)
            .file_size()
            .unwrap();
        let stdin_sz = LogReader::new(dir.path(), StreamFilter::Stdin)
            .file_size()
            .unwrap();
        assert_eq!(
            reader.file_size().unwrap(),
            stdout_sz + stderr_sz + stdin_sz
        );
    }

    #[test]
    fn combined_merge_read_frames_since() {
        let dir = setup_merge_dir();
        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        let frames = reader.read_frames_since(200).unwrap();
        assert_eq!(frames.len(), 3); // ts 300, 400, 500
        let timestamps: Vec<u64> = frames.iter().map(|f| f.timestamp_ns).collect();
        assert_eq!(timestamps, vec![300, 400, 500]);
    }

    #[test]
    fn combined_merge_empty_streams() {
        let dir = TempDir::new().unwrap();
        // Create empty stream files
        write_frames_to_file(&dir.path().join("stdout.log"), &[]);
        write_frames_to_file(&dir.path().join("stderr.log"), &[]);
        write_frames_to_file(&dir.path().join("stdin.log"), &[]);

        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        assert!(reader.is_combined());
        assert_eq!(reader.read_frames().unwrap().len(), 0);
        assert_eq!(reader.read_head(5).unwrap().len(), 0);
        assert_eq!(reader.read_tail(5).unwrap().len(), 0);
    }

    #[test]
    fn combined_merge_missing_stream_files() {
        let dir = TempDir::new().unwrap();
        // Only create stdout, leave others missing
        write_frames_to_file(
            &dir.path().join("stdout.log"),
            &[make_frame(StreamType::Stdout, b"hello\n", 100)],
        );

        let reader = LogReader::new(dir.path(), StreamFilter::Combined);
        assert!(reader.is_combined());
        let frames = reader.read_frames().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"hello\n");
    }
}
