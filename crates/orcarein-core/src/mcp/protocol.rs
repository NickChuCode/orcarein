//! JSON-RPC 2.0 + MCP wire types (serde only, no I/O).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP protocol version this client advertises (latest as of the spec).
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// A JSON-RPC request (has an id; expects a response).
#[derive(Debug, Serialize)]
pub struct JsonRpcRequest<'a> {
    pub jsonrpc: &'a str, // always "2.0"
    pub id: u64,
    pub method: &'a str,
    pub params: Value,
}

/// A JSON-RPC notification (no id; no response).
#[derive(Debug, Serialize)]
pub struct JsonRpcNotification<'a> {
    pub jsonrpc: &'a str,
    pub method: &'a str,
    pub params: Value,
}

/// A parsed JSON-RPC response line. `id` is absent on notifications the
/// server may interleave; `result`/`error` are mutually exclusive.
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

/// `tools/list` result.
#[derive(Debug, Deserialize)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tools: Vec<RemoteTool>,
}

/// One tool as advertised by the server.
#[derive(Debug, Deserialize)]
pub struct RemoteTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

/// `tools/call` result.
#[derive(Debug, Deserialize)]
pub struct ToolCallResult {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(rename = "isError", default)]
    pub is_error: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_serializes_to_jsonrpc_2_0() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 7,
            method: "tools/list",
            params: json!({}),
        };
        let s = serde_json::to_string(&req).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "tools/list");
        assert!(v["params"].is_object());
    }

    #[test]
    fn parses_tools_list_result_with_input_schema_rename() {
        let raw = json!({
            "tools": [{
                "name": "echo",
                "description": "echoes input",
                "inputSchema": {"type": "object"}
            }]
        });
        let parsed: ToolsListResult = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.tools.len(), 1);
        assert_eq!(parsed.tools[0].name, "echo");
        assert_eq!(parsed.tools[0].description.as_deref(), Some("echoes input"));
        assert_eq!(parsed.tools[0].input_schema["type"], "object");
    }

    #[test]
    fn parses_tool_call_result_with_is_error_rename() {
        let raw = json!({
            "content": [{"type": "text", "text": "hello"}],
            "isError": false
        });
        let parsed: ToolCallResult = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.content.len(), 1);
        assert_eq!(parsed.content[0].kind, "text");
        assert_eq!(parsed.content[0].text.as_deref(), Some("hello"));
        assert_eq!(parsed.is_error, Some(false));
    }
}
