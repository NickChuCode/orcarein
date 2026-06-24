//! Adapts one remote MCP tool to the `Tool` trait.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::tool::{RiskLevel, Tool, ToolError, ToolOutput};

use super::client::McpClient;

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
            .map(ToolOutput::new)
            .map_err(|e| ToolError::Other(e.to_string()))
    }
}
