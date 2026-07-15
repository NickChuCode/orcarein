//! Per-tool permission cache and decision type.
//!
//! The interactive prompt itself lives in the binary (it touches stdin);
//! `orcarein-core` owns only the **pure** pieces: the [`Decision`] enum
//! and a [`PermissionStore`] that remembers a user's *sticky* answers
//! (`AllowAlways` / `DenyAlways`) for the rest of the session.
//!
//! Session-scoped only. Persistence is deferred to Ch15; the user must
//! re-grant always-permissions every time OrcaRein starts.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::tool::protocol::ToolDefinition;

pub mod rules;
pub use rules::{Action, PermissionRequest, PermissionRule, RuleAction, Ruleset};

/// A session-wide, user-chosen authorization posture. The model cannot change
/// it. See spec 2026-07-13 §3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Today's behavior: Safe→allow, Risky→ask.
    #[default]
    Default,
    /// Like Default, but `edit`/`write_file` auto-allow (bash still asks).
    AcceptEdits,
    /// Read-only: write tools are hidden from the model and denied at the gate.
    Plan,
    /// Every Ask becomes Allow (deny rules still fire).
    Yolo,
}

impl PermissionMode {
    /// THE single source of truth for what a mode exposes. Plan is a read-only
    /// whitelist; every other mode allows every tool. A whitelist (not a
    /// blacklist) means unknown tools — MCP, future tools — are hidden in Plan.
    pub fn allows_tool(&self, tool: &str) -> bool {
        match self {
            PermissionMode::Plan => {
                matches!(tool, "read_file" | "list_dir" | "search" | "skill" | "task")
            }
            _ => true,
        }
    }

    /// Drops the tool definitions this mode hides from the model. Lives in core
    /// so the subagent (also core) filters its child defs the same way.
    pub fn filter_defs(&self, defs: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
        defs.into_iter()
            .filter(|d| self.allows_tool(&d.function.name))
            .collect()
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
            PermissionMode::Yolo => "yolo",
        };
        f.write_str(s)
    }
}

impl FromStr for PermissionMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "default" => Ok(PermissionMode::Default),
            "acceptedits" => Ok(PermissionMode::AcceptEdits),
            "plan" => Ok(PermissionMode::Plan),
            "yolo" => Ok(PermissionMode::Yolo),
            other => Err(format!(
                "unknown permission mode '{other}' (expected: default, acceptEdits, plan, yolo)"
            )),
        }
    }
}

impl PermissionMode {
    fn to_u8(self) -> u8 {
        match self {
            PermissionMode::Default => 0,
            PermissionMode::AcceptEdits => 1,
            PermissionMode::Plan => 2,
            PermissionMode::Yolo => 3,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => PermissionMode::AcceptEdits,
            2 => PermissionMode::Plan,
            3 => PermissionMode::Yolo,
            _ => PermissionMode::Default,
        }
    }
}

/// The current mode, shared between the REPL (writer) and the subagent factory
/// (reader). `Arc<AtomicU8>` — zero deps, no lock poisoning. `Relaxed` is fine:
/// tool calls within a turn are sequential (`run_turn` iterates on `&self`).
#[derive(Clone, Debug)]
pub struct SharedMode(Arc<AtomicU8>);

impl SharedMode {
    pub fn new(m: PermissionMode) -> Self {
        SharedMode(Arc::new(AtomicU8::new(m.to_u8())))
    }
    pub fn get(&self) -> PermissionMode {
        PermissionMode::from_u8(self.0.load(Ordering::Relaxed))
    }
    pub fn set(&self, m: PermissionMode) {
        self.0.store(m.to_u8(), Ordering::Relaxed);
    }
}

/// A user's decision about whether a tool may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Allow this single invocation; ask again next time.
    AllowOnce,
    /// Allow this invocation AND all future invocations of this tool
    /// in this session.
    AllowAlways,
    /// Deny this single invocation; ask again next time.
    DenyOnce,
    /// Deny this invocation AND all future invocations of this tool
    /// in this session.
    DenyAlways,
}

impl Decision {
    /// `true` if this decision allows execution.
    pub fn is_allow(self) -> bool {
        matches!(self, Decision::AllowOnce | Decision::AllowAlways)
    }

    /// `true` if this decision should be cached for the session.
    pub fn is_sticky(self) -> bool {
        matches!(self, Decision::AllowAlways | Decision::DenyAlways)
    }
}

/// Caches the user's sticky decisions keyed by tool name for the
/// duration of the session.
///
/// `AllowOnce` / `DenyOnce` are intentionally **never** cached — the
/// "once" semantics is the whole point of those variants.
pub struct PermissionStore {
    cache: HashMap<String, Decision>,
}

