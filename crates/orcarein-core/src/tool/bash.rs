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

/// Max bytes kept per stream (stdout / stderr). A single noisy command (e.g.
/// `dir /s /b` over a huge tree) can emit megabytes; without a cap that whole
/// blob enters the model's prompt and blows the context window. 32 KiB/stream
/// (~64 KiB combined) is plenty to act on while staying well under any limit.
const MAX_STREAM_BYTES: usize = 32 * 1024;

/// Truncate `s` to at most `max_bytes` on a char boundary, appending a notice
/// when anything was dropped. Returns the input unchanged when it already fits.
/// Char-boundary safe (never splits a multibyte UTF-8 sequence).
fn cap(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = s.len() - end;
    format!(
        "{}\n…[truncated {dropped} bytes; {} total]",
        &s[..end],
        s.len()
    )
}

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
        // Cap each stream so one noisy command can't overflow the model context.
        let stdout = cap(&String::from_utf8_lossy(&output.stdout), MAX_STREAM_BYTES);
        let stderr = cap(&String::from_utf8_lossy(&output.stderr), MAX_STREAM_BYTES);

        Ok(ToolOutput::new(format!(
            "exit_code: {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_leaves_short_output_unchanged() {
        assert_eq!(cap("hello", 1024), "hello");
        assert_eq!(cap("", 1024), "");
    }

    #[test]
    fn cap_truncates_long_output_with_notice() {
        let big = "a".repeat(100);
        let out = cap(&big, 10);
        assert!(out.starts_with("aaaaaaaaaa")); // first 10 bytes kept
        assert!(out.contains("truncated 90 bytes; 100 total"));
        assert!(out.len() < big.len() + 64); // notice is small, not a second copy
    }

    #[test]
    fn cap_never_splits_a_multibyte_char() {
        // "中文" is 6 bytes (3 each); capping at 2 lands mid-"中", so it must back
        // off to byte 0 — keeping nothing rather than half a char (no panic).
        let out = cap("中文", 2);
        assert_eq!(out, "\n…[truncated 6 bytes; 6 total]");
        // And capping at 3 keeps exactly "中".
        let out3 = cap("中文", 3);
        assert_eq!(out3, "中\n…[truncated 3 bytes; 6 total]");
    }

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
