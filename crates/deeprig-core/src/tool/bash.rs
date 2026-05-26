//! `bash` — run a shell command and return exit code + stdout + stderr.
//!
//! Cross-platform: `bash -c` on Unix, `cmd /C` on Windows. We keep the
//! tool's name `"bash"` regardless of host so the model has a stable
//! identifier (mirrors `claw-code`'s approach).
//!
//! Non-zero exit codes are **success** from the tool's point of view —
//! the model needs to see the exit code to react. Only failures to
//! spawn the process surface as `ToolError::Io`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::process::Stdio;

use super::{RiskLevel, Tool, ToolError, ToolOutput};

pub struct BashTool;

#[derive(Deserialize)]
struct BashArgs {
    command: String,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command and return its exit code, stdout, and stderr. Uses `bash -c` on Unix and `cmd /C` on Windows."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Risky
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to run.",
                }
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let BashArgs { command } = serde_json::from_value(args)?;

        let (program, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("bash", "-c")
        };

        let output = tokio::process::Command::new(program)
            .arg(flag)
            .arg(&command)
            .stdin(Stdio::null())
            .output()
            .await?;

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        Ok(ToolOutput::new(format!(
            "exit_code: {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let t = BashTool;
        assert_eq!(t.name(), "bash");
        assert_eq!(t.risk_level(), RiskLevel::Risky);
        let s = t.schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["properties"]["command"]["type"], "string");
        assert!(s["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "command"));
        assert_eq!(s["additionalProperties"], false);
    }
}
