//! `write_file` — overwrite a UTF-8 text file.
//!
//! `RiskLevel::Risky` because the operation mutates the filesystem.
//! Parent directories are NOT created automatically — surfacing typos
//! to the model as `ToolError::Io` is more useful than silently
//! materializing directory trees.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{RiskLevel, Tool, ToolError, ToolOutput};

pub struct WriteFileTool;

#[derive(Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write the given UTF-8 text content to a file, overwriting it if it exists."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Risky
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file. Parent directory must already exist.",
                },
                "content": {
                    "type": "string",
                    "description": "UTF-8 text to write.",
                },
            },
            "required": ["path", "content"],
            "additionalProperties": false,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let WriteFileArgs { path, content } = serde_json::from_value(args)?;
        let bytes = content.len();
        tokio::fs::write(&path, &content).await?;
        Ok(ToolOutput::new(format!("Wrote {bytes} bytes to {path}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let t = WriteFileTool;
        assert_eq!(t.name(), "write_file");
        assert_eq!(t.risk_level(), RiskLevel::Risky);
        let s = t.schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["properties"]["path"]["type"], "string");
        assert_eq!(s["properties"]["content"]["type"], "string");
        let req = s["required"].as_array().unwrap();
        assert!(req.iter().any(|v| v == "path"));
        assert!(req.iter().any(|v| v == "content"));
        assert_eq!(s["additionalProperties"], false);
    }
}
