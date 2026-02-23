pub mod anthropic;
pub mod enrichment;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::LlmConfig;

#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, messages: Vec<Message>) -> Result<String>;
}

pub fn create_client(config: &LlmConfig) -> Option<Box<dyn LlmClient>> {
    let provider = config.provider.as_deref()?;

    match provider {
        "anthropic" | "claude" => Some(Box::new(anthropic::AnthropicClient::new(config))),
        "ollama" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434/v1".to_string());
            Some(Box::new(openai::OpenAiClient::new(
                base_url,
                config.resolve_api_key(),
                config
                    .model
                    .clone()
                    .unwrap_or_else(|| "llama3.2:3b".to_string()),
                config.timeout_secs,
            )))
        }
        "openai" | "openrouter" | "gemini" => {
            let base_url = config.base_url.clone().unwrap_or_else(|| match provider {
                "openrouter" => "https://openrouter.ai/api/v1".to_string(),
                "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai".to_string(),
                _ => "https://api.openai.com/v1".to_string(),
            });
            Some(Box::new(openai::OpenAiClient::new(
                base_url,
                config.resolve_api_key(),
                config
                    .model
                    .clone()
                    .unwrap_or_else(|| "gpt-4o-mini".to_string()),
                config.timeout_secs,
            )))
        }
        _ => {
            // Try as generic OpenAI-compatible with custom base_url
            if let Some(ref base_url) = config.base_url {
                Some(Box::new(openai::OpenAiClient::new(
                    base_url.clone(),
                    config.resolve_api_key(),
                    config
                        .model
                        .clone()
                        .unwrap_or_else(|| "default".to_string()),
                    config.timeout_secs,
                )))
            } else {
                None
            }
        }
    }
}
