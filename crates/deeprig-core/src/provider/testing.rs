//! `MockProvider` — a programmable `Provider` for tests.
//!
//! Scripted responses are pushed into a FIFO queue; each `chat_stream`
//! call pops the next batch and turns it into a stream. Lets us
//! exercise the REPL's dispatcher, the permission gate, and any future
//! orchestration code without making real HTTP calls.
//!
//! Lives in `provider::testing` (always-on, no feature flag) so
//! integration tests in `crates/deeprig-core/tests/` and downstream
//! embedders can both use it.

use anyhow::Result;
use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use std::collections::VecDeque;
use std::sync::Mutex;

use super::{ChatOptions, Provider, StreamEvent};
use crate::{FunctionCall, Message, ToolCall, ToolDefinition};

/// A `Provider` that replays pre-canned event sequences.
///
/// Each `chat_stream` call pops the next batch. If the queue is empty
/// the provider yields a single placeholder `Content` event so tests
/// fail loudly rather than hang.
pub struct MockProvider {
    name: String,
    default_model: String,
    scripted: Mutex<VecDeque<Vec<StreamEvent>>>,
}

impl MockProvider {
    /// Creates an empty mock with name `"mock"` and default model
    /// `"mock-model"`.
    pub fn new() -> Self {
        MockProvider {
            name: "mock".into(),
            default_model: "mock-model".into(),
            scripted: Mutex::new(VecDeque::new()),
        }
    }

    /// Appends a raw event sequence to the FIFO queue.
    pub fn push_response(&self, events: Vec<StreamEvent>) {
        self.scripted.lock().unwrap().push_back(events);
    }

    /// Convenience: a single text reply.
    pub fn push_text(&self, content: &str) {
        self.push_response(vec![StreamEvent::Content(content.into())]);
    }

    /// Convenience: a tool-call-only reply with the given id, function
    /// name, and JSON-encoded arguments string.
    pub fn push_tool_call(&self, id: &str, name: &str, args: &str) {
        self.push_response(vec![StreamEvent::ToolCalls(vec![ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }])]);
    }

    /// Number of scripted responses still in the queue.
    pub fn pending(&self) -> usize {
        self.scripted.lock().unwrap().len()
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        MockProvider::new()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _opts: &ChatOptions,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        let events = self
            .scripted
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| vec![StreamEvent::Content("(mock: no scripted response)".into())]);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn plays_back_a_text_response() {
        let m = MockProvider::new();
        m.push_text("hello");
        let mut s = m
            .chat_stream(&[], &[], &ChatOptions::new("any"))
            .await
            .unwrap();
        let mut got = Vec::new();
        while let Some(ev) = s.next().await {
            got.push(ev.unwrap());
        }
        assert_eq!(got.len(), 1);
        match &got[0] {
            StreamEvent::Content(c) => assert_eq!(c, "hello"),
            other => panic!("expected Content, got {other:?}"),
        }
        assert_eq!(m.pending(), 0);
    }

    #[tokio::test]
    async fn plays_back_a_tool_call() {
        let m = MockProvider::new();
        m.push_tool_call("call_1", "read_file", r#"{"path":"x"}"#);
        let mut s = m
            .chat_stream(&[], &[], &ChatOptions::new("any"))
            .await
            .unwrap();
        let mut got = Vec::new();
        while let Some(ev) = s.next().await {
            got.push(ev.unwrap());
        }
        assert_eq!(got.len(), 1);
        match &got[0] {
            StreamEvent::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].function.name, "read_file");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_queue_yields_placeholder() {
        let m = MockProvider::new();
        let mut s = m
            .chat_stream(&[], &[], &ChatOptions::new("any"))
            .await
            .unwrap();
        let ev = s.next().await.unwrap().unwrap();
        match ev {
            StreamEvent::Content(c) => assert!(c.contains("no scripted response")),
            other => panic!("expected placeholder Content, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fifo_order_preserved_across_calls() {
        let m = MockProvider::new();
        m.push_text("first");
        m.push_text("second");
        assert_eq!(m.pending(), 2);

        for expected in ["first", "second"] {
            let mut s = m
                .chat_stream(&[], &[], &ChatOptions::new("any"))
                .await
                .unwrap();
            let ev = s.next().await.unwrap().unwrap();
            match ev {
                StreamEvent::Content(c) => assert_eq!(c, expected),
                other => panic!("expected Content, got {other:?}"),
            }
        }
        assert_eq!(m.pending(), 0);
    }
}
