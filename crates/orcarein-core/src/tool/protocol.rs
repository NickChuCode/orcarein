//! Tool-calling protocol types — DeepSeek / OpenAI wire format.
//!
//! Chapter 9 defines the types and the streaming-accumulator (in
//! `orcarein::deepseek`). Chapter 10 introduces `trait Tool`, the first
//! concrete tool (`read_file`), and the dispatch loop that executes
//! these calls and feeds results back to the model.

use serde::{Deserialize, Serialize};

/// JSON Schema describing a tool's input parameters.
///
/// Loose `serde_json::Value` for v0.1 — Ch10's per-tool implementations
/// produce one of these from their input type's schema.
pub type ToolSchema = serde_json::Value;

/// A tool advertised to the model in the request body.
///
/// Wire shape (OpenAI/DeepSeek compatible):
/// ```json
/// {"type": "function", "function": {"name": "...", "description": "...", "parameters": {...}}}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDefinition,
}

impl ToolDefinition {
    /// Convenience constructor for a function tool.
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: ToolSchema,
    ) -> Self {
        ToolDefinition {
            kind: "function".into(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: ToolSchema,
}

/// One tool call requested by the model in its response.
///
/// Wire shape:
/// ```json
/// {"id": "call_abc", "type": "function",
///  "function": {"name": "...", "arguments": "{\"k\":\"v\"}"}}
/// ```
///
/// Note: `function.arguments` is a JSON-encoded **string**, not a parsed
/// object — call [`FunctionCall::parse_arguments`] to obtain a [`Value`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_call_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn default_call_kind() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded arguments string. Always parse before use.
    pub arguments: String,
}

impl FunctionCall {
    /// Parses [`Self::arguments`] from its JSON string form into a [`Value`].
    pub fn parse_arguments(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_definition_round_trips() {
        let td = ToolDefinition::function(
            "get_weather",
            "Look up the weather in a city",
            json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"],
            }),
        );
        let s = serde_json::to_string(&td).unwrap();
        let parsed: ToolDefinition = serde_json::from_str(&s).unwrap();
        assert_eq!(td, parsed);
    }

    #[test]
    fn tool_definition_serializes_with_type_field() {
        let td = ToolDefinition::function("x", "y", json!({}));
        let v = serde_json::to_value(&td).unwrap();
        assert_eq!(v["type"], "function"); // the `kind` field maps to "type"
        assert_eq!(v["function"]["name"], "x");
    }

    #[test]
    fn tool_call_round_trips() {
        let tc = ToolCall {
            id: "call_abc".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city":"Tokyo"}"#.into(),
            },
        };
        let s = serde_json::to_string(&tc).unwrap();
        let parsed: ToolCall = serde_json::from_str(&s).unwrap();
        assert_eq!(tc, parsed);
    }

    #[test]
    fn tool_call_deserializes_without_explicit_type_field() {
        // Some implementations omit "type"; default is "function".
        let body = r#"{"id":"call_x","function":{"name":"f","arguments":"{}"}}"#;
        let tc: ToolCall = serde_json::from_str(body).unwrap();
        assert_eq!(tc.kind, "function");
    }

    #[test]
    fn arguments_is_a_string_not_an_object() {
        // Critical wire-format quirk: arguments arrives as a JSON STRING.
        let body =
            r#"{"id":"x","type":"function","function":{"name":"f","arguments":"{\"k\":1}"}}"#;
        let tc: ToolCall = serde_json::from_str(body).unwrap();
        assert_eq!(tc.function.arguments, r#"{"k":1}"#);
        let parsed = tc.function.parse_arguments().unwrap();
        assert_eq!(parsed["k"], 1);
    }

    #[test]
    fn parse_arguments_errors_on_malformed_json() {
        let fc = FunctionCall {
            name: "x".into(),
            arguments: "not json".into(),
        };
        assert!(fc.parse_arguments().is_err());
    }
}
