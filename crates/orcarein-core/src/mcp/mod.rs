//! Minimal MCP (Model Context Protocol) **client** over the stdio transport.
//!
//! Hand-rolled JSON-RPC 2.0 (no SDK): `initialize` -> `tools/list` ->
//! `tools/call`. Remote tools are adapted to [`crate::tool::Tool`] and
//! registered into the existing registry. See the design spec for scope.

pub mod client;
pub mod protocol;
pub mod transport;

/// Errors from the MCP client.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("failed to spawn MCP server: {0}")]
    Spawn(std::io::Error),
    #[error("io error talking to MCP server: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed JSON from MCP server: {0}")]
    Json(#[from] serde_json::Error),
    #[error("MCP server returned an error: [{code}] {message}")]
    Rpc { code: i64, message: String },
    #[error("MCP server closed the connection")]
    Closed,
    #[error("MCP protocol error: {0}")]
    Protocol(String),
}
