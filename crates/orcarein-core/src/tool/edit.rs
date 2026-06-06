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

/// Files larger than this are refused. A single-snippet edit on a multi-MB
/// file is almost always a mistake, and reading it wastes memory — fail loud.
const MAX_EDIT_BYTES: usize = 5 * 1024 * 1024;

/// Rewrites `s`'s line endings to the target file's: CRLF if `crlf`, else LF.
/// Lets a model that emitted `\n` still match a Windows `\r\n` file (and vice
/// versa) — the match stays exact after normalisation, never fuzzy.
fn normalize_eol(s: &str, crlf: bool) -> String {
    let lf = s.replace("\r\n", "\n");
    if crlf {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

/// Whitespace-collapsed copy — used only to diagnose "you got the indentation
/// wrong", never to actually match-and-replace.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 1-based line numbers where `sub` starts in `s` (first five), for the
/// ambiguous-match error so the model can add disambiguating context.
fn match_lines(s: &str, sub: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(pos) = s[start..].find(sub) {
        let abs = start + pos;
        out.push(s[..abs].bytes().filter(|&b| b == b'\n').count() + 1);
        start = abs + sub.len().max(1);
        if out.len() >= 5 {
            break;
        }
    }
    out
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace the unique occurrence of `old_str` with `new_str` in the given file. Copy `old_str` verbatim from the file, including its exact indentation (line endings are matched tolerantly). Errors if `old_str` is absent or appears more than once — refine the snippet until it matches exactly one location."
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

        // Cheap fast-fails before any IO.
        if old_str == new_str {
            return Err(ToolError::Other(
                "old_str and new_str are identical — no-op".into(),
            ));
        }
        if old_str.is_empty() {
            return Err(ToolError::Other(
                "old_str is empty — provide the exact snippet to locate".into(),
            ));
        }

        let original = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ToolError::Other(format!("file not found: {path}")));
            }
            Err(e) => return Err(ToolError::Io(e)),
        };

        if original.len() > MAX_EDIT_BYTES {
            return Err(ToolError::Other(format!(
                "{path} is {} bytes — too large to edit (limit {MAX_EDIT_BYTES}). \
                 Edit a smaller file or split the change.",
                original.len()
            )));
        }

        // Line-ending tolerance: match against the file's dominant ending so a
        // model that sent `\n` still matches a `\r\n` file. Still an EXACT
        // match — never fuzzy.
        let crlf = original.contains("\r\n");
        let old_norm = normalize_eol(&old_str, crlf);
        let new_norm = normalize_eol(&new_str, crlf);

        match original.matches(&old_norm).count() {
            1 => {
                let updated = original.replacen(&old_norm, &new_norm, 1);
                tokio::fs::write(&path, &updated).await?;
                Ok(ToolOutput::new(format!("Edited {path}: 1 replacement")))
            }
            0 => {
                // Distinguish "genuinely absent" from "only the whitespace is
                // off" so the model knows whether to look elsewhere or just fix
                // its indentation. We diagnose — we do NOT silently edit.
                let near = !collapse_ws(&old_norm).is_empty()
                    && collapse_ws(&original).contains(&collapse_ws(&old_norm));
                if near {
                    Err(ToolError::Other(format!(
                        "old_str not found in {path} as written, but a match exists differing \
                         only in whitespace/indentation. Resend old_str copied verbatim from the \
                         file (keep its exact indentation; don't trim or reflow)."
                    )))
                } else {
                    Err(ToolError::Other(format!(
                        "old_str not found in {path}. Read the file and copy the exact snippet, \
                         including indentation."
                    )))
                }
            }
            n => {
                let lines = match_lines(&original, &old_norm)
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(ToolError::Other(format!(
                    "old_str matches {n} times in {path} (around line(s) {lines}); \
                     must be unique — add surrounding context to disambiguate."
                )))
            }
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
