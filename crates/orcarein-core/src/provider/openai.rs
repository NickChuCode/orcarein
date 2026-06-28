//! OpenAI Chat Completions `Provider` implementation.
//!
//! Wire-format identical to DeepSeek modulo the URL, the bearer token,
//! and the absence of `reasoning_content`. Delegates the streaming work
//! to `openai_compat`.
//!
//! Ch13 ships this as an offline-only implementation — spec decision
//! D10 says introducing a trait without a second impl is an anti-
//! pattern, and OpenAI is the natural second impl since DeepSeek is
//! deliberately OpenAI-compatible. A live smoke test against
//! `api.openai.com` is left to the user (it costs real money).

use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::BoxStream;

use super::openai_compat;
use super::{ChatOptions, Provider, StreamEvent};
use crate::{Message, ToolDefinition};

const API_URL: &str = "https://api.openai.com/v1/chat/completions";
const MODELS_URL: &str = "https://api.openai.com/v1/models";
const DEFAULT_MODEL: &str = "gpt-4o-mini";

pub struct OpenAIProvider {
    client: reqwest::Client,
    api_key: String,
}

impl OpenAIProvider {
    /// Creates a provider that authenticates with `api_key`.
    pub fn new(api_key: impl Into<String>) -> Self {
        OpenAIProvider {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
        }
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
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
            messages,
            tools,
            opts,
        )
        .await
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        openai_compat::list_models_compat(&self.client, MODELS_URL, &self.api_key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let p = OpenAIProvider::new("dummy");
        assert_eq!(p.name(), "openai");
        assert_eq!(p.default_model(), "gpt-4o-mini");
    }
}
