//! `skill` — load a named, project-defined instruction pack on demand.
//!
//! The model sees a compact index (name + description) in the system prompt
//! and calls `skill({"name": "..."})` to pull one skill's markdown body into
//! context as the tool result. Read-only (`Safe`); bodies are held in memory
//! (name -> body) and capped at [`MAX_SKILL_BODY_BYTES`]. Mirrors the
//! constructor/`cap`/`ERROR:`-string conventions of `tool/subagent.rs`.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

use super::{RiskLevel, Tool, ToolError, ToolOutput};
use crate::skill::{Skill, MAX_SKILL_BODY_BYTES};

/// Marker appended when [`cap`] truncates an oversized body.
const TRUNC_MARKER: &str = "\n[skill truncated]";

#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

/// The `skill` tool — loads a discovered skill's body by name.
pub struct SkillTool {
    bodies: BTreeMap<String, String>,
}

impl SkillTool {
    /// Builds the tool from discovered skills (drops descriptions — those live
    /// in the system-prompt index). `BTreeMap` keeps the "available skills"
    /// listing deterministic.
    pub fn new(skills: Vec<Skill>) -> Self {
        let bodies = skills.into_iter().map(|s| (s.name, s.body)).collect();
        SkillTool { bodies }
    }
}

/// Caps a body at [`MAX_SKILL_BODY_BYTES`] total, reserving room for the
/// marker and cutting on a char boundary (same discipline as the bash /
/// subagent tools).
fn cap(s: &str) -> String {
    if s.len() <= MAX_SKILL_BODY_BYTES {
        return s.to_string();
    }
    let mut end = MAX_SKILL_BODY_BYTES.saturating_sub(TRUNC_MARKER.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNC_MARKER}", &s[..end])
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load a named skill — a project-specific instruction pack — by name. When \
one of the skills listed in the system prompt applies to the current task, call \
this to load its instructions (returned as the result), then follow them. \
Argument: the skill's `name`."
    }

    fn schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let SkillArgs { name } = serde_json::from_value(args)?;
        match self.bodies.get(&name) {
            Some(body) => Ok(ToolOutput::new(cap(body))),
            None => {
                let available = self.bodies.keys().cloned().collect::<Vec<_>>().join(", ");
                Ok(ToolOutput::new(format!(
                    "ERROR: unknown skill '{name}'. Available skills: {available}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> SkillTool {
        SkillTool::new(vec![
            Skill { name: "release".into(), description: "d".into(), body: "RELEASE BODY".into() },
            Skill { name: "triage".into(), description: "d".into(), body: "TRIAGE BODY".into() },
        ])
    }

    #[tokio::test]
    async fn known_name_returns_body() {
        let out = tool().execute(json!({ "name": "release" })).await.unwrap();
        assert_eq!(out.content, "RELEASE BODY");
    }

    #[tokio::test]
    async fn unknown_name_errors_and_lists_available() {
        let out = tool().execute(json!({ "name": "nope" })).await.unwrap();
        assert!(out.content.starts_with("ERROR:"));
        assert!(out.content.contains("release"));
        assert!(out.content.contains("triage"));
    }

    #[tokio::test]
    async fn missing_name_is_invalid_arguments() {
        let err = tool().execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn risk_level_is_safe() {
        assert_eq!(tool().risk_level(), RiskLevel::Safe);
    }

    #[test]
    fn cap_truncates_multibyte_within_bound() {
        let s = "你".repeat(MAX_SKILL_BODY_BYTES); // ~3x the cap in bytes
        let out = cap(&s);
        assert!(out.len() <= MAX_SKILL_BODY_BYTES);
        assert!(out.ends_with(TRUNC_MARKER));
    }
}
