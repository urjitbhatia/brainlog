use anyhow::{Context, Result};
use async_trait::async_trait;

use super::{LlmClient, Message};
use crate::config::LlmConfig;

pub struct AnthropicClient {
    model: String,
}

impl AnthropicClient {
    pub fn new(config: &LlmConfig) -> Self {
        Self {
            model: config
                .model
                .clone()
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string()),
        }
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn complete(&self, messages: Vec<Message>) -> Result<String> {
        // Use `claude -p` CLI for Anthropic
        let prompt = messages
            .iter()
            .map(|m| {
                if m.role == "system" {
                    format!("System: {}", m.content)
                } else {
                    m.content.clone()
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let output = tokio::process::Command::new("claude")
            .args(["-p", &prompt, "--model", &self.model])
            .output()
            .await
            .context("Failed to run `claude` CLI")?;

        if !output.status.success() {
            anyhow::bail!(
                "claude CLI failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}
