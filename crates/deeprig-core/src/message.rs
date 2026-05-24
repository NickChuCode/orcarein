//! Chat message types shared across DeepRig.
//!
//! A conversation is, at bottom, a list of [`Message`]s. DeepSeek's
//! `/chat/completions` endpoint accepts and returns this same shape, so
//! `Message` doubles as both our internal model and the wire format.
//!
//! Chapter 9 extended `Message` with optional `tool_calls` (used on
//! `assistant` messages that ask for a tool) and `tool_call_id` (used on
//! `tool` role messages carrying a result).

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
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Creates a `"user"` message (input from the human).
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: "user".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Creates an `"assistant"` message (output from the model, content only).
    pub fn assistant(content: impl Into<String>) -> Self {
        Message {
            role: "assistant".into(),
            content: content.into(),
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
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Creates a `"tool"` result message — sent back to the model after we
    /// executed a tool the model requested.
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Message {
            role: "tool".into(),
            content: content.into(),
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
    }
}
