//! Minimal MCP (Model Context Protocol) **client** over the stdio transport.
//!
//! Hand-rolled JSON-RPC 2.0 (no SDK): `initialize` -> `tools/list` ->
//! `tools/call`. Remote tools are adapted to [`crate::tool::Tool`] and
//! registered into the existing registry. See the design spec for scope.

pub mod client;
pub mod protocol;
pub mod tool;
pub mod transport;

pub use crate::config::McpServerConfig;
pub use client::McpClient;

use std::sync::Arc;

use crate::tool::ToolRegistry;

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

/// Connects each configured server, registers its tools (prefixed
/// `mcp__<server>__<tool>`), and returns the live clients (the caller must
/// keep them alive — dropping a client kills its server). A server that fails
/// to connect/list is logged and skipped; the REPL keeps running.
pub async fn setup_servers(
    cfgs: &[McpServerConfig],
    registry: &mut ToolRegistry,
) -> Vec<Arc<McpClient>> {
    let mut clients = Vec::new();
    for cfg in cfgs {
        let client = match McpClient::connect(cfg).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "warning: MCP server '{}' failed to connect ({e}); skipping",
                    cfg.name
                );
                continue;
            }
        };
        match client.list_tools().await {
            Ok(tools) => {
                for t in tools {
                    let exposed = format!("mcp__{}__{}", cfg.name, t.name);
                    if registry.names().contains(&exposed.as_str()) {
                        eprintln!("warning: duplicate MCP tool name '{exposed}'; skipping");
                        continue;
                    }
                    let desc = t.description.unwrap_or_else(|| exposed.clone());
                    registry.register(Box::new(tool::McpTool::new(
                        exposed,
                        t.name,
                        desc,
                        t.input_schema,
                        client.clone(),
                    )));
                }
            }
            Err(e) => eprintln!(
                "warning: MCP server '{}' tools/list failed ({e}); no tools registered",
                cfg.name
            ),
        }
        clients.push(client);
    }
    clients
}
