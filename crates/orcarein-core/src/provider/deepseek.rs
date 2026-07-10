//! DeepSeek V4 `Provider` implementation.
//!
//! The DeepSeek Chat Completions endpoint is OpenAI-compatible, so the
//! work happens in `openai_compat`. This module is a thin wrapper that
//! supplies the URL and bearer token.

use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::BoxStream;

use super::openai_compat;
use super::retry::RetryPolicy;
use super::{ChatOptions, Provider, StreamEvent};
use crate::{Message, ToolDefinition};

const API_URL: &str = "https://api.deepseek.com/v1/chat/completions";
const MODELS_URL: &str = "https://api.deepseek.com/v1/models";
const DEFAULT_MODEL: &str = "deepseek-v4-flash";

pub struct DeepSeekProvider {
    client: reqwest::Client,
    api_key: String,
    retry: RetryPolicy,
}

impl DeepSeekProvider {
    /// Creates a provider that authenticates with `api_key`.
    pub fn new(api_key: impl Into<String>) -> Self {
        DeepSeekProvider {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            retry: RetryPolicy::default(),
        }
    }

    /// Overrides the retry policy (the binary wires this from `config.toml`).
    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn default_model(&self) -> &str {
        DEFAULT_MODEL
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        opts: &ChatOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        openai_compat::chat_stream_compat(
            &self.client,
            API_URL,
            &self.api_key,
            &self.retry,
            messages,
            tools,
            opts,
        )
        .await
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        openai_compat::list_models_compat(&self.client, MODELS_URL, &self.api_key, &self.retry)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let p = DeepSeekProvider::new("dummy");
        assert_eq!(p.name(), "deepseek");
        assert_eq!(p.default_model(), "deepseek-v4-flash");
    }
}
