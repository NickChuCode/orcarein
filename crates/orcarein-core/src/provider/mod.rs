//! `trait Provider` — the seam over model backends.
//!
//! Chapter 13 introduces this abstraction together with two real
//! implementations (`DeepSeekProvider`, `OpenAIProvider`) and a
//! programmable `MockProvider` for tests, satisfying spec decision D10
//! ("never introduce a trait with only one impl").
//!
//! The trait emits a stream of `StreamEvent`s rather than driving a
//! caller-supplied callback — the trait method signature cannot use
//! `impl FnMut`, and a `BoxStream` keeps backpressure under the
//! caller's control.

pub mod deepseek;
pub mod openai;
mod openai_compat;
pub mod retry;
pub mod testing;

pub use deepseek::DeepSeekProvider;
pub use openai::OpenAIProvider;
pub use retry::RetryPolicy;
pub use testing::MockProvider;

use async_trait::async_trait;
use futures_util::stream::BoxStream;

use crate::{Message, TokenUsage, ToolCall, ToolDefinition};

/// One event in a streaming chat response.
///
/// `Reasoning` is emitted by DeepSeek V4 in thinking mode; OpenAI never
/// emits it. `ToolCalls` is emitted **once** at the end of the stream
/// when the provider has finished assembling all tool calls. `Usage`
/// is emitted **once** when the model reports its token usage.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Reasoning(String),
    Content(String),
    ToolCalls(Vec<ToolCall>),
    Usage(TokenUsage),
}

/// Per-call provider options. v0.1 carries only the model name; future
/// chapters will add `temperature`, `max_tokens`, etc. as needed.
#[derive(Debug, Clone)]
pub struct ChatOptions {
    pub model: String,
}

impl ChatOptions {
    /// Builds an options bundle for the named model.
    pub fn new(model: impl Into<String>) -> Self {
        ChatOptions {
            model: model.into(),
        }
    }
}

/// A model backend.
///
/// `Provider` is the seam every Ch13+ chapter relies on for swapping
/// models, mocking in tests, and (Ch24) running headless against issue
/// payloads.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Short, stable identifier used in banner and diagnostics
    /// (e.g. `"deepseek"`, `"openai"`, `"mock"`).
    fn name(&self) -> &str;

    /// Default model when none is supplied on the command line.
    fn default_model(&self) -> &str;

    /// Streams a chat completion. The returned stream emits
    /// [`StreamEvent`]s in arrival order and terminates when the
    /// provider has nothing more to say. Network errors mid-stream
    /// surface as `Err` items.
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        opts: &ChatOptions,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>>;

    /// List the model ids available to this provider/account, so the REPL can
    /// offer a `/model` picker and validate switches. The default returns just
    /// [`Provider::default_model`]; network-backed providers override it to query
    /// their catalog (`GET /v1/models`).
    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec![self.default_model().to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_options_new_sets_model() {
        let o = ChatOptions::new("gpt-4o-mini");
        assert_eq!(o.model, "gpt-4o-mini");
    }

    #[test]
    fn chat_options_is_clone() {
        let o = ChatOptions::new("m");
        let _ = o.clone();
    }

    #[test]
    fn stream_event_debug_contains_variant() {
        let s = format!("{:?}", StreamEvent::Content("hi".into()));
        assert!(s.contains("Content"));
        assert!(s.contains("hi"));
    }
}
