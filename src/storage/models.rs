use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub executable: String,
    pub command_line: Vec<String>,
    pub working_dir: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub enrichment_status: EnrichmentStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentStatus {
    Pending,
    Completed,
    Failed,
    Skipped,
}

impl EnrichmentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "skipped" => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub service_id: String,
    pub pid: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub log_dir: String,
    pub status: RunStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Crashed,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Crashed => "crashed",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "crashed" => Self::Crashed,
            _ => Self::Running,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub service_id: String,
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Port {
    pub id: i64,
    pub run_id: String,
    pub port: u16,
    pub protocol: String,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum StreamType {
    Stdout = 0x01,
    Stderr = 0x02,
    Stdin = 0x03,
}

impl StreamType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x01 => Some(Self::Stdout),
            0x02 => Some(Self::Stderr),
            0x03 => Some(Self::Stdin),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Stdin => "stdin",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub timestamp_ns: u64,
    pub stream_type: StreamType,
    pub payload: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrichment_status_roundtrip() {
        for (s, expected) in [
            ("pending", EnrichmentStatus::Pending),
            ("completed", EnrichmentStatus::Completed),
            ("failed", EnrichmentStatus::Failed),
            ("skipped", EnrichmentStatus::Skipped),
        ] {
            let status = EnrichmentStatus::parse(s);
            assert_eq!(status, expected);
            assert_eq!(status.as_str(), s);
        }
    }

    #[test]
    fn enrichment_status_unknown_defaults_to_pending() {
        let status = EnrichmentStatus::parse("garbage");
        assert_eq!(status, EnrichmentStatus::Pending);
    }

    #[test]
    fn run_status_roundtrip() {
        for (s, expected) in [
            ("running", RunStatus::Running),
            ("completed", RunStatus::Completed),
            ("failed", RunStatus::Failed),
            ("crashed", RunStatus::Crashed),
        ] {
            let status = RunStatus::parse(s);
            assert_eq!(status, expected);
            assert_eq!(status.as_str(), s);
        }
    }

    #[test]
    fn run_status_unknown_defaults_to_running() {
        let status = RunStatus::parse("xyz");
        assert_eq!(status, RunStatus::Running);
    }

    #[test]
    fn stream_type_roundtrip() {
        for (byte, expected, name) in [
            (0x01, StreamType::Stdout, "stdout"),
            (0x02, StreamType::Stderr, "stderr"),
            (0x03, StreamType::Stdin, "stdin"),
        ] {
            let st = StreamType::from_u8(byte).unwrap();
            assert_eq!(st, expected);
            assert_eq!(st.as_str(), name);
            assert_eq!(st as u8, byte);
        }
    }

    #[test]
    fn stream_type_invalid_byte() {
        assert!(StreamType::from_u8(0x00).is_none());
        assert!(StreamType::from_u8(0x04).is_none());
        assert!(StreamType::from_u8(0xFF).is_none());
    }
}