impl PermissionStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        PermissionStore {
            cache: HashMap::new(),
        }
    }

    /// The cached sticky decision for `tool`, if any.
    pub fn cached(&self, tool: &str) -> Option<Decision> {
        self.cache.get(tool).copied()
    }

    /// Records a decision. Non-sticky decisions are silently discarded.
    pub fn remember(&mut self, tool: &str, decision: Decision) {
        if decision.is_sticky() {
            self.cache.insert(tool.to_owned(), decision);
        }
    }

    /// Number of remembered (sticky) decisions.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// `true` if no sticky decisions are cached.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

impl Default for PermissionStore {
    fn default() -> Self {
        PermissionStore::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{PermissionMode, SharedMode};
    use crate::tool::protocol::ToolDefinition;
    use std::str::FromStr;

    #[test]
    fn new_is_empty() {
        let s = PermissionStore::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.cached("bash").is_none());
    }

    #[test]
    fn default_matches_new() {
        let a = PermissionStore::default();
        let b = PermissionStore::new();
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn decision_classifies_allow_vs_deny() {
        assert!(Decision::AllowOnce.is_allow());
        assert!(Decision::AllowAlways.is_allow());
        assert!(!Decision::DenyOnce.is_allow());
        assert!(!Decision::DenyAlways.is_allow());
    }

    #[test]
    fn decision_classifies_sticky_vs_once() {
        assert!(!Decision::AllowOnce.is_sticky());
        assert!(Decision::AllowAlways.is_sticky());
        assert!(!Decision::DenyOnce.is_sticky());
        assert!(Decision::DenyAlways.is_sticky());
    }

    #[test]
    fn remember_sticky_then_cached_returns_some() {
        let mut s = PermissionStore::new();
        s.remember("bash", Decision::AllowAlways);
        assert_eq!(s.cached("bash"), Some(Decision::AllowAlways));
        assert_eq!(s.len(), 1);

        s.remember("write_file", Decision::DenyAlways);
        assert_eq!(s.cached("write_file"), Some(Decision::DenyAlways));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn remember_non_sticky_does_not_cache() {
        let mut s = PermissionStore::new();
        s.remember("bash", Decision::AllowOnce);
        s.remember("edit", Decision::DenyOnce);
        assert!(s.is_empty());
        assert!(s.cached("bash").is_none());
        assert!(s.cached("edit").is_none());
    }

    #[test]
    fn unknown_tool_returns_none() {
        let s = PermissionStore::new();
        assert!(s.cached("never_registered").is_none());
    }

    #[test]
    fn later_sticky_overwrites_earlier() {
        let mut s = PermissionStore::new();
        s.remember("bash", Decision::AllowAlways);
        s.remember("bash", Decision::DenyAlways);
        assert_eq!(s.cached("bash"), Some(Decision::DenyAlways));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn plan_allows_only_readonly_tools() {
        let m = PermissionMode::Plan;
        for t in ["read_file", "list_dir", "search", "skill", "task"] {
            assert!(m.allows_tool(t), "plan should allow {t}");
        }
        for t in ["bash", "edit", "write_file", "mcp__srv__do"] {
            assert!(!m.allows_tool(t), "plan should forbid {t}");
        }
    }

    #[test]
    fn nonplan_modes_allow_everything() {
        for m in [
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::Yolo,
        ] {
            for t in ["bash", "edit", "write_file", "read_file", "mcp__x__y"] {
                assert!(m.allows_tool(t), "{m} should allow {t}");
            }
        }
    }

    #[test]
    fn filter_defs_drops_write_tools_in_plan() {
        let defs = vec![
            ToolDefinition::function("read_file", "", serde_json::json!({})),
            ToolDefinition::function("bash", "", serde_json::json!({})),
            ToolDefinition::function("edit", "", serde_json::json!({})),
        ];
        let kept = PermissionMode::Plan.filter_defs(defs);
        let names: Vec<_> = kept.iter().map(|d| d.function.name.as_str()).collect();
        assert_eq!(names, ["read_file"]);
    }

    #[test]
    fn filter_defs_is_identity_for_default() {
        let defs = vec![
            ToolDefinition::function("bash", "", serde_json::json!({})),
            ToolDefinition::function("edit", "", serde_json::json!({})),
        ];
        let kept = PermissionMode::Default.filter_defs(defs.clone());
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn mode_from_str_case_insensitive() {
        assert_eq!(
            PermissionMode::from_str("plan").unwrap(),
            PermissionMode::Plan
        );
        assert_eq!(
            PermissionMode::from_str("acceptEdits").unwrap(),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::from_str("YOLO").unwrap(),
            PermissionMode::Yolo
        );
        assert!(PermissionMode::from_str("nope").is_err());
    }

    #[test]
    fn shared_mode_roundtrips_across_clone() {
        let a = SharedMode::new(PermissionMode::Default);
        let b = a.clone();
        a.set(PermissionMode::Plan);
        assert_eq!(b.get(), PermissionMode::Plan); // shared Arc
    }
}
