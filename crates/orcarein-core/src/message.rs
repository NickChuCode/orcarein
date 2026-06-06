//! Chat message types shared across OrcaRein.
//!
//! A conversation is, at bottom, a list of [`Message`]s. DeepSeek's
//! `/chat/completions` endpoint accepts and returns this same shape, so
//! `Message` doubles as both our internal model and the wire format.
//!
//! Chapter 9 extended `Message` with optional `tool_calls` (used on
//! `assistant` messages that ask for a tool) and `tool_call_id` (used on
//! `tool` role messages carrying a result).
//!
//! Chapter 10 (live-API patch) added `reasoning_content` — DeepSeek V4's
//! "thinking" trace. The API requires the assistant's `reasoning_content`
//! to be **passed back** on the next call when the assistant message had
//! one; dropping it crashes the second call of a tool loop with HTTP 400
//! "The `reasoning_content` in the thinking mode must be passed back to
//! the API."

use crate::tool::ToolCall;
use serde::{Deserialize, Serialize};

/// A single message in a conversation.
///
/// `role` is one of `"system"`, `"user"`, `"assistant"`, or `"tool"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who produced this message: `"system"` / `"user"` / `"assistant"` / `"tool"`.
    pub role: String,
    /// The message text. Can be empty when an assistant message carries only
    /// tool calls.
    pub content: String,
    /// DeepSeek V4 "thinking" trace (`assistant` role only). MUST be
    /// echoed back to the API on the next call when present, otherwise V4
    /// rejects the request with HTTP 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Tool calls the model wants to make (`assistant` role only). Empty
    /// vectors are omitted from the JSON wire format.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// ID of the tool call this message is the result of (`tool` role only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// Creates a `"system"` message (instructions that steer the assistant).
    pub fn system(content: impl Into<String>) -> Self {
        Message {
            role: "system".into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Creates a `"user"` message (input from the human).
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: "user".into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Creates an `"assistant"` message (output from the model, content only).
    pub fn assistant(content: impl Into<String>) -> Self {
        Message {
            role: "assistant".into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Creates an `"assistant"` message that carries tool calls.
    /// `content` can be empty when the model is purely requesting tools.
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Message {
            role: "assistant".into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Sets the V4 reasoning trace on an `assistant` message. Mandatory
    /// when the model produced thinking output — V4 rejects the next
    /// request otherwise.
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        let text = reasoning.into();
        if !text.is_empty() {
            self.reasoning_content = Some(text);
        }
        self
    }

    /// Creates a `"tool"` result message — sent back to the model after we
    /// executed a tool the model requested.
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            role: "tool".into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::FunctionCall;

    #[test]
    fn constructors_set_the_expected_role() {
        assert_eq!(Message::system("x").role, "system");
        assert_eq!(Message::user("x").role, "user");
        assert_eq!(Message::assistant("x").role, "assistant");
        assert_eq!(Message::tool("call_1", "x").role, "tool");
    }

    #[test]
    fn round_trips_through_json() {
        let original = Message::user("Hello, DeepSeek");
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn plain_user_message_omits_tool_fields_on_the_wire() {
        // Ch08 behaviour preserved: vanilla messages don't carry tool noise.
        let json = serde_json::to_value(Message::user("hi")).unwrap();
        assert_eq!(json, serde_json::json!({ "role": "user", "content": "hi" }));
    }

    #[test]
    fn assistant_with_tool_calls_serializes_them() {
        let tc = ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"Tokyo"}"#.into(),
            },
        };
        let m = Message::assistant_with_tool_calls("", vec![tc.clone()]);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "");
        assert_eq!(v["tool_calls"][0]["id"], "call_1");
        assert!(v.get("tool_call_id").is_none()); // omitted when None
    }

    #[test]
    fn tool_result_serializes_with_tool_call_id() {
        let m = Message::tool("call_1", "It's 22°C in Tokyo");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["content"], "It's 22°C in Tokyo");
        assert_eq!(v["tool_call_id"], "call_1");
        assert!(v.get("tool_calls").is_none()); // omitted when empty
        assert!(v.get("reasoning_content").is_none());
    }

    #[test]
    fn with_reasoning_attaches_thinking_trace() {
        let m = Message::assistant("the answer is 42").with_reasoning("let me think... it's 42");
        assert_eq!(
            m.reasoning_content.as_deref(),
            Some("let me think... it's 42")
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["reasoning_content"], "let me think... it's 42");
        assert_eq!(v["content"], "the answer is 42");
    }

    #[test]
    fn with_reasoning_skips_empty_string() {
        // Empty thinking → field stays None and is skipped on the wire.
        let m = Message::assistant("hi").with_reasoning("");
        assert!(m.reasoning_content.is_none());
        let v = serde_json::to_value(&m).unwrap();
        assert!(v.get("reasoning_content").is_none());
    }

    #[test]
    fn reasoning_round_trips_through_json() {
        // Critical for tool-loop replay: thinking that arrives in a
        // response must survive a round-trip back through the request body.
        let original = Message::assistant("done").with_reasoning("step 1; step 2");
        let json = serde_json::to_string(&original).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reasoning_content.as_deref(), Some("step 1; step 2"));
    }

    #[test]
    fn message_list_serializes_deterministically() {
        // Cache discipline: the request prefix must be byte-stable so
        // DeepSeek's auto-cache hits it. serde gives deterministic field
        // order; this guards against a future change (e.g. a HashMap on the
        // wire) silently breaking caching.
        let messages = vec![
            Message::system("you are helpful"),
            Message::user("hi"),
            Message::assistant("thinking").with_reasoning("step 1"),
            Message::assistant_with_tool_calls(
                "",
                vec![ToolCall {
                    id: "c1".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"x"}"#.into(),
                    },
                }],
            ),
            Message::tool("c1", "file contents"),
        ];
        let a = serde_json::to_string(&messages).unwrap();
        let b = serde_json::to_string(&messages).unwrap();
        assert_eq!(a, b);
    }
}
