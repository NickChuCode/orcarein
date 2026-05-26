//! `edit` — replace the unique occurrence of `old_str` in a file.
//!
//! The "unique" requirement is deliberate: it forces the model to send a
//! distinctive enough snippet that the edit is unambiguous, and lets us
//! detect "snippet too generic" failures early without diffing.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{RiskLevel, Tool, ToolError, ToolOutput};

pub struct EditTool;

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_str: String,
    new_str: String,
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace the unique occurrence of `old_str` with `new_str` in the given file. Errors if `old_str` is absent or appears more than once — refine the snippet until it matches exactly one location."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Risky
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path":    { "type": "string", "description": "Path to the file to edit." },
                "old_str": { "type": "string", "description": "Substring to replace. Must match exactly once." },
                "new_str": { "type": "string", "description": "Replacement string." },
            },
            "required": ["path", "old_str", "new_str"],
            "additionalProperties": false,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let EditArgs {
            path,
            old_str,
            new_str,
        } = serde_json::from_value(args)?;

        // Cheap fast-fail: identical strings produce no observable change.
        if old_str == new_str {
            return Err(ToolError::Other(
                "old_str and new_str are identical — no-op".into(),
            ));
        }

        let original = tokio::fs::read_to_string(&path).await?;
        let count = original.matches(&old_str).count();
        match count {
            0 => Err(ToolError::Other(format!("old_str not found in {path}"))),
            1 => {
                let updated = original.replacen(&old_str, &new_str, 1);
                tokio::fs::write(&path, &updated).await?;
                Ok(ToolOutput::new(format!("Edited {path}: 1 replacement")))
            }
            n => Err(ToolError::Other(format!(
                "old_str matches {n} times in {path}; must be unique — refine the snippet"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let t = EditTool;
        assert_eq!(t.name(), "edit");
        assert_eq!(t.risk_level(), RiskLevel::Risky);
        let s = t.schema();
        assert_eq!(s["type"], "object");
        for f in ["path", "old_str", "new_str"] {
            assert_eq!(s["properties"][f]["type"], "string");
        }
        let req = s["required"].as_array().unwrap();
        for f in ["path", "old_str", "new_str"] {
            assert!(req.iter().any(|v| v == f));
        }
        assert_eq!(s["additionalProperties"], false);
    }
}
