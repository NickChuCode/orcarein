//! Newline-delimited JSON-RPC 2.0 over any duplex byte stream.
//!
//! MCP stdio framing: one JSON message per line, no embedded newlines. The
//! transport is generic over `AsyncRead + AsyncWrite` so the protocol logic is
//! tested with `tokio::io::duplex()` (no subprocess); production wires a child
//! process's stdout/stdin.

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use super::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use super::McpError;

pub struct McpConnection<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> McpConnection<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        McpConnection {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        }
    }

    /// Sends a request and blocks until the matching-id response arrives,
    /// skipping any interleaved notifications / server-initiated requests
    /// (MVP does not service server->client calls).
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        loop {
            let mut buf = String::new();
            let n = self.reader.read_line(&mut buf).await?;
            if n == 0 {
                return Err(McpError::Closed);
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let resp: JsonRpcResponse = serde_json::from_str(trimmed)?;
            if resp.id != Some(id) {
                continue; // notification or unrelated message
            }
            if let Some(e) = resp.error {
                return Err(McpError::Rpc {
                    code: e.code,
                    message: e.message,
                });
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    /// Sends a fire-and-forget notification (no id, no response awaited).
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        let note = JsonRpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        };
        let mut line = serde_json::to_string(&note)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{split, AsyncWriteExt};

    // Builds a McpConnection wired to a scripted server over an in-memory duplex.
    fn connect_scripted<F, Fut>(
        server: F,
    ) -> McpConnection<
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    >
    where
        F: FnOnce(tokio::io::DuplexStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let (client_end, server_end) = tokio::io::duplex(8192);
        tokio::spawn(async move { server(server_end).await });
        let (cr, cw) = split(client_end);
        McpConnection::new(cr, cw)
    }

    #[tokio::test]
    async fn request_skips_notification_then_returns_matching_result() {
        let mut conn = connect_scripted(|end| async move {
            let (sr, mut sw) = split(end);
            let mut br = tokio::io::BufReader::new(sr);
            let mut line = String::new();
            use tokio::io::AsyncBufReadExt;
            br.read_line(&mut line).await.unwrap();
            sw.write_all(
                b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}\n",
            )
            .await
            .unwrap();
            sw.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n")
                .await
                .unwrap();
        });
        let result = conn.request("tools/list", json!({})).await.unwrap();
        assert_eq!(result["tools"], json!([]));
    }

    #[tokio::test]
    async fn request_maps_error_response_to_rpc_error() {
        let mut conn = connect_scripted(|end| async move {
            let (sr, mut sw) = split(end);
            let mut br = tokio::io::BufReader::new(sr);
            let mut line = String::new();
            use tokio::io::AsyncBufReadExt;
            br.read_line(&mut line).await.unwrap();
            sw.write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"no\"}}\n",
            )
            .await
            .unwrap();
        });
        let err = conn.request("tools/call", json!({})).await.unwrap_err();
        assert!(matches!(err, McpError::Rpc { code: -32601, .. }));
    }

    #[tokio::test]
    async fn request_on_closed_stream_returns_closed() {
        let mut conn = connect_scripted(|end| async move {
            drop(end);
        });
        let err = conn.request("tools/list", json!({})).await.unwrap_err();
        assert!(matches!(err, McpError::Closed));
    }
}
