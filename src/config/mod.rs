use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub llm: LlmConfig,
    pub storage: StorageConfig,
    pub port_detection: PortDetectionConfig,
    pub capture: CaptureConfig,
    pub enrichment: EnrichmentConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            storage: StorageConfig::default(),
            port_detection: PortDetectionConfig::default(),
            capture: CaptureConfig::default(),
            enrichment: EnrichmentConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: None,
            model: None,
            api_key: None,
            base_url: None,
            timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub base_dir: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            base_dir: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("~"))
                .join(".brainlog"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PortDetectionConfig {
    pub enabled: bool,
    pub poll_interval_secs: u64,
    pub method: String,
}

impl Default for PortDetectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_secs: 2,
            method: "lsof".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub flush_interval_ms: u64,
    pub flush_buffer_bytes: usize,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            flush_interval_ms: 100,
            flush_buffer_bytes: 65536,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EnrichmentConfig {
    pub enabled: bool,
    pub project_file_patterns: Vec<String>,
    pub max_file_preview_bytes: usize,
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            project_file_patterns: vec![
                "package.json".to_string(),
                "Cargo.toml".to_string(),
                "go.mod".to_string(),
                "pyproject.toml".to_string(),
                "Makefile".to_string(),
                "docker-compose.yml".to_string(),
            ],
            max_file_preview_bytes: 2048,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let config: Config = serde_yaml::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    pub fn config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".brainlog")
            .join("config.yaml")
    }

    pub fn base_dir(&self) -> &Path {
        &self.storage.base_dir
    }

    pub fn db_path(&self) -> PathBuf {
        self.storage.base_dir.join("brainlog.db")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.storage.base_dir.join("logs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = Config::default();
        assert!(config.port_detection.enabled);
        assert_eq!(config.port_detection.poll_interval_secs, 2);
        assert_eq!(config.port_detection.method, "lsof");
        assert_eq!(config.capture.flush_interval_ms, 100);
        assert_eq!(config.capture.flush_buffer_bytes, 65536);
        assert_eq!(config.llm.timeout_secs, 30);
        assert!(config.llm.provider.is_none());
        assert!(config.enrichment.enabled);
    }

    #[test]
    fn db_path_under_base_dir() {
        let config = Config::default();
        let db_path = config.db_path();
        assert!(db_path.ends_with("brainlog.db"));
        assert!(db_path.starts_with(config.base_dir()));
    }

    #[test]
    fn logs_dir_under_base_dir() {
        let config = Config::default();
        let logs = config.logs_dir();
        assert!(logs.ends_with("logs"));
        assert!(logs.starts_with(config.base_dir()));
    }

    #[test]
    fn config_path_is_yaml() {
        let path = Config::config_path();
        assert!(path.ends_with("config.yaml"));
    }

    #[test]
    fn enrichment_defaults() {
        let config = Config::default();
        assert!(config
            .enrichment
            .project_file_patterns
            .contains(&"Cargo.toml".to_string()));
        assert!(config
            .enrichment
            .project_file_patterns
            .contains(&"package.json".to_string()));
        assert_eq!(config.enrichment.max_file_preview_bytes, 2048);
    }
}
