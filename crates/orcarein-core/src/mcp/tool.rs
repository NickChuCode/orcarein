//! Adapts one remote MCP tool to the `Tool` trait.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::tool::{RiskLevel, Tool, ToolError, ToolOutput};

use super::client::McpClient;

/// Cap on a single MCP tool result (mirrors the bash tool's 32 KiB/stream
/// cap). A big a11y snapshot or file dump can't flood the model's context.
const MAX_TOOL_BYTES: usize = 32 * 1024;

/// A remote MCP tool exposed to the agent as a local `Tool`.
pub struct McpTool {
    exposed_name: String, // mcp__<server>__<tool>
    remote_name: String,  // the server's own tool name
    description: String,
    schema: Value,
    client: Arc<McpClient>,
}

impl McpTool {
    pub fn new(
        exposed_name: String,
        remote_name: String,
        description: String,
        schema: Value,
        client: Arc<McpClient>,
    ) -> Self {
        McpTool {
            exposed_name,
            remote_name,
            description,
            schema,
            client,
        }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.exposed_name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn schema(&self) -> Value {
        self.schema.clone()
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Risky
    }
    async fn execute(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.client
            .call_tool(&self.remote_name, args)
            .await
            .map(|t| ToolOutput::new(crate::text::cap(&t, MAX_TOOL_BYTES)))
            .map_err(|e| ToolError::Other(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tool_result_cap_matches_bash_budget() {
        // The MCP cap reuses the shared text::cap at the same 32 KiB budget the
        // bash tool uses; verify the budget constant + shared truncation shape.
        assert_eq!(super::MAX_TOOL_BYTES, 32 * 1024);
        let big = "x".repeat(super::MAX_TOOL_BYTES + 100);
        let out = crate::text::cap(&big, super::MAX_TOOL_BYTES);
        assert!(out.contains("truncated 100 bytes"));
        assert!(out.starts_with(&"x".repeat(64)));
    }
}
