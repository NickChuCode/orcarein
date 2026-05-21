//! Chat message types shared across DeepRig.
//!
//! A conversation is, at bottom, a list of [`Message`]s. DeepSeek's
//! `/chat/completions` endpoint accepts and returns this same shape, so
//! `Message` doubles as both our internal model and the wire format.

use serde::{Deserialize, Serialize};

/// A single message in a conversation.
///
/// `role` is one of `"system"`, `"user"`, or `"assistant"`. We keep it as a
/// plain `String` for now; a stricter enum would reject typos at compile time,
/// but the open API surface (providers add roles like `"tool"`) makes a string
/// the pragmatic v0.1 choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who produced this message: `"system"` / `"user"` / `"assistant"`.
    pub role: String,
    /// The message text.
    pub content: String,
}

impl Message {
    /// Creates a `"system"` message (instructions that steer the assistant).
    pub fn system(content: impl Into<String>) -> Self {
        Message {
            role: "system".into(),
            content: content.into(),
        }
    }

    /// Creates a `"user"` message (input from the human).
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: "user".into(),
            content: content.into(),
        }
    }

    /// Creates an `"assistant"` message (output from the model).
    pub fn assistant(content: impl Into<String>) -> Self {
        Message {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_the_expected_role() {
        assert_eq!(Message::system("x").role, "system");
        assert_eq!(Message::user("x").role, "user");
        assert_eq!(Message::assistant("x").role, "assistant");
    }

    #[test]
    fn round_trips_through_json() {
        let original = Message::user("Hello, DeepSeek");
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn serializes_to_the_wire_shape_deepseek_expects() {
        let json = serde_json::to_value(Message::user("hi")).expect("serialize");
        assert_eq!(json, serde_json::json!({ "role": "user", "content": "hi" }));
    }
}
