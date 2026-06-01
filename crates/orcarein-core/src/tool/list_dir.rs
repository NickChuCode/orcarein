//! `list_dir` — list a single directory non-recursively.
//!
//! `RiskLevel::Risky` — read-only, but enumerating a directory still leaks its
//! structure to the model (e.g. `~/.ssh/` key filenames), so it goes through
//! the permission gate. Output is sorted alphabetically so
//! repeated calls on the same directory are deterministic — that helps
//! the model reason about state across turns.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::{RiskLevel, Tool, ToolError, ToolOutput};

pub struct ListDirTool;

#[derive(Deserialize)]
struct ListDirArgs {
    path: String,
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the entries of a directory (non-recursive). Each line is `d <name>/` for directories or `f <name>` for files."
    }

    fn risk_level(&self) -> RiskLevel {
        // `Risky`, not `Safe`: enumerating a directory leaks its structure to
        // the model. Pointing `list_dir` at e.g. `~/.ssh/` would reveal key
        // filenames without ever reading a byte, so the permission gate must
        // see it. (Ch12 silently let this through.)
        RiskLevel::Risky
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list.",
                }
            },
            "required": ["path"],
            "additionalProperties": false,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let ListDirArgs { path } = serde_json::from_value(args)?;
        let mut read_dir = tokio::fs::read_dir(&path).await?;

        let mut entries: Vec<(String, bool)> = Vec::new();
        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `file_type` may follow symlinks on some platforms; treat
            // anything-non-dir as `f` for v0.1.
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push((name, is_dir));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let mut out = String::new();
        for (name, is_dir) in entries {
            if is_dir {
                out.push_str(&format!("d {name}/\n"));
            } else {
                out.push_str(&format!("f {name}\n"));
            }
        }
        Ok(ToolOutput::new(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let t = ListDirTool;
        assert_eq!(t.name(), "list_dir");
        assert_eq!(t.risk_level(), RiskLevel::Risky);
        let s = t.schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["properties"]["path"]["type"], "string");
        assert!(s["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "path"));
        assert_eq!(s["additionalProperties"], false);
    }

    #[test]
    fn list_dir_is_risky_not_safe() {
        // Directory enumeration leaks structure (e.g. `~/.ssh/` filenames), so
        // it must reach the permission gate. Guard against a silent downgrade
        // back to `Safe`.
        assert_eq!(ListDirTool.risk_level(), RiskLevel::Risky);
    }
}
