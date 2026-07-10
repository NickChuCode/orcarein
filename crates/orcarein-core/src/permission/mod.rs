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
}
