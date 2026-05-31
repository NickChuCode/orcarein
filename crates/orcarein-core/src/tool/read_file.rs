//! `read_file` — the first concrete `Tool` implementation.
//!
//! Returns the UTF-8 contents of a file. No sandboxing or path
//! validation: in v0.1 the user-facing safety net is the permission
//! prompt that arrives in Ch12, and read access is classified as
//! `RiskLevel::Safe`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{RiskLevel, Tool, ToolError, ToolOutput};

/// The `read_file` tool — reads a UTF-8 text file and returns its
/// contents to the model.
pub struct ReadFileTool;

/// Schema-validated arguments for `read_file`.
#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the local filesystem and return its full contents."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description":
                        "Path to the file. Relative paths are resolved against OrcaRein's current working directory.",
                }
            },
            "required": ["path"],
            "additionalProperties": false,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let ReadFileArgs { path } = serde_json::from_value(args)?;
        let content = tokio::fs::read_to_string(&path).await?;
        Ok(ToolOutput::new(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let t = ReadFileTool;
        assert_eq!(t.name(), "read_file");
        assert_eq!(t.risk_level(), RiskLevel::Safe);
        let schema = t.schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["path"]["type"], "string");
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "path"));
        assert_eq!(schema["additionalProperties"], false);
    }
}
