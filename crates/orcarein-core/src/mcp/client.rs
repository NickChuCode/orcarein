//! Protocol orchestration (generic, duplex-testable) + the concrete
//! `McpClient` (added in a later task).

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};

use super::protocol::{
    InitializeResult, RemoteTool, ToolCallResult, ToolsListResult, PROTOCOL_VERSION,
};
use super::transport::McpConnection;
use super::McpError;

/// Runs the MCP handshake: `initialize` request, then the
/// `notifications/initialized` notification.
#[allow(dead_code)]
pub(crate) async fn initialize<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    conn: &mut McpConnection<R, W>,
) -> Result<InitializeResult, McpError> {
    let params = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "orcarein", "version": env!("CARGO_PKG_VERSION") }
    });
    let result = conn.request("initialize", params).await?;
    let parsed: InitializeResult = serde_json::from_value(result)?;
    conn.notify("notifications/initialized", json!({})).await?;
    Ok(parsed)
}

/// Fetches the server's advertised tools.
#[allow(dead_code)]
pub(crate) async fn fetch_tools<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    conn: &mut McpConnection<R, W>,
) -> Result<Vec<RemoteTool>, McpError> {
    let result = conn.request("tools/list", json!({})).await?;
    let parsed: ToolsListResult = serde_json::from_value(result)?;
    Ok(parsed.tools)
}

/// Calls one tool and returns its concatenated text content.
/// `isError: true` -> `McpError`.
#[allow(dead_code)]
pub(crate) async fn invoke_tool<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    conn: &mut McpConnection<R, W>,
    name: &str,
    args: Value,
) -> Result<String, McpError> {
    let result = conn
        .request("tools/call", json!({ "name": name, "arguments": args }))
        .await?;
    let parsed: ToolCallResult = serde_json::from_value(result)?;
    let text: String = parsed
        .content
        .iter()
        .filter_map(|b| b.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    if parsed.is_error.unwrap_or(false) {
        return Err(McpError::Protocol(format!(
            "tool '{name}' reported an error: {text}"
        )));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader};

    // A scripted server that answers initialize/tools/list/tools/call generically.
    fn conn_with_server() -> crate::mcp::transport::McpConnection<
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    > {
        let (client_end, server_end) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let (sr, mut sw) = split(server_end);
            let mut br = BufReader::new(sr);
            loop {
                let mut line = String::new();
                if br.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let v: serde_json::Value = match serde_json::from_str(line.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let id = v["id"].clone();
                if id.is_null() {
                    continue;
                } // notification
                let method = v["method"].as_str().unwrap_or("");
                let result = match method {
                    "initialize" => {
                        json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"mock"}})
                    }
                    "tools/list" => {
                        json!({"tools":[{"name":"echo","description":"d","inputSchema":{"type":"object"}}]})
                    }
                    "tools/call" => {
                        json!({"content":[{"type":"text","text":"called echo"}],"isError":false})
                    }
                    _ => json!(null),
                };
                let resp = json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
                sw.write_all(resp.as_bytes()).await.unwrap();
                sw.write_all(b"\n").await.unwrap();
            }
        });
        let (cr, cw) = split(client_end);
        crate::mcp::transport::McpConnection::new(cr, cw)
    }

    #[tokio::test]
    async fn initialize_then_list_then_call() {
        let mut conn = conn_with_server();
        initialize(&mut conn).await.unwrap();
        let tools = fetch_tools(&mut conn).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let out = invoke_tool(&mut conn, "echo", json!({"x":1}))
            .await
            .unwrap();
        assert_eq!(out, "called echo");
    }

    #[tokio::test]
    async fn invoke_tool_is_error_maps_to_err() {
        let (client_end, server_end) = tokio::io::duplex(8192);
        tokio::spawn(async move {
            let (sr, mut sw) = split(server_end);
            let mut br = BufReader::new(sr);
            let mut line = String::new();
            br.read_line(&mut line).await.unwrap();
            let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            let resp = json!({"jsonrpc":"2.0","id":v["id"],"result":{"content":[{"type":"text","text":"boom"}],"isError":true}}).to_string();
            sw.write_all(resp.as_bytes()).await.unwrap();
            sw.write_all(b"\n").await.unwrap();
        });
        let (cr, cw) = split(client_end);
        let mut conn = crate::mcp::transport::McpConnection::new(cr, cw);
        let err = invoke_tool(&mut conn, "echo", json!({})).await.unwrap_err();
        assert!(matches!(err, McpError::Rpc { .. }) || matches!(err, McpError::Protocol(_)));
    }
}
